//! `sinter context`: one evidence packet per task, budgeted by default,
//! byte-identical between CLI `--json` and the MCP `context` tool.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn build_repo(root: &Path) -> PathBuf {
    let write = |rel: &str, content: &str| std::fs::write(root.join(rel), content).unwrap();
    write("go.mod", "module example.com/fixture\n\ngo 1.22\n");
    write(
        "lib.go",
        "package main\n\n// Base is the root of the blast radius.\nfunc Base() int {\n\treturn 1\n}\n\n// A1 calls Base.\nfunc A1() int { return Base() }\n\n// A2 calls Base.\nfunc A2() int { return Base() }\n",
    );
    write(
        "lib_test.go",
        "package main\n\nimport \"testing\"\n\n// TestBase covers Base.\nfunc TestBase(t *testing.T) { Base() }\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["build"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    root.to_path_buf()
}

fn cli(repo: &Path, task: &str, extra: &[&str]) -> (Vec<u8>, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["context", task, "--json"])
        .args(extra)
        .current_dir(repo)
        .output()
        .unwrap();
    let value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.stdout, value)
}

fn mcp(repo: &Path, task: &str, budget: Option<u64>) -> serde_json::Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["serve", "--repo", repo.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "context", "arguments": {"task": task}},
    });
    let mut request = request;
    if let Some(budget) = budget {
        request["params"]["arguments"]["budget_bytes"] = serde_json::json!(budget);
    }
    writeln!(child.stdin.as_mut().unwrap(), "{request}").unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.lines().next().expect("one response")).unwrap()
}

#[test]
fn cli_packet_has_every_section_within_default_budget() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let (bytes, packet) = cli(&repo, "change the Base root of the blast radius", &[]);
    assert!(bytes.len() <= 8000, "{} bytes", bytes.len());
    for key in [
        "candidates",
        "tests",
        "gaps",
        "next_actions",
        "coverage",
        "outcome",
    ] {
        assert!(packet.get(key).is_some(), "missing {key}: {packet}");
    }
    let candidates = packet["candidates"].as_array().unwrap();
    assert!(!candidates.is_empty(), "{packet}");
    let top = &candidates[0];
    assert_eq!(top["focus"], true);
    assert_eq!(top["name"], "Base");
    assert!(top["excerpt"].as_str().unwrap().contains("func Base"));
    assert_eq!(top["affected"]["direct"], 3, "{top}");
    assert!(top["why"]["matched"].is_array());
    assert!(
        packet["tests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["qualified"] == "TestBase"),
        "{packet}"
    );
    let actions = packet["next_actions"].as_array().unwrap();
    assert!(
        actions
            .iter()
            .any(|a| a.as_str().unwrap() == "sinter show Base@lib.go"),
        "{actions:?}"
    );
    assert!(
        actions
            .iter()
            .any(|a| a.as_str().unwrap().starts_with("sinter impact"))
    );
}

#[test]
fn mcp_context_matches_cli_json() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let task = "change the Base root of the blast radius";
    // Unbudgeted on both sides: the MCP budget measures the whole tool
    // result (legacy text body duplicates `data`), so default-budget
    // outputs legitimately differ in what they collapse.
    let (_, packet) = cli(&repo, task, &["--budget-bytes", "0"]);
    let response = mcp(&repo, task, Some(0));
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["operation"], "context");
    assert_eq!(structured["data"], packet);
    let text: serde_json::Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, packet);
    // Default MCP budget applies to `context` like every other tool.
    let bounded = mcp(&repo, task, None);
    let wire = serde_json::to_string(&bounded["result"]).unwrap();
    assert!(wire.len() <= 8000, "{} bytes", wire.len());
    assert_eq!(
        bounded["result"]["structuredContent"]["data"]["outcome"],
        "ranked"
    );
}

#[test]
fn abstaining_ask_still_yields_a_packet_with_fallbacks() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let (_, packet) = cli(&repo, "zzqx vvplor wibble", &[]);
    assert_eq!(packet["outcome"], "abstain", "{packet}");
    assert!(packet["gaps"]["abstain_reason"].is_string(), "{packet}");
    let actions = packet["next_actions"].as_array().unwrap();
    assert!(
        actions
            .iter()
            .any(|a| a.as_str().unwrap().starts_with("rg -n")),
        "{actions:?}"
    );
    assert!(
        actions
            .iter()
            .any(|a| a.as_str().unwrap().starts_with("sinter impact"))
    );
}
