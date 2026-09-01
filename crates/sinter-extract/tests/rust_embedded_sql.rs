//! Embedded SQL at query sinks: Rust string literals passed to sqlx
//! macros/functions, diesel's sql_query, and prepare/execute/query-style
//! methods are re-parsed with the SQL grammar, producing reads/writes
//! references from the enclosing Rust function to the referenced tables.
//! Dynamically built SQL records nothing; fragmentary SQL records one
//! conservative never-binding reference.

use sinter_core::{Reference, Relation};
use sinter_extract::{Extractor, spec_for_path};

fn refs(src: &str) -> Vec<Reference> {
    let spec = spec_for_path("x.rs").unwrap();
    Extractor::new(spec)
        .unwrap()
        .extract("x.rs", src)
        .unwrap()
        .references
}

fn data_refs(src: &str) -> Vec<(Relation, String, String)> {
    refs(src)
        .into_iter()
        .filter(|r| matches!(r.relation, Relation::Reads | Relation::Writes))
        .map(|r| {
            (
                r.relation,
                r.name,
                r.enclosing
                    .map(|e| e.as_str().to_string())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn sqlx_query_macro_literal_reads_table() {
    let out = data_refs(
        r#"fn load_users(pool: &Pool) {
            let rows = sqlx::query!("SELECT id, name FROM users WHERE active = $1", true);
        }"#,
    );
    assert!(
        out.iter().any(|(rel, name, enc)| *rel == Relation::Reads
            && name == "users"
            && enc.contains("#load_users@")),
        "expected reads(users) enclosed by load_users, got {out:?}"
    );
}

#[test]
fn sqlx_query_as_macro_and_fn_write_and_read() {
    let out = data_refs(
        r#"fn save(pool: &Pool) {
            sqlx::query_as!(Order, "INSERT INTO orders (id, total) VALUES ($1, $2)");
            let q = sqlx::query_as::<_, User>("SELECT * FROM users");
        }"#,
    );
    assert!(
        out.iter()
            .any(|(rel, name, _)| *rel == Relation::Writes && name == "orders"),
        "writes(orders) missing: {out:?}"
    );
    assert!(
        out.iter()
            .any(|(rel, name, _)| *rel == Relation::Reads && name == "users"),
        "reads(users) missing: {out:?}"
    );
}

#[test]
fn method_sink_execute_writes_table() {
    let out = data_refs(
        r#"fn touch(conn: &Connection) {
            conn.execute("UPDATE users SET name = $1 WHERE id = $2", params).unwrap();
            client.query_one("DELETE FROM sessions WHERE id = $1", &[&id]);
        }"#,
    );
    assert!(
        out.iter().any(|(rel, name, enc)| *rel == Relation::Writes
            && name == "users"
            && enc.contains("#touch@")),
        "writes(users) missing: {out:?}"
    );
    assert!(
        out.iter()
            .any(|(rel, name, _)| *rel == Relation::Writes && name == "sessions"),
        "writes(sessions) missing: {out:?}"
    );
}

#[test]
fn diesel_sql_query_reads_table() {
    let out = data_refs(
        r#"fn report(conn: &mut PgConnection) {
            let rows = diesel::sql_query("SELECT * FROM order_totals").load(conn);
        }"#,
    );
    assert!(
        out.iter()
            .any(|(rel, name, _)| *rel == Relation::Reads && name == "order_totals"),
        "reads(order_totals) missing: {out:?}"
    );
}

#[test]
fn dynamic_sql_records_nothing() {
    let out = data_refs(
        r#"fn dynamic(conn: &Connection, table: &str) {
            let q = format!("SELECT * FROM {}", table);
            conn.execute(&q, params).unwrap();
            sqlx::query(&q);
        }"#,
    );
    assert!(out.is_empty(), "dynamic SQL must record no edges: {out:?}");
}

#[test]
fn non_sink_string_is_not_parsed_as_sql() {
    let out = data_refs(
        r#"fn logging() {
            log("SELECT * FROM users");
            let s = "DELETE FROM users";
        }"#,
    );
    assert!(
        out.is_empty(),
        "non-sink strings must not produce edges: {out:?}"
    );
}

#[test]
fn non_sql_string_at_sink_records_nothing() {
    let out = refs(
        r#"fn run(shell: &Shell) {
            shell.execute("ls -la /tmp");
        }"#,
    );
    assert!(
        !out.iter().any(|r| matches!(
            r.relation,
            Relation::Reads | Relation::Writes | Relation::Uses
        ) && r.name.contains("ls -la")),
        "non-SQL sink string must stay silent: {out:?}"
    );
}

#[test]
fn fragmentary_sql_yields_conservative_unresolvable_reference() {
    // Misspelled keyword: parses with errors and yields no table facts.
    // One never-binding Uses reference marks the site for `sinter
    // unresolved` instead of guessing a table.
    let out = refs(
        r#"fn broken(conn: &Connection) {
            conn.execute("SELECT * FRMO usrs WHRE", params);
        }"#,
    );
    let markers: Vec<&Reference> = out
        .iter()
        .filter(|r| r.relation == Relation::Uses && r.name.starts_with("SELECT"))
        .collect();
    assert_eq!(markers.len(), 1, "expected one fragment marker: {out:?}");
    assert!(
        markers[0].name.contains(' '),
        "marker must never bind: {:?}",
        markers[0].name
    );
}

#[test]
fn later_string_arguments_are_not_sql() {
    // Only the first argument of a fn/method sink is SQL; bind values
    // that happen to be string literals stay out.
    let out = refs(
        r#"fn bind(conn: &Connection) {
            conn.query("SELECT id FROM accounts WHERE name = $1", "SELECT");
        }"#,
    );
    assert!(
        out.iter()
            .any(|r| r.relation == Relation::Reads && r.name == "accounts"),
        "reads(accounts) missing: {out:?}"
    );
    let markers: Vec<&Reference> = out
        .iter()
        .filter(|r| r.relation == Relation::Uses && r.name == "SELECT")
        .collect();
    assert!(markers.is_empty(), "bind argument treated as SQL: {out:?}");
}
