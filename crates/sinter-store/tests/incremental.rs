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
        site: None,
        extra_sites: Vec::new(),
        sites_total: 0,
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
        fields: Vec::new(),
        embeds: Vec::new(),
        trait_impls: Vec::new(),
        scopes: Vec::new(),
        body_terms: Vec::new(),
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
        site: None,
        extra_sites: Vec::new(),
        sites_total: 0,
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

/// apply_resolution's teardown drops only non-structural edges of the
/// given files.
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
            site: None,
            extra_sites: Vec::new(),
            sites_total: 0,
        }])
        .unwrap();

    let mut files = BTreeSet::new();
    files.insert("a.rs".to_string());
    store.apply_resolution(&files, &[], &files, &[]).unwrap();

    assert!(store.out_edges(&a.nodes[1].id).unwrap().is_empty());
    // Structural contains edge from the file node survives.
    assert_eq!(store.in_edges(&a.nodes[1].id).unwrap().len(), 1);
}

/// The lethal crash window: update_files tears down a dependent file's
/// binding (an in-edge into the changed file), then the process dies
/// before the resolution pass re-derives it. The dependent set is gone
/// from the tables — only the pending delta, committed atomically with
/// the teardown, still names it. It must survive reopen and clear only
/// on demand.
#[test]
fn pending_delta_survives_crash_between_update_and_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("g.redb");
    let store = Store::create(&db).unwrap();

    let a = facts("a.rs", "h1", &["alpha"], &[]);
    // b's binding into a is an import bound to a's file node: b references
    // no name a defines, so ref_files(def_names) can never rediscover it.
    let b = facts("b.rs", "h2", &["beta"], &[]);
    store.update_files(&[a.clone(), b.clone()], &[]).unwrap();
    store.commit_hashes(&[a.clone(), b.clone()]).unwrap();
    let bind = Edge {
        src: b.nodes[0].id.clone(),
        dst: a.nodes[0].id.clone(),
        relation: Relation::Imports,
        evidence: Evidence::Import,
        confidence: Confidence::Inferred,
        site: None,
        extra_sites: Vec::new(),
        sites_total: 0,
    };
    store.insert_edges(std::slice::from_ref(&bind)).unwrap();
    store.clear_pending_delta().unwrap();

    // Change a.rs; the crash happens here: no resolution pass follows.
    let a2 = facts("a.rs", "h3", &["alpha2"], &[]);
    let delta = store.update_files(std::slice::from_ref(&a2), &[]).unwrap();
    assert!(delta.dependent_files.contains("b.rs"));
    let bound_into_a = |store: &Store| {
        store
            .out_edges(&b.nodes[0].id)
            .unwrap()
            .iter()
            .any(|e| e.dst == a.nodes[0].id)
    };
    assert!(!bound_into_a(&store), "binding should be torn down");
    drop(store);

    // Next build: the persisted delta still names the dependent file.
    let store = Store::open(&db).unwrap();
    let residue = store.pending_delta().unwrap();
    assert!(
        residue.dependent_files.contains("b.rs"),
        "crash residue lost: {residue:?}"
    );
    assert!(residue.def_names.contains("alpha"));

    // Replay what the pipeline does with the residue: re-resolve b.rs and
    // commit its re-derived binding atomically with the teardown.
    let files: BTreeSet<String> = ["a.rs".to_string(), "b.rs".to_string()].into();
    store
        .apply_resolution(&files, std::slice::from_ref(&bind), &files, &[])
        .unwrap();
    store.clear_pending_delta().unwrap();
    assert!(bound_into_a(&store), "binding must be recovered");
    let cleared = store.pending_delta().unwrap();
    assert!(cleared.dependent_files.is_empty() && cleared.def_names.is_empty());
}

#[test]
fn snapshot_token_is_stable_until_committed_content_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(dir.path().join("g.redb")).unwrap();
    let first = facts("a.rs", "content-a", &["alpha"], &[]);
    store
        .update_files(std::slice::from_ref(&first), &[])
        .unwrap();
    store.commit_hashes(std::slice::from_ref(&first)).unwrap();
    let token = store.snapshot_token().unwrap();

    assert_eq!(store.snapshot_token().unwrap(), token);

    let second = facts("a.rs", "content-b", &["alpha"], &[]);
    store
        .update_files(std::slice::from_ref(&second), &[])
        .unwrap();
    store.commit_hashes(std::slice::from_ref(&second)).unwrap();
    assert_ne!(store.snapshot_token().unwrap(), token);
}
