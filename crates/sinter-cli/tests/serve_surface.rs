//! `sinter serve` (repo scope) response shape: summary-first, terse,
//! capped, batchable, compact-encoded — agent context is a budget.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One symbol (`Base`) with four dependents across two files, plus a
/// second symbol (`Helper`) for the batch case.
fn build_repo(root: &Path) -> PathBuf {
    let write = |rel: &str, content: &str| {
        std::fs::write(root.join(rel), content).unwrap();
    };
    write("go.mod", "module example.com/fixture\n\ngo 1.22\n");
    write(
        "lib.go",
        "package main\n\n// Base is the root of the blast radius.\nfunc Base() int {\n\treturn 1\n}\n\n// Helper is the second batch symbol.\nfunc Helper() int {\n\treturn 2\n}\n\n// A1 calls Base.\nfunc A1() int { return Base() }\n\n// A2 calls Base.\nfunc A2() int { return Base() }\n",
    );
    write(
        "more.go",
        "package main\n\n// A3 calls Base.\nfunc A3() int { return Base() }\n\n// A4 calls Base.\nfunc A4() int { return Base() }\n\n// H1 calls Helper.\nfunc H1() int { return Helper() }\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["build"])
        .current_dir(root)
        .output()
        .expect("run sinter build");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    root.to_path_buf()
}

/// Drive `serve --repo` over stdio; returns one parsed response per request.
fn serve(repo: &Path, requests: &[String]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["serve", "--repo", repo.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn serve");
    {
        let stdin = child.stdin.as_mut().unwrap();
        for r in requests {
            writeln!(stdin, "{r}").unwrap();
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn call_tool(id: u64, name: &str, args: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": args}
    })
    .to_string()
}

fn call(id: u64, args: serde_json::Value) -> String {
    call_tool(id, "affected", args)
}

fn body(response: &serde_json::Value) -> serde_json::Value {
    let data = &response["result"]["structuredContent"]["data"];
    assert!(
        data.is_object(),
        "missing MCP structuredContent: {response}"
    );
    data.clone()
}

fn cli_impact(repo: &Path, limit: Option<usize>) -> serde_json::Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sinter"));
    command
        .args(["impact", "HEAD~1..HEAD", "--json", "--repo"])
        .arg(repo);
    if let Some(limit) = limit {
        command.args(["--limit", &limit.to_string()]);
    }
    let output = command.output().expect("run impact CLI");
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse impact CLI JSON")
}

#[test]
fn impact_budget_has_cli_mcp_parity_and_zero_means_all() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "go.mod", "lib.go", "more.go"]);
    git(&["commit", "-qm", "base"]);
    let source = std::fs::read_to_string(repo.join("lib.go")).unwrap();
    std::fs::write(repo.join("lib.go"), source.replace("return 1", "return 10")).unwrap();
    git(&["add", "lib.go"]);
    git(&["commit", "-qm", "change Base"]);

    let cli_default = cli_impact(&repo, None);
    let cli_all = cli_impact(&repo, Some(0));
    let responses = serve(
        &repo,
        &[
            call_tool(
                1,
                "impact",
                serde_json::json!({"rev_range": "HEAD~1..HEAD"}),
            ),
            call_tool(
                2,
                "impact",
                serde_json::json!({"rev_range": "HEAD~1..HEAD", "limit": 0}),
            ),
        ],
    );
    let mcp_default = &responses[0]["result"]["structuredContent"]["data"];
    let mcp_all = &responses[1]["result"]["structuredContent"]["data"];

    assert_eq!(cli_default["limit"], 20);
    assert_eq!(cli_default, *mcp_default);
    assert_eq!(cli_all, *mcp_all);
    assert_eq!(cli_all["limit"], 0);
    for collection in ["changed_symbols", "blast_radius", "affected_tests"] {
        assert_eq!(
            cli_all[collection].as_array().unwrap().len() as u64,
            cli_all["totals"][collection].as_u64().unwrap(),
            "limit 0 omitted {collection}: {cli_all}"
        );
        assert_eq!(cli_all["truncated"][collection], 0);
    }
}

#[test]
fn affected_is_terse_capped_and_batchable() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let responses = serve(
        &repo,
        &[
            call(1, serde_json::json!({"symbol": "Base"})),
            call(2, serde_json::json!({"symbol": "Base", "limit": 2})),
            call(3, serde_json::json!({"symbols": ["Base", "Helper"]})),
            call_tool(4, "path", serde_json::json!({"from": "Base", "to": "A3"})),
        ],
    );

    // (1) Summary-first, terse dependents, no per-dependent doc/signature.
    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        text.starts_with("affected Base:") && text.len() <= 200,
        "text body must be a one-line summary: {text}"
    );
    let v = body(&responses[0]);
    let text = &v;
    assert!(v["total"].as_u64().unwrap() > 3, "{text}");
    assert!(!v["by_file"].as_array().unwrap().is_empty(), "{text}");
    let deps = v["dependents"].as_array().unwrap();
    assert!(deps.len() > 3, "{text}");
    for d in deps {
        assert!(d["s"].is_string() && d["e"].is_string(), "{d}");
        assert!(
            d.get("doc").is_none() && d.get("signature").is_none(),
            "{d}"
        );
    }
    // Root symbol keeps its full node.
    assert!(v["symbol"]["doc"].is_string(), "{text}");
    // Direct callers stated apart from the transitive total (CLI parity).
    assert!(v["direct"].as_u64().unwrap() >= 1, "{text}");
    assert!(v["direct_files"].as_u64().unwrap() >= 1, "{text}");
    assert!(v["snapshot"].is_string(), "{text}");
    assert_eq!(v["coverage"]["status"], "found", "{text}");
    assert_eq!(v["coverage"]["completeness"], "partial", "{text}");
    assert!(
        v["coverage"]["evidence"]["possible"]["results"]
            .as_u64()
            .unwrap()
            >= 1,
        "{text}"
    );
    assert_eq!(
        responses[0]["result"]["structuredContent"]["data"], v,
        "CLI-compatible payload must be the MCP agent contract data"
    );

    // (2) limit caps dependents and reports the omission.
    let v: serde_json::Value = body(&responses[1]);
    assert_eq!(v["dependents"].as_array().unwrap().len(), 2);
    assert!(v["truncated"].as_u64().unwrap() >= 1, "{v}");

    // (3) Batch: one call, two results.
    let v: serde_json::Value = body(&responses[2]);
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "{v}");
    assert!(results.iter().all(|r| r.get("error").is_none()), "{v}");
    assert!(results.iter().all(|r| r["coverage"].is_object()), "{v}");

    // (4) A missed path explains itself (CLI parity): Base never reaches
    // A3, so the answer carries forward reach, who reaches A3, and the
    // filter-excluded count.
    let v: serde_json::Value = body(&responses[3]);
    assert_eq!(v["found"], false, "{v}");
    assert!(v["miss"]["forward_reached"].is_u64(), "{v}");
    assert!(v["miss"]["reached_by"].is_array(), "{v}");
    assert!(v["miss"]["excluded_by_filter"].is_u64(), "{v}");
    assert_eq!(v["coverage"]["status"], "not_proven", "{v}");
    assert_eq!(v["coverage"]["completeness"], "partial", "{v}");
    assert_eq!(v["coverage"]["conclusive"], false, "{v}");
    assert_eq!(
        v["coverage"]["filters"]["relations"]["mode"], "all_dependencies",
        "{v}"
    );
    assert_eq!(v["coverage"]["compiler_index"]["state"], "missing", "{v}");
}

/// The repo surface lists map and overlap; map is a real inventory card;
/// unknown-symbol errors carry a recovery hint; overlap validates arity.
#[test]
fn map_tool_and_error_hints() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let responses = serve(
        &repo,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_string(),
            call_tool(2, "map", serde_json::json!({})),
            call_tool(3, "show", serde_json::json!({"symbol": "NoSuchThingZz"})),
            call_tool(4, "overlap", serde_json::json!({"ranges": ["only-one"]})),
            call_tool(5, "map", serde_json::json!({"scope": ["all"]})),
        ],
    );

    let names: Vec<&str> = responses[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "map",
        "ask",
        "show",
        "query",
        "affected",
        "path",
        "impact",
        "overlap",
        "unresolved",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
    let map_schema = responses[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "map")
        .unwrap();
    assert!(
        map_schema["inputSchema"]["properties"]["scope"].is_object(),
        "{map_schema}"
    );

    // map: explicit inventory semantics, health, modules, and dependency
    // hubs (Base has four dependents).
    let v: serde_json::Value = body(&responses[1]);
    assert_eq!(v["scope"], serde_json::json!(["production", "docs"]));
    assert_eq!(v["orientation"]["kind"], "repository_inventory");
    assert_eq!(v["health"]["status"], "partial");
    assert!(v["nodes"].as_u64().unwrap() > 0, "{v}");
    assert!(!v["modules"].as_array().unwrap().is_empty(), "{v}");
    assert!(
        v["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["name"].as_str().unwrap().contains("Base")),
        "{v}"
    );
    assert_eq!(
        responses[1]["result"]["structuredContent"]["outcome"]["status"],
        "partial"
    );

    // Unknown symbol with no close names: point at concept search —
    // the MCP hint, and only that one (no leftover CLI-flavored hint).
    let msg = responses[2]["error"]["message"].as_str().unwrap();
    assert!(msg.contains("ask tool"), "no recovery hint: {msg}");
    assert!(
        !msg.contains("sinter ask"),
        "CLI hint should be replaced, not doubled: {msg}"
    );

    // Overlap needs at least two ranges.
    let msg = responses[3]["error"]["message"].as_str().unwrap();
    assert!(msg.contains("two rev-ranges"), "{msg}");

    let all: serde_json::Value = body(&responses[4]);
    assert_eq!(all["scope"].as_array().unwrap().len(), 7, "{all}");
}

/// Two identical rev ranges collide on every touched symbol: risk high.
#[test]
fn overlap_ranks_pairwise_risk() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let lib = std::fs::read_to_string(repo.join("lib.go")).unwrap();
    std::fs::write(repo.join("lib.go"), lib.replace("return 1", "return 10")).unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "change Base"]);

    let responses = serve(
        &repo,
        &[call_tool(
            1,
            "overlap",
            serde_json::json!({"ranges": ["a=HEAD~1..HEAD", "b=HEAD~1..HEAD"]}),
        )],
    );
    let v: serde_json::Value = body(&responses[0]);
    assert_eq!(v["changes"].as_array().unwrap().len(), 2, "{v}");
    let pair = &v["pairs"].as_array().unwrap()[0];
    assert_eq!(pair["risk"], "high", "{v}");
    assert!(
        pair["direct"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap().contains("Base")),
        "{v}"
    );
}

/// A long-lived MCP process must reuse a clean generation and still ingest
/// an uncommitted edit once its watcher marks the repository dirty.
#[test]
fn server_refreshes_after_source_event() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["serve", "--repo", repo.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let request = |id| call_tool(id, "query", serde_json::json!({"symbol": "FreshSymbol"}));

    writeln!(stdin, "{}", request(1)).unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let first: serde_json::Value = serde_json::from_str(&line).unwrap();
    let first_body: serde_json::Value = body(&first);
    assert_eq!(first_body["exact"], false);

    let mut source = std::fs::read_to_string(repo.join("lib.go")).unwrap();
    source.push_str("\nfunc FreshSymbol() int { return 7 }\n");
    std::fs::write(repo.join("lib.go"), source).unwrap();

    let mut found = false;
    for id in 2..42 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        writeln!(stdin, "{}", request(id)).unwrap();
        stdin.flush().unwrap();
        line.clear();
        stdout.read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        let value: serde_json::Value = body(&response);
        if value["exact"] == true {
            found = true;
            break;
        }
    }
    assert!(found, "server never ingested the watched source edit");
    drop(stdin);
    child.wait().unwrap();
}

#[test]
fn mcp_reports_snapshot_staleness_and_handle_relocation_as_typed_errors() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let initial = serve(
        &repo,
        &[call_tool(1, "query", serde_json::json!({"symbol": "Base"}))],
    );
    let query: serde_json::Value = body(&initial[0]);
    let snapshot = query["snapshot"].as_str().unwrap().to_string();
    let id = query["results"][0]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    let symbol_key = query["results"][0]["symbol_key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(query["results"][0]["id"], symbol_key);

    let source = std::fs::read_to_string(repo.join("lib.go")).unwrap();
    std::fs::write(repo.join("lib.go"), format!("// offset shift\n{source}")).unwrap();

    let responses = serve(
        &repo,
        &[
            call_tool(2, "show", serde_json::json!({"symbol": id})),
            call_tool(
                3,
                "show",
                serde_json::json!({
                    "symbol": symbol_key,
                    "if_snapshot": snapshot,
                }),
            ),
        ],
    );
    assert_eq!(
        responses[0]["error"]["data"]["error"]["code"], "relocated_handle",
        "{}",
        responses[0]
    );
    assert!(
        responses[0]["error"]["data"]["error"]["candidates"][0]["symbol_key"].is_string(),
        "{}",
        responses[0]
    );
    assert_eq!(
        responses[1]["error"]["data"]["error"]["code"], "stale_snapshot",
        "{}",
        responses[1]
    );
    assert!(
        responses[1]["error"]["data"]["error"]["actual_snapshot"].is_string(),
        "{}",
        responses[1]
    );
}
