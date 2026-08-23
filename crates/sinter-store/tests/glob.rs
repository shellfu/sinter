//! `Store::nodes_glob`: `Type::*`, `*::method`, and bare-name `pre*`/`*fix`.

use sinter_core::{FileFacts, Node, NodeId, Span, SymbolKind};
use sinter_store::Store;

fn node(file: &str, qualified: &str, start: u64) -> Node {
    Node {
        id: NodeId::new(format!("{file}#{qualified}@{start}")),
        kind: SymbolKind::Function,
        name: qualified.rsplit("::").next().unwrap().to_string(),
        file: file.to_string(),
        span: Span {
            start,
            end: start + 5,
        },
        signature: String::new(),
        doc: None,
    }
}

fn facts(file: &str, nodes: Vec<Node>) -> FileFacts {
    FileFacts {
        file: file.to_string(),
        content_hash: file.to_string(),
        has_syntax_errors: false,
        nodes,
        contains: Vec::new(),
        references: Vec::new(),
        locals: Vec::new(),
        fields: Vec::new(),
        embeds: Vec::new(),
        trait_impls: Vec::new(),
        scopes: Vec::new(),
        body_terms: Vec::new(),
    }
}

fn ids(nodes: &[Node]) -> Vec<&str> {
    nodes.iter().map(|n| n.id.as_str()).collect()
}

#[test]
fn glob_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(dir.path().join("g.redb")).unwrap();
    store
        .update_files(
            &[
                facts(
                    "a.rs",
                    vec![
                        node("a.rs", "Hooks::run", 10),
                        node("a.rs", "Hooks::resolve_one", 30),
                        node("a.rs", "run", 50),
                    ],
                ),
                facts(
                    "b.rs",
                    vec![
                        node("b.rs", "Other::run", 10),
                        node("b.rs", "resolve_two", 20),
                    ],
                ),
            ],
            &[],
        )
        .unwrap();

    let members = store.nodes_glob("Hooks::", "").unwrap();
    assert_eq!(
        ids(&members),
        ["a.rs#Hooks::resolve_one@30", "a.rs#Hooks::run@10"]
    );

    let runs = store.nodes_glob("", "::run").unwrap();
    assert_eq!(ids(&runs), ["a.rs#Hooks::run@10", "b.rs#Other::run@10"]);

    let prefix = store.nodes_glob("resolve_", "").unwrap();
    assert_eq!(
        ids(&prefix),
        ["a.rs#Hooks::resolve_one@30", "b.rs#resolve_two@20"]
    );

    let suffix = store.nodes_glob("", "_two").unwrap();
    assert_eq!(ids(&suffix), ["b.rs#resolve_two@20"]);
}
