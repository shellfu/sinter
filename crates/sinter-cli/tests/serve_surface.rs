//! `sinter serve` (repo scope) response shape: summary-first, terse,
//! capped, batchable, compact-encoded — agent context is a budget.

use std::io::Write;
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

fn call(id: u64, args: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": "affected", "arguments": args}
    })
    .to_string()
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

    // (2) limit caps dependents and reports the omission.
    let v: serde_json::Value = serde_json::from_str(body(&responses[1])).unwrap();
    assert_eq!(v["dependents"].as_array().unwrap().len(), 2);
    assert!(v["truncated"].as_u64().unwrap() >= 1, "{v}");

    // (3) Batch: one call, two results.
    let v: serde_json::Value = serde_json::from_str(body(&responses[2])).unwrap();
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "{v}");
    assert!(results.iter().all(|r| r.get("error").is_none()), "{v}");
}
