use grep_printer::HyperlinkFormat;
#[test]
fn zed_alias_resolves() {
    let f: HyperlinkFormat = "zed".parse().unwrap();
    assert_eq!(f.to_string(), "zed://file{path}:{line}:{column}");
}
