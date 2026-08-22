use sinter_extract::{Extractor, spec_for_path};

fn extracted(source: &str) -> sinter_core::Node {
    let spec = spec_for_path("src/lib.rs").expect("Rust language");
    Extractor::new(spec)
        .expect("extractor")
        .extract("src/lib.rs", source)
        .expect("extract")
        .nodes
        .into_iter()
        .find(|node| node.name == "run")
        .expect("run node")
}

#[test]
fn unrelated_prefix_text_moves_id_but_not_symbol_key() {
    let before = extracted("pub fn run() -> u8 { 1 }\n");
    let after = extracted("// unrelated prefix\n\npub fn run() -> u8 { 1 }\n");

    assert_ne!(before.id, after.id);
    assert_ne!(before.span, after.span);
    assert_eq!(before.symbol_key(), after.symbol_key());
}
