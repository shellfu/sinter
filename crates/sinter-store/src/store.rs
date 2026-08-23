use std::collections::HashMap;
use std::path::Path;

use redb::{
    Database, MultimapTableDefinition, ReadableDatabase, ReadableMultimapTable, ReadableTable,
    ReadableTableMetadata, TableDefinition,
};
use sinter_core::{
    CorpusScope, Edge, FileFacts, Graph, Node, NodeId, Reference, UnresolvedReference,
};

use crate::error::StoreError;

pub(crate) const NODES: TableDefinition<&str, &[u8]> = TableDefinition::new("nodes");
/// Adjacency, keyed by src id. Values are postcard-encoded edges; the
/// multimap holds parallel edges and dedups byte-identical ones, matching
/// `Graph` semantics.
pub(crate) const OUT_EDGES: MultimapTableDefinition<&str, &[u8]> =
    MultimapTableDefinition::new("out_edges");
/// Reverse adjacency, keyed by dst id — reverse blast radius reads this.
pub(crate) const IN_EDGES: MultimapTableDefinition<&str, &[u8]> =
    MultimapTableDefinition::new("in_edges");
/// Unresolved references, keyed by file. First-class outcome (R2): stored
/// and countable, replaced per file on re-resolution.
pub(crate) const UNRESOLVED: MultimapTableDefinition<&str, &[u8]> =
    MultimapTableDefinition::new("unresolved");
/// Per-file extraction truth, content-addressed. Every derived table
/// (nodes, edges, indexes) rebuilds from here for exactly the changed files.
pub(crate) const FILE_FACTS: TableDefinition<&str, &[u8]> = TableDefinition::new("file_facts");
/// file -> content hash, decoded without touching the facts blob.
pub(crate) const FILE_HASH: TableDefinition<&str, &str> = TableDefinition::new("file_hash");
/// Repo-relative file -> corpus role. Nodes inherit their file's scope at
/// query time, avoiding duplicated metadata in every node blob.
pub(crate) const FILE_SCOPE: TableDefinition<&str, &str> = TableDefinition::new("file_scope");
/// node id -> corpus role override for nodes whose role differs from their
/// file's (inline test modules, generated banners). Sparse; see `scope`.
pub(crate) const NODE_SCOPE: TableDefinition<&str, &str> = TableDefinition::new("node_scope");
/// reference name -> files containing a reference with that name; the
/// resolution invalidation index.
pub(crate) const NAME_REFS: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("name_refs");
/// symbol name -> interned node ids, exact-match query index.
pub(crate) const NAME_NODES: MultimapTableDefinition<&str, u32> =
    MultimapTableDefinition::new("name_nodes");
/// lowercased trigram -> interned node ids, fuzzy query index.
pub(crate) const TRIGRAMS: MultimapTableDefinition<&str, u32> =
    MultimapTableDefinition::new("trigrams");
/// lowercased word -> interned node ids: recall index over name subwords,
/// doc, signature, and path segments (see `search::node_tokens`).
/// Values are interned (u32) — node-id strings repeated dozens of times
/// were 58% of stored bytes before interning (bench finding).
pub(crate) const TOKENS_WORDS: MultimapTableDefinition<&str, u32> =
    MultimapTableDefinition::new("tokens_words");
/// lowercased body-only word -> interned node ids (see
/// `FileFacts::body_terms`): evidence for concept questions whose terms
/// appear only inside a function body.
pub(crate) const BODY_TERMS: MultimapTableDefinition<&str, u32> =
    MultimapTableDefinition::new("body_terms");
/// Interner: u32 -> node id string and its reverse. Index tables store the
/// u32; readers translate back on materialization.
pub(crate) const INTERN: TableDefinition<u32, &str> = TableDefinition::new("intern");
pub(crate) const INTERN_REV: TableDefinition<&str, u32> = TableDefinition::new("intern_rev");
/// file -> import references only (compact). Re-export chain walking needs
/// every file's imports without decoding full facts corpus-wide.
pub(crate) const IMPORTS: MultimapTableDefinition<&str, &[u8]> =
    MultimapTableDefinition::new("imports");
/// Single-row schema stamp; a mismatch on open wipes the database (facts
/// are derivable, a stale-format db is not worth migrating).
pub(crate) const META: TableDefinition<&str, u32> = TableDefinition::new("meta");
/// Fingerprints of non-source resolution inputs (SCIP index, manifest
/// module roots), keyed by input kind. A change re-resolves the corpus
/// without re-extracting. Additive table — absent in older dbs, which
/// makes the first build after upgrade re-resolve once.
pub(crate) const RESOLVE_META: TableDefinition<&str, &str> = TableDefinition::new("resolve_meta");
/// Single-row crash-recovery intent: the union of update deltas not yet
/// followed by a completed resolution pass (see `Store::update_files` /
/// `Store::clear_pending_delta`). Additive table — absent in older dbs,
/// read as empty.
pub(crate) const PENDING: TableDefinition<&str, &[u8]> = TableDefinition::new("pending_delta");
// v10: explicit per-file corpus scope. Older graphs are derived state and
// rebuild so every query observes classified metadata, never a mixed corpus.
// v11: node-level scope overrides (node_scope table, FileFacts.scopes).
// v12: body-identifier terms (body_terms table, FileFacts.body_terms).
const SCHEMA_VERSION: u32 = 12;

/// Per-file freshness record: content hash plus the stat identity it was
/// hashed at. On Unix the identity combines modification and change time,
/// so rewriting bytes while restoring mtime cannot hide an edit. Encoded
/// in FILE_HASH as `hash|identity_nanos|len`; old mtime-only rows decode
/// but miss once against the new identity and are refreshed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStamp {
    pub hash: String,
    pub identity_nanos: u128,
    pub len: u64,
}

impl FileStamp {
    pub(crate) fn encode(&self) -> String {
        format!("{}|{}|{}", self.hash, self.identity_nanos, self.len)
    }

    pub(crate) fn decode(value: &str) -> Self {
        let mut parts = value.split('|');
        let hash = parts.next().unwrap_or_default().to_string();
        Self {
            hash,
            identity_nanos: parts.next().and_then(|p| p.parse().ok()).unwrap_or(0),
            // len 0 never matches: scan only stamps non-empty files.
            len: parts.next().and_then(|p| p.parse().ok()).unwrap_or(0),
        }
    }
}

impl Store {
    /// The schema version this binary writes.
    pub const CURRENT_SCHEMA: u32 = SCHEMA_VERSION;

    /// Read a database's schema stamp without opening for write and
    /// without triggering the wipe-on-mismatch in [`Store::create`].
    pub fn schema_of(path: impl AsRef<Path>) -> Result<Option<u32>, StoreError> {
        Self::open(path)?.schema()
    }
}

/// Persistent graph store. Point queries never load the whole graph.
pub struct Store {
    pub(crate) db: Database,
}

/// redb opens are exclusive, so a query racing a short-lived build (or a
/// queue of sibling queries — parallel agents fan out dozens) sees
/// AlreadyOpen. Backoff rides out the queue; a handle held by a
/// long-lived process still errors after the budget. Windows gets a
/// longer budget: file locks release lazily after process exit (handle
/// teardown + AV scans), so a waiter there can outlive 5s of real
/// contention that unix clears instantly.
pub(crate) fn open_retrying(
    path: &Path,
    open: fn(&Path) -> Result<Database, redb::DatabaseError>,
) -> Result<Database, redb::DatabaseError> {
    let budget = std::time::Duration::from_secs(if cfg!(windows) { 20 } else { 5 });
    let started = std::time::Instant::now();
    let mut delay = std::time::Duration::from_millis(10);
    loop {
        match open(path) {
            Err(redb::DatabaseError::DatabaseAlreadyOpen) if started.elapsed() < budget => {
                std::thread::sleep(delay);
                delay = (delay * 2).min(std::time::Duration::from_millis(200));
            }
            other => return other,
        }
    }
}

/// Create (or open) any redb database under sinter's contention policy —
/// the one named owner of open-retry behavior for auxiliary databases
/// (workspace link store) that are not the repository [`Store`].
pub fn create_database(path: &Path) -> Result<Database, StoreError> {
    Ok(open_retrying(path, |p| Database::create(p))?)
}

impl Store {
    /// Create or open the database and ensure all tables exist. An
    /// existing database with a different schema version is deleted and
    /// recreated — the next build re-derives everything from source.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if path.exists() {
            let db = open_retrying(path, |p| Database::open(p))?;
            let txn = db.begin_read()?;
            let stored = match txn.open_table(META) {
                Ok(table) => table.get("schema")?.map(|g| g.value()),
                Err(redb::TableError::TableDoesNotExist(_)) => None,
                Err(e) => return Err(e.into()),
            };
            // Older schema: wipe and rebuild forward from source. Newer:
            // refuse — an outdated binary must never destroy a graph it
            // cannot rebuild equivalently.
            if let Some(v) = stored
                && v > SCHEMA_VERSION
            {
                return Err(StoreError::NewerSchema {
                    stored: v,
                    supported: SCHEMA_VERSION,
                });
            }
            if stored != Some(SCHEMA_VERSION) {
                drop(txn);
                drop(db);
                std::fs::remove_file(path).map_err(StoreError::Reset)?;
            }
        }
        let store = Self {
            db: open_retrying(path, |p| Database::create(p))?,
        };
        let txn = store.db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            meta.insert("schema", SCHEMA_VERSION)?;
            drop(meta);
            txn.open_table(NODES)?;
            txn.open_table(FILE_FACTS)?;
            txn.open_table(FILE_HASH)?;
            txn.open_table(FILE_SCOPE)?;
            txn.open_table(NODE_SCOPE)?;
            txn.open_multimap_table(OUT_EDGES)?;
            txn.open_multimap_table(IN_EDGES)?;
            txn.open_multimap_table(UNRESOLVED)?;
            txn.open_multimap_table(NAME_REFS)?;
            txn.open_multimap_table(NAME_NODES)?;
            txn.open_multimap_table(TRIGRAMS)?;
            txn.open_multimap_table(TOKENS_WORDS)?;
            txn.open_multimap_table(BODY_TERMS)?;
            txn.open_multimap_table(IMPORTS)?;
            txn.open_table(INTERN)?;
            txn.open_table(INTERN_REV)?;
            txn.open_table(RESOLVE_META)?;
            txn.open_table(PENDING)?;
        }
        txn.commit()?;
        Ok(store)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self {
            db: open_retrying(path.as_ref(), |p| Database::open(p))?,
        })
    }

    /// The schema stamp of this open database, if any.
    pub fn schema(&self) -> Result<Option<u32>, StoreError> {
        let txn = self.db.begin_read()?;
        match txn.open_table(META) {
            Ok(table) => Ok(table.get("schema")?.map(|g| g.value())),
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Persist a whole graph in one transaction (test/export convenience;
    /// the incremental path goes through `update_files`).
    pub fn write_graph(&self, graph: &Graph) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut nodes = txn.open_table(NODES)?;
            let mut scopes = txn.open_table(FILE_SCOPE)?;
            let mut out = txn.open_multimap_table(OUT_EDGES)?;
            let mut inn = txn.open_multimap_table(IN_EDGES)?;
            for node in graph.nodes() {
                nodes.insert(node.id.as_str(), postcard::to_allocvec(node)?.as_slice())?;
                scopes.insert(
                    node.file.as_str(),
                    CorpusScope::classify_path(&node.file).as_str(),
                )?;
            }
            for edge in graph.edges() {
                let bytes = postcard::to_allocvec(edge)?;
                out.insert(edge.src.as_str(), bytes.as_slice())?;
                inn.insert(edge.dst.as_str(), bytes.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Total stored unresolved references.
    pub fn unresolved_count(&self) -> Result<u64, StoreError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_multimap_table(UNRESOLVED) {
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
            other => other?,
        };
        Ok(table.len()?)
    }

    /// Every stored unresolved reference — cross-repo boundary resolution
    /// input (a workspace resolves these against other members' symbols).
    pub fn all_unresolved(&self) -> Result<Vec<Reference>, StoreError> {
        Ok(self
            .all_unresolved_details()?
            .into_iter()
            .map(|u| u.reference)
            .collect())
    }

    /// Every unresolved outcome including why the graph could not prove a
    /// target. Query surfaces use this; workspace linking consumes the raw
    /// references through [`Store::all_unresolved`].
    pub fn all_unresolved_details(&self) -> Result<Vec<UnresolvedReference>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_multimap_table(UNRESOLVED) {
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            other => other?,
        };
        let mut refs = Vec::new();
        for entry in table.iter()? {
            let (_, values) = entry?;
            for guard in values {
                refs.push(postcard::from_bytes(guard?.value())?);
            }
        }
        Ok(refs)
    }

    /// Stored unresolved references, optionally narrowed to one file
    /// and/or a name (final-segment match, same rule as
    /// [`Store::unresolved_named`]). The `sinter unresolved` listing.
    pub fn unresolved_refs(
        &self,
        file: Option<&str>,
        name: Option<&str>,
    ) -> Result<Vec<Reference>, StoreError> {
        let mut refs = match file {
            Some(file) => self.references_in(file)?,
            None => self.all_unresolved()?,
        };
        if let Some(name) = name {
            refs.retain(|r| name_tail_matches(&r.name, name));
        }
        Ok(refs)
    }

    pub fn unresolved_details(
        &self,
        file: Option<&str>,
        name: Option<&str>,
    ) -> Result<Vec<UnresolvedReference>, StoreError> {
        let mut refs = match file {
            Some(file) => self.unresolved_details_in(file)?,
            None => self.all_unresolved_details()?,
        };
        if let Some(name) = name {
            refs.retain(|u| name_tail_matches(&u.reference.name, name));
        }
        Ok(refs)
    }

    /// Unresolved references recorded for one file.
    pub fn references_in(&self, file: &str) -> Result<Vec<Reference>, StoreError> {
        Ok(self
            .unresolved_details_in(file)?
            .into_iter()
            .map(|u| u.reference)
            .collect())
    }

    pub fn unresolved_details_in(
        &self,
        file: &str,
    ) -> Result<Vec<UnresolvedReference>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_multimap_table(UNRESOLVED) {
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            other => other?,
        };
        let mut refs = Vec::new();
        for guard in table.get(file)? {
            refs.push(postcard::from_bytes(guard?.value())?);
        }
        Ok(refs)
    }

    /// The fingerprint a non-source resolution input (key: "scip",
    /// "module_roots") was last resolved against, if any.
    pub fn resolve_fingerprint(&self, key: &str) -> Result<Option<String>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table(RESOLVE_META) {
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            other => other?,
        };
        Ok(table.get(key)?.map(|g| g.value().to_string()))
    }

    /// Idempotent: an unchanged fingerprint opens no write transaction,
    /// keeping a clean build write-free (parallel readers never queue
    /// behind redb's exclusive writer for a no-op).
    pub fn set_resolve_fingerprint(
        &self,
        key: &str,
        fingerprint: Option<&str>,
    ) -> Result<(), StoreError> {
        if self.resolve_fingerprint(key)?.as_deref() == fingerprint {
            return Ok(());
        }
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(RESOLVE_META)?;
            match fingerprint {
                Some(f) => {
                    table.insert(key, f)?;
                }
                None => {
                    table.remove(key)?;
                }
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Unresolved references whose written name ends in this name — the
    /// honest-empty signal for blast-radius queries: a nonzero count means
    /// the graph may be missing dependents of a symbol with that name.
    pub fn unresolved_named(&self, name: &str) -> Result<usize, StoreError> {
        let files = self.ref_files(&std::collections::BTreeSet::from([name.to_string()]))?;
        let mut count = 0;
        for file in files {
            count += self
                .references_in(&file)?
                .iter()
                .filter(|r| name_tail_matches(&r.name, name))
                .count();
        }
        Ok(count)
    }

    pub fn node(&self, id: &NodeId) -> Result<Option<Node>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(NODES)?;
        match table.get(id.as_str())? {
            Some(guard) => Ok(Some(postcard::from_bytes(guard.value())?)),
            None => Ok(None),
        }
    }

    pub fn node_count(&self) -> Result<u64, StoreError> {
        let txn = self.db.begin_read()?;
        Ok(txn.open_table(NODES)?.len()?)
    }

    pub fn edge_count(&self) -> Result<u64, StoreError> {
        let txn = self.db.begin_read()?;
        Ok(txn.open_multimap_table(OUT_EDGES)?.len()?)
    }

    /// Edges leaving `id`.
    pub fn out_edges(&self, id: &NodeId) -> Result<Vec<Edge>, StoreError> {
        self.adjacent(OUT_EDGES, id)
    }

    /// Edges arriving at `id`.
    pub fn in_edges(&self, id: &NodeId) -> Result<Vec<Edge>, StoreError> {
        self.adjacent(IN_EDGES, id)
    }

    /// Incoming edges for several nodes under one read transaction. Query
    /// ranking uses this instead of opening one redb snapshot per candidate.
    pub fn in_edges_many(&self, ids: &[NodeId]) -> Result<HashMap<NodeId, Vec<Edge>>, StoreError> {
        self.adjacent_many(IN_EDGES, ids)
    }

    /// Outgoing edges for several nodes under one read transaction.
    pub fn out_edges_many(&self, ids: &[NodeId]) -> Result<HashMap<NodeId, Vec<Edge>>, StoreError> {
        self.adjacent_many(OUT_EDGES, ids)
    }

    fn adjacent_many(
        &self,
        table: MultimapTableDefinition<&str, &[u8]>,
        ids: &[NodeId],
    ) -> Result<HashMap<NodeId, Vec<Edge>>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(table)?;
        let mut found = HashMap::with_capacity(ids.len());
        for id in ids {
            let mut edges = Vec::new();
            for guard in table.get(id.as_str())? {
                edges.push(postcard::from_bytes(guard?.value())?);
            }
            found.insert(id.clone(), edges);
        }
        Ok(found)
    }

    fn adjacent(
        &self,
        table: MultimapTableDefinition<&str, &[u8]>,
        id: &NodeId,
    ) -> Result<Vec<Edge>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(table)?;
        let mut edges = Vec::new();
        for guard in table.get(id.as_str())? {
            edges.push(postcard::from_bytes(guard?.value())?);
        }
        Ok(edges)
    }

    /// (file, stamp) for every stored file — the changed-set diff base.
    pub fn file_hashes(&self) -> Result<Vec<(String, FileStamp)>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILE_HASH)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            out.push((k.value().to_string(), FileStamp::decode(v.value())));
        }
        Ok(out)
    }

    /// Persist repository classification overrides for already indexed
    /// files. Clean builds remain write-free when every row is unchanged.
    pub fn set_file_scopes(&self, rows: &[(String, CorpusScope)]) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let existing = self.file_scopes()?;
        if rows
            .iter()
            .all(|(file, scope)| existing.get(file) == Some(scope))
        {
            return Ok(());
        }
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(FILE_SCOPE)?;
            for (file, scope) in rows {
                table.insert(file.as_str(), scope.as_str())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Complete persisted scope map. Unknown legacy/malformed values fall
    /// back to conservative path classification instead of hiding nodes.
    pub fn file_scopes(&self) -> Result<HashMap<String, CorpusScope>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILE_SCOPE)?;
        let mut scopes = HashMap::new();
        for entry in table.iter()? {
            let (file, scope) = entry?;
            let file = file.value().to_string();
            scopes.insert(
                file.clone(),
                CorpusScope::from_str_opt(scope.value())
                    .unwrap_or_else(|| CorpusScope::classify_path(&file)),
            );
        }
        Ok(scopes)
    }

    pub fn file_scope(&self, file: &str) -> Result<CorpusScope, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILE_SCOPE)?;
        Ok(table
            .get(file)?
            .and_then(|guard| CorpusScope::from_str_opt(guard.value()))
            .unwrap_or_else(|| CorpusScope::classify_path(file)))
    }

    pub fn facts(&self, file: &str) -> Result<Option<FileFacts>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILE_FACTS)?;
        match table.get(file)? {
            Some(guard) => Ok(Some(crate::update::decode_facts(guard.value())?)),
            None => Ok(None),
        }
    }

    /// Files whose most recently extracted syntax tree contained errors.
    /// Coverage reporting uses the complete persisted set, not only files
    /// changed by the latest incremental pass.
    pub fn syntax_error_files(&self) -> Result<Vec<String>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILE_FACTS)?;
        let mut files = Vec::new();
        for entry in table.iter()? {
            let (file, bytes) = entry?;
            let facts = crate::update::decode_facts(bytes.value())?;
            if facts.has_syntax_errors {
                files.push(file.value().to_string());
            }
        }
        files.sort();
        Ok(files)
    }

    /// Reclaim free pages. Worth running after bulk rebuilds; skipped on
    /// incremental updates (it rewrites the file and would blow the <1s
    /// one-file-edit budget). redb compaction is iterative — repeat until
    /// it reports no further progress (bounded).
    pub fn compact(&mut self) -> Result<bool, StoreError> {
        let mut any = false;
        for _ in 0..16 {
            if !self.db.compact()? {
                break;
            }
            any = true;
        }
        Ok(any)
    }

    /// Every stored import reference — re-export chain-walking input.
    pub fn all_imports(&self) -> Result<Vec<Reference>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(IMPORTS)?;
        let mut refs = Vec::new();
        for entry in table.iter()? {
            let (_, values) = entry?;
            for guard in values {
                refs.push(postcard::from_bytes(guard?.value())?);
            }
        }
        Ok(refs)
    }

    /// Every stored node — resolution index input. Compact scan of the node
    /// table; queries never need this.
    pub fn all_nodes(&self) -> Result<Vec<Node>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(NODES)?;
        let mut nodes = Vec::new();
        for entry in table.iter()? {
            nodes.push(postcard::from_bytes(entry?.1.value())?);
        }
        Ok(nodes)
    }

    /// Non-`Contains` in-degree per node id, streamed straight off the
    /// IN_EDGES table — hub ranking without materializing (and
    /// re-validating) the whole graph. Nodes with zero such in-edges are
    /// omitted.
    pub fn in_degrees(&self) -> Result<Vec<(String, usize)>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(IN_EDGES)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (key, values) = entry?;
            let mut n = 0usize;
            for guard in values {
                let edge: Edge = postcard::from_bytes(guard?.value())?;
                if edge.relation != sinter_core::Relation::Contains {
                    n += 1;
                }
            }
            if n > 0 {
                out.push((key.value().to_string(), n));
            }
        }
        Ok(out)
    }

    /// Rebuild the full in-memory graph, re-validating every invariant.
    /// Export/debug path only — queries must not need this.
    pub fn read_graph(&self) -> Result<Graph, StoreError> {
        let txn = self.db.begin_read()?;
        let mut graph = Graph::new();
        {
            let nodes = txn.open_table(NODES)?;
            for entry in nodes.iter()? {
                let (_, value) = entry?;
                graph.add_node(postcard::from_bytes(value.value())?)?;
            }
        }
        {
            let out = txn.open_multimap_table(OUT_EDGES)?;
            for entry in out.iter()? {
                let (_, values) = entry?;
                for guard in values {
                    graph.add_edge(postcard::from_bytes(guard?.value())?)?;
                }
            }
        }
        Ok(graph)
    }
}

/// Does a written reference name (`acme_common::connect_grpc_channel`,
/// `pkg.Func`) end at exactly this name?
fn name_tail_matches(written: &str, name: &str) -> bool {
    // Exact final-segment equality across every language's separators —
    // boundary substring matching over-counted short common names
    // (`run`, `install`) into the honest-empty note.
    let tail = written.rsplit("::").next().unwrap_or(written);
    let tail = tail.rsplit(['/', '.']).next().unwrap_or(tail);
    tail == name
}
