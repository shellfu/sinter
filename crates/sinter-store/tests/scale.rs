//! R8 scale exercise: 500k-node synthetic graph. Slow — ignored in the PR
//! gate, run nightly in release mode:
//! `cargo test --release -p sinter-store --test scale -- --ignored`

use std::time::{Duration, Instant};

use sinter_core::{Confidence, Edge, Evidence, Graph, Node, NodeId, Relation, Span, SymbolKind};
use sinter_store::{EdgeFilter, Store};

#[test]
#[ignore = "500k-node exercise; nightly release-mode gate"]
fn synthetic_500k_nodes_within_budgets() {
    let n = 500_000usize;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.redb");
    // This is an ignored release/nightly gate, so enforce the product
    // budgets directly instead of weakening them with a host multiplier.
    let cold_budget = Duration::from_millis(100);
    let warm_budget = Duration::from_millis(50);
    let traversal_budget = Duration::from_millis(100);

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
                site: None,
            })
            .unwrap();
        }
    }

    let store = Store::create(&path).unwrap();
    store.write_graph(&g).unwrap();
    drop(store);

    let target = NodeId::new("f345.rs#n345@0");
    // Cold open + point query. Hosted CI runners can preempt a process in
    // the middle of a wall-clock sample; use the best of three independent
    // opens as the uncontended cost. A real regression slows every sample.
    let mut cold_samples = [Duration::ZERO; 3];
    for sample in &mut cold_samples {
        let cold = Instant::now();
        let store = Store::open(&path).unwrap();
        assert!(store.node(&target).unwrap().is_some());
        assert_eq!(store.out_edges(&target).unwrap().len(), 3);
        *sample = cold.elapsed();
    }
    let cold_elapsed = cold_samples.iter().copied().min().unwrap();
    eprintln!("cold query samples: {cold_samples:?}; best {cold_elapsed:?}");
    assert!(
        cold_elapsed < cold_budget,
        "best cold query {cold_elapsed:?}, budget {cold_budget:?}, samples {cold_samples:?} at 500k nodes"
    );

    // Warm queries across the graph.
    let store = Store::open(&path).unwrap();
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
        "warm query {per_query:?} avg, budget {warm_budget:?}"
    );

    // Real query verbs traverse, rather than issuing isolated point reads.
    // Depth four visits up to 120 outgoing nodes in this 3-way graph.
    let traversal = Instant::now();
    let reached = store
        .dependencies(&target, &EdgeFilter::default(), 4)
        .unwrap();
    let traversal_elapsed = traversal.elapsed();
    assert!(!reached.is_empty());
    assert!(
        traversal_elapsed < traversal_budget,
        "depth-4 traversal {traversal_elapsed:?}, budget {traversal_budget:?} at 500k nodes"
    );
}
