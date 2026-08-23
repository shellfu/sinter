use sinter_core::NodeId;
#[test]
fn nodeid_file() {
    assert_eq!(NodeId::new("src/a.rs#foo::bar@12").file(), "src/a.rs");
    assert_eq!(NodeId::new("src/a.rs#x").file(), "src/a.rs");
    assert_eq!(NodeId::new("src/a.rs").file(), "src/a.rs");
}
