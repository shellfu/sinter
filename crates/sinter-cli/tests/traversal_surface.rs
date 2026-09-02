//! Surface gate for what a traversal answer leaves out and says so: test
//! rows counted not listed, hubs stopped at, filter-excluded misses,
//! node-disjoint routes, and the seed's own file inside a grep bound.

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

fn json(out: &str) -> serde_json::Value {
    serde_json::from_str(out.lines().next().unwrap_or("")).unwrap_or_else(|e| panic!("{e}: {out}"))
}

/// top -> entry -> core_fn and top -> other -> core_fn (a diamond), plus
/// one `#[cfg(test)]` caller of core_fn.
fn fixture(repo: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn core_fn() -> u32 {\n    41\n}\n\npub fn entry() -> u32 {\n    core_fn()\n}\n\npub fn other() -> u32 {\n    core_fn()\n}\n\npub fn top() -> u32 {\n    entry() + other()\n}\n\n#[cfg(test)]\nmod tests {\n    use super::core_fn;\n    #[test]\n    fn test_core() {\n        let v = core_fn();\n        assert!(v > 0);\n    }\n}\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
}

#[test]
fn test_rows_are_counted_by_default_and_listed_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);

    let (ok, out) = sinter(repo, &["affected", "core_fn"]);
    assert!(ok, "{out}");
    assert!(out.contains("tests: 1 (--include-tests)"), "{out}");
    assert!(!out.contains("test_core"), "{out}");

    let (_, out) = sinter(repo, &["affected", "core_fn", "--json"]);
    let v = json(&out);
    assert_eq!(v["tests_hidden"], 1, "{out}");
    assert!(
        v["dependents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["scope"] != "test"),
        "{out}"
    );

    // Listed on request, ranked after every production row.
    let (_, out) = sinter(repo, &["affected", "core_fn", "--include-tests", "--json"]);
    let v = json(&out);
    assert!(v.get("tests_hidden").is_none(), "{out}");
    let scopes: Vec<&str> = v["dependents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["scope"].as_str().unwrap())
        .collect();
    assert_eq!(scopes.last().copied(), Some("test"), "{out}");
    assert!(
        scopes[..scopes.len() - 1].iter().all(|s| *s != "test"),
        "{out}"
    );
    let (_, out) = sinter(repo, &["affected", "core_fn", "--include-tests"]);
    assert!(out.contains("test_core"), "{out}");
    assert!(!out.contains("--include-tests)"), "{out}");
}

#[test]
fn an_empty_answer_names_the_filter_that_emptied_it() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);

    let (ok, out) = sinter(repo, &["affected", "core_fn", "--max-depth", "0", "--json"]);
    assert!(!ok, "{out}");
    let v = json(&out);
    assert_eq!(v["status"], "not_proven", "{out}");
    assert_eq!(v["reason"], "filter_excluded", "{out}");

    // Syntax-only edges are never certain: --certain empties the answer,
    // and the answer says so instead of reading as "no callers".
    let (_, out) = sinter(repo, &["affected", "core_fn", "--certain"]);
    assert!(out.contains("not proven: 0 dependents"), "{out}");
    assert!(out.contains("reason: filter excluded"), "{out}");

    let (_, out) = sinter(repo, &["deps", "entry", "--max-depth", "0", "--json"]);
    assert_eq!(json(&out)["reason"], "filter_excluded", "{out}");

    // A symbol with genuinely nothing keeps the bare verdict.
    let (_, out) = sinter(repo, &["affected", "top", "--json"]);
    let v = json(&out);
    assert_eq!(v["status"], "not_proven", "{out}");
    assert!(v.get("reason").is_none(), "{out}");
}

#[test]
fn deps_defaults_to_direct_and_names_the_widening_flag() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);

    let (ok, out) = sinter(repo, &["deps", "top"]);
    assert!(ok, "{out}");
    assert!(out.contains("entry"), "{out}");
    assert!(!out.contains("core_fn"), "direct only by default: {out}");
    assert!(out.contains("direct; --max-depth 3 to widen"), "{out}");

    let (_, out) = sinter(repo, &["deps", "top", "--json"]);
    assert_eq!(json(&out)["max_depth"], 1, "{out}");

    let (ok, out) = sinter(repo, &["deps", "top", "--max-depth", "3"]);
    assert!(ok, "{out}");
    assert!(out.contains("core_fn"), "{out}");
    assert!(!out.contains("to widen"), "{out}");
}

#[test]
fn k_routes_are_node_disjoint_and_a_miss_always_shows_frontier_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);

    let (ok, out) = sinter(repo, &["path", "top", "core_fn", "-k", "3"]);
    assert!(ok, "{out}");
    let routes: Vec<&str> = out.lines().filter(|l| l.starts_with("top -[")).collect();
    assert_eq!(routes.len(), 2, "{out}");
    assert!(routes.iter().any(|r| r.contains("-> entry ")), "{out}");
    assert!(routes.iter().any(|r| r.contains("-> other ")), "{out}");
    assert!(out.contains("2 node-disjoint route(s)"), "{out}");

    let (_, out) = sinter(repo, &["path", "top", "core_fn", "-k", "3", "--json"]);
    let v = json(&out);
    assert_eq!(v["paths"].as_array().unwrap().len(), 2, "{out}");
    assert_eq!(v["steps"], v["paths"][0], "{out}");
    // Without -k the shape is unchanged.
    let (_, out) = sinter(repo, &["path", "top", "core_fn", "--json"]);
    assert!(json(&out).get("paths").is_none(), "{out}");

    // A miss carries the frontier and retries in text, even when empty.
    let (ok, out) = sinter(repo, &["path", "core_fn", "top"]);
    assert!(!ok, "{out}");
    assert!(out.contains("closest frontier: none"), "{out}");
    assert!(out.contains("suggested retries: none"), "{out}");
    let (_, out) = sinter(repo, &["path", "core_fn", "top", "--json"]);
    let miss = &json(&out)["miss"];
    assert!(miss["closest_frontier"].is_array(), "{out}");
    assert!(miss["suggested_retries"].is_array(), "{out}");
    assert!(miss["reached_by_total"].is_number(), "{out}");

    // A filter that refuses the final hop is the reason for the miss.
    let (_, out) = sinter(repo, &["path", "top", "core_fn", "--certain", "--json"]);
    let v = json(&out);
    assert_eq!(v["status"], "not_proven", "{out}");
    assert_eq!(v["reason"], "filter_excluded", "{out}");
}

#[test]
fn default_coverage_is_a_summary_and_the_full_block_is_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);

    let (_, out) = sinter(repo, &["affected", "core_fn", "--json"]);
    let v = json(&out);
    let mut keys: Vec<&str> = v["coverage"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "compiler_index",
            "completeness",
            "conclusive",
            "evidence",
            "snapshot",
            "status"
        ],
        "{out}"
    );
    assert_eq!(
        v["coverage"]["compiler_index"],
        serde_json::json!({"state": "missing"})
    );
    assert!(
        v["coverage"]["evidence"]["unresolved"]["within_radius"].is_object(),
        "{out}"
    );

    for verb in [
        vec!["affected", "core_fn"],
        vec!["deps", "entry"],
        vec!["path", "entry", "core_fn"],
    ] {
        let mut args = verb.clone();
        args.extend(["--json", "--coverage"]);
        let (_, out) = sinter(repo, &args);
        let v = json(&out);
        assert!(v["coverage"]["limitations"].is_array(), "{verb:?}: {out}");
        assert!(v["coverage"]["filters"].is_object(), "{verb:?}: {out}");
        assert!(v["coverage"]["universe"].is_object(), "{verb:?}: {out}");
    }
    // Text: the one gap line names the degraded relation, not input counts.
    let (_, out) = sinter(repo, &["affected", "core_fn"]);
    assert!(
        out.contains("gap: scip missing — receiver/method calls may be missing"),
        "{out}"
    );
    assert_eq!(out.matches("gap:").count(), 1, "{out}");
}

#[test]
fn grep_bound_includes_the_seed_definition_file() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);

    // core_fn depends on nothing: the bound is exactly its own file.
    let (ok, out) = sinter(
        repo,
        &["grep", "core_fn", "--within", "deps(core_fn)", "--json"],
    );
    assert!(ok, "{out}");
    let v = json(&out);
    assert_eq!(v["files_in_bound"], 1, "{out}");
    assert!(out.contains("src/lib.rs"), "{out}");
}
