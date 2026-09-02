//! Every call site of an edge survives to the graph and to the answer.
//! The shape this gates is the real one found in a Python repo: one
//! function calls `cluster(...)` directly and, further down, calls the
//! same function through an alias import. Both bind to the same target
//! from the same enclosing symbol, so they are one edge — and "where is
//! this called from" must still name both lines, not one of them.

use std::process::Command;

fn sinter(repo: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .current_dir(repo)
        .output()
        .expect("run sinter");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn git(repo: &std::path::Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git")
        .status
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn repeated_calls_from_one_caller_all_survive() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("pkg")).unwrap();
    std::fs::write(repo.join("pkg/__init__.py"), "").unwrap();
    std::fs::write(
        repo.join("pkg/cluster.py"),
        "def cluster(g):\n    return g\n",
    )
    .unwrap();
    // dispatch calls cluster on line 5 and again, aliased, on line 9.
    std::fs::write(
        repo.join("pkg/cli.py"),
        "from pkg.cluster import cluster\n\
         from pkg.cluster import cluster as _cluster\n\
         \n\
         def dispatch(g, mode):\n\
         \x20   if mode == \"a\":\n\
         \x20       return cluster(g)\n\
         \x20   filler = 1\n\
         \x20   filler += 1\n\
         \x20   return _cluster(g) if filler else None\n",
    )
    .unwrap();
    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "init"]);
    let (ok, out) = sinter(repo, &["build", "."]);
    assert!(ok, "{out}");

    // Both sites, one edge: the `affected` row names them together.
    let (ok, out) = sinter(repo, &["affected", "cluster@pkg/cluster.py", "--json"]);
    assert!(ok, "{out}");
    let data: serde_json::Value = serde_json::from_str(&out).unwrap();
    let row = data["dependents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["s"].as_str() == Some("dispatch"))
        .unwrap_or_else(|| panic!("no dispatch row in {out}"));
    assert_eq!(row["site"], serde_json::json!("pkg/cli.py:6"));
    assert_eq!(
        row["sites"],
        serde_json::json!(["pkg/cli.py:6", "pkg/cli.py:9"])
    );
    assert_eq!(row["sites_total"], serde_json::json!(2));

    // Human output: one row, both lines, the second one abbreviated.
    let (ok, out) = sinter(repo, &["affected", "cluster@pkg/cluster.py"]);
    assert!(ok, "{out}");
    assert!(out.contains("pkg/cli.py:6, :9"), "{out}");

    // `used by` groups by source file, so its row carries the file's
    // import lines and both call lines.
    let (ok, out) = sinter(repo, &["show", "cluster@pkg/cluster.py", "--callers"]);
    assert!(ok, "{out}");
    assert!(out.contains("pkg/cli.py:1, :2, :6, :9"), "{out}");
}

/// The bound stays bounded: 12 calls from one caller print the first eight
/// lines and say how many are left, in text and in JSON.
#[test]
fn many_sites_are_capped_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("pkg")).unwrap();
    std::fs::write(repo.join("pkg/__init__.py"), "").unwrap();
    std::fs::write(repo.join("pkg/target.py"), "def work(g):\n    return g\n").unwrap();
    let mut caller = String::from("from pkg.target import work\n\ndef dispatch(g):\n");
    for _ in 0..12 {
        caller.push_str("    work(g)\n");
    }
    caller.push_str("    return g\n");
    std::fs::write(repo.join("pkg/cli.py"), caller).unwrap();
    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "init"]);
    let (ok, out) = sinter(repo, &["build", "."]);
    assert!(ok, "{out}");

    let (ok, out) = sinter(repo, &["affected", "work@pkg/target.py"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("pkg/cli.py:4, :5, :6, :7, :8, :9, :10, :11 (+4 more)"),
        "{out}"
    );

    let (ok, out) = sinter(repo, &["affected", "work@pkg/target.py", "--json"]);
    assert!(ok, "{out}");
    let data: serde_json::Value = serde_json::from_str(&out).unwrap();
    let row = data["dependents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["s"].as_str() == Some("dispatch"))
        .unwrap_or_else(|| panic!("no dispatch row in {out}"));
    assert_eq!(row["sites"].as_array().unwrap().len(), 8);
    assert_eq!(row["sites_total"], serde_json::json!(12));
}
