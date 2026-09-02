use std::collections::BTreeSet;

use protobuf::Message;
use scip::types::{Document, Index, Occurrence};
use sinter_core::{Evidence, Node, NodeId, Reference, Relation, Span, SymbolKind};
use sinter_resolve::{load_index, prefix_index_paths, resolve_with_index};

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

#[test]
fn nested_project_document_paths_are_rebased_to_the_repository() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.scip");
    let index = Index {
        documents: vec![
            Document {
                relative_path: "src/lib.rs".to_string(),
                ..Default::default()
            },
            Document {
                relative_path: "packages/core/src/already.rs".to_string(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    std::fs::write(&path, index.write_to_bytes().unwrap()).unwrap();

    prefix_index_paths(&path, "packages/core").unwrap();

    let paths: Vec<String> = load_index(&path)
        .unwrap()
        .documents
        .into_iter()
        .map(|document| document.relative_path)
        .collect();
    assert_eq!(
        paths,
        ["packages/core/src/lib.rs", "packages/core/src/already.rs"]
    );
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

    let resolution =
        resolve_with_index(
            &index,
            &nodes,
            &references,
            &BTreeSet::new(),
            |path| match path {
                "a.rs" => Some(a_src.to_string()),
                "b.rs" => Some(b_src.to_string()),
                _ => None,
            },
        );

    let bindings = resolution.bindings;
    assert_eq!(bindings.len(), 1);
    assert!(resolution.external.is_empty());
    assert_eq!(resolution.external_skipped, 0);
    let edge = &bindings[0].edge;
    assert_eq!(edge.src.as_str(), "b.rs#caller@0");
    assert_eq!(edge.dst.as_str(), "a.rs#target@0");
    assert_eq!(edge.relation, Relation::Calls);
    assert_eq!(edge.evidence, Evidence::Scip);
    assert_eq!(bindings[0].reference, 0);
}

/// A reference occurrence whose symbol has no in-corpus definition
/// synthesizes a dependency-surface node from the moniker (D29): pseudo-file
/// `dep:<package>@<version>`, package-rooted qualified path, kind from the
/// final descriptor marker.
#[test]
fn external_moniker_synthesizes_dep_node() {
    let src = "fn caller() { tokio::task::spawn(f); }\n";
    let nodes = vec![node(
        "m.rs#caller@0",
        "m.rs",
        "caller",
        Span { start: 0, end: 38 },
    )];
    let references = vec![Reference {
        file: "m.rs".to_string(),
        name: "spawn".to_string(),
        path: Some("tokio::task::spawn".to_string()),
        relation: Relation::Calls,
        span: Span { start: 14, end: 32 },
        enclosing: Some(NodeId::new("m.rs#caller@0")),
        alias: None,
    }];
    let index = Index {
        documents: vec![Document {
            relative_path: "m.rs".to_string(),
            occurrences: vec![
                // Rightmost rule: the module occurrence loses to the item.
                Occurrence {
                    range: vec![0, 14, 19],
                    symbol: "rust-analyzer cargo tokio 1.0.0 task/".to_string(),
                    symbol_roles: 0,
                    ..Default::default()
                },
                Occurrence {
                    range: vec![0, 27, 32],
                    symbol: "rust-analyzer cargo tokio 1.0.0 task/spawn().".to_string(),
                    symbol_roles: 0,
                    ..Default::default()
                },
                // Unparseable moniker overlapping the ref: skipped, counted.
                Occurrence {
                    range: vec![0, 14, 19],
                    symbol: "rust-analyzer cargo . . task/".to_string(),
                    symbol_roles: 0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };

    let resolution = resolve_with_index(&index, &nodes, &references, &BTreeSet::new(), |path| {
        (path == "m.rs").then(|| src.to_string())
    });

    assert!(resolution.bindings.is_empty());
    assert_eq!(resolution.external.len(), 1);
    let edge = &resolution.external[0].edge;
    assert_eq!(edge.src.as_str(), "m.rs#caller@0");
    assert_eq!(edge.dst.as_str(), "dep:tokio@1.0.0#tokio::task::spawn@0");
    assert_eq!(edge.evidence, Evidence::Scip);
    let dep = resolution
        .external_nodes
        .iter()
        .find(|n| n.id.as_str() == "dep:tokio@1.0.0#tokio::task::spawn@0")
        .expect("dep node synthesized");
    assert_eq!(dep.file, "dep:tokio@1.0.0");
    assert_eq!(dep.name, "spawn");
    assert_eq!(dep.kind, SymbolKind::Function);
    assert_eq!(dep.span, Span { start: 0, end: 0 });
    assert_eq!(resolution.external_skipped, 1);
}

/// Descriptor markers map to kinds: `#` type -> Struct, `#name().` ->
/// Method, trailing `.` term -> Constant, `!` -> Macro, `/` -> Module.
#[test]
fn descriptor_markers_pick_kinds() {
    let cases = [
        ("runtime/Runtime#", SymbolKind::Struct, "Runtime"),
        (
            "runtime/Runtime#block_on().",
            SymbolKind::Method,
            "block_on",
        ),
        ("sync/MAX.", SymbolKind::Constant, "MAX"),
        ("select!", SymbolKind::Macro, "select"),
        ("task/", SymbolKind::Module, "task"),
    ];
    for (descriptors, kind, name) in cases {
        let symbol = format!("rust-analyzer cargo tokio 1.0.0 {descriptors}");
        let src = "fn c() { xxxxx }\n";
        let references = vec![Reference {
            file: "m.rs".to_string(),
            name: name.to_string(),
            path: None,
            relation: Relation::Uses,
            span: Span { start: 9, end: 14 },
            enclosing: None,
            alias: None,
        }];
        let index = Index {
            documents: vec![Document {
                relative_path: "m.rs".to_string(),
                occurrences: vec![Occurrence {
                    range: vec![0, 9, 14],
                    symbol,
                    symbol_roles: 0,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let resolution = resolve_with_index(&index, &[], &references, &BTreeSet::new(), |path| {
            (path == "m.rs").then(|| src.to_string())
        });
        assert_eq!(resolution.external.len(), 1, "{descriptors}");
        let dep = &resolution.external_nodes[0];
        assert_eq!(dep.kind, kind, "{descriptors}");
        assert_eq!(dep.name, name, "{descriptors}");
    }
}

/// A SCIP occurrence inside a macro token tree overlaps no extracted
/// reference; in a file being resolved it still becomes an edge from the
/// enclosing node to the definition. Out of scope, nothing is produced.
#[test]
fn unanchored_occurrence_in_scope_becomes_edge() {
    let a_src = "fn target() {}\n";
    let b_src = "fn caller() { assert_eq!(target(), 1); }\n";
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
            Span { start: 0, end: 40 },
        ),
    ];
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
                    range: vec![0, 25, 31],
                    symbol: "test . . . target().".to_string(),
                    symbol_roles: 0,
                    ..Default::default()
                }],
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let read = |path: &str| match path {
        "a.rs" => Some(a_src.to_string()),
        "b.rs" => Some(b_src.to_string()),
        _ => None,
    };

    let out_of_scope = resolve_with_index(&index, &nodes, &[], &BTreeSet::new(), read);
    assert!(out_of_scope.bindings.is_empty());
    assert!(out_of_scope.unanchored.is_empty());

    let scope = BTreeSet::from(["b.rs".to_string()]);
    let resolution = resolve_with_index(&index, &nodes, &[], &scope, read);
    assert!(resolution.bindings.is_empty());
    assert_eq!(resolution.unanchored.len(), 1);
    let edge = &resolution.unanchored[0];
    assert_eq!(edge.src.as_str(), "b.rs#caller@0");
    assert_eq!(edge.dst.as_str(), "a.rs#target@0");
    assert_eq!(edge.relation, Relation::Calls);
    assert_eq!(edge.evidence, Evidence::Scip);
    assert_eq!(edge.site, Some(Span { start: 25, end: 31 }));
}

/// A document the caller declines to read (edited since indexing) withholds
/// its symbols everywhere: references to them neither bind by stale
/// position nor fall through to a synthesized dependency node.
#[test]
fn unread_definition_document_withholds_its_symbols() {
    let b_src = "fn caller() { target(); }\n";
    let nodes = vec![
        node(
            "a.rs#renamed@0",
            "a.rs",
            "renamed",
            Span { start: 0, end: 15 },
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
    let symbol = "rust-analyzer cargo fixture 0.1.0 a/target().";
    let index = Index {
        documents: vec![
            Document {
                relative_path: "a.rs".to_string(),
                occurrences: vec![Occurrence {
                    range: vec![0, 3, 9],
                    symbol: symbol.to_string(),
                    symbol_roles: scip::types::SymbolRole::Definition as i32,
                    ..Default::default()
                }],
                ..Default::default()
            },
            Document {
                relative_path: "b.rs".to_string(),
                occurrences: vec![Occurrence {
                    range: vec![0, 14, 20],
                    symbol: symbol.to_string(),
                    symbol_roles: 0,
                    ..Default::default()
                }],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let resolution = resolve_with_index(&index, &nodes, &references, &BTreeSet::new(), |path| {
        (path == "b.rs").then(|| b_src.to_string())
    });

    assert!(resolution.bindings.is_empty());
    assert!(resolution.external.is_empty());
    assert!(resolution.external_nodes.is_empty());
}
