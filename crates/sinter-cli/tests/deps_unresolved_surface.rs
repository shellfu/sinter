//! Surface gate for the forward-traversal and gap-listing verbs: deps,
//! unresolved, and --relations filtering on the traversal verbs.

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

fn build_fixture(repo: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "mod util;\nuse crate::util::core_fn;\n\n/// Entry point.\npub fn entry() -> u32 {\n    core_fn()\n}\n\npub fn unrelated() -> u32 {\n    missing_fn()\n}\n\npub fn target() {}\n\npub fn unresolved_receiver(receiver: &UnknownReceiver) {\n    receiver.target();\n}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/util.rs"),
        "/// The core.\npub fn core_fn() -> u32 {\n    helper()\n}\n\n/// The helper.\npub fn helper() -> u32 {\n    41\n}\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
}

#[test]
fn deps_walks_forward_transitively() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    build_fixture(repo);

    // entry -> core_fn -> helper, cross-file, depth-annotated. The
    // default is direct only; --max-depth widens.
    let (ok, out) = sinter(repo, &["deps", "entry"]);
    assert!(ok, "{out}");
    assert!(out.contains("dependencies of"), "{out}");
    assert!(out.contains("core_fn"), "{out}");
    assert!(!out.contains("helper"), "{out}");
    let (ok, out) = sinter(repo, &["deps", "entry", "--max-depth", "3"]);
    assert!(ok, "{out}");
    assert!(out.contains("helper"), "{out}");

    // JSON mirrors the MCP shape.
    let (ok, out) = sinter(repo, &["deps", "entry", "--max-depth", "3", "--json"]);
    assert!(ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert!(v["total"].as_u64().unwrap() >= 2, "{out}");
    assert!(
        v["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["s"].as_str().unwrap().contains("helper"))
    );

    // Leaf symbol: valid query, no results — grep-style exit 1.
    let (ok, out) = sinter(repo, &["deps", "helper"]);
    assert!(!ok, "{out}");
    assert!(out.contains("0 dependencies"), "{out}");

    // Unknown symbol: exit 1 with suggestions path (NoMatch).
    let (ok, out) = sinter(repo, &["deps", "no_such_symbol_here"]);
    assert!(!ok, "{out}");

    // --limit footer names the widening command.
    let (ok, out) = sinter(repo, &["deps", "entry", "--max-depth", "3", "--limit", "1"]);
    assert!(ok, "{out}");
    assert!(out.contains("more dependencies below cutoff"), "{out}");
}

#[test]
fn relations_filter_restricts_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    build_fixture(repo);

    // calls-only still reaches the call chain.
    let (ok, out) = sinter(repo, &["deps", "entry", "--relations", "calls"]);
    assert!(ok, "{out}");
    assert!(out.contains("core_fn"), "{out}");

    // affected with calls-only reaches entry.
    let (ok, out) = sinter(repo, &["affected", "core_fn", "--relations", "calls"]);
    assert!(ok, "{out}");
    assert!(out.contains("entry"), "{out}");
    // The relation column only ever shows admitted relations.
    assert!(!out.contains("[imports/"), "{out}");
    assert!(!out.contains("[uses/"), "{out}");

    // An unknown relation is a usage error (exit 2, named in the message).
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["affected", "core_fn", "--relations", "bogus"])
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .current_dir(repo)
        .output()
        .expect("run sinter");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown relation"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // path accepts the flag too.
    let (ok, out) = sinter(repo, &["path", "entry", "core_fn", "--relations", "calls"]);
    assert!(ok, "{out}");
    assert!(out.contains("-[calls/"), "{out}");
}

#[test]
fn unresolved_lists_and_filters_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    build_fixture(repo);

    // missing_fn is extracted but never bound — it must be listed with
    // its site and enclosing definition.
    // Default output is counts plus actionable rows; likely-external names
    // are hidden behind `--all`.
    let (ok, out) = sinter(repo, &["unresolved"]);
    assert!(ok, "{out}");
    assert!(out.contains("unresolved reference(s)"), "{out}");
    assert!(out.contains("likely_external"), "{out}");
    assert!(!out.contains("missing_fn"), "{out}");
    assert!(out.contains("--all"), "{out}");
    let (ok, out) = sinter(repo, &["unresolved", "--all"]);
    assert!(ok, "{out}");
    assert!(out.contains("missing_fn"), "{out}");
    assert!(out.contains("unrelated"), "{out}");

    // --name narrows; a name with no gaps is exit 1.
    let (ok, out) = sinter(repo, &["unresolved", "--name", "missing_fn"]);
    assert!(ok, "{out}");
    assert!(out.contains("missing_fn"), "{out}");
    let (ok, _) = sinter(repo, &["unresolved", "--name", "definitely_absent"]);
    assert!(!ok);

    // --file narrows to one file's gaps.
    let (ok, out) = sinter(repo, &["unresolved", "--file", "src/util.rs"]);
    assert!(!ok, "{out}");
    assert!(out.contains("0 unresolved"), "{out}");

    // JSON default keeps the totals but lists only default rows; `--all`
    // carries every entry.
    let (ok, out) = sinter(repo, &["unresolved", "--json"]);
    assert!(ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert!(v["total"].as_u64().unwrap() >= 1, "{out}");
    assert!(
        v["by_category"]["likely_external"].as_u64().unwrap() >= 1,
        "{out}"
    );
    let rows = v["unresolved"].as_array().unwrap();
    assert!(!rows.is_empty(), "{out}");
    assert!(
        rows.iter().all(|r| r["category"] != "likely_external"),
        "{out}"
    );
    let (ok, out) = sinter(repo, &["unresolved", "--json", "--all"]);
    assert!(ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert!(
        v["unresolved"].as_array().unwrap().iter().any(|r| {
            r["name"].as_str() == Some("missing_fn") && r["reason"].as_str() == Some("syntax_only")
        }),
        "{out}"
    );
}

#[test]
fn negative_traversals_lead_with_not_proven_and_keep_observed_counts() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    build_fixture(repo);

    let (ok, out) = sinter(repo, &["affected", "target"]);
    assert!(
        !ok,
        "a valid empty traversal keeps grep-style exit 1: {out}"
    );
    assert!(out.starts_with("not proven: 0 dependents"), "{out}");
    assert!(
        out.contains("unresolved ref(s) also name `target`"),
        "{out}"
    );
    // No index: scip is the fix. Footer is one coverage line carrying the
    // snapshot, plus the index gap; the generic disclaimers stay out.
    assert!(out.contains("`sinter scip` would bind them"), "{out}");
    assert!(out.contains("coverage: partial · 0 certain"), "{out}");
    assert!(out.contains("· snapshot graph-"), "{out}");
    assert!(!out.contains("  snapshot: "), "{out}");
    assert!(!out.contains("filters:"), "{out}");
    assert!(!out.contains("not proof that no runtime path"), "{out}");
    assert_eq!(out.matches("gap:").count(), 1, "{out}");

    // Fresh index: the same refs are not bindable by scip.
    std::fs::write(repo.join(".sinter/index.scip"), b"").unwrap();
    let (_, out) = sinter(repo, &["affected", "target"]);
    assert!(out.contains("not bindable by scip"), "{out}");
    assert!(!out.contains("gap:"), "{out}");
    std::fs::remove_file(repo.join(".sinter/index.scip")).unwrap();

    let (ok, out) = sinter(repo, &["affected", "target", "--json"]);
    assert!(!ok, "{out}");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["status"], "not_proven", "{out}");
    assert_eq!(value["total"], 0, "{out}");
    assert!(value["unresolved_refs_matching_name"].as_u64().unwrap() >= 1);
    // CLI JSON carries the slim compiler index; `doctor` keeps the projects.
    assert!(
        value["coverage"]["compiler_index"]
            .get("projects")
            .is_none(),
        "{out}"
    );
    assert_eq!(
        value["coverage"]["compiler_index"]["state"], "missing",
        "{out}"
    );

    let (ok, out) = sinter(repo, &["deps", "unresolved_receiver"]);
    assert!(!ok, "{out}");
    assert!(out.starts_with("not proven: 0 dependencies"), "{out}");

    let (ok, out) = sinter(repo, &["deps", "unresolved_receiver", "--json"]);
    assert!(!ok, "{out}");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["status"], "not_proven", "{out}");
    assert_eq!(value["total"], 0, "{out}");
    assert!(value["unresolved_refs_in_symbol"].as_u64().unwrap() >= 1);

    let (ok, out) = sinter(repo, &["path", "unresolved_receiver", "target"]);
    assert!(!ok, "{out}");
    assert!(out.starts_with("not proven: no path"), "{out}");
    assert!(
        out.contains("unresolved ref(s) on the forward frontier"),
        "{out}"
    );

    let (ok, out) = sinter(repo, &["path", "unresolved_receiver", "target", "--json"]);
    assert!(!ok, "{out}");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["status"], "not_proven", "{out}");
    assert_eq!(value["found"], false, "{out}");
    assert!(
        value["miss"]["unresolved_matching_target"]
            .as_u64()
            .unwrap()
            >= 1,
        "{out}"
    );
    assert_eq!(value["coverage"]["status"], "not_proven", "{out}");
}
