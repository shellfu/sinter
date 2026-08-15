use std::collections::{BTreeMap, BTreeSet};

use crate::edge::Edge;
use crate::error::GraphError;
use crate::node::{Node, NodeId};

/// Directed multigraph over typed nodes.
///
/// Invariants, enforced at construction:
/// - node ids are unique and case-sensitive; a collision is an error
/// - every edge endpoint refers to an existing node
/// - every node has a non-empty id, name, and file, and a span with `end > start`
///
/// Parallel edges that differ in relation or confidence coexist; exact
/// duplicate edges deduplicate silently.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Graph {
    nodes: BTreeMap<NodeId, Node>,
    edges: BTreeSet<Edge>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, node: Node) -> Result<(), GraphError> {
        let empty = |field| GraphError::EmptyField {
            id: node.id.clone(),
            field,
        };
        if node.id.as_str().is_empty() {
            return Err(empty("id"));
        }
        if node.name.is_empty() {
            return Err(empty("name"));
        }
        if node.file.is_empty() {
            return Err(empty("file"));
        }
        if node.span.end <= node.span.start {
            return Err(GraphError::InvalidSpan {
                id: node.id.clone(),
                start: node.span.start,
                end: node.span.end,
            });
        }
        if self.nodes.contains_key(&node.id) {
            return Err(GraphError::DuplicateNode(node.id));
        }
        self.nodes.insert(node.id.clone(), node);
        Ok(())
    }

    pub fn add_edge(&mut self, edge: Edge) -> Result<(), GraphError> {
        if !self.nodes.contains_key(&edge.src) {
            return Err(GraphError::MissingEndpoint(edge.src));
        }
        if !self.nodes.contains_key(&edge.dst) {
            return Err(GraphError::MissingEndpoint(edge.dst));
        }
        self.edges.insert(edge);
        Ok(())
    }

    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    pub fn edges(&self) -> impl Iterator<Item = &Edge> {
        self.edges.iter()
    }

    // ponytail: linear scans; indexed adjacency lives in sinter-store, which
    // owns all at-scale queries.
    pub fn edges_from<'a>(&'a self, id: &'a NodeId) -> impl Iterator<Item = &'a Edge> {
        self.edges.iter().filter(move |e| &e.src == id)
    }

    pub fn edges_to<'a>(&'a self, id: &'a NodeId) -> impl Iterator<Item = &'a Edge> {
        self.edges.iter().filter(move |e| &e.dst == id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}
