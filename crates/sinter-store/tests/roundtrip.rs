use std::collections::BTreeSet;

use proptest::prelude::*;
use sinter_core::{
    Confidence, CorpusScope, Edge, Evidence, Graph, Node, NodeId, Relation, Span, SymbolKind,
};
use sinter_store::EdgeFilter;
use sinter_store::Store;

fn node(id: &str) -> Node {
    Node {
        id: NodeId::new(id),
        kind: SymbolKind::Function,
        name: id.to_string(),
        file: format!("src/{id}.rs"),
        span: Span { start: 3, end: 40 },
        signature: format!("fn {id}()"),
        doc: Some(format!("does {id}")),
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

/// Hand-built graph round-trips through the store byte-exactly (Phase 1 deliverable).
#[test]
fn hand_built_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.redb");

    let mut g = Graph::new();
    for id in ["main", "parse", "Config", "config"] {
        g.add_node(node(id)).unwrap();
    }
    for (s, d, r) in [
        ("main", "parse", Relation::Calls),
        ("main", "Config", Relation::Uses),
        ("main", "Config", Relation::Imports), // parallel edge
        ("parse", "config", Relation::Calls),
    ] {
        g.add_edge(Edge {
            src: NodeId::new(s),
            dst: NodeId::new(d),
            relation: r,
            evidence: Evidence::Structural,
            confidence: Confidence::Certain,
            site: None,
        })
        .unwrap();
    }

    let store = Store::create(&path).unwrap();
    store.write_graph(&g).unwrap();
    drop(store);

    let store = Store::open(&path).unwrap();
    assert_eq!(store.read_graph().unwrap(), g);

    // Point queries hit indexes, not the whole graph.
    let main = NodeId::new("main");
    assert_eq!(store.node(&main).unwrap().unwrap().signature, "fn main()");
    assert_eq!(store.out_edges(&main).unwrap().len(), 3);
    assert_eq!(store.in_edges(&NodeId::new("Config")).unwrap().len(), 2);
    assert_eq!(store.in_edges(&NodeId::new("config")).unwrap().len(), 1);
    assert_eq!(
        store.file_scope("src/main.rs").unwrap(),
        CorpusScope::Production
    );
    let batch = store
        .in_edges_many(&[NodeId::new("Config"), NodeId::new("config")])
        .unwrap();
    assert_eq!(batch[&NodeId::new("Config")].len(), 2);
    assert_eq!(batch[&NodeId::new("config")].len(), 1);
}

#[test]
fn file_scope_is_persisted_and_changes_snapshot_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.redb");
    let store = Store::create(&path).unwrap();
    let mut graph = Graph::new();
    graph.add_node(node("check")).unwrap();
    store.write_graph(&graph).unwrap();
    let before = store.snapshot_token().unwrap();

    store
        .set_file_scopes(&[("src/check.rs".to_string(), CorpusScope::Fixture)])
        .unwrap();
    assert_eq!(
        store.file_scope("src/check.rs").unwrap(),
        CorpusScope::Fixture
    );
    assert_ne!(store.snapshot_token().unwrap(), before);
}

#[test]
fn traversal_scope_blocks_excluded_nodes_without_blocking_exact_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.redb");
    let store = Store::create(&path).unwrap();
    let mut graph = Graph::new();
    let mut source = node("source");
    source.id = NodeId::new("src/source.rs#source@3");
    source.file = "src/source.rs".to_string();
    let mut fixture = node("check");
    fixture.id = NodeId::new("harness/golden/check.rs#check@3");
    fixture.file = "harness/golden/check.rs".to_string();
    graph.add_node(source.clone()).unwrap();
    graph.add_node(fixture.clone()).unwrap();
    graph
        .add_edge(Edge {
            src: source.id.clone(),
            dst: fixture.id.clone(),
            relation: Relation::Calls,
            evidence: Evidence::Structural,
            confidence: Confidence::Certain,
            site: None,
        })
        .unwrap();
    store.write_graph(&graph).unwrap();

    let production_only = EdgeFilter {
        scopes: Some(BTreeSet::from([CorpusScope::Production])),
        ..EdgeFilter::default()
    };
    assert!(
        store
            .dependencies(&source.id, &production_only, 10)
            .unwrap()
            .is_empty()
    );
    assert!(store.node(&fixture.id).unwrap().is_some());
}

/// Sites persist, and several call sites for one dependency fact keep a
/// single representative edge (the smallest site) — cardinality never
/// multiplies with call-site count.
#[test]
fn representative_site_per_edge_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.redb");
    let store = Store::create(&path).unwrap();
    let edge = |start: u64| Edge {
        src: NodeId::new("src/a.rs#caller@3"),
        dst: NodeId::new("src/b.rs#callee@3"),
        relation: Relation::Calls,
        evidence: Evidence::Scope,
        confidence: Confidence::Inferred,
        site: Some(Span {
            start,
            end: start + 6,
        }),
    };
    store
        .insert_edges(&[edge(120), edge(80), edge(80)])
        .unwrap();
    let out = store.out_edges(&NodeId::new("src/a.rs#caller@3")).unwrap();
    assert_eq!(out, vec![edge(80)]);
    let inn = store.in_edges(&NodeId::new("src/b.rs#callee@3")).unwrap();
    assert_eq!(inn, vec![edge(80)]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Any valid graph round-trips through the store unchanged.
    #[test]
    fn any_graph_roundtrips(
        ids in proptest::collection::btree_set("[A-Za-z][A-Za-z0-9_:]{0,15}", 1..20)
            .prop_map(|s| s.into_iter().collect::<Vec<_>>()),
        pairs in proptest::collection::vec(
            (any::<prop::sample::Index>(), any::<prop::sample::Index>(), 0usize..6, 0usize..2),
            0..40,
        ),
    ) {
        let mut g = Graph::new();
        for id in &ids {
            g.add_node(node(id)).unwrap();
        }
        for (a, b, r, c) in &pairs {
            g.add_edge(Edge {
                src: NodeId::new(a.get(&ids)),
                dst: NodeId::new(b.get(&ids)),
                relation: RELATIONS[*r],
                evidence: if *c == 0 { Evidence::Structural } else { Evidence::Scope },
                confidence: if *c == 0 { Confidence::Certain } else { Confidence::Inferred },
        site: None,
            }).unwrap();
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.redb");
        let store = Store::create(&path).unwrap();
        store.write_graph(&g).unwrap();
        drop(store);
        prop_assert_eq!(Store::open(&path).unwrap().read_graph().unwrap(), g);
    }
}

#[test]
fn unchanged_file_gets_rescoped_when_classification_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(dir.path().join("g.redb")).unwrap();
    let file = "harness/h.rs".to_string();
    let mut graph = Graph::new();
    graph.add_node(node("h")).unwrap();
    store.write_graph(&graph).unwrap();
    // First classifier: harness/ is production.
    let n = store
        .set_file_scopes(&[(file.clone(), CorpusScope::Production)])
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(store.file_scopes().unwrap()[&file], CorpusScope::Production);
    // Classifier changed, file bytes did not: no update_files call.
    let n = store
        .set_file_scopes(&[(file.clone(), CorpusScope::Test)])
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(store.file_scopes().unwrap()[&file], CorpusScope::Test);
    // Same classification again: write-free, zero rows.
    assert_eq!(
        store.set_file_scopes(&[(file, CorpusScope::Test)]).unwrap(),
        0
    );
}
