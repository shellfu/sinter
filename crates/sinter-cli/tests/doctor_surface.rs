//! Doctor never dies mid-report: an old-schema graph is one FIX row and
//! the integration section still renders.

use std::path::Path;
use std::process::Command;

use redb::{Database, TableDefinition};

const META: TableDefinition<&str, u32> = TableDefinition::new("meta");

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

#[test]
fn doctor_reports_old_schema_and_keeps_going() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn entry() {}\n").unwrap();
    let (code, out) = sinter_raw(repo, &["build", "."]);
    assert_eq!(code, Some(0), "{out}");

    // Stamp the graph as an older schema; its rows may no longer decode
    // under the current codecs, so doctor must not read them.
    {
        let db = Database::open(repo.join(".sinter/graph.redb")).unwrap();
        let txn = db.begin_write().unwrap();
        txn.open_table(META)
            .unwrap()
            .insert("schema", 8u32)
            .unwrap();
        txn.commit().unwrap();
    }

    let (code, out) = sinter_raw(repo, &["doctor", "."]);
    assert_eq!(code, Some(1), "old schema is a problem, not a crash: {out}");
    assert!(out.contains("graph schema v8"), "{out}");
    assert!(!out.contains("codec error"), "{out}");
    assert!(
        !out.contains(" nodes, "),
        "read checks must be skipped: {out}"
    );
    assert!(
        out.contains("integration"),
        "later sections must render: {out}"
    );
    assert!(
        out.contains("graph problem(s)"),
        "summary must render: {out}"
    );
}

/// `--verbose` is a workspace flag too: the partial-syntax file list is
/// what it exists for, and workspace mode reports the same gaps per member.
#[test]
fn workspace_doctor_honors_verbose_per_member() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("a/src/lib.rs", "pub fn ok() {}\n");
    // Tree-sitter recovers past the error, so `b` indexes from a partial tree.
    write("b/src/lib.rs", "pub fn ok() {}\npub fn broken( {\n");
    write(
        "workspace.toml",
        "[workspace]\nname = \"t\"\n\n[members]\na = \"a\"\nb = \"b\"\n",
    );
    let (code, out) = sinter_raw(root, &["workspace", "workspace.toml"]);
    assert_eq!(code, Some(0), "{out}");

    let (_, quiet) = sinter_raw(root, &["doctor", "--workspace", "workspace.toml"]);
    assert!(
        quiet.contains("member b: 1 file(s) indexed from partial syntax trees"),
        "{quiet}"
    );
    assert!(quiet.contains("--verbose` lists them"), "{quiet}");
    assert!(!quiet.contains("src/lib.rs"), "{quiet}");

    let (_, loud) = sinter_raw(
        root,
        &["doctor", "--workspace", "workspace.toml", "--verbose"],
    );
    assert!(loud.contains("member b: 1 file(s)"), "{loud}");
    assert!(loud.contains("src/lib.rs"), "{loud}");
    assert!(!loud.contains("--verbose` lists them"), "{loud}");
}
