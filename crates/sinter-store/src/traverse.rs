//! Graph traversals over the persisted adjacency tables: reverse blast
//! radius and shortest path. Point reads only — never loads the corpus.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use sinter_core::{Confidence, Edge, Evidence, Node, NodeId, Relation};

use crate::error::StoreError;
use crate::store::Store;

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
}

/// One step of a traversal result: the node reached, how deep, and the edge
/// that reached it.
pub struct Reached {
    pub node: Node,
    pub depth: usize,
    pub via: Edge,
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
        let mut seen: HashSet<NodeId> = HashSet::from([id.clone()]);
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::from([(id.clone(), 0)]);
        let mut out = Vec::new();
        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            for edge in self.in_edges(&current)? {
                if !filter.admits(&edge) || !seen.insert(edge.src.clone()) {
                    continue;
                }
                if let Some(node) = self.node(&edge.src)? {
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
        let mut seen: HashSet<NodeId> = HashSet::from([id.clone()]);
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::from([(id.clone(), 0)]);
        if self
            .node(id)?
            .is_some_and(|n| n.kind == sinter_core::SymbolKind::File)
        {
            for edge in self.out_edges(id)? {
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
            for edge in self.out_edges(&current)? {
                if !filter.admits(&edge) || !seen.insert(edge.dst.clone()) {
                    continue;
                }
                if let Some(node) = self.node(&edge.dst)? {
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
        let mut prev: HashMap<NodeId, Edge> = HashMap::new();
        let mut seen: HashSet<NodeId> = HashSet::from([from.clone()]);
        let mut queue: VecDeque<NodeId> = VecDeque::from([from.clone()]);
        // A file's dependencies live in the symbols it contains; a file
        // start seeds through its contains edges (shown as path steps).
        // Containment stays non-traversable everywhere past the start.
        if self
            .node(from)?
            .is_some_and(|n| n.kind == sinter_core::SymbolKind::File)
        {
            for edge in self.out_edges(from)? {
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
            for edge in self.out_edges(&current)? {
                if !filter.admits(&edge) || !seen.insert(edge.dst.clone()) {
                    continue;
                }
                prev.insert(edge.dst.clone(), edge.clone());
                queue.push_back(edge.dst.clone());
            }
        }
        Ok(None)
    }
}
