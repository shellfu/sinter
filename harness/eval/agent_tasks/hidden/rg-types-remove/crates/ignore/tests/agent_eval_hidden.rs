use ignore::types::TypesBuilder;
#[test]
fn remove_definition() {
    let mut b = TypesBuilder::new();
    b.add("foo", "*.foo").unwrap();
    b.add("bar", "*.bar").unwrap();
    assert!(b.remove("foo"));
    assert!(!b.remove("foo"));
    let names: Vec<String> = b.definitions().iter().map(|d| d.name().to_string()).collect();
    assert_eq!(names, vec!["bar".to_string()]);
}
