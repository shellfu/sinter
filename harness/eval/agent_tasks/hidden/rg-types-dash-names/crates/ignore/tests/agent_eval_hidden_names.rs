use ignore::types::TypesBuilder;
#[test]
fn dash_and_underscore_names() {
    let mut b = TypesBuilder::new();
    b.add("my-type", "*.mt").unwrap();
    b.add_def("my_type:*.mt2").unwrap();
    assert!(b.add("all", "*").is_err());
    assert!(b.add("bad name", "*").is_err());
    assert!(b.add("bad.name", "*").is_err());
    let t = b.select("my-type").build().unwrap();
    assert!(t.matched("x.mt", false).is_whitelist());
}
