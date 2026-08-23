use sinter_core::Relation;
use sinter_extract::{Extractor, spec_for_path};

fn refs(src: &str) -> Vec<(String, Option<String>, Relation)> {
    let spec = spec_for_path("x.rs").unwrap();
    Extractor::new(spec)
        .unwrap()
        .extract("x.rs", src)
        .unwrap()
        .references
        .into_iter()
        .map(|r| (r.name, r.path, r.relation))
        .collect()
}

#[test]
fn method_chain_segments_are_separate_calls_with_immediate_receiver() {
    let out = refs("fn f() { store.commit_hashes(&[a.clone()]).unwrap().len(); }");
    let calls: Vec<_> = out
        .iter()
        .filter(|(_, _, rel)| *rel == Relation::Calls)
        .map(|(n, p, _)| (n.as_str(), p.as_deref().unwrap()))
        .collect();
    assert_eq!(
        calls,
        vec![
            ("commit_hashes", "store.commit_hashes"),
            ("clone", "a.clone"),
            ("unwrap", "store.commit_hashes(&[a.clone()]).unwrap"),
            ("len", "store.commit_hashes(&[a.clone()]).unwrap().len"),
        ]
    );
}

#[test]
fn wildcard_and_prelude_names_emit_no_reference() {
    let out = refs(
        "fn f() -> Result<Vec<String>, u32> { let _: HashSet<_> = x; Self::High; Ok(Vec::new()) }",
    );
    let names: Vec<&str> = out.iter().map(|(n, _, _)| n.as_str()).collect();
    for noise in ["_", "Self", "Result", "Vec", "String", "u32", "Ok"] {
        assert!(!names.contains(&noise), "{noise} in {names:?}");
    }
    assert!(names.contains(&"HashSet"));
    assert!(names.contains(&"new"), "Vec::new call kept: {names:?}");
}

#[test]
fn shadowed_prelude_name_is_still_referenced() {
    let out = refs("struct Vec; fn f(v: Vec) {}");
    assert!(out.iter().any(|(n, _, _)| n == "Vec"));
}
