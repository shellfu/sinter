//! R8 scale exercise: 500k-node synthetic graph. Slow — ignored in the PR
//! gate, run nightly in release mode:
//! `cargo test --release -p sinter-store --test scale -- --ignored`

use std::time::{Duration, Instant};

use sinter_core::{Confidence, Edge, Evidence, Graph, Node, NodeId, Relation, Span, SymbolKind};
use sinter_store::Store;

/// Time a cold open + point query on a 1k-node store built the same way as
/// the exercise graph. This is the "unit cost" of the host's disk and page
/// cache; budgets below are multiples of it so the gate measures sinter's
/// scaling, not the runner's hardware (a fixed 100ms passed locally and
/// failed on GitHub's shared runners at 127ms with nothing regressed).
fn host_unit_cost(dir: &std::path::Path) -> Duration {
    let path = dir.join("probe.redb");
    let mut g = Graph::new();
    for i in 0..1_000usize {
        g.add_node(Node {
            id: NodeId::new(format!("p.rs#n{i}@0")),
            kind: SymbolKind::Function,
            name: format!("n{i}"),
            file: "p.rs".into(),
            span: Span { start: 0, end: 50 },
            signature: format!("fn n{i}()"),
            doc: None,
        })
        .unwrap();
    }
    let store = Store::create(&path).unwrap();
    store.write_graph(&g).unwrap();
    drop(store);
    let cold = Instant::now();
    let store = Store::open(&path).unwrap();
    assert!(store.node(&NodeId::new("p.rs#n500@0")).unwrap().is_some());
    cold.elapsed()
}

#[test]
#[ignore = "500k-node exercise; nightly release-mode gate"]
fn synthetic_500k_nodes_within_budgets() {
    let n = 500_000usize;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.redb");
    let unit = host_unit_cost(dir.path());
    // Floors keep the gate at least as strict as the original absolute
    // budgets on fast hardware; the multiple absorbs slow shared runners.
    let cold_budget = Duration::from_millis(100).max(unit * 50);
    let warm_budget = Duration::from_millis(50).max(unit * 25);

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
        cold_elapsed < cold_budget,
        "cold query {cold_elapsed:?}, budget {cold_budget:?} (host unit {unit:?}) at 500k nodes"
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
        per_query < warm_budget,
        "warm query {per_query:?} avg, budget {warm_budget:?} (host unit {unit:?})"
    );
}
