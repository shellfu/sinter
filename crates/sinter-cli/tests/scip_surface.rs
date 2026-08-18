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
/// fresh"; a source file newer than the index exits 1 with the count.
#[test]
fn check_reports_freshness_without_indexing() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
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
    assert!(err.contains("1 source file"), "{err}");
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
