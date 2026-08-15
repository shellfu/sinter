use scip::types::{Document, Index, Occurrence};
use sinter_core::{Evidence, Node, NodeId, Reference, Relation, Span, SymbolKind};
use sinter_resolve::resolve_with_index;

fn node(id: &str, file: &str, name: &str, span: Span) -> Node {
    Node {
        id: NodeId::new(id),
        kind: SymbolKind::Function,
        name: name.to_string(),
        file: file.to_string(),
        span,
        signature: format!("fn {name}()"),
        doc: None,
    }
}

/// A SCIP reference occurrence overlapping one of our reference spans binds
/// it to the node containing the symbol's definition occurrence.
#[test]
fn scip_binds_reference_to_definition() {
    let a_src = "fn target() {}\n";
    let b_src = "fn caller() { target(); }\n";

    let nodes = vec![
        node(
            "a.rs#target@0",
            "a.rs",
            "target",
            Span { start: 0, end: 14 },
        ),
        node(
            "b.rs#caller@0",
            "b.rs",
            "caller",
            Span { start: 0, end: 25 },
        ),
    ];
    let references = vec![Reference {
        file: "b.rs".to_string(),
        name: "target".to_string(),
        path: None,
        relation: Relation::Calls,
        span: Span { start: 14, end: 20 },
        enclosing: Some(NodeId::new("b.rs#caller@0")),
        alias: None,
    }];

    let index = Index {
        documents: vec![
            Document {
                relative_path: "a.rs".to_string(),
                occurrences: vec![Occurrence {
                    range: vec![0, 3, 9],
                    symbol: "test . . . target().".to_string(),
                    symbol_roles: scip::types::SymbolRole::Definition as i32,
                    ..Default::default()
                }],
                ..Default::default()
            },
            Document {
                relative_path: "b.rs".to_string(),
                occurrences: vec![Occurrence {
                    range: vec![0, 14, 20],
                    symbol: "test . . . target().".to_string(),
                    symbol_roles: 0,
                    ..Default::default()
                }],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let bindings = resolve_with_index(&index, &nodes, &references, |path| match path {
        "a.rs" => Some(a_src.to_string()),
        "b.rs" => Some(b_src.to_string()),
        _ => None,
    });

    assert_eq!(bindings.len(), 1);
    let edge = &bindings[0].edge;
    assert_eq!(edge.src.as_str(), "b.rs#caller@0");
    assert_eq!(edge.dst.as_str(), "a.rs#target@0");
    assert_eq!(edge.relation, Relation::Calls);
    assert_eq!(edge.evidence, Evidence::Scip);
    assert_eq!(bindings[0].reference, 0);
}
