use std::time::{Duration, Instant};

use sinter_core::{Confidence, Edge, Evidence, Graph, Node, NodeId, Relation, Span, SymbolKind};
use sinter_store::Store;

/// Phase 1 perf deliverable: cold open + node + neighbors in under 100ms,
/// independent of graph size. 20k nodes / ~60k edges here; the R8 500k-node
/// exercise joins CI with the harness in later phases.
#[test]
fn cold_query_under_100ms() {
    let n = 20_000usize;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.redb");

    let mut g = Graph::new();
    for i in 0..n {
        g.add_node(Node {
            id: NodeId::new(format!("n{i}")),
            kind: SymbolKind::Function,
            name: format!("n{i}"),
            file: format!("src/m{}.rs", i % 500),
            span: Span { start: 0, end: 80 },
            signature: format!("fn n{i}()"),
            doc: None,
        })
        .unwrap();
    }
    for i in 0..n {
        for k in 1..=3u64 {
            g.add_edge(Edge {
                src: NodeId::new(format!("n{i}")),
                dst: NodeId::new(format!("n{}", (i as u64 * 7 + k * 131) as usize % n)),
                relation: Relation::Calls,
                evidence: Evidence::Structural,
                confidence: Confidence::Certain,
                site: None,
                extra_sites: Vec::new(),
                sites_total: 0,
            })
            .unwrap();
        }
    }

    let store = Store::create(&path).unwrap();
    store.write_graph(&g).unwrap();
    drop(store);

    let start = Instant::now();
    let store = Store::open(&path).unwrap();
    let target = NodeId::new("n12345");
    let node = store.node(&target).unwrap().expect("node present");
    let out = store.out_edges(&target).unwrap();
    // (12345 * 7 + 131) % 20000 = 6546: a known dst of n12345.
    let inn = store.in_edges(&NodeId::new("n6546")).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(node.name, "n12345");
    assert_eq!(out.len(), 3);
    assert!(inn.iter().any(|e| e.src == target));
    assert!(
        elapsed < Duration::from_millis(100),
        "cold open + point query took {elapsed:?}, budget 100ms"
    );
}
