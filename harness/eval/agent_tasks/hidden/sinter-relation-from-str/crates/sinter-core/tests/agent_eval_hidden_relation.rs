use sinter_core::{Evidence, Relation};
#[test]
fn relation_roundtrip() {
    for r in [Relation::Calls, Relation::Uses, Relation::Imports, Relation::Contains, Relation::Implements, Relation::Extends] {
        assert_eq!(Relation::from_str_opt(r.as_str()), Some(r));
    }
    assert_eq!(Relation::from_str_opt("Calls"), None);
}
#[test]
fn evidence_roundtrip() {
    for e in [Evidence::Structural, Evidence::Scope, Evidence::Import, Evidence::Scip, Evidence::Declared, Evidence::Dynamic] {
        assert_eq!(Evidence::from_str_opt(e.as_str()), Some(e));
    }
    assert_eq!(Evidence::from_str_opt("x"), None);
}
