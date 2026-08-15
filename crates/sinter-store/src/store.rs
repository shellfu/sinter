use std::path::Path;

use redb::{
    Database, MultimapTableDefinition, ReadableDatabase, ReadableMultimapTable, ReadableTable,
    ReadableTableMetadata, TableDefinition,
};
use sinter_core::{Edge, FileFacts, Graph, Node, NodeId, Reference};

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
/// reference name -> files containing a reference with that name; the
/// resolution invalidation index.
pub(crate) const NAME_REFS: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("name_refs");
/// symbol name -> node ids, exact-match query index.
pub(crate) const NAME_NODES: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("name_nodes");
/// lowercased trigram -> node ids, fuzzy query index.
pub(crate) const TRIGRAMS: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("trigrams");
/// file -> import references only (compact). Re-export chain walking needs
/// every file's imports without decoding full facts corpus-wide.
pub(crate) const IMPORTS: MultimapTableDefinition<&str, &[u8]> =
    MultimapTableDefinition::new("imports");
/// Single-row schema stamp; a mismatch on open wipes the database (facts
/// are derivable, a stale-format db is not worth migrating).
const META: TableDefinition<&str, u32> = TableDefinition::new("meta");
const SCHEMA_VERSION: u32 = 2;

/// Persistent graph store. Point queries never load the whole graph.
pub struct Store {
    pub(crate) db: Database,
}

impl Store {
    /// Create or open the database and ensure all tables exist. An
    /// existing database with a different schema version is deleted and
    /// recreated — the next build re-derives everything from source.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if path.exists() {
            let db = Database::open(path)?;
            let txn = db.begin_read()?;
            let stored = match txn.open_table(META) {
                Ok(table) => table.get("schema")?.map(|g| g.value()),
                Err(redb::TableError::TableDoesNotExist(_)) => None,
                Err(e) => return Err(e.into()),
            };
            if stored != Some(SCHEMA_VERSION) {
                drop(txn);
                drop(db);
                std::fs::remove_file(path).map_err(StoreError::Reset)?;
            }
        }
        let store = Self {
            db: Database::create(path)?,
        };
        let txn = store.db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            meta.insert("schema", SCHEMA_VERSION)?;
            drop(meta);
            txn.open_table(NODES)?;
            txn.open_table(FILE_FACTS)?;
            txn.open_table(FILE_HASH)?;
            txn.open_multimap_table(OUT_EDGES)?;
            txn.open_multimap_table(IN_EDGES)?;
            txn.open_multimap_table(UNRESOLVED)?;
            txn.open_multimap_table(NAME_REFS)?;
            txn.open_multimap_table(NAME_NODES)?;
            txn.open_multimap_table(TRIGRAMS)?;
            txn.open_multimap_table(IMPORTS)?;
        }
        txn.commit()?;
        Ok(store)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self {
            db: Database::open(path)?,
        })
    }

    /// Persist a whole graph in one transaction (test/export convenience;
    /// the incremental path goes through `update_files`).
    pub fn write_graph(&self, graph: &Graph) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut nodes = txn.open_table(NODES)?;
            let mut out = txn.open_multimap_table(OUT_EDGES)?;
            let mut inn = txn.open_multimap_table(IN_EDGES)?;
            for node in graph.nodes() {
                nodes.insert(node.id.as_str(), postcard::to_allocvec(node)?.as_slice())?;
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

    /// Unresolved references recorded for one file.
    pub fn references_in(&self, file: &str) -> Result<Vec<Reference>, StoreError> {
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

    /// (file, content hash) for every stored file — the changed-set diff base.
    pub fn file_hashes(&self) -> Result<Vec<(String, String)>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILE_HASH)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            out.push((k.value().to_string(), v.value().to_string()));
        }
        Ok(out)
    }

    pub fn facts(&self, file: &str) -> Result<Option<FileFacts>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILE_FACTS)?;
        match table.get(file)? {
            Some(guard) => Ok(Some(postcard::from_bytes(guard.value())?)),
            None => Ok(None),
        }
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
