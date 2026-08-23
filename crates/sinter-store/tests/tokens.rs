//! TOKENS_WORDS index: word -> node id recall index maintained by
//! `update_files`, queried via `nodes_with_token` / `candidates_for_terms`.

use sinter_core::{FileFacts, Node, NodeId, Span, SymbolKind};
use sinter_store::Store;

fn node(file: &str, name: &str, signature: &str, doc: Option<&str>) -> Node {
    Node {
        id: NodeId::new(format!("{file}#{name}@10")),
        kind: SymbolKind::Class,
        name: name.to_string(),
        file: file.to_string(),
        span: Span { start: 10, end: 20 },
        signature: signature.to_string(),
        doc: doc.map(str::to_string),
    }
}

fn facts(file: &str, hash: &str, nodes: Vec<Node>) -> FileFacts {
    FileFacts {
        file: file.to_string(),
        content_hash: hash.to_string(),
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

#[test]
fn roundtrip_indexes_name_doc_signature_and_path_words() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(dir.path().join("g.redb")).unwrap();

    let n = node(
        "src/player/BLPlayerCharacterV2.h",
        "ABLPlayerCharacterV2",
        "class ABLPlayerCharacterV2 : public ACharacter",
        Some("Main player character controller: movement, traversal."),
    );
    store
        .update_files(&[facts(&n.file.clone(), "h1", vec![n.clone()])], &[])
        .unwrap();

    // doc word, signature word, name subword, path segment, whole identifier.
    for word in [
        "traversal",
        "public",
        "character",
        "player",
        "ablplayercharacterv2",
    ] {
        let hits = store.nodes_with_token(word).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            vec![n.id.as_str()],
            "word {word:?}"
        );
    }
    assert!(store.nodes_with_token("missing").unwrap().is_empty());
}

#[test]
fn reupdate_removes_stale_words() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(dir.path().join("g.redb")).unwrap();
    let file = "src/thing.rs";

    let old = node(file, "OldWidget", "fn ancient()", Some("obsolete gadget"));
    store
        .update_files(&[facts(file, "h1", vec![old])], &[])
        .unwrap();
    assert_eq!(store.nodes_with_token("obsolete").unwrap().len(), 1);

    let new = node(file, "NewWidget", "fn modern()", Some("shiny gadget"));
    store
        .update_files(&[facts(file, "h2", vec![new.clone()])], &[])
        .unwrap();

    for stale in ["obsolete", "ancient", "oldwidget", "old"] {
        assert!(
            store.nodes_with_token(stale).unwrap().is_empty(),
            "stale {stale:?}"
        );
    }
    assert_eq!(store.nodes_with_token("shiny").unwrap()[0].id, new.id);

    // Removal tears down words too.
    store.update_files(&[], &[file.to_string()]).unwrap();
    assert!(store.nodes_with_token("shiny").unwrap().is_empty());
    assert!(store.nodes_with_token("thing").unwrap().is_empty());
}

#[test]
fn candidates_dedup_and_deterministic_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(dir.path().join("g.redb")).unwrap();

    let a = node("a.rs", "ClimbController", "", Some("handles climbs"));
    let b = node("b.rs", "ClimbAnim", "", None);
    let c = node("c.rs", "Unrelated", "", None);
    store
        .update_files(
            &[
                facts("a.rs", "h1", vec![a.clone()]),
                facts("b.rs", "h2", vec![b.clone()]),
                facts("c.rs", "h3", vec![c]),
            ],
            &[],
        )
        .unwrap();

    // "climbs" hits `a` via the singular variant AND `a` again via its doc
    // word "climbs"; "climb" hits both a and b. Dedup by id, sorted by id.
    let terms = vec!["climbs".to_string(), "climb".to_string()];
    let hits = store.candidates_for_terms(&terms).unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, vec![a.id.as_str(), b.id.as_str()]);

    // Determinism: same result across runs and term order.
    let rev = vec!["climb".to_string(), "climbs".to_string()];
    let hits2 = store.candidates_for_terms(&rev).unwrap();
    assert_eq!(hits2.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ids);
}

#[test]
fn body_terms_index_is_replaced_per_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(dir.path().join("g.redb")).unwrap();
    let n = node("src/a.rs", "scan", "fn scan()", None);
    let mut f = facts("src/a.rs", "h1", vec![n.clone()]);
    f.body_terms = vec![(n.id.clone(), vec!["stat".into(), "walk".into()])];
    store.update_files(&[f], &[]).unwrap();
    assert_eq!(store.body_term_df("stat").unwrap(), 1);
    let hits = store.nodes_with_body_term("stat", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, n.id);

    // Re-extracted without the word: the old row goes away.
    let mut f = facts("src/a.rs", "h2", vec![n.clone()]);
    f.body_terms = vec![(n.id.clone(), vec!["walk".into()])];
    store.update_files(&[f], &[]).unwrap();
    assert_eq!(store.body_term_df("stat").unwrap(), 0);
    assert_eq!(store.body_term_df("walk").unwrap(), 1);
    store.update_files(&[], &["src/a.rs".to_string()]).unwrap();
    assert_eq!(store.body_term_df("walk").unwrap(), 0);
}
