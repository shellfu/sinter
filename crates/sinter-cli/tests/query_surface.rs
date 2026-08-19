//! Phase 5/6 gate: query, affected, path, impact against a small repo.

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

    // evidence filter: scip-only finds nothing (no index present) —
    // grep-style exit 1 for a valid query with no results.
    let (ok, out) = sinter(repo, &["affected", "core_fn", "--evidence", "scip"]);
    assert!(!ok, "{out}");
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
    let (_, out) = sinter(
        repo,
        &["affected", "Dog::speak", "--evidence", "import,scope,scip"],
    );
    assert!(!out.contains("announce"), "{out}");

    // --certain also excludes it (Dynamic is Inferred by construction).
    let (_, out) = sinter(repo, &["affected", "Dog::speak", "--certain"]);
    assert!(!out.contains("announce"), "{out}");
}

fn sinter_code(repo: &Path, args: &[&str]) -> (Option<i32>, String) {
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

/// New surface: `--json` on the read commands (mirroring the MCP tool
/// shapes), grep-style exit codes, `--limit` on affected, and `--repo`
/// accepted by lifecycle commands.
#[test]
fn json_flags_exit_codes_and_repo_flag() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "mod util;\nuse crate::util::core_fn;\n\n/// Entry point.\npub fn entry() -> u32 {\n    core_fn()\n}\n\npub fn other_entry() -> u32 {\n    core_fn()\n}\n",
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

    // Lifecycle commands accept --repo (positional still works elsewhere).
    let (code, out) = sinter_code(repo, &["build", "--repo", "."]);
    assert_eq!(code, Some(0), "{out}");
    // Both at once is a usage error.
    let (code, _) = sinter_code(repo, &["build", ".", "--repo", "."]);
    assert_eq!(code, Some(2));

    // query --json: MCP `query` shape.
    let (code, out) = sinter_code(repo, &["query", "core_fn", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["exact"], serde_json::json!(true), "{out}");
    assert_eq!(v["results"][0]["name"], serde_json::json!("core_fn"));

    // show --json: MCP `show` shape.
    let (code, out) = sinter_code(repo, &["show", "core_fn", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["symbol"]["name"], serde_json::json!("core_fn"));
    assert!(v["incoming"].as_array().is_some(), "{out}");
    // Every edge exposes `site`; the call from `entry` names its call site
    // (`core_fn()` on line 6 of src/lib.rs).
    let entry_call = v["incoming"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| {
            e["symbol"].as_str().is_some_and(|s| s.ends_with("entry")) && e["relation"] == "calls"
        })
        .unwrap_or_else(|| panic!("no call edge from entry: {out}"));
    assert_eq!(
        entry_call["site"],
        serde_json::json!("src/lib.rs:6"),
        "{out}"
    );

    // affected --json: MCP `affected` shape, terse entries.
    let (code, out) = sinter_code(repo, &["affected", "core_fn", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["total"].as_u64().unwrap() >= 2, "{out}");
    assert!(v["dependents"][0]["s"].is_string(), "{out}");

    // affected --limit: truncation footer, ask-style.
    let (code, out) = sinter_code(repo, &["affected", "core_fn", "--limit", "1"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("more dependents below cutoff"), "{out}");
    assert!(out.contains("--limit"), "{out}");

    // path --json: MCP `path` shape.
    let (code, out) = sinter_code(repo, &["path", "entry", "core_fn", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["found"], serde_json::json!(true), "{out}");
    assert!(v["steps"][0]["relation"].is_string(), "{out}");
    // Each hop names where it is written.
    assert_eq!(
        v["steps"][0]["site"],
        serde_json::json!("src/lib.rs:6"),
        "{out}"
    );

    // impact --json: the ImpactReport shape.
    let (code, out) = sinter_code(repo, &["impact", "HEAD", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["changed_symbols"].is_array(), "{out}");

    // Grep-style exit codes: 1 = valid query, no results.
    let (code, out) = sinter_code(repo, &["show", "no_such_symbol_zzqx"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("sinter ask"), "no concept-search hint: {out}");
    let (code, _) = sinter_code(repo, &["query", "zzqxzzqx"]);
    assert_eq!(code, Some(1));
    let (code, out) = sinter_code(repo, &["path", "core_fn", "entry"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("no path"), "{out}");
    // 2 = usage/execution error (bad evidence kind).
    let (code, _) = sinter_code(repo, &["affected", "core_fn", "--evidence", "bogus"]);
    assert_eq!(code, Some(2));

    // map accepts --repo like the other read commands.
    let (code, out) = sinter_code(repo, &["map", "--repo", "."]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("nodes"), "{out}");
}

/// Empty graph: read commands say so instead of "no match", build warns.
#[test]
fn empty_graph_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(repo.join("notes.txt"), "no source here\n").unwrap();

    let (code, out) = sinter_code(repo, &["build"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("0 source files found under"), "{out}");
    assert!(out.contains("wrong directory?"), "{out}");

    let (code, out) = sinter_code(repo, &["query", "anything"]);
    assert_eq!(code, Some(2), "{out}");
    assert!(out.contains("graph") && out.contains("is empty"), "{out}");
    assert!(out.contains("right directory"), "{out}");
}
