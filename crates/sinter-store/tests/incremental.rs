use std::collections::BTreeSet;

use sinter_core::{
    Confidence, Edge, Evidence, FileFacts, Node, NodeId, Reference, Relation, Span, SymbolKind,
};
use sinter_store::Store;

fn node(file: &str, name: &str, start: u64) -> Node {
    Node {
        id: NodeId::new(format!("{file}#{name}@{start}")),
        kind: SymbolKind::Function,
        name: name.to_string(),
        file: file.to_string(),
        span: Span {
            start,
            end: start + 10,
        },
        signature: format!("fn {name}()"),
        doc: None,
    }
}

fn file_node(file: &str) -> Node {
    Node {
        id: NodeId::new(file),
        kind: SymbolKind::File,
        name: file.to_string(),
        file: file.to_string(),
        span: Span { start: 0, end: 100 },
        signature: String::new(),
        doc: None,
    }
}

fn contains(src: &Node, dst: &Node) -> Edge {
    Edge {
        src: src.id.clone(),
        dst: dst.id.clone(),
        relation: Relation::Contains,
        evidence: Evidence::Structural,
        confidence: Confidence::Certain,
    }
}

fn facts(file: &str, hash: &str, defs: &[&str], ref_names: &[&str]) -> FileFacts {
    let f = file_node(file);
    let mut nodes = vec![f.clone()];
    let mut edges = Vec::new();
    for (i, d) in defs.iter().enumerate() {
        let n = node(file, d, (i as u64 + 1) * 10);
        edges.push(contains(&f, &n));
        nodes.push(n);
    }
    let references = ref_names
        .iter()
        .enumerate()
        .map(|(i, r)| Reference {
            file: file.to_string(),
            name: r.to_string(),
            path: None,
            relation: Relation::Calls,
            span: Span {
                start: 90 + i as u64,
                end: 91 + i as u64,
            },
            enclosing: None,
            alias: None,
        })
        .collect();
    FileFacts {
        file: file.to_string(),
        content_hash: hash.to_string(),
        has_syntax_errors: false,
        nodes,
        contains: edges,
        references,
        locals: Vec::new(),
        embeds: Vec::new(),
        trait_impls: Vec::new(),
    }
}

/// Updating one file replaces exactly its derived state: old nodes and their
/// edges vanish (both directions), new state queryable, other files intact.
#[test]
fn update_replaces_only_touched_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(dir.path().join("g.redb")).unwrap();

    let a1 = facts("a.rs", "h1", &["alpha", "beta"], &["gamma"]);
    let b1 = facts("b.rs", "h2", &["gamma"], &["alpha"]);
    store.update_files(&[a1.clone(), b1.clone()], &[]).unwrap();
    store.commit_hashes(&[a1.clone(), b1.clone()]).unwrap();

    // Cross-file resolution edge b.rs#gamma -> a.rs#alpha.
    let cross = Edge {
        src: b1.nodes[1].id.clone(),
        dst: a1.nodes[1].id.clone(),
        relation: Relation::Calls,
        evidence: Evidence::Scope,
        confidence: Confidence::Inferred,
    };
    store.insert_edges(std::slice::from_ref(&cross)).unwrap();
    assert_eq!(store.in_edges(&a1.nodes[1].id).unwrap().len(), 2); // contains + cross

    // Change a.rs: alpha renamed to alpha2.
    let a2 = facts("a.rs", "h3", &["alpha2", "beta"], &["gamma"]);
    let delta = store.update_files(std::slice::from_ref(&a2), &[]).unwrap();
    store.commit_hashes(std::slice::from_ref(&a2)).unwrap();

    // b.rs held a resolution edge into a.rs: it must be re-resolved even
    // if it referenced none of the changed names (H1 regression).
    assert!(delta.dependent_files.contains("b.rs"));

    // Delta names cover old and new defs of the touched file.
    for name in ["alpha", "alpha2", "beta", "a.rs"] {
        assert!(delta.def_names.contains(name), "missing {name}");
    }
    // Old node gone, edges to it gone from b.rs's out list too.
    assert!(store.node(&a1.nodes[1].id).unwrap().is_none());
    assert!(
        store
            .out_edges(&b1.nodes[1].id)
            .unwrap()
            .iter()
            .all(|e| e.dst != a1.nodes[1].id)
    );
    // New node present and searchable; b.rs untouched.
    assert!(store.node(&a2.nodes[1].id).unwrap().is_some());
    assert_eq!(store.nodes_named("alpha2").unwrap().len(), 1);
    assert!(store.nodes_named("alpha").unwrap().is_empty());
    assert!(store.node(&b1.nodes[1].id).unwrap().is_some());

    // Invalidation index: who references the changed names?
    let files = store.ref_files(&delta.def_names).unwrap();
    assert!(files.contains("b.rs")); // b.rs references alpha

    // Removal tears everything down.
    store.update_files(&[], &["b.rs".to_string()]).unwrap();
    assert!(store.node(&b1.nodes[1].id).unwrap().is_none());
    assert!(store.nodes_named("gamma").unwrap().is_empty());
    assert_eq!(store.file_hashes().unwrap().len(), 1);
}

/// remove_resolution_edges drops only non-structural edges of the given files.
#[test]
fn resolution_edges_removed_structural_kept() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(dir.path().join("g.redb")).unwrap();
    let a = facts("a.rs", "h1", &["alpha"], &[]);
    let b = facts("b.rs", "h2", &["beta"], &[]);
    store.update_files(&[a.clone(), b.clone()], &[]).unwrap();
    store
        .insert_edges(&[Edge {
            src: a.nodes[1].id.clone(),
            dst: b.nodes[1].id.clone(),
            relation: Relation::Calls,
            evidence: Evidence::Import,
            confidence: Confidence::Inferred,
        }])
        .unwrap();

    let mut files = BTreeSet::new();
    files.insert("a.rs".to_string());
    store.remove_resolution_edges(&files).unwrap();

    assert!(store.out_edges(&a.nodes[1].id).unwrap().is_empty());
    // Structural contains edge from the file node survives.
    assert_eq!(store.in_edges(&a.nodes[1].id).unwrap().len(), 1);
}
