//! Phase 5/6 gate: query, affected, path, impact against a small repo.

use std::path::Path;
use std::process::Command;

fn sinter(repo: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
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
    let ok = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("run git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

#[test]
fn query_affected_path_impact() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "mod util;\nuse crate::util::core_fn;\n\n/// Entry point.\npub fn entry() -> u32 {\n    core_fn()\n}\n\npub fn test_entry() -> u32 {\n    entry()\n}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/util.rs"),
        "/// The core.\npub fn core_fn() -> u32 {\n    41\n}\n",
    )
    .unwrap();

    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "init"]);

    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    // query: content-bearing result.
    let (ok, out) = sinter(repo, &["query", "core_fn"]);
    assert!(ok, "{out}");
    assert!(out.contains("fn core_fn()"), "{out}");
    assert!(out.contains("The core."), "{out}");

    // affected: transitive, cross-file (entry via import, test_entry via scope).
    let (ok, out) = sinter(repo, &["affected", "core_fn"]);
    assert!(ok, "{out}");
    assert!(out.contains("entry"), "{out}");
    assert!(out.contains("test_entry"), "{out}");

    // evidence filter: scip-only finds nothing (no index present).
    let (ok, out) = sinter(repo, &["affected", "core_fn", "--evidence", "scip"]);
    assert!(ok, "{out}");
    assert!(out.contains("0 dependents"), "{out}");

    // path: entry reaches core_fn through the import-evidence call edge.
    let (ok, out) = sinter(repo, &["path", "test_entry", "core_fn"]);
    assert!(ok, "{out}");
    assert!(out.contains("-[calls/"), "{out}");
    assert!(out.trim_end().ends_with("core_fn"), "{out}");

    // impact: edit core_fn, commit, ask for the blast radius of the commit.
    std::fs::write(
        repo.join("src/util.rs"),
        "/// The core.\npub fn core_fn() -> u32 {\n    42\n}\n",
    )
    .unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "change core"]);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["impact", "HEAD~1..HEAD"]);
    assert!(ok, "{out}");
    assert!(out.contains("core_fn"), "{out}");
    assert!(out.contains("entry"), "{out}");
    assert!(out.contains("test_entry"), "{out}"); // matched as affected test

    // H1 regression: touching the imported file must not drop the
    // cross-file import-evidence edges — dependents survive the rebuild.
    std::fs::write(
        repo.join("src/util.rs"),
        "/// The core.\npub fn core_fn() -> u32 {\n    43\n}\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["affected", "core_fn"]);
    assert!(ok, "{out}");
    assert!(out.contains("entry"), "{out}");
    assert!(out.contains("test_entry"), "{out}");
}

/// Dynamic dispatch is blast radius: `affected <ImplType::method>` must
/// reach callers of the trait method through the `dynamic` fan-out edge,
/// and evidence filtering without "dynamic" must exclude them.
#[test]
fn affected_through_dyn_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "mod cat;\nmod dog;\n\npub trait Speak {\n    fn speak(&self);\n}\n\npub fn announce(s: &dyn Speak) {\n    Speak::speak(s);\n}\n",
    )
    .unwrap();
    let dog =
        "use crate::Speak;\n\npub struct Dog;\n\nimpl Speak for Dog {\n    fn speak(&self) {}\n}\n";
    std::fs::write(repo.join("src/dog.rs"), dog).unwrap();
    std::fs::write(
        repo.join("src/cat.rs"),
        "use crate::Speak;\n\npub struct Cat;\n\nimpl Speak for Cat {\n    fn speak(&self) {}\n}\n",
    )
    .unwrap();

    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "init"]);

    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    // Changing the impl must surface the caller of the trait method, with
    // the dynamic evidence visible on the bridging edge.
    let (ok, out) = sinter(repo, &["affected", "Dog::speak"]);
    assert!(ok, "{out}");
    assert!(out.contains("announce"), "{out}");
    assert!(out.contains("/dynamic"), "{out}");

    // Incremental: touching one impl file re-resolves the trait's file and
    // tears down its dynamic fan-out — the OTHER impl's edge must survive
    // the rebuild (dst-file facts rejoin the resolution set).
    std::fs::write(repo.join("src/dog.rs"), dog.replace("{}", "{ }")).unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["affected", "Cat::speak"]);
    assert!(ok, "{out}");
    assert!(out.contains("announce"), "{out}");
    assert!(out.contains("/dynamic"), "{out}");

    // Incremental: touching the trait's own file re-derives the fan-out
    // (impl files re-resolve through their `uses` reference on the trait).
    std::fs::write(
        repo.join("src/lib.rs"),
        "mod cat;\nmod dog;\n\npub trait Speak {\n    fn speak(&self);\n}\n\npub fn announce(s: &dyn Speak) {\n    Speak::speak(s)\n}\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["affected", "Dog::speak"]);
    assert!(ok, "{out}");
    assert!(out.contains("announce"), "{out}");
    assert!(out.contains("/dynamic"), "{out}");

    // Honesty: excluding dynamic evidence excludes the fan-out.
    let (ok, out) = sinter(
        repo,
        &["affected", "Dog::speak", "--evidence", "import,scope,scip"],
    );
    assert!(ok, "{out}");
    assert!(!out.contains("announce"), "{out}");

    // --certain also excludes it (Dynamic is Inferred by construction).
    let (ok, out) = sinter(repo, &["affected", "Dog::speak", "--certain"]);
    assert!(ok, "{out}");
    assert!(!out.contains("announce"), "{out}");
}
