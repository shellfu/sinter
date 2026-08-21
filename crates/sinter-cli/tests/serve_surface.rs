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

fn body(response: &serde_json::Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
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
    let text = body(&responses[0]);
    assert!(
        !text.contains("\n  "),
        "must be compact, not pretty: {text}"
    );
    let v: serde_json::Value = serde_json::from_str(text).unwrap();
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

    // (2) limit caps dependents and reports the omission.
    let v: serde_json::Value = serde_json::from_str(body(&responses[1])).unwrap();
    assert_eq!(v["dependents"].as_array().unwrap().len(), 2);
    assert!(v["truncated"].as_u64().unwrap() >= 1, "{v}");

    // (3) Batch: one call, two results.
    let v: serde_json::Value = serde_json::from_str(body(&responses[2])).unwrap();
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "{v}");
    assert!(results.iter().all(|r| r.get("error").is_none()), "{v}");

    // (4) A missed path explains itself (CLI parity): Base never reaches
    // A3, so the answer carries forward reach, who reaches A3, and the
    // filter-excluded count.
    let v: serde_json::Value = serde_json::from_str(body(&responses[3])).unwrap();
    assert_eq!(v["found"], false, "{v}");
    assert!(v["miss"]["forward_reached"].is_u64(), "{v}");
    assert!(v["miss"]["reached_by"].is_array(), "{v}");
    assert!(v["miss"]["excluded_by_filter"].is_u64(), "{v}");
    assert_eq!(v["coverage"]["status"], "not_proven", "{v}");
    assert_eq!(v["coverage"]["conclusive"], false, "{v}");
    assert_eq!(v["coverage"]["compiler_index"]["state"], "missing", "{v}");
}

/// The repo surface lists map and overlap; map is a real orientation card;
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

    // map: totals, modules, and hubs (Base has four dependents).
    let v: serde_json::Value = serde_json::from_str(body(&responses[1])).unwrap();
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
    let v: serde_json::Value = serde_json::from_str(body(&responses[0])).unwrap();
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
    let first_body: serde_json::Value = serde_json::from_str(body(&first)).unwrap();
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
        let value: serde_json::Value = serde_json::from_str(body(&response)).unwrap();
        if value["exact"] == true {
            found = true;
            break;
        }
    }
    assert!(found, "server never ingested the watched source edit");
    drop(stdin);
    child.wait().unwrap();
}
