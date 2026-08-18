//! Multi-line block-comment docs strip their interior `*` continuation
//! markers (Javadoc/C style) while markdown `**bold**` survives.

use sinter_extract::{Extractor, spec_for_path};

fn doc_of(file: &str, source: &str, symbol: &str) -> String {
    let spec = spec_for_path(file).expect("language for fixture path");
    let facts = Extractor::new(spec)
        .expect("extractor")
        .extract(file, source)
        .expect("extract");
    facts
        .nodes
        .iter()
        .find(|n| n.name == symbol)
        .and_then(|n| n.doc.clone())
        .unwrap_or_default()
}

#[test]
fn javadoc_interior_stars_strip() {
    let doc = doc_of(
        "com/acme/T.java",
        "/**\n * First line.\n * Second line.\n */\npublic class T {}\n",
        "T",
    );
    assert_eq!(doc, "First line.\nSecond line.");
}

#[test]
fn c_block_comment_stars_strip_but_markdown_bold_survives() {
    let doc = doc_of(
        "lib.c",
        "/*\n * Adds numbers.\n * **not** decoration.\n */\nint add(int a, int b) { return a + b; }\n",
        "add",
    );
    assert_eq!(doc, "Adds numbers.\n**not** decoration.");
}
