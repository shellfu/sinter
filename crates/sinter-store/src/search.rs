//! Symbol search: exact name index plus lowercased-trigram fuzzy index.
//! Both are maintained incrementally by `update.rs`.

use std::collections::HashMap;

use redb::ReadableDatabase;
use sinter_core::{Node, NodeId};

use crate::error::StoreError;
use crate::store::{NAME_NODES, Store};

/// Lowercased character trigrams of a name; names shorter than 3 chars
/// index as one whole-name gram.
pub(crate) fn trigrams(name: &str) -> Vec<String> {
    let lower: Vec<char> = name.to_lowercase().chars().collect();
    if lower.len() < 3 {
        return vec![lower.iter().collect()];
    }
    let mut grams: Vec<String> = lower.windows(3).map(|w| w.iter().collect()).collect();
    grams.sort();
    grams.dedup();
    grams
}

impl Store {
    /// Nodes whose name matches exactly (case-sensitive).
    pub fn nodes_named(&self, name: &str) -> Result<Vec<Node>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(NAME_NODES)?;
        let mut ids = Vec::new();
        for guard in table.get(name)? {
            ids.push(guard?.value().to_string());
        }
        drop(table);
        drop(txn);
        let mut nodes = Vec::new();
        for id in ids {
            if let Some(node) = self.node(&NodeId::new(id))? {
                nodes.push(node);
            }
        }
        Ok(nodes)
    }

    /// Fuzzy candidates: nodes sharing the most trigrams with the query,
    /// best first, capped at `limit`.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Node>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_multimap_table(crate::store::TRIGRAMS)?;
        let mut hits: HashMap<String, usize> = HashMap::new();
        let query_grams = trigrams(query);
        for gram in &query_grams {
            for guard in table.get(gram.as_str())? {
                *hits.entry(guard?.value().to_string()).or_default() += 1;
            }
        }
        drop(table);
        drop(txn);
        let mut ranked: Vec<(String, usize)> = hits.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut nodes = Vec::new();
        for (id, shared) in ranked.into_iter().take(limit.max(1)) {
            // Require a majority of query trigrams to appear in the name.
            if shared * 2 >= query_grams.len()
                && let Some(node) = self.node(&NodeId::new(id))?
            {
                nodes.push(node);
            }
        }
        Ok(nodes)
    }
}
