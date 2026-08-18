//! Acceptance for `sinter overlap`: pairwise merge risk between in-flight
//! changes. pr-a and pr-b both modify Backoff (direct); pr-c modifies
//! Login, which calls Backoff (radius vs pr-a); pr-d touches an unrelated
//! file (clean).

use std::path::Path;
use std::process::Command;

fn sinter(repo: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .current_dir(repo)
        .output()
        .expect("run sinter");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

#[test]
fn overlap_ranks_pairwise_merge_risk() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(
        repo.join("retry.go"),
        "package main\n\nfunc Backoff(n int) int {\n\treturn n\n}\n\nfunc Login(u string) int {\n\treturn Backoff(3)\n}\n",
    )
    .unwrap();
    std::fs::write(repo.join("other.go"), "package main\n\nfunc Other() {}\n").unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "base"]);

    let branch = |name: &str, file: &str, content: &str| {
        git(repo, &["checkout", "-qb", name, "main"]);
        std::fs::write(repo.join(file), content).unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-qm", name]);
    };
    branch(
        "pr-a",
        "retry.go",
        "package main\n\nfunc Backoff(n int) int {\n\treturn n * 2\n}\n\nfunc Login(u string) int {\n\treturn Backoff(3)\n}\n",
    );
    branch(
        "pr-b",
        "retry.go",
        "package main\n\nfunc Backoff(n int) int {\n\treturn n + 1\n}\n\nfunc Login(u string) int {\n\treturn Backoff(3)\n}\n",
    );
    branch(
        "pr-c",
        "retry.go",
        "package main\n\nfunc Backoff(n int) int {\n\treturn n\n}\n\nfunc Login(u string) int {\n\treturn Backoff(9)\n}\n",
    );
    branch(
        "pr-d",
        "other.go",
        "package main\n\nfunc Other() int {\n\treturn 1\n}\n",
    );
    git(repo, &["checkout", "-q", "main"]);

    // Graph at the merge base — the documented fidelity point.
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (ok, out) = sinter(
        repo,
        &[
            "overlap",
            "a=main...pr-a",
            "b=main...pr-b",
            "c=main...pr-c",
            "d=main...pr-d",
        ],
    );
    assert!(ok, "{out}");
    // a×b touch the same node.
    assert!(out.contains("a × b: HIGH"), "{out}");
    assert!(out.contains("direct  retry.go:Backoff"), "{out}");
    // a changes Backoff; c changes Login which depends on Backoff.
    assert!(out.contains("a × c: MEDIUM"), "{out}");
    assert!(out.contains("radius  retry.go:Login"), "{out}");
    // d is disjoint from everything.
    assert!(out.contains("a × d: clean"), "{out}");
    // Riskiest pair prints first.
    let high = out.find("a × b: HIGH").unwrap();
    let medium = out.find("a × c: MEDIUM").unwrap();
    let clean = out.find("a × d: clean").unwrap();
    assert!(high < medium && medium < clean, "{out}");

    // JSON carries the same structure.
    let (ok, out) = sinter(
        repo,
        &["overlap", "a=main...pr-a", "b=main...pr-b", "--json"],
    );
    assert!(ok, "{out}");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(parsed["pairs"][0]["risk"], "high");

    // Fewer than two ranges fails loudly.
    let (ok, out) = sinter(repo, &["overlap", "main...pr-a"]);
    assert!(!ok, "{out}");
    assert!(out.contains("at least two"), "{out}");
}
