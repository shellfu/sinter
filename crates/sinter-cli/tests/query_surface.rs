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
    // Direct callers are stated apart from the transitive total, so a
    // blast radius never reads as a caller count.
    assert!(out.contains("3 dependents of core_fn"), "{out}");
    assert!(
        out.contains("1 direct in 1 file(s); 1 file(s) import it, 1 transitive"),
        "{out}"
    );
    let (_, out) = sinter(repo, &["affected", "core_fn", "--depth", "1"]);
    assert!(
        out.contains("2 dependents of core_fn") && !out.contains("test_entry"),
        "{out}"
    );
    let (_, out) = sinter(repo, &["affected", "core_fn", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        (
            v["total"].as_u64(),
            v["direct"].as_u64(),
            v["direct_files"].as_u64(),
            v["importing_files"].as_u64()
        ),
        (Some(3), Some(1), Some(1), Some(1)),
        "{out}"
    );

    // evidence filter: scip-only finds nothing (no index present) —
    // grep-style exit 1 for a valid query with no results.
    let (ok, out) = sinter(repo, &["affected", "core_fn", "--evidence", "scip"]);
    assert!(!ok, "{out}");
    assert!(out.contains("0 dependents"), "{out}");

    // path: entry reaches core_fn through the import-evidence call edge.
    let (ok, out) = sinter(repo, &["path", "test_entry", "core_fn"]);
    assert!(ok, "{out}");
    assert!(out.contains("-[calls/"), "{out}");
    assert!(
        out.lines()
            .next()
            .is_some_and(|line| line.ends_with("core_fn")),
        "{out}"
    );

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
    assert!(v["dependents"][0]["c"].is_string(), "{out}");
    assert_eq!(v["coverage"]["status"], "found", "{out}");
    assert_eq!(v["coverage"]["completeness"], "partial", "{out}");
    assert_eq!(
        v["coverage"]["filters"]["relations"]["mode"], "all_dependencies",
        "{out}"
    );
    assert!(
        v["coverage"]["evidence"]["possible"]["results"]
            .as_u64()
            .unwrap()
            >= 1,
        "{out}"
    );

    // deps uses the same snapshot/filter/evidence coverage contract.
    let (code, out) = sinter_code(repo, &["deps", "entry", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["snapshot"].is_string(), "{out}");
    assert_eq!(v["coverage"]["status"], "found", "{out}");
    assert_eq!(v["coverage"]["completeness"], "partial", "{out}");
    assert!(v["dependencies"][0]["c"].is_string(), "{out}");

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
    assert!(v["steps"][0]["confidence"].is_string(), "{out}");
    assert_eq!(v["coverage"]["status"], "found", "{out}");
    assert_eq!(v["coverage"]["completeness"], "partial", "{out}");
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

/// Every traversal carries coverage. Missing/stale SCIP makes hits and misses
/// partial; a fresh index can only claim completeness for the indexed graph,
/// never runtime exhaustiveness.
#[test]
fn negative_answers_flag_stale_scip() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn entry() -> u32 {\n    core_fn()\n}\n\npub fn core_fn() -> u32 {\n    41\n}\n\npub fn orphan() {}\n",
    )
    .unwrap();
    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "init"]);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    // No index: explicit syntax-only coverage gap.
    let (_, out) = sinter(repo, &["path", "core_fn", "entry"]);
    assert!(
        out.contains("no path")
            && out.contains("coverage: partial")
            && out.contains("compiler index missing for rust"),
        "{out}"
    );

    // Index older than the source: inconclusive.
    let index = repo.join(".sinter/index.scip");
    std::fs::write(&index, b"").unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&index)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let (_, out) = sinter(repo, &["path", "core_fn", "entry"]);
    assert!(
        out.contains("no path") && out.contains("compiler index is stale"),
        "{out}"
    );
    let (_, out) = sinter(repo, &["affected", "orphan"]);
    assert!(
        out.contains("0 dependents") && out.contains("compiler index is stale"),
        "{out}"
    );
    // A hit carries the same partial coverage instead of looking exhaustive.
    let (_, out) = sinter(repo, &["path", "entry", "core_fn"]);
    assert!(out.contains("coverage: partial"), "{out}");

    // Index newer than the source: still not-proven, without stale/missing.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&index)
        .unwrap()
        .set_modified(std::time::SystemTime::now())
        .unwrap();
    let (_, out) = sinter(repo, &["path", "core_fn", "entry"]);
    assert!(
        out.contains("no path")
            && out.contains("coverage: complete_for_indexed_snapshot")
            && !out.contains("compiler index is stale")
            && !out.contains("compiler index missing"),
        "{out}"
    );

    let (_, out) = sinter(repo, &["path", "entry", "core_fn", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        value["coverage"]["completeness"], "complete_for_indexed_snapshot",
        "{out}"
    );
    assert_eq!(value["coverage"]["conclusive"], false, "{out}");
    assert!(
        value["coverage"]["available_sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source["kind"] == "scip" && source["status"] == "available"),
        "{out}"
    );
}

/// Installation drift is reported by maintenance verbs, never beside a
/// query answer: agents read stderr with stdout, and a nag on every
/// `show` obscures the result it decorates.
#[test]
fn query_verbs_never_nag_about_stale_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn entry() -> u32 {\n    41\n}\n",
    )
    .unwrap();
    // A managed AGENTS.md block with stale contents.
    std::fs::write(
        repo.join("AGENTS.md"),
        "<!-- BEGIN sinter (managed by `sinter install`; edits inside are overwritten) -->\nold\n<!-- END sinter -->\n",
    )
    .unwrap();
    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "init"]);

    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    assert!(out.contains("AGENTS.md sinter block is stale"), "{out}");

    for verb in [
        &["show", "entry"][..],
        &["ask", "entry"],
        &["affected", "entry"],
        &["path", "entry", "entry"],
    ] {
        let (_, out) = sinter(repo, verb);
        assert!(!out.contains("is stale"), "{verb:?} nagged: {out}");
    }
}

/// `show` names trait implementors and dynamic fan-out explicitly rather
/// than folding them into used-by / calls tallies.
#[test]
fn show_lists_implementations_and_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub trait Speak {\n    fn speak(&self);\n}\n\npub struct Dog;\npub struct Cat;\n\nimpl Speak for Dog {\n    fn speak(&self) {}\n}\n\nimpl Speak for Cat {\n    fn speak(&self) {}\n}\n\npub fn announce(s: &dyn Speak) {\n    Speak::speak(s);\n}\n",
    )
    .unwrap();
    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "init"]);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (_, out) = sinter(repo, &["show", "Speak"]);
    assert!(out.contains("implemented by (2)    Cat, Dog"), "{out}");
    let (_, out) = sinter(repo, &["show", "Speak::speak"]);
    assert!(
        out.contains("dispatches to (2)    Cat::speak, Dog::speak"),
        "{out}"
    );
    assert!(
        !out.contains("calls ("),
        "dynamic edges leaked into calls: {out}"
    );
    let (_, out) = sinter(repo, &["show", "Dog"]);
    assert!(out.contains("implements       Speak"), "{out}");

    // A miss explains itself: forward reach, and who does reach the target.
    let (_, out) = sinter(repo, &["path", "Dog::speak", "announce"]);
    assert!(out.contains("no path Dog::speak -> announce"), "{out}");
    assert!(
        out.contains("forward search from Dog::speak reached 0 symbol(s)"),
        "{out}"
    );
    assert!(
        out.contains("nothing reaches announce under this filter"),
        "{out}"
    );
    let (_, out) = sinter(repo, &["path", "Dog", "Dog::speak"]);
    assert!(out.contains("Dog::speak is reached by (1):"), "{out}");
    assert!(out.contains("Speak::speak [calls/dynamic]"), "{out}");
    let (_, out) = sinter(repo, &["path", "announce", "Dog::speak", "--certain"]);
    assert!(
        out.contains("1 incoming edge(s) excluded by --evidence/--certain"),
        "{out}"
    );
}

#[test]
fn stable_handles_relocate_and_snapshot_preconditions_fail_typed() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn run() -> u8 { 1 }\n").unwrap();

    let (code, out) = sinter_code(repo, &["build"]);
    assert_eq!(code, Some(0), "{out}");
    let (code, out) = sinter_code(repo, &["query", "run", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let before: serde_json::Value = serde_json::from_str(&out).unwrap();
    let old_id = before["results"][0]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    let symbol_key = before["results"][0]["symbol_key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(before["results"][0]["id"], symbol_key);
    let old_snapshot = before["snapshot"].as_str().unwrap().to_string();

    std::fs::write(
        repo.join("src/lib.rs"),
        "// harmless offset shift\n\npub fn run() -> u8 { 1 }\n",
    )
    .unwrap();

    let (code, out) = sinter_code(repo, &["show", &symbol_key, "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let current: serde_json::Value = serde_json::from_str(&out).unwrap();
    let new_id = current["symbol"]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    let new_snapshot = current["snapshot"].as_str().unwrap().to_string();
    assert_ne!(new_id, old_id);
    assert_ne!(new_snapshot, old_snapshot);
    assert_eq!(current["symbol"]["id"], symbol_key);
    assert_eq!(current["symbol"]["symbol_key"], symbol_key);

    let (code, out) = sinter_code(repo, &["show", &old_id, "--json"]);
    assert_eq!(code, Some(2), "{out}");
    let relocated: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(relocated["error"]["code"], "relocated_handle", "{out}");
    assert_eq!(relocated["outcome"]["status"], "relocated", "{out}");
    assert_eq!(relocated["error"]["candidates"][0]["snapshot_id"], new_id);
    assert_eq!(relocated["error"]["candidates"][0]["id"], symbol_key);
    assert_eq!(
        relocated["error"]["candidates"][0]["symbol_key"],
        symbol_key
    );

    let (code, out) = sinter_code(
        repo,
        &[
            "show",
            &symbol_key,
            "--if-snapshot",
            &old_snapshot,
            "--json",
        ],
    );
    assert_eq!(code, Some(2), "{out}");
    let stale: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(stale["error"]["code"], "stale_snapshot", "{out}");
    assert_eq!(stale["error"]["expected_snapshot"], old_snapshot);
    assert_eq!(stale["error"]["actual_snapshot"], new_snapshot);

    let (code, out) = sinter_code(
        repo,
        &["show", &new_id, "--if-snapshot", &new_snapshot, "--json"],
    );
    assert_eq!(code, Some(0), "{out}");
}

#[test]
fn duplicate_declarations_make_stable_key_ambiguity_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(
        repo.join("duplicate.py"),
        "def duplicate(value):\n    return value\n\ndef duplicate(value, other):\n    return value + other\n",
    )
    .unwrap();
    let (code, out) = sinter_code(repo, &["build"]);
    assert_eq!(code, Some(0), "{out}");
    let (code, out) = sinter_code(repo, &["query", "duplicate", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let query: serde_json::Value = serde_json::from_str(&out).unwrap();
    let results = query["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "{out}");
    let key = results[0]["symbol_key"].as_str().unwrap();
    assert_eq!(results[1]["symbol_key"], key);

    let (code, out) = sinter_code(repo, &["show", key, "--json"]);
    assert_eq!(code, Some(2), "{out}");
    let ambiguous: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(ambiguous["error"]["code"], "ambiguous_symbol", "{out}");
    assert_eq!(
        ambiguous["error"]["candidates"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn syntax_visible_test_callers_drive_affected_and_impact_selection() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let source = |leaf: usize| {
        format!(
            "pub fn leaf() -> usize {{ {leaf} }}\n\
             pub fn dispatch() -> usize {{ leaf() }}\n\
             #[cfg(test)]\n\
             mod tests {{\n\
                 use super::dispatch;\n\
                 #[test]\n\
                 fn dispatch_works() {{\n\
                     let result = dispatch();\n\
                     assert!(result > 0);\n\
                 }}\n\
             }}\n"
        )
    };
    std::fs::write(repo.join("src/lib.rs"), source(1)).unwrap();
    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "base"]);
    std::fs::write(repo.join("src/lib.rs"), source(2)).unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "change leaf"]);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (code, out) = sinter_code(
        repo,
        &[
            "affected",
            "dispatch",
            "--depth",
            "1",
            "--relations",
            "calls",
            "--json",
        ],
    );
    assert_eq!(code, Some(0), "{out}");
    let affected: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(
        affected["dependents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dependent| dependent["s"] == "tests::dispatch_works"),
        "direct test caller missing: {out}"
    );

    let (code, out) = sinter_code(
        repo,
        &["affected", "leaf", "--relations", "calls", "--json"],
    );
    assert_eq!(code, Some(0), "{out}");
    let affected: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(
        affected["dependents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dependent| dependent["s"] == "tests::dispatch_works"),
        "transitive test caller missing: {out}"
    );

    let (code, out) = sinter_code(repo, &["impact", "HEAD~1..HEAD", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let impact: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(
        impact["affected_tests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|test| test["qualified"] == "tests::dispatch_works"),
        "impact did not select the evidence-backed test: {out}"
    );
}

#[test]
fn lookup_prefers_production_over_fixture_copies() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("tests/fixtures/a/src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "use crate::helper;\npub fn entry() -> u32 { 1 }\npub fn caller() -> u32 { entry() }\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("tests/fixtures/a/src/lib.rs"),
        "pub fn entry() -> u32 { 2 }\n",
    )
    .unwrap();
    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "init"]);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    // Ambiguous only because of the fixture copy: resolve, mention the rest.
    let (ok, out) = sinter(repo, &["show", "entry"]);
    assert!(ok, "{out}");
    assert!(out.contains("src/lib.rs"), "{out}");
    assert!(out.contains("1 more `entry` outside scope"), "{out}");

    // Explicit file suffix still reaches the fixture copy.
    let (ok, out) = sinter(repo, &["show", "entry@tests/fixtures/a/src/lib.rs"]);
    assert!(ok, "{out}");

    // affected counts file imports separately from callers.
    let (ok, out) = sinter(repo, &["affected", "entry", "--json"]);
    assert!(ok, "{out}");
    let json = out.lines().find(|l| l.starts_with('{')).unwrap();
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(v["direct"], 1, "{out}");
    assert!(v.get("importing_files").is_some(), "{out}");
}
