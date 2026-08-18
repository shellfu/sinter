//! Incremental derivation: apply changed/removed file facts and keep every
//! derived table (nodes, edges, name/trigram/token indexes, unresolved refs)
//! consistent for exactly the touched files. Nothing here scans the corpus.

use std::collections::BTreeSet;

use redb::{ReadableDatabase, ReadableMultimapTable, ReadableTable};
use sinter_core::{Edge, Evidence, FileFacts, Reference};

use crate::error::StoreError;
use crate::search::{node_tokens, trigrams};
use crate::store::{
    FILE_FACTS, FILE_HASH, IMPORTS, IN_EDGES, INTERN, INTERN_REV, META, NAME_NODES, NAME_REFS,
    NODES, OUT_EDGES, Store, TOKENS_WORDS, TRIGRAMS, UNRESOLVED,
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

impl Store {
    /// Apply extraction results: `changed` files get their derived state
    /// replaced, `removed` files get theirs deleted. One transaction.
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
            let mut out = txn.open_multimap_table(OUT_EDGES)?;
            let mut inn = txn.open_multimap_table(IN_EDGES)?;
            let mut unresolved = txn.open_multimap_table(UNRESOLVED)?;
            let mut name_refs = txn.open_multimap_table(NAME_REFS)?;
            let mut name_nodes = txn.open_multimap_table(NAME_NODES)?;
            let mut grams = txn.open_multimap_table(TRIGRAMS)?;
            let mut tokens = txn.open_multimap_table(TOKENS_WORDS)?;
            let mut imports = txn.open_multimap_table(IMPORTS)?;
            let mut intern = txn.open_table(INTERN)?;
            let mut intern_rev = txn.open_table(INTERN_REV)?;
            let mut meta = txn.open_table(META)?;
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
            }

            // Install new derived state for changed files.
            for facts in changed {
                let file = facts.file.as_str();
                facts_table.insert(file, encode_facts(facts)?.as_slice())?;
                // content hash is deliberately NOT written here: it commits
                // last (commit_hashes), so a crash mid-derivation re-runs
                // these files as changed instead of freezing the damage.
                for node in &facts.nodes {
                    let id = node.id.as_str();
                    nodes.insert(id, postcard::to_allocvec(node)?.as_slice())?;
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
                    name_nodes.insert(node.name.as_str(), interned)?;
                    for gram in trigrams(&node.name) {
                        grams.insert(gram.as_str(), interned)?;
                    }
                    for word in node_tokens(node) {
                        tokens.insert(word.as_str(), interned)?;
                    }
                    delta.def_names.insert(node.name.clone());
                }
                for edge in &facts.contains {
                    let bytes = postcard::to_allocvec(edge)?;
                    out.insert(edge.src.as_str(), bytes.as_slice())?;
                    inn.insert(edge.dst.as_str(), bytes.as_slice())?;
                }
                for r in &facts.references {
                    name_refs.insert(r.name.as_str(), file)?;
                    if r.relation == sinter_core::Relation::Imports {
                        imports.insert(file, postcard::to_allocvec(r)?.as_slice())?;
                    }
                }
            }
            meta.insert("intern_next", next_intern)?;
        }
        txn.commit()?;
        Ok(delta)
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

    /// Drop non-structural (resolution) edges whose src node lives in one of
    /// these files, ahead of re-resolution.
    pub fn remove_resolution_edges(&self, files: &BTreeSet<String>) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let facts_table = txn.open_table(FILE_FACTS)?;
            let mut out = txn.open_multimap_table(OUT_EDGES)?;
            let mut inn = txn.open_multimap_table(IN_EDGES)?;
            for file in files {
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
            for edge in edges {
                let bytes = postcard::to_allocvec(edge)?;
                out.insert(edge.src.as_str(), bytes.as_slice())?;
                inn.insert(edge.dst.as_str(), bytes.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Replace the unresolved set for these files.
    pub fn replace_unresolved(
        &self,
        files: &BTreeSet<String>,
        unresolved: &[Reference],
    ) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_multimap_table(UNRESOLVED)?;
            for file in files {
                table.remove_all(file.as_str())?;
            }
            for r in unresolved {
                table.insert(r.file.as_str(), postcard::to_allocvec(r)?.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }
}

fn collect_values(
    values: redb::MultimapValue<'_, &'static [u8]>,
) -> Result<Vec<Vec<u8>>, StoreError> {
    let mut out = Vec::new();
    for guard in values {
        out.push(guard?.value().to_vec());
    }
    Ok(out)
}
