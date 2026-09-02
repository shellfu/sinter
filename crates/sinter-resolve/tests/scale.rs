//! Absolute resolver hot-path budgets at a corpus size large enough to expose
//! accidental whole-graph work. Run in release mode from the nightly job.

use std::time::{Duration, Instant};

use sinter_core::{Node, NodeId, Reference, Relation, Span, SymbolKind};
use sinter_resolve::{Index, resolve};

const NODE_COUNT: usize = 200_000;
const REFERENCE_COUNT: usize = 20_000;
const FILE_COUNT: usize = 10_000;

fn node(index: usize) -> Node {
    let file = format!("src/module_{}/file_{}.rs", index % 100, index % FILE_COUNT);
    let name = format!("symbol_{index}");
    Node {
        id: NodeId::new(format!("{file}#{name}@{index}")),
        kind: SymbolKind::Function,
        name,
        file,
        span: Span {
            start: index as u64,
            end: index as u64 + 1,
        },
        signature: String::new(),
        doc: None,
    }
}

#[test]
#[ignore = "nightly release-mode performance gate"]
fn index_and_resolve_200k_nodes_with_absolute_budgets() {
    let nodes: Vec<Node> = (0..NODE_COUNT).map(node).collect();
    let references: Vec<Reference> = nodes
        .iter()
        .take(REFERENCE_COUNT)
        .map(|target| Reference {
            file: target.file.clone(),
            name: target.name.clone(),
            path: None,
            relation: Relation::Calls,
            span: target.span,
            enclosing: None,
            alias: None,
        })
        .collect();

    let started = Instant::now();
    let index = Index::build(&nodes, &[], &[], &[], &[], &[]);
    let index_elapsed = started.elapsed();

    let started = Instant::now();
    let (bindings, stats, ..) = resolve(&index, &references);
    let resolve_elapsed = started.elapsed();

    assert_eq!(bindings.len(), REFERENCE_COUNT);
    assert_eq!(stats.unresolved(), 0);
    assert!(
        index_elapsed < Duration::from_secs(3),
        "200k-node resolver index took {index_elapsed:?}; budget is 3s"
    );
    assert!(
        resolve_elapsed < Duration::from_secs(1),
        "20k same-file resolutions took {resolve_elapsed:?}; budget is 1s"
    );
    eprintln!("resolver scale: index={index_elapsed:?}, resolve={resolve_elapsed:?}");
}
