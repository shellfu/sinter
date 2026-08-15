//! R8 scale exercise: 500k-node synthetic graph. Slow — ignored in the PR
//! gate, run nightly in release mode:
//! `cargo test --release -p sinter-store --test scale -- --ignored`

use std::time::{Duration, Instant};

use sinter_core::{Confidence, Edge, Evidence, Graph, Node, NodeId, Relation, Span, SymbolKind};
use sinter_store::Store;

#[test]
#[ignore = "500k-node exercise; nightly release-mode gate"]
fn synthetic_500k_nodes_within_budgets() {
    let n = 500_000usize;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.redb");

    let mut g = Graph::new();
    for i in 0..n {
        g.add_node(Node {
            id: NodeId::new(format!("f{}.rs#n{i}@0", i % 5000)),
            kind: SymbolKind::Function,
            name: format!("n{i}"),
            file: format!("f{}.rs", i % 5000),
            span: Span { start: 0, end: 50 },
            signature: format!("fn n{i}()"),
            doc: None,
        })
        .unwrap();
    }
    for i in 0..n {
        for k in 1..=3u64 {
            g.add_edge(Edge {
                src: NodeId::new(format!("f{}.rs#n{i}@0", i % 5000)),
                dst: NodeId::new(format!(
                    "f{}.rs#n{}@0",
                    ((i as u64 * 31 + k * 977) as usize % n) % 5000,
                    (i as u64 * 31 + k * 977) as usize % n
                )),
                relation: Relation::Calls,
                evidence: Evidence::Scope,
                confidence: Confidence::Inferred,
            })
            .unwrap();
        }
    }

    let store = Store::create(&path).unwrap();
    store.write_graph(&g).unwrap();
    drop(store);

    // Cold open + point query.
    let cold = Instant::now();
    let store = Store::open(&path).unwrap();
    let target = NodeId::new("f345.rs#n345@0");
    assert!(store.node(&target).unwrap().is_some());
    assert_eq!(store.out_edges(&target).unwrap().len(), 3);
    let cold_elapsed = cold.elapsed();
    assert!(
        cold_elapsed < Duration::from_millis(100),
        "cold query {cold_elapsed:?}, budget 100ms at 500k nodes"
    );

    // Warm queries across the graph.
    let warm = Instant::now();
    for i in (0..n).step_by(50_000) {
        let id = NodeId::new(format!("f{}.rs#n{i}@0", i % 5000));
        store.node(&id).unwrap().expect("node present");
        store.out_edges(&id).unwrap();
        store.in_edges(&id).unwrap();
    }
    let per_query = warm.elapsed() / 10;
    assert!(
        per_query < Duration::from_millis(50),
        "warm query {per_query:?} avg, budget 50ms"
    );
}
