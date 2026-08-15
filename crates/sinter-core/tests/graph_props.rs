use proptest::prelude::*;
use sinter_core::{
    Confidence, Edge, Evidence, Graph, GraphError, Node, NodeId, Relation, Span, SymbolKind,
};

fn node(id: &str) -> Node {
    Node {
        id: NodeId::new(id),
        kind: SymbolKind::Function,
        name: id.to_string(),
        file: "src/lib.rs".to_string(),
        span: Span { start: 0, end: 10 },
        signature: format!("fn {id}()"),
        doc: None,
    }
}

fn edge(src: &str, dst: &str, relation: Relation) -> Edge {
    Edge {
        src: NodeId::new(src),
        dst: NodeId::new(dst),
        relation,
        evidence: Evidence::Structural,
        confidence: Confidence::Certain,
    }
}

const RELATIONS: [Relation; 6] = [
    Relation::Calls,
    Relation::Uses,
    Relation::Imports,
    Relation::Contains,
    Relation::Implements,
    Relation::Extends,
];

fn arb_ids() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::btree_set("[A-Za-z][A-Za-z0-9_:]{0,15}", 1..30)
        .prop_map(|s| s.into_iter().collect())
}

proptest! {
    /// Every edge endpoint exists after arbitrary valid construction.
    #[test]
    fn endpoints_always_exist(ids in arb_ids(), pairs in proptest::collection::vec((any::<prop::sample::Index>(), any::<prop::sample::Index>(), 0usize..6), 0..60)) {
        let mut g = Graph::new();
        for id in &ids {
            g.add_node(node(id)).unwrap();
        }
        for (a, b, r) in &pairs {
            g.add_edge(edge(a.get(&ids), b.get(&ids), RELATIONS[*r])).unwrap();
        }
        for e in g.edges() {
            prop_assert!(g.node(&e.src).is_some());
            prop_assert!(g.node(&e.dst).is_some());
        }
    }

    /// An edge to a node not in the graph always errors.
    #[test]
    fn missing_endpoint_rejected(ids in arb_ids(), ghost in "[A-Za-z][A-Za-z0-9_:]{0,15}") {
        prop_assume!(!ids.contains(&ghost));
        let mut g = Graph::new();
        for id in &ids {
            g.add_node(node(id)).unwrap();
        }
        let err = g.add_edge(edge(&ids[0], &ghost, Relation::Calls)).unwrap_err();
        prop_assert_eq!(err, GraphError::MissingEndpoint(NodeId::new(ghost)));
    }

    /// Re-inserting any existing id errors and leaves the graph unchanged.
    #[test]
    fn duplicate_id_rejected(ids in arb_ids(), pick in any::<prop::sample::Index>()) {
        let mut g = Graph::new();
        for id in &ids {
            g.add_node(node(id)).unwrap();
        }
        let dup = pick.get(&ids);
        let before = g.clone();
        let err = g.add_node(node(dup)).unwrap_err();
        prop_assert_eq!(err, GraphError::DuplicateNode(NodeId::new(dup)));
        prop_assert_eq!(g, before);
    }

    /// A span with end <= start is always rejected.
    #[test]
    fn invalid_span_rejected(start in 0u64..1000, slack in 0u64..1000) {
        let end = start.saturating_sub(slack); // end <= start
        let mut n = node("f");
        n.span = Span { start, end };
        let mut g = Graph::new();
        let rejected = matches!(g.add_node(n), Err(GraphError::InvalidSpan { .. }));
        prop_assert!(rejected);
    }
}

/// Ids differing only by case are distinct nodes, never merged.
#[test]
fn ids_are_case_sensitive() {
    let mut g = Graph::new();
    g.add_node(node("Config")).unwrap();
    g.add_node(node("config")).unwrap();
    assert_eq!(g.node_count(), 2);
    assert!(g.node(&NodeId::new("Config")).is_some());
    assert!(g.node(&NodeId::new("config")).is_some());
}

/// Parallel edges with different relations coexist; exact duplicates dedup.
#[test]
fn multigraph_parallel_edges() {
    let mut g = Graph::new();
    g.add_node(node("a")).unwrap();
    g.add_node(node("b")).unwrap();
    g.add_edge(edge("a", "b", Relation::Calls)).unwrap();
    g.add_edge(edge("a", "b", Relation::Uses)).unwrap();
    g.add_edge(edge("a", "b", Relation::Calls)).unwrap(); // exact duplicate
    assert_eq!(g.edge_count(), 2);
    let a = NodeId::new("a");
    assert_eq!(g.edges_from(&a).count(), 2);
}

/// Empty name/file rejected.
#[test]
fn empty_fields_rejected() {
    let mut g = Graph::new();
    let mut n = node("f");
    n.name = String::new();
    assert!(matches!(
        g.add_node(n),
        Err(GraphError::EmptyField { field: "name", .. })
    ));
    let mut n = node("f");
    n.file = String::new();
    assert!(matches!(
        g.add_node(n),
        Err(GraphError::EmptyField { field: "file", .. })
    ));
}
