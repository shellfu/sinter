//! Surface gate for loud degradation: radius-scoped coverage notices,
//! truncation notices that name their retry, skipped-SQL-construct
//! surfacing, and the `assert no-writers` table assertion.

use std::path::Path;
use std::process::Command;

use protobuf::Message;
use scip::types::Index;

fn sinter_raw(repo: &Path, args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .current_dir(repo)
        .output()
        .expect("run sinter");
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn sinter(repo: &Path, args: &[&str]) -> (bool, String) {
    let (code, out) = sinter_raw(repo, args);
    (code == Some(0), out)
}

/// entry (lib.rs) -> core_fn (util.rs); util.rs also calls a function
/// nobody defines, so the radius of anything touching util.rs has a gap.
fn rust_fixture_with_gap(repo: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "mod util;\nuse crate::util::core_fn;\n\npub fn entry() -> u32 {\n    core_fn()\n}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/util.rs"),
        "pub fn core_fn() -> u32 {\n    missing_fn()\n}\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
}

#[test]
fn affected_names_unresolved_refs_within_its_radius() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    rust_fixture_with_gap(repo);

    // util.rs is in the radius (seed file) and carries an unresolved ref.
    let (ok, out) = sinter(repo, &["affected", "core_fn"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("unresolved within this radius"),
        "degraded radius must be loud: {out}"
    );
    assert!(out.contains("see `sinter unresolved`"), "{out}");

    let (ok, out) = sinter(repo, &["affected", "core_fn", "--json"]);
    assert!(ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let radius = &v["coverage"]["evidence"]["unresolved"]["within_radius"];
    assert!(
        radius["references"].as_u64().unwrap() >= 1,
        "structured radius gap missing: {out}"
    );
    assert_eq!(radius["sql"], 0, "{out}");
}

#[test]
fn deps_names_unresolved_refs_within_its_radius_and_stays_quiet_when_clean() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    rust_fixture_with_gap(repo);

    // entry's forward radius reaches util.rs, which has the gap.
    let (ok, out) = sinter(repo, &["deps", "entry"]);
    assert!(ok, "{out}");
    assert!(out.contains("unresolved within this radius"), "{out}");

    let (ok, out) = sinter(repo, &["deps", "entry", "--json"]);
    assert!(ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert!(
        v["coverage"]["evidence"]["unresolved"]["within_radius"]["references"]
            .as_u64()
            .unwrap()
            >= 1,
        "{out}"
    );

    // A clean radius must not carry the notice.
    let clean = tempfile::tempdir().unwrap();
    let repo = clean.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn helper() -> u32 { 41 }\npub fn entry() -> u32 { helper() }\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["deps", "entry"]);
    assert!(ok, "{out}");
    assert!(
        !out.contains("unresolved within this radius"),
        "clean radius must stay quiet: {out}"
    );
}

#[test]
fn sql_gaps_in_the_radius_are_counted_separately() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("db")).unwrap();
    std::fs::write(
        repo.join("db/schema.sql"),
        "CREATE TABLE accounts (id INT);\n",
    )
    .unwrap();
    // One resolved write into the radius, one unresolved reference.
    std::fs::write(
        repo.join("db/queries.sql"),
        "INSERT INTO accounts VALUES (1);\nINSERT INTO missing_table VALUES (2);\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (ok, out) = sinter(repo, &["affected", "accounts"]);
    assert!(ok, "{out}");
    assert!(out.contains("in SQL)"), "SQL gap breakdown missing: {out}");

    let (ok, out) = sinter(repo, &["affected", "accounts", "--json"]);
    assert!(ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let radius = &v["coverage"]["evidence"]["unresolved"]["within_radius"];
    assert!(radius["sql"].as_u64().unwrap() >= 1, "{out}");
}

#[test]
fn query_truncation_is_loud_and_names_the_retry() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn entry_a() {}\npub fn entry_b() {}\npub fn entry_c() {}\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (ok, out) = sinter(repo, &["query", "entry_*", "--limit", "1"]);
    assert!(ok, "{out}");
    assert!(out.contains("more matches below cutoff"), "{out}");
    assert!(out.contains("--limit 3"), "retry must be runnable: {out}");

    let (ok, out) = sinter(repo, &["query", "entry_*", "--limit", "1", "--json"]);
    assert!(ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["truncated"], 2, "{out}");
    assert_eq!(v["results"].as_array().unwrap().len(), 1, "{out}");

    // Uncapped output stays byte-stable: no truncated field, no notice.
    let (ok, out) = sinter(repo, &["query", "entry_*", "--json"]);
    assert!(ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert!(v.get("truncated").is_none(), "{out}");
}

#[test]
fn doctor_counts_skipped_sql_constructs() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("db")).unwrap();
    // tree-sitter-sequel 0.3 misparses CREATE PROCEDURE; the file is
    // flagged partial and the skipped statement must be counted, not
    // dropped silently.
    std::fs::write(
        repo.join("db/proc.sql"),
        "CREATE TABLE t (id INT);\nCREATE PROCEDURE add_row() AS BEGIN INSERT INTO t VALUES (1); END;\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (_, out) = sinter_raw(repo, &["doctor", "."]);
    assert!(
        out.contains("SQL statement(s) skipped") && out.contains("CREATE PROCEDURE"),
        "doctor must surface skipped SQL constructs: {out}"
    );
}

#[test]
fn no_writers_assertion_mirrors_no_callers_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("db")).unwrap();
    std::fs::write(
        repo.join("db/schema.sql"),
        "CREATE TABLE audit_log (id INT);\nCREATE TABLE settings (id INT);\nINSERT INTO audit_log VALUES (1);\n",
    )
    .unwrap();
    // A fresh (empty) compiler index makes the empty traversal complete
    // for this indexed snapshot, as in the no-callers surface gate.
    std::fs::write(
        repo.join("index.scip"),
        Index::default().write_to_bytes().unwrap(),
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    // Violated: something writes audit_log. Exit 1, same JSON shape.
    let (code, out) = sinter_raw(repo, &["assert", "no-writers", "audit_log", "--json"]);
    assert_eq!(code, Some(1), "violated must exit 1: {out}");
    let violated: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(violated["status"], "violated");
    assert_eq!(violated["assertion"]["kind"], "no_writers");
    assert_eq!(violated["assertion"]["runtime_exhaustive"], false);
    assert!(violated["observed_callers"].as_u64().unwrap() >= 1, "{out}");
    assert_eq!(violated["coverage"]["universe"]["mode"], "repository");
    assert!(violated["snapshot"].is_string(), "{out}");

    // Holds: nothing writes settings (its CREATE is not a write).
    let (code, out) = sinter_raw(repo, &["assert", "no-writers", "settings", "--json"]);
    assert_eq!(code, Some(0), "holding must exit 0: {out}");
    let holds: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(holds["status"], "holds_for_indexed_snapshot", "{out}");
    assert_eq!(holds["observed_callers"], 0);
    assert_eq!(
        holds["coverage"]["completeness"],
        "complete_for_indexed_snapshot"
    );
    assert_eq!(holds["coverage"]["conclusive"], false);
    assert!(
        holds["coverage"]["limitations"].as_array().is_some(),
        "{out}"
    );

    // Unknown table: structured no_match, exit 1 — identical to the
    // `assert no-callers` lookup-miss contract (grep-style exit codes).
    let (code, out) = sinter_raw(repo, &["assert", "no-writers", "no_such_table", "--json"]);
    assert_eq!(code, Some(1), "lookup miss must exit 1: {out}");
    let miss: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(miss["error"]["code"], "no_match", "{out}");

    // Human output names the assertion and the noun.
    let (_, out) = sinter_raw(repo, &["assert", "no-writers", "audit_log"]);
    assert!(out.contains("assert no-writers"), "{out}");
    assert!(out.contains("observed writer(s)"), "{out}");
}
