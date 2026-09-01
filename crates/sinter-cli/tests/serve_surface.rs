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
    assert_eq!(
        v["coverage"]["filters"]["relations"]["mode"], "all_dependencies",
        "{v}"
    );
    // The repository-wide half was already sent by the first answer in this
    // session, so the fourth carries only the reference to it.
    let first = body(&responses[0]);
    assert_eq!(first["coverage"]["completeness"], "partial", "{first}");
    assert_eq!(
        first["coverage"]["compiler_index"]["state"], "missing",
        "{first}"
    );
    assert_eq!(v["coverage"]["ref"], first["coverage"]["ref"], "{v}");
    assert_eq!(v["coverage"]["completeness"], "partial", "{v}");
    assert_eq!(v["coverage"]["conclusive"], false, "{v}");
    assert_eq!(v["coverage"]["universe"]["mode"], "repository", "{v}");
    assert!(v["coverage"]["compiler_index"].is_null(), "{v}");
    assert!(
        v["coverage"]["ref_note"]
            .as_str()
            .is_some_and(|note| note.contains("sinter://coverage")),
        "{v}"
    );
}

/// The collapsed `coverage.ref` is resolvable: `sinter://coverage` serves
/// exactly the repository-wide half the reference names.
#[test]
fn coverage_reference_resolves_to_a_resource() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let read = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "resources/read",
        "params": {"uri": "sinter://coverage"},
    })
    .to_string();
    let list = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "resources/list", "params": {},
    })
    .to_string();
    let responses = serve(
        &repo,
        &[call(1, serde_json::json!({"symbol": "Base"})), read, list],
    );

    let carried = body(&responses[0])["coverage"]["ref"].clone();
    assert!(
        carried.as_str().is_some_and(|r| r.starts_with("cov-")),
        "{carried}"
    );
    let text = responses[1]["result"]["contents"][0]["text"]
        .as_str()
        .unwrap();
    let document: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(document["ref"], carried, "{document}");
    assert_eq!(document["completeness"], "partial", "{document}");
    assert!(document["limitations"].is_array(), "{document}");
    assert!(
        responses[2]["result"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["uri"] == "sinter://coverage"),
        "{}",
        responses[2]
    );
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
    assert_eq!(
        v["scope"],
        serde_json::json!(["production", "test", "docs"])
    );
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

    // Unknown symbol with no close names: a tool outcome (`isError`), not a
    // JSON-RPC error, pointing at concept search — the MCP hint, and only
    // that one (no leftover CLI-flavored hint).
    assert!(responses[2].get("error").is_none(), "{}", responses[2]);
    let miss = &responses[2]["result"];
    assert_eq!(miss["isError"], true, "{miss}");
    assert_eq!(miss["structuredContent"]["outcome"]["status"], "not_found");
    let msg = miss["structuredContent"]["error"]["message"]
        .as_str()
        .unwrap();
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

/// A near-miss lookup is a tool result with `isError`: the close names
/// reach the summary text and `structuredContent`, never `error.data`
/// (most clients drop that). A tie-break note leads the summary line and
/// is mirrored under `outcome.warnings`. Stderr stays clean throughout.
#[test]
fn lookup_miss_and_tie_break_notes_are_visible_to_mcp_clients() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    // Same-name symbol in another language so `show Helper` must tie-break
    // (the dominant language wins, the other is reported as ignored).
    std::fs::write(
        repo.join("extra.ts"),
        "/** Helper duplicated in another language. */\nexport function Helper(): number { return 3; }\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["serve", "--repo", repo.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn serve");
    {
        let stdin = child.stdin.as_mut().unwrap();
        for request in [
            call_tool(1, "show", serde_json::json!({"symbol": "Helpr"})),
            call_tool(2, "show", serde_json::json!({"symbol": "Helper"})),
            call_tool(3, "nope", serde_json::json!({})),
        ] {
            writeln!(stdin, "{request}").unwrap();
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.stderr.is_empty(),
        "stderr must stay silent on stdio: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert!(responses[0].get("error").is_none(), "{}", responses[0]);
    let miss = &responses[0]["result"];
    assert_eq!(miss["isError"], true, "{miss}");
    let text = miss["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("show Helpr: not found; close names: Helper"),
        "{text}"
    );
    let structured = &miss["structuredContent"];
    assert_eq!(structured["operation"], "show");
    assert_eq!(structured["outcome"]["status"], "not_found");
    assert_eq!(structured["outcome"]["partial"], false);
    assert_eq!(structured["error"]["code"], "no_match");
    assert_eq!(structured["error"]["retryable"], false);
    assert!(
        structured["error"]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().starts_with("Helper")),
        "{structured}"
    );

    let hit = &responses[1]["result"];
    assert_eq!(hit["isError"], false, "{hit}");
    let text = hit["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("show Helper: 1 other `Helper` ignored ("),
        "{text}"
    );
    let outcome = &hit["structuredContent"]["outcome"];
    assert_eq!(outcome["status"], "complete");
    assert_eq!(outcome["warnings"], hit["structuredContent"]["warnings"]);
    assert_eq!(
        outcome["warnings"].as_array().unwrap().len(),
        1,
        "{outcome}"
    );

    // A protocol fault is still a JSON-RPC error.
    assert_eq!(responses[2]["error"]["code"], -32000, "{}", responses[2]);
    assert_eq!(
        responses[2]["error"]["data"]["error"]["code"],
        "unknown_operation"
    );
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

/// One CLI read command in `--json` mode. Read verbs exit grep-style, so
/// exit 1 (a valid query with no results) is not a harness failure.
fn cli_json(repo: &Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .arg("--repo")
        .arg(repo)
        .output()
        .expect("run sinter CLI");
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "sinter {args:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse CLI JSON")
}

/// `grep` is the verb that removes a tool switch, so it has to exist over
/// MCP, be advertised, and return exactly what the CLI returns.
#[test]
fn grep_is_advertised_and_matches_cli_json() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());

    // `--scope all` on the CLI is the MCP default, left implicit here so the
    // comparison also pins that default.
    let cli = cli_json(
        &repo,
        &[
            "grep",
            "Base",
            "--within",
            "affected(Base)",
            "--scope",
            "all",
            "--json",
        ],
    );
    let responses = serve(
        &repo,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_string(),
            call_tool(
                2,
                "grep",
                serde_json::json!({"pattern": "Base", "within": ["affected(Base)"]}),
            ),
        ],
    );

    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    let grep = tools
        .iter()
        .find(|tool| tool["name"] == "grep")
        .unwrap_or_else(|| panic!("grep missing from tools/list: {}", responses[0]));
    assert_eq!(
        grep["inputSchema"]["required"],
        serde_json::json!(["pattern", "within"])
    );
    assert_eq!(grep["inputSchema"]["properties"]["within"]["type"], "array");

    let mut data = body(&responses[1]);
    assert_eq!(data["status"], "found", "{data}");
    assert!(data["total"].as_u64().unwrap() > 0, "{data}");
    assert!(data["matches"][0]["f"].is_string(), "{data}");
    assert!(data["matches"][0]["l"].is_u64(), "{data}");
    assert!(data["matches"][0]["t"].is_string(), "{data}");
    // The envelope adds `legend` to any response carrying terse rows; the
    // payload underneath must be the CLI document.
    assert_eq!(
        data["legend"].as_str().unwrap(),
        "f=file l=line t=text",
        "{data}"
    );
    data.as_object_mut().unwrap().remove("legend");
    assert_eq!(data, cli, "MCP grep data diverged from CLI --json");
    assert_eq!(
        responses[1]["result"]["structuredContent"]["outcome"]["status"],
        "complete"
    );

    // A bound that reaches nothing is a bounded answer, not an error.
    let empty = serve(
        &repo,
        &[call_tool(
            1,
            "grep",
            serde_json::json!({"pattern": "zzz_no_such_text", "within": ["file(lib.go)"]}),
        )],
    );
    assert_eq!(body(&empty[0])["status"], "not_proven");
    assert_eq!(
        empty[0]["result"]["structuredContent"]["outcome"]["status"],
        "not_proven"
    );
}

/// `show --body` over MCP: the excerpt is the CLI's excerpt, and absent
/// unless asked for.
#[test]
fn show_excerpt_matches_cli_json_and_is_omitted_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());

    let cli = cli_json(
        &repo,
        &[
            "show",
            "Base",
            "--scope",
            "all",
            "--json",
            "--body",
            "--context-lines",
            "2",
        ],
    );
    let responses = serve(
        &repo,
        &[
            call_tool(
                1,
                "show",
                serde_json::json!({"symbol": "Base", "body": true, "context_lines": 2}),
            ),
            call_tool(2, "show", serde_json::json!({"symbol": "Base"})),
        ],
    );

    let with_body = body(&responses[0]);
    assert!(
        with_body["excerpt"]
            .as_str()
            .unwrap()
            .contains("func Base()"),
        "{with_body}"
    );
    assert_eq!(with_body, cli, "MCP show data diverged from CLI --json");
    assert!(
        body(&responses[1]).get("excerpt").is_none(),
        "excerpt must be absent when not requested: {}",
        responses[1]
    );
}

/// `impact --expect` over MCP: the unfinished-refactor check, byte-identical
/// to the CLI and omitted entirely when nothing is expected.
#[test]
fn impact_expect_matches_cli_json_and_is_omitted_when_unused() {
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
    // Only A1 is updated; A2/A3/A4 still call Base and are what `--expect`
    // reports as owed.
    let source = std::fs::read_to_string(repo.join("lib.go")).unwrap();
    std::fs::write(
        repo.join("lib.go"),
        source.replace(
            "func A1() int { return Base() }",
            "func A1() int { return Base() + 1 }",
        ),
    )
    .unwrap();
    git(&["add", "lib.go"]);
    git(&["commit", "-qm", "update A1"]);

    let cli = cli_json(
        &repo,
        &["impact", "HEAD~1..HEAD", "--json", "--expect", "Base"],
    );
    let responses = serve(
        &repo,
        &[
            call_tool(
                1,
                "impact",
                serde_json::json!({"rev_range": "HEAD~1..HEAD", "expect": ["Base"]}),
            ),
            call_tool(
                2,
                "impact",
                serde_json::json!({"rev_range": "HEAD~1..HEAD"}),
            ),
        ],
    );

    let expected = body(&responses[0]);
    assert_eq!(expected["expect"][0]["symbol"], "Base", "{expected}");
    assert!(
        expected["expect"][0]["untouched_total"].as_u64().unwrap() > 0,
        "{expected}"
    );
    assert_eq!(expected, cli, "MCP impact data diverged from CLI --json");
    assert!(
        body(&responses[1]).get("expect").is_none(),
        "expect must be absent when not requested: {}",
        responses[1]
    );
}
