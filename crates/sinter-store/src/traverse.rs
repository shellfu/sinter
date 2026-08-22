//! Graph traversals over the persisted adjacency tables: reverse blast
//! radius and shortest path. Point reads only — never loads the corpus.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use redb::ReadableDatabase;
use sinter_core::{Confidence, CorpusScope, Edge, Evidence, Node, NodeId, Relation};

use crate::error::StoreError;
use crate::store::{FILE_SCOPE, IN_EDGES, NODES, OUT_EDGES, Store};

/// Which edges a traversal may walk. Containment is structure, not
/// dependency, so it never participates.
#[derive(Debug, Default, Clone)]
pub struct EdgeFilter {
    /// Allowed evidence kinds; None = all.
    pub evidence: Option<BTreeSet<Evidence>>,
    /// Minimum confidence; None = any.
    pub min_confidence: Option<Confidence>,
    /// Allowed relations; None = all (Contains stays excluded either way).
    pub relations: Option<BTreeSet<Relation>>,
    /// Allowed corpus roles for nodes entered after the explicit start.
    /// None = all. Exact lookup of the start remains independent of scope.
    pub scopes: Option<BTreeSet<CorpusScope>>,
}

impl EdgeFilter {
    pub fn admits(&self, edge: &Edge) -> bool {
        if edge.relation == Relation::Contains {
            return false;
        }
        if let Some(allowed) = &self.relations
            && !allowed.contains(&edge.relation)
        {
            return false;
        }
        if let Some(allowed) = &self.evidence
            && !allowed.contains(&edge.evidence)
        {
            return false;
        }
        if self.min_confidence == Some(Confidence::Certain)
            && edge.confidence != Confidence::Certain
        {
            return false;
        }
        true
    }

    pub fn admits_scope(&self, scope: CorpusScope) -> bool {
        self.scopes
            .as_ref()
            .is_none_or(|allowed| allowed.contains(&scope))
    }
}

fn file_of(id: &NodeId) -> &str {
    id.as_str()
        .split_once('#')
        .map_or(id.as_str(), |(file, _)| file)
}

/// One step of a traversal result: the node reached, how deep, and the edge
/// that reached it.
pub struct Reached {
    pub node: Node,
    pub depth: usize,
    pub via: Edge,
}

/// Depth-1 dependents and the distinct files they live in — the "who
/// actually calls this" number, distinct from the transitive total that
/// otherwise reads as a caller count.
pub fn direct_summary(reached: &[Reached]) -> (usize, usize) {
    let direct: Vec<&Reached> = reached.iter().filter(|r| r.depth == 1).collect();
    let files: std::collections::HashSet<&str> =
        direct.iter().map(|r| r.node.file.as_str()).collect();
    (direct.len(), files.len())
}

impl Store {
    /// Reverse blast radius: everything transitively depending on `id`
    /// (incoming non-Contains edges), breadth-first, deduplicated.
    pub fn dependents(
        &self,
        id: &NodeId,
        filter: &EdgeFilter,
        max_depth: usize,
    ) -> Result<Vec<Reached>, StoreError> {
        let txn = self.db.begin_read()?;
        let nodes = txn.open_table(NODES)?;
        let scopes = txn.open_table(FILE_SCOPE)?;
        let incoming = txn.open_multimap_table(IN_EDGES)?;
        let mut seen: HashSet<NodeId> = HashSet::from([id.clone()]);
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::from([(id.clone(), 0)]);
        let mut out = Vec::new();
        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            for guard in incoming.get(current.as_str())? {
                let edge: Edge = postcard::from_bytes(guard?.value())?;
                let file = file_of(&edge.src);
                let scope = scopes
                    .get(file)?
                    .and_then(|guard| CorpusScope::from_str_opt(guard.value()))
                    .unwrap_or_else(|| CorpusScope::classify_path(file));
                if !filter.admits(&edge)
                    || !filter.admits_scope(scope)
                    || !seen.insert(edge.src.clone())
                {
                    continue;
                }
                if let Some(guard) = nodes.get(edge.src.as_str())? {
                    let node = postcard::from_bytes(guard.value())?;
                    queue.push_back((edge.src.clone(), depth + 1));
                    out.push(Reached {
                        node,
                        depth: depth + 1,
                        via: edge,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Forward transitive closure: everything `id` depends on (outgoing
    /// non-Contains edges), breadth-first, deduplicated. A file start seeds
    /// through its Contains edges (a file's dependencies live in the
    /// symbols it contains), silently — containment is not a dependency.
    pub fn dependencies(
        &self,
        id: &NodeId,
        filter: &EdgeFilter,
        max_depth: usize,
    ) -> Result<Vec<Reached>, StoreError> {
        let txn = self.db.begin_read()?;
        let nodes = txn.open_table(NODES)?;
        let scopes = txn.open_table(FILE_SCOPE)?;
        let outgoing = txn.open_multimap_table(OUT_EDGES)?;
        let mut seen: HashSet<NodeId> = HashSet::from([id.clone()]);
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::from([(id.clone(), 0)]);
        if nodes
            .get(id.as_str())?
            .map(|guard| postcard::from_bytes::<Node>(guard.value()))
            .transpose()?
            .is_some_and(|n| n.kind == sinter_core::SymbolKind::File)
        {
            for guard in outgoing.get(id.as_str())? {
                let edge: Edge = postcard::from_bytes(guard?.value())?;
                if edge.relation == Relation::Contains && seen.insert(edge.dst.clone()) {
                    queue.push_back((edge.dst.clone(), 0));
                }
            }
        }
        let mut out = Vec::new();
        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            for guard in outgoing.get(current.as_str())? {
                let edge: Edge = postcard::from_bytes(guard?.value())?;
                let file = file_of(&edge.dst);
                let scope = scopes
                    .get(file)?
                    .and_then(|guard| CorpusScope::from_str_opt(guard.value()))
                    .unwrap_or_else(|| CorpusScope::classify_path(file));
                if !filter.admits(&edge)
                    || !filter.admits_scope(scope)
                    || !seen.insert(edge.dst.clone())
                {
                    continue;
                }
                if let Some(guard) = nodes.get(edge.dst.as_str())? {
                    let node = postcard::from_bytes(guard.value())?;
                    queue.push_back((edge.dst.clone(), depth + 1));
                    out.push(Reached {
                        node,
                        depth: depth + 1,
                        via: edge,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Shortest edge path `from -> to` over outgoing edges, or None.
    pub fn shortest_path(
        &self,
        from: &NodeId,
        to: &NodeId,
        filter: &EdgeFilter,
    ) -> Result<Option<Vec<Edge>>, StoreError> {
        let txn = self.db.begin_read()?;
        let nodes = txn.open_table(NODES)?;
        let scopes = txn.open_table(FILE_SCOPE)?;
        let outgoing = txn.open_multimap_table(OUT_EDGES)?;
        let mut prev: HashMap<NodeId, Edge> = HashMap::new();
        let mut seen: HashSet<NodeId> = HashSet::from([from.clone()]);
        let mut queue: VecDeque<NodeId> = VecDeque::from([from.clone()]);
        // A file's dependencies live in the symbols it contains; a file
        // start seeds through its contains edges (shown as path steps).
        // Containment stays non-traversable everywhere past the start.
        if nodes
            .get(from.as_str())?
            .map(|guard| postcard::from_bytes::<Node>(guard.value()))
            .transpose()?
            .is_some_and(|n| n.kind == sinter_core::SymbolKind::File)
        {
            for guard in outgoing.get(from.as_str())? {
                let edge: Edge = postcard::from_bytes(guard?.value())?;
                if edge.relation == Relation::Contains && seen.insert(edge.dst.clone()) {
                    prev.insert(edge.dst.clone(), edge.clone());
                    queue.push_back(edge.dst.clone());
                }
            }
        }
        while let Some(current) = queue.pop_front() {
            if &current == to {
                let mut path = Vec::new();
                let mut at = to.clone();
                while &at != from {
                    let edge = prev[&at].clone();
                    at = edge.src.clone();
                    path.push(edge);
                }
                path.reverse();
                return Ok(Some(path));
            }
            for guard in outgoing.get(current.as_str())? {
                let edge: Edge = postcard::from_bytes(guard?.value())?;
                let file = file_of(&edge.dst);
                let scope = scopes
                    .get(file)?
                    .and_then(|guard| CorpusScope::from_str_opt(guard.value()))
                    .unwrap_or_else(|| CorpusScope::classify_path(file));
                if !filter.admits(&edge)
                    || !filter.admits_scope(scope)
                    || !seen.insert(edge.dst.clone())
                {
                    continue;
                }
                prev.insert(edge.dst.clone(), edge.clone());
                queue.push_back(edge.dst.clone());
            }
        }
        Ok(None)
    }
}
