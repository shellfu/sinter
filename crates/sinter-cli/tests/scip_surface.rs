//! `sinter scip check` / bare `sinter scip`: the CI guard and the idempotent
//! index job. Freshness is mtime-based, so tests pin mtimes explicitly
//! instead of sleeping.

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

fn sinter(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .current_dir(repo)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run sinter")
}

fn set_mtime(path: &Path, t: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// `scip check`: missing index exits 1; fresh index exits 0 with "index
/// fresh"; a source/configuration input newer than the index exits 1 with
/// the count.
#[test]
fn check_reports_freshness_without_indexing() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("b.rs"), "pub fn g() {}\n").unwrap();

    let out = sinter(repo, &["scip", "check"]);
    assert!(
        !out.status.success(),
        "missing index must fail `scip check`"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no SCIP index"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    std::fs::create_dir_all(repo.join(".sinter")).unwrap();
    std::fs::write(repo.join(".sinter/index.scip"), b"stub").unwrap();
    let past = SystemTime::now() - Duration::from_secs(60);
    set_mtime(&repo.join("a.rs"), past);
    set_mtime(&repo.join("b.rs"), past);

    let out = sinter(repo, &["scip", "check"]);
    assert!(out.status.success(), "fresh index must pass `scip check`");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("index fresh"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    set_mtime(
        &repo.join("a.rs"),
        SystemTime::now() + Duration::from_secs(60),
    );
    let out = sinter(repo, &["scip", "check"]);
    assert!(!out.status.success(), "newer source must fail `scip check`");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("1 source/config input"), "{err}");

    set_mtime(&repo.join("a.rs"), past);
    set_mtime(
        &repo.join("Cargo.toml"),
        SystemTime::now() + Duration::from_secs(60),
    );
    let out = sinter(repo, &["scip", "check"]);
    assert!(
        !out.status.success(),
        "newer project configuration must make SCIP stale"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("1 source/config input"), "{err}");

    // A file no compiler indexer covers (init writes AGENTS.md after
    // indexing) must not make the index stale.
    set_mtime(&repo.join("Cargo.toml"), past);
    std::fs::write(repo.join("AGENTS.md"), "# agents\n").unwrap();
    set_mtime(
        &repo.join("AGENTS.md"),
        SystemTime::now() + Duration::from_secs(60),
    );
    let out = sinter(repo, &["scip", "check"]);
    assert!(
        out.status.success(),
        "non-indexable file must not stale the index: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A negative path answer is an explicit coverage verdict. It identifies
/// the indexed commit/dirty worktree, missing compiler index, unresolved
/// reason classes, and files extraction could not index.
#[test]
fn negative_path_reports_snapshot_and_coverage_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("lib.rs"), "pub fn from() {}\npub fn to() {}\n").unwrap();
    std::fs::write(repo.join("bad.rs"), [0xff, 0xfe]).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "Cargo.toml", "lib.rs", "bad.rs"]);
    git(&["commit", "-qm", "fixture"]);

    let out = sinter(repo, &["build"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::write(
        repo.join("lib.rs"),
        "pub fn from() {}\npub fn to() {}\n// dirty, but query self-syncs it\n",
    )
    .unwrap();
    let out = sinter(repo, &["path", "from", "to", "--json"]);
    assert!(!out.status.success(), "a miss uses grep exit 1");
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let coverage = &value["coverage"];
    assert_eq!(coverage["status"], "not_proven", "{value}");
    assert_eq!(coverage["conclusive"], false, "{value}");
    assert_eq!(coverage["snapshot"]["dirty"], true, "{value}");
    assert_eq!(
        coverage["snapshot"]["head"].as_str().unwrap().len(),
        40,
        "{value}"
    );
    assert_eq!(coverage["snapshot"]["working_tree_indexed"], true);
    assert_eq!(coverage["compiler_index"]["state"], "missing");
    assert!(
        coverage["compiler_index"]["indexable_languages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|language| language == "rust"),
        "{value}"
    );
    assert!(
        coverage["graph"]["unindexed_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file == "bad.rs"),
        "{value}"
    );
}

/// bare `scip`: fresh index is a one-line no-op that executes no indexer
/// (fake rust-analyzer on PATH records execution); stale index runs it.
#[cfg(unix)]
#[test]
fn bare_scip_skips_indexers_when_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    // Fake rust-analyzer first on PATH: records execution, produces nothing.
    let marker = bin.path().join("executed");
    std::fs::write(
        bin.path().join("rust-analyzer"),
        format!("#!/bin/sh\ntouch {}\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        bin.path().join("rust-analyzer"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let path_env = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_sinter"))
            .args(args)
            .current_dir(repo)
            .env("PATH", &path_env)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run sinter")
    };

    std::fs::create_dir_all(repo.join(".sinter")).unwrap();
    std::fs::write(repo.join(".sinter/index.scip"), b"stub").unwrap();
    set_mtime(
        &repo.join("a.rs"),
        SystemTime::now() - Duration::from_secs(60),
    );

    let out = run(&["scip"]);
    assert!(out.status.success());
    assert!(!marker.exists(), "fresh bare `scip` executed an indexer");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("nothing to do"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Stale: the indexer must run (it produces nothing, so the command
    // fails afterwards — execution is the assertion).
    set_mtime(
        &repo.join("a.rs"),
        SystemTime::now() + Duration::from_secs(60),
    );
    run(&["scip"]);
    assert!(marker.exists(), "stale bare `scip` must run the indexer");
}

/// Source snippets are not projects. An extension alone must not launch an
/// indexer from a parent Rust repository or a fixture directory.
#[cfg(unix)]
#[test]
fn scip_skips_language_without_project_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(repo.join("fixture.ts"), "export const value = 1;\n").unwrap();
    let marker = bin.path().join("typescript-executed");
    std::fs::write(
        bin.path().join("scip-typescript"),
        format!("#!/bin/sh\ntouch {}\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        bin.path().join("scip-typescript"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let path_env = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .arg("scip")
        .current_dir(repo)
        .env("PATH", path_env)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "no configured project can produce an index"
    );
    assert!(
        !marker.exists(),
        "extension-only fixture launched the indexer"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no tsconfig*.json found"), "{stderr}");
}

/// A nested module must be indexed from its own project root. Running every
/// indexer at the repository root breaks multi-module repositories and makes
/// the emitted document paths disagree with Sinter's repo-relative paths.
#[cfg(unix)]
#[test]
fn scip_runs_indexer_from_nested_project_root() {
    let dir = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let project = repo.join("services/worker");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("go.mod"), "module example.com/worker\n").unwrap();
    std::fs::write(project.join("main.go"), "package main\nfunc main() {}\n").unwrap();

    let marker = bin.path().join("working-directory");
    std::fs::write(
        bin.path().join("scip-go"),
        format!("#!/bin/sh\npwd > '{}'\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        bin.path().join("scip-go"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let path_env = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .arg("scip")
        .current_dir(repo)
        .env("PATH", path_env)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();

    assert!(!out.status.success(), "fake indexer writes no SCIP output");
    assert_eq!(
        std::fs::read_to_string(marker).unwrap().trim(),
        project.canonicalize().unwrap().to_string_lossy()
    );
}

/// A build ingests `.sinter/index.scip` whenever it exists — including an
/// index older than the code it binds. The report has to say so: silent
/// ingestion of stale compiler evidence is how a graph starts lying.
#[test]
fn build_names_an_ingested_stale_index() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join(".sinter")).unwrap();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
    // Empty but valid SCIP: parses, binds nothing, ingests cleanly.
    let index = repo.join(".sinter/index.scip");
    std::fs::write(&index, b"").unwrap();

    set_mtime(&index, SystemTime::now() - Duration::from_secs(60));
    let out = sinter(repo, &["build"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("SCIP index is older"), "{text}");
    assert!(text.contains("rerun `sinter scip`"), "{text}");

    set_mtime(&index, SystemTime::now() + Duration::from_secs(60));
    let out = sinter(repo, &["build"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("SCIP index is older"), "{text}");
}
