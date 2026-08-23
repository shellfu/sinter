//! Incremental derivation: apply changed/removed file facts and keep every
//! derived table (nodes, edges, name/trigram/token indexes, unresolved refs)
//! consistent for exactly the touched files. Nothing here scans the corpus.

use std::collections::BTreeSet;

use redb::{ReadableDatabase, ReadableMultimapTable, ReadableTable};
use sinter_core::{CorpusScope, Edge, Evidence, FileFacts, UnresolvedReference};

use crate::error::StoreError;
use crate::search::{node_tokens, trigrams};
use crate::store::{
    BODY_TERMS, FILE_FACTS, FILE_HASH, FILE_SCOPE, IMPORTS, IN_EDGES, INTERN, INTERN_REV, META,
    NAME_NODES, NAME_REFS, NODE_SCOPE, NODES, OUT_EDGES, PENDING, Store, TOKENS_WORDS, TRIGRAMS,
    UNRESOLVED,
};

/// FileFacts blobs are zstd-compressed postcard (19% of stored bytes at
/// level 1 cost ~µs per file; read only on incremental paths, never hot).
pub(crate) fn encode_facts(facts: &FileFacts) -> Result<Vec<u8>, StoreError> {
    let raw = postcard::to_allocvec(facts)?;
    zstd::encode_all(raw.as_slice(), 1).map_err(StoreError::Compress)
}

pub(crate) fn decode_facts(bytes: &[u8]) -> Result<FileFacts, StoreError> {
    let raw = zstd::decode_all(bytes).map_err(StoreError::Compress)?;
    Ok(postcard::from_bytes(&raw)?)
}

/// What an update invalidated: definition names whose binding targets may
/// have changed, and files that held resolution edges into the touched
/// files (their import/module bindings were torn down and must re-resolve
/// even when no name they use changed — e.g. package imports bound to a
/// file node).
#[derive(Debug, Default)]
pub struct NameDelta {
    pub def_names: BTreeSet<String>,
    pub dependent_files: BTreeSet<String>,
}

/// File a node id belongs to: `{file}#...` or a bare file-node id.
fn file_of_id(id: &str) -> &str {
    id.split_once('#').map_or(id, |(file, _)| file)
}

/// Everything derivable from one file's facts without table access,
/// precomputed off the writer thread: serialization and index tokenization
/// were 55% of a cold build inside the single write transaction.
struct PreparedFile {
    /// zstd facts blob for FILE_FACTS.
    encoded: Vec<u8>,
    /// Per node (aligned with `facts.nodes`): postcard blob, trigram list,
    /// token set.
    nodes: Vec<(Vec<u8>, Vec<String>, BTreeSet<String>)>,
    /// Postcard blobs aligned with `facts.contains`.
    edges: Vec<Vec<u8>>,
    /// Postcard blobs of the Imports-relation references, in order.
    imports: Vec<Vec<u8>>,
}

fn prepare_file(facts: &FileFacts) -> Result<PreparedFile, StoreError> {
    Ok(PreparedFile {
        encoded: encode_facts(facts)?,
        nodes: facts
            .nodes
            .iter()
            .map(|n| Ok((postcard::to_allocvec(n)?, trigrams(&n.name), node_tokens(n))))
            .collect::<Result<_, StoreError>>()?,
        edges: facts
            .contains
            .iter()
            .map(postcard::to_allocvec)
            .collect::<Result<_, _>>()?,
        imports: facts
            .references
            .iter()
            .filter(|r| r.relation == sinter_core::Relation::Imports)
            .map(postcard::to_allocvec)
            .collect::<Result<_, _>>()?,
    })
}

/// Order-preserving parallel map over all available cores. Plain
/// std::thread::scope: this crate has no rayon, and one work-stealing
/// index is all a per-file map needs.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(items.len());
    if workers <= 1 {
        return items.iter().map(f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let chunks: Vec<Vec<(usize, R)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        out.push((i, f(item)));
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("prepare worker panicked"))
            .collect()
    });
    let mut slots: Vec<Option<R>> = std::iter::repeat_with(|| None).take(items.len()).collect();
    for chunk in chunks {
        for (i, r) in chunk {
            slots[i] = Some(r);
        }
    }
    slots
        .into_iter()
        .map(|s| s.expect("every index visited"))
        .collect()
}

/// Pending-delta wire form: (def_names, dependent_files).
type PendingSets = (BTreeSet<String>, BTreeSet<String>);

/// Files whose prepared rows are buffered at once during install.
/// Bounds precompute memory to a few hundred MB on huge corpora while
/// keeping per-chunk sorted index inserts and full-core parallelism.
const PREPARE_CHUNK: usize = 512;

impl Store {
    /// Apply extraction results: `changed` files get their derived state
    /// replaced, `removed` files get theirs deleted. One transaction.
    ///
    /// The returned delta is also merged into a persistent pending record
    /// committed atomically with this transaction; it survives a crash
    /// between this call and the resolution pass, and the pipeline clears
    /// it (see [`Store::clear_pending_delta`]) only after hash stamps
    /// commit. Replaying it on the next build recovers dependent-file
    /// bindings that would otherwise be lost with their in-edges.
    pub fn update_files(
        &self,
        changed: &[FileFacts],
        removed: &[String],
    ) -> Result<NameDelta, StoreError> {
        let mut delta = NameDelta::default();
        // Clean build: nothing touched, no write transaction.
        if changed.is_empty() && removed.is_empty() {
            return Ok(delta);
        }
        let txn = self.db.begin_write()?;
        {
            let mut nodes = txn.open_table(NODES)?;
            let mut facts_table = txn.open_table(FILE_FACTS)?;
            let mut hash_table = txn.open_table(FILE_HASH)?;
            let mut scope_table = txn.open_table(FILE_SCOPE)?;
            let mut node_scope_table = txn.open_table(NODE_SCOPE)?;
            let mut out = txn.open_multimap_table(OUT_EDGES)?;
            let mut inn = txn.open_multimap_table(IN_EDGES)?;
            let mut unresolved = txn.open_multimap_table(UNRESOLVED)?;
            let mut name_refs = txn.open_multimap_table(NAME_REFS)?;
            let mut name_nodes = txn.open_multimap_table(NAME_NODES)?;
            let mut grams = txn.open_multimap_table(TRIGRAMS)?;
            let mut tokens = txn.open_multimap_table(TOKENS_WORDS)?;
            let mut body = txn.open_multimap_table(BODY_TERMS)?;
            let mut imports = txn.open_multimap_table(IMPORTS)?;
            let mut intern = txn.open_table(INTERN)?;
            let mut intern_rev = txn.open_table(INTERN_REV)?;
            let mut meta = txn.open_table(META)?;
            let mut pending = txn.open_table(PENDING)?;
            let mut next_intern = meta.get("intern_next")?.map(|g| g.value()).unwrap_or(0);

            let touched: Vec<&str> = changed
                .iter()
                .map(|f| f.file.as_str())
                .chain(removed.iter().map(String::as_str))
                .collect();

            // Tear down old derived state for every touched file.
            for file in &touched {
                let Some(old): Option<FileFacts> = facts_table
                    .get(*file)?
                    .map(|g| decode_facts(g.value()))
                    .transpose()?
                else {
                    continue;
                };
                for (id, terms) in &old.body_terms {
                    if let Some(interned) = intern_rev.get(id.as_str())?.map(|g| g.value()) {
                        for term in terms {
                            body.remove(term.as_str(), interned)?;
                        }
                    }
                }
                for node in &old.nodes {
                    let id = node.id.as_str();
                    // Bidirectional edge cleanup: every edge listed on this
                    // node also lives on its opposite endpoint's list.
                    let out_bytes: Vec<Vec<u8>> = collect_values(out.get(id)?)?;
                    for bytes in out_bytes {
                        let edge: Edge = postcard::from_bytes(&bytes)?;
                        inn.remove(edge.dst.as_str(), bytes.as_slice())?;
                    }
                    let in_bytes: Vec<Vec<u8>> = collect_values(inn.get(id)?)?;
                    for bytes in in_bytes {
                        let edge: Edge = postcard::from_bytes(&bytes)?;
                        out.remove(edge.src.as_str(), bytes.as_slice())?;
                        // The src file just lost a binding into this file;
                        // it must re-resolve even if no name it uses changed.
                        if edge.evidence != Evidence::Structural {
                            delta
                                .dependent_files
                                .insert(file_of_id(edge.src.as_str()).to_string());
                        }
                    }
                    out.remove_all(id)?;
                    inn.remove_all(id)?;
                    nodes.remove(id)?;
                    node_scope_table.remove(id)?;
                    let interned_opt = intern_rev.get(id)?.map(|g| g.value());
                    if let Some(interned) = interned_opt {
                        name_nodes.remove(node.name.as_str(), interned)?;
                        for gram in trigrams(&node.name) {
                            grams.remove(gram.as_str(), interned)?;
                        }
                        for word in node_tokens(node) {
                            tokens.remove(word.as_str(), interned)?;
                        }
                        intern.remove(interned)?;
                        intern_rev.remove(id)?;
                    }
                    delta.def_names.insert(node.name.clone());
                }
                for r in &old.references {
                    name_refs.remove(r.name.as_str(), *file)?;
                }
                imports.remove_all(*file)?;
                unresolved.remove_all(*file)?;
                facts_table.remove(*file)?;
                hash_table.remove(*file)?;
                scope_table.remove(*file)?;
            }

            // Install new derived state for changed files. CPU-heavy
            // derivation (postcard, zstd, trigrams, tokens) runs parallel
            // off the writer thread, chunked so the buffered rows stay a
            // few hundred MB instead of one prepared corpus (peak-RSS
            // budget). Multimap index rows are inserted sorted by key per
            // chunk: keyed B-tree inserts in key order touch far fewer
            // pages than per-node interleaving.
            for chunk in changed.chunks(PREPARE_CHUNK) {
                let prepared: Vec<PreparedFile> = par_map(chunk, prepare_file)
                    .into_iter()
                    .collect::<Result<_, _>>()?;
                let mut name_pairs: Vec<(&str, u32)> = Vec::new();
                let mut gram_pairs: Vec<(&str, u32)> = Vec::new();
                let mut token_pairs: Vec<(&str, u32)> = Vec::new();
                let mut body_pairs: Vec<(&str, u32)> = Vec::new();
                let mut ref_pairs: Vec<(&str, &str)> = Vec::new();
                for (facts, prep) in chunk.iter().zip(&prepared) {
                    let file = facts.file.as_str();
                    facts_table.insert(file, prep.encoded.as_slice())?;
                    scope_table.insert(file, CorpusScope::classify_path(file).as_str())?;
                    for (id, scope) in &facts.scopes {
                        node_scope_table.insert(id.as_str(), scope.as_str())?;
                    }
                    // content hash is deliberately NOT written here: it commits
                    // last (commit_hashes), so a crash mid-derivation re-runs
                    // these files as changed instead of freezing the damage.
                    for (node, (blob, node_grams, node_words)) in
                        facts.nodes.iter().zip(&prep.nodes)
                    {
                        let id = node.id.as_str();
                        nodes.insert(id, blob.as_slice())?;
                        let interned_existing = intern_rev.get(id)?.map(|g| g.value());
                        let interned = match interned_existing {
                            Some(existing) => existing,
                            None => {
                                let assigned = next_intern;
                                next_intern += 1;
                                intern.insert(assigned, id)?;
                                intern_rev.insert(id, assigned)?;
                                assigned
                            }
                        };
                        name_pairs.push((node.name.as_str(), interned));
                        for gram in node_grams {
                            gram_pairs.push((gram.as_str(), interned));
                        }
                        for word in node_words {
                            token_pairs.push((word.as_str(), interned));
                        }
                        delta.def_names.insert(node.name.clone());
                    }
                    for (id, terms) in &facts.body_terms {
                        if let Some(interned) = intern_rev.get(id.as_str())?.map(|g| g.value()) {
                            body_pairs.extend(terms.iter().map(|t| (t.as_str(), interned)));
                        }
                    }
                    for (edge, bytes) in facts.contains.iter().zip(&prep.edges) {
                        out.insert(edge.src.as_str(), bytes.as_slice())?;
                        inn.insert(edge.dst.as_str(), bytes.as_slice())?;
                    }
                    for r in &facts.references {
                        ref_pairs.push((r.name.as_str(), file));
                    }
                    for bytes in &prep.imports {
                        imports.insert(file, bytes.as_slice())?;
                    }
                }
                name_pairs.sort_unstable();
                for (name, interned) in name_pairs {
                    name_nodes.insert(name, interned)?;
                }
                gram_pairs.sort_unstable();
                for (gram, interned) in gram_pairs {
                    grams.insert(gram, interned)?;
                }
                token_pairs.sort_unstable();
                for (word, interned) in token_pairs {
                    tokens.insert(word, interned)?;
                }
                body_pairs.sort_unstable();
                for (word, interned) in body_pairs {
                    body.insert(word, interned)?;
                }
                ref_pairs.sort_unstable();
                for (name, file) in ref_pairs {
                    name_refs.insert(name, file)?;
                }
            }
            meta.insert("intern_next", next_intern)?;

            // Persist the delta (merged with any crash residue) atomically
            // with the derivation it describes.
            let mut merged: PendingSets = match pending.get(PENDING_KEY)? {
                Some(guard) => postcard::from_bytes(guard.value())?,
                None => Default::default(),
            };
            merged.0.extend(delta.def_names.iter().cloned());
            merged.1.extend(delta.dependent_files.iter().cloned());
            pending.insert(PENDING_KEY, postcard::to_allocvec(&merged)?.as_slice())?;
        }
        txn.commit()?;
        Ok(delta)
    }

    /// The crash-residue delta: the union of every [`Store::update_files`]
    /// delta since the last [`Store::clear_pending_delta`]. Empty on a
    /// cleanly finished build.
    pub fn pending_delta(&self) -> Result<NameDelta, StoreError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table(PENDING) {
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(NameDelta::default()),
            other => other?,
        };
        let Some(guard) = table.get(PENDING_KEY)? else {
            return Ok(NameDelta::default());
        };
        let (def_names, dependent_files): PendingSets = postcard::from_bytes(guard.value())?;
        Ok(NameDelta {
            def_names,
            dependent_files,
        })
    }

    /// Mark the current build's derivation fully resolved and stamped.
    /// Call only after hash stamps commit; a crash before this leaves the
    /// pending delta for the next build to replay (idempotent — replay
    /// re-resolves files into the same edges).
    pub fn clear_pending_delta(&self) -> Result<(), StoreError> {
        let residue = self.pending_delta()?;
        // Clean builds stay write-free: no residue, no write transaction.
        if residue.def_names.is_empty() && residue.dependent_files.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(PENDING)?;
            table.remove(PENDING_KEY)?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Mark files fully derived by recording their content hashes. Call
    /// only after every derived table (edges, unresolved) is consistent.
    /// Stores a bare hash (no stat stamp), so the next scan re-hashes
    /// these files once; the build path uses [`Store::commit_stamps`].
    pub fn commit_hashes(&self, changed: &[FileFacts]) -> Result<(), StoreError> {
        if changed.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write()?;
        {
            let mut hash_table = txn.open_table(FILE_HASH)?;
            for facts in changed {
                hash_table.insert(facts.file.as_str(), facts.content_hash.as_str())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// [`Store::commit_hashes`] with the stat identity attached: the scan
    /// reuses each stored hash while (mtime, len) still match. Also the
    /// stamp-refresh path for touched-but-unchanged files. Empty input
    /// opens no write transaction (the clean-build no-op path).
    pub fn commit_stamps(&self, rows: &[(String, crate::FileStamp)]) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write()?;
        {
            let mut hash_table = txn.open_table(FILE_HASH)?;
            for (file, stamp) in rows {
                hash_table.insert(file.as_str(), stamp.encode().as_str())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Files containing references with any of these names — the set an
    /// update invalidates beyond the changed files themselves.
    pub fn ref_files(&self, names: &BTreeSet<String>) -> Result<BTreeSet<String>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(NAME_REFS)?;
        let mut files = BTreeSet::new();
        for name in names {
            for guard in table.get(name.as_str())? {
                files.insert(guard?.value().to_string());
            }
        }
        Ok(files)
    }

    /// Read-only lookahead for [`Store::apply_resolution`]: the dst files
    /// of Dynamic edges whose src node lives in one of these files. Those
    /// files' trait-impl facts must join the re-resolution set or their
    /// fan-out edges would be silently lost (dynamic edges are src-owned
    /// like every resolution edge, but derived from dst-file facts).
    pub fn dynamic_edge_dst_files(
        &self,
        files: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, StoreError> {
        let mut dynamic_dst_files = BTreeSet::new();
        let txn = self.db.begin_read()?;
        let facts_table = txn.open_table(FILE_FACTS)?;
        let out = txn.open_multimap_table(OUT_EDGES)?;
        for file in files {
            let Some(facts): Option<FileFacts> = facts_table
                .get(file.as_str())?
                .map(|g| decode_facts(g.value()))
                .transpose()?
            else {
                continue;
            };
            for node in &facts.nodes {
                for guard in out.get(node.id.as_str())? {
                    let edge: Edge = postcard::from_bytes(guard?.value())?;
                    if edge.evidence == Evidence::Dynamic {
                        dynamic_dst_files.insert(file_of_id(edge.dst.as_str()).to_string());
                    }
                }
            }
        }
        Ok(dynamic_dst_files)
    }

    /// Commit one resolution pass atomically: drop non-structural
    /// (resolution) edges whose src node lives in a `teardown` file,
    /// insert the re-derived `edges` (both directions), and replace the
    /// unresolved set for `unresolved_files`. One transaction — a crash
    /// leaves either the old resolution state or the new one, never a
    /// torn-down middle.
    pub fn apply_resolution(
        &self,
        teardown: &BTreeSet<String>,
        edges: &[Edge],
        unresolved_files: &BTreeSet<String>,
        unresolved: &[UnresolvedReference],
    ) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let facts_table = txn.open_table(FILE_FACTS)?;
            let mut out = txn.open_multimap_table(OUT_EDGES)?;
            let mut inn = txn.open_multimap_table(IN_EDGES)?;
            for file in teardown {
                let Some(facts): Option<FileFacts> = facts_table
                    .get(file.as_str())?
                    .map(|g| decode_facts(g.value()))
                    .transpose()?
                else {
                    continue;
                };
                for node in &facts.nodes {
                    let bytes_list = collect_values(out.get(node.id.as_str())?)?;
                    for bytes in bytes_list {
                        let edge: Edge = postcard::from_bytes(&bytes)?;
                        if edge.evidence != Evidence::Structural {
                            out.remove(node.id.as_str(), bytes.as_slice())?;
                            inn.remove(edge.dst.as_str(), bytes.as_slice())?;
                        }
                    }
                }
            }
            for edge in representative_sites(edges) {
                let bytes = postcard::to_allocvec(edge)?;
                out.insert(edge.src.as_str(), bytes.as_slice())?;
                inn.insert(edge.dst.as_str(), bytes.as_slice())?;
            }
            let mut table = txn.open_multimap_table(UNRESOLVED)?;
            for file in unresolved_files {
                table.remove_all(file.as_str())?;
            }
            for unresolved in unresolved {
                table.insert(
                    unresolved.reference.file.as_str(),
                    postcard::to_allocvec(unresolved)?.as_slice(),
                )?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Insert resolution edges (both directions).
    pub fn insert_edges(&self, edges: &[Edge]) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut out = txn.open_multimap_table(OUT_EDGES)?;
            let mut inn = txn.open_multimap_table(IN_EDGES)?;
            for edge in representative_sites(edges) {
                let bytes = postcard::to_allocvec(edge)?;
                out.insert(edge.src.as_str(), bytes.as_slice())?;
                inn.insert(edge.dst.as_str(), bytes.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }
}

/// One edge per identity: several call sites binding the same
/// (src, dst, relation, evidence) keep a single representative site (the
/// smallest — deterministic), so `site` never multiplies edge cardinality.
/// The multimap's byte-identical dedup handles exact repeats; this handles
/// same-identity edges whose sites differ.
fn representative_sites(edges: &[Edge]) -> Vec<&Edge> {
    let mut ordered: Vec<&Edge> = edges.iter().collect();
    // Edge's derived Ord puts `site` last, so identity groups are adjacent
    // and the smallest site sorts first within each group.
    ordered.sort();
    ordered.dedup_by(|a, b| a.identity() == b.identity());
    ordered
}

const PENDING_KEY: &str = "delta";

fn collect_values(
    values: redb::MultimapValue<'_, &'static [u8]>,
) -> Result<Vec<Vec<u8>>, StoreError> {
    let mut out = Vec::new();
    for guard in values {
        out.push(guard?.value().to_vec());
    }
    Ok(out)
}
