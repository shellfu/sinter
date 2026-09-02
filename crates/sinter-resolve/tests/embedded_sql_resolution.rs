//! Embedded SQL binding: reads/writes references extracted from Rust
//! string literals bind to a corpus-unique table defined in a .sql file;
//! two same-named tables in different database roots stay unresolved.

use sinter_core::{Node, Reference, Relation};
use sinter_extract::{Extractor, spec_for_path};
use sinter_resolve::resolve;

fn extract(files: &[(&str, &str)]) -> (Vec<Node>, Vec<Reference>) {
    let (mut nodes, mut references) = (Vec::new(), Vec::new());
    for (path, source) in files {
        let spec = spec_for_path(path).unwrap();
        let facts = Extractor::new(spec).unwrap().extract(path, source).unwrap();
        nodes.extend(facts.nodes);
        references.extend(facts.references);
    }
    (nodes, references)
}

fn reads_of_users(files: &[(&str, &str)]) -> Vec<(String, String)> {
    let (nodes, references) = extract(files);
    let imports: Vec<Reference> = references
        .iter()
        .filter(|r| r.relation == Relation::Imports)
        .cloned()
        .collect();
    let index = sinter_resolve::Index::build(&nodes, &imports, &[], &[], &[], &[]);
    let (bindings, _, _, _) = resolve(&index, &references);
    bindings
        .iter()
        .map(|b| &b.edge)
        .filter(|e| e.relation == Relation::Reads)
        .map(|e| (e.src.as_str().to_string(), e.dst.as_str().to_string()))
        .collect()
}

const RUST_READER: &str = r#"fn load_users(pool: &Pool) {
    let rows = sqlx::query!("SELECT id, name FROM users WHERE active = $1", true);
}"#;

const USERS_DDL: &str = "CREATE TABLE users (id INTEGER PRIMARY KEY);\n";

#[test]
fn rust_query_literal_binds_to_unique_table() {
    let reads = reads_of_users(&[
        ("migrations/001.sql", USERS_DDL),
        ("src/lib.rs", RUST_READER),
    ]);
    assert!(
        reads.iter().any(|(src, dst)| src.contains("load_users")
            && dst.starts_with("migrations/001.sql#")
            && dst.contains("users")),
        "expected load_users -> users@migrations/001.sql, got {reads:?}"
    );
}

#[test]
fn ambiguous_table_name_stays_unresolved() {
    let reads = reads_of_users(&[
        ("svc_a/db/migrations/001.sql", USERS_DDL),
        ("svc_b/db/migrations/001.sql", USERS_DDL),
        ("src/lib.rs", RUST_READER),
    ]);
    assert!(
        !reads.iter().any(|(src, _)| src.contains("load_users")),
        "ambiguous table must stay unresolved, got {reads:?}"
    );
}
