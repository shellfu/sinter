//! The one place the per-node scope rule lives: a node's own override, else
//! its file node's override (generated banners), else the persisted file
//! scope, else conservative path classification.

use std::collections::HashMap;

use redb::ReadableTable;
use sinter_core::{CorpusScope, Node};

use crate::error::StoreError;
use crate::store::{FILE_SCOPE, NODE_SCOPE, Store};

pub(crate) fn resolve(
    node_scope: impl Fn(&str) -> Option<CorpusScope>,
    file_scope: Option<CorpusScope>,
    id: &str,
    file: &str,
) -> CorpusScope {
    node_scope(id)
        .or_else(|| node_scope(file))
        .or(file_scope)
        .unwrap_or_else(|| CorpusScope::classify_path(file))
}

/// Read-side scope lookup loaded once per command.
#[derive(Debug, Default, Clone)]
pub struct ScopeIndex {
    files: HashMap<String, CorpusScope>,
    nodes: HashMap<String, CorpusScope>,
}

impl ScopeIndex {
    pub fn scope_of(&self, node: &Node) -> CorpusScope {
        self.scope_of_id(node.id.as_str(), &node.file)
    }

    pub fn scope_of_id(&self, id: &str, file: &str) -> CorpusScope {
        resolve(
            |key| self.nodes.get(key).copied(),
            self.files.get(file).copied(),
            id,
            file,
        )
    }

    /// File-level scope only (path/override based); node overrides ignored.
    pub fn file_scope(&self, file: &str) -> CorpusScope {
        self.scope_of_id(file, file)
    }
}

impl Store {
    pub fn scope_index(&self) -> Result<ScopeIndex, StoreError> {
        let files = self.file_scopes()?;
        let txn = self.db.begin_read()?;
        let table = txn.open_table(NODE_SCOPE)?;
        let mut nodes = HashMap::new();
        for entry in table.iter()? {
            let (id, scope) = entry?;
            if let Some(scope) = CorpusScope::from_str_opt(scope.value()) {
                nodes.insert(id.value().to_string(), scope);
            }
        }
        Ok(ScopeIndex { files, nodes })
    }

    pub fn node_scope(&self, node: &Node) -> Result<CorpusScope, StoreError> {
        let txn = self.db.begin_read()?;
        let nodes = txn.open_table(NODE_SCOPE)?;
        let files = txn.open_table(FILE_SCOPE)?;
        let lookup = |key: &str| {
            nodes
                .get(key)
                .ok()
                .flatten()
                .and_then(|g| CorpusScope::from_str_opt(g.value()))
        };
        let file_scope = files
            .get(node.file.as_str())?
            .and_then(|g| CorpusScope::from_str_opt(g.value()));
        Ok(resolve(lookup, file_scope, node.id.as_str(), &node.file))
    }
}
