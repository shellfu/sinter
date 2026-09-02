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
        "package main\n\n// Base is the root of the blast radius.\nfunc Base() int {\n\treturn 1\n}\n\n// A1 calls Base.\nfunc A1() int { return Base() }\n\n// A2 calls Base.\nfunc A2() int { return Base() }\n\n// NewThreadField is lexical bait: its name is the task's English.\nfunc NewThreadField() int { return 2 }\n",
    );
    std::fs::create_dir_all(root.join("sub")).unwrap();
    write(
        "sub/lib.go",
        "package sub\n\n// Helper lives in a second lib.go.\nfunc Helper() int { return 3 }\n",
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
        "params": {"name": "context", "arguments": {"task": task, "include_coverage": true}},
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
    // result (envelope included), so default-budget outputs legitimately
    // differ in what they collapse.
    let (_, packet) = cli(&repo, task, &["--budget-bytes", "0"]);
    let response = mcp(&repo, task, Some(0));
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["operation"], "context");
    // MCP adds a terse-key `legend`, stamps `coverage.ref` so a session can
    // collapse the repeated repository-wide half, and slims
    // `coverage.compiler_index`; everything else is the CLI packet byte for
    // byte.
    let mut data = structured["data"].clone();
    let mut packet = packet;
    assert!(data["legend"].is_string(), "{data}");
    data.as_object_mut().unwrap().remove("legend");
    assert!(
        data["coverage"]["ref"]
            .as_str()
            .is_some_and(|value| value.starts_with("cov-")),
        "{data}"
    );
    data["coverage"].as_object_mut().unwrap().remove("ref");
    for side in [&mut data, &mut packet] {
        side["coverage"]["compiler_index"] = serde_json::Value::Null;
    }
    // Over MCP every next action is a tool call to send back, never a
    // shell command; the CLI renders the same actions as commands.
    let actions = data["next_actions"].as_array().unwrap();
    assert!(!actions.is_empty(), "{data}");
    for action in actions {
        let tool = action["tool"]
            .as_str()
            .unwrap_or_else(|| panic!("{action}"));
        assert!(
            matches!(tool, "show" | "affected" | "impact" | "map" | "ask"),
            "{action}"
        );
        assert!(action["args"].is_object(), "{action}");
        assert!(action.get("cli").is_none(), "{action}");
    }
    assert!(
        actions
            .iter()
            .any(|a| a["tool"] == "show" && a["args"]["symbol"] == "Base@lib.go"),
        "{data}"
    );
    assert!(
        packet["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(serde_json::Value::is_string),
        "{packet}"
    );
    for side in [&mut data, &mut packet] {
        side.as_object_mut().unwrap().remove("next_actions");
    }
    assert_eq!(data, packet);
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("context:") && text.len() <= 200, "{text}");
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

#[test]
fn resolved_identifiers_anchor_the_packet_over_lexical_bait() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    // `NewThreadField` matches "new thread field" lexically; `Base` is the
    // only token in the task that names a real node.
    let (_, packet) = cli(&repo, "add a new thread field to Base", &[]);
    assert_eq!(packet["outcome"], "ranked", "{packet}");
    let anchors = packet["anchors"].as_array().unwrap();
    assert_eq!(anchors.len(), 1, "{packet}");
    assert_eq!(anchors[0]["term"], "Base");
    assert_eq!(anchors[0]["qualified"], "Base");
    let candidates = packet["candidates"].as_array().unwrap();
    assert_eq!(candidates[0]["anchor"], "Base", "{packet}");
    assert_eq!(candidates[0]["rank"], 1);
    for c in &candidates[1..] {
        assert_eq!(
            c["focus"], false,
            "lexical hit expanded past an anchor: {c}"
        );
    }
    let intents: Vec<&str> = packet["unresolved_intents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(intents.contains(&"thread"), "{intents:?}");
    assert!(!intents.contains(&"Base"), "{intents:?}");
    // Tests come from the anchor's blast radius and carry a command.
    let tests = packet["tests"].as_array().unwrap();
    assert!(
        tests.iter().any(|t| t["qualified"] == "TestBase"),
        "{tests:?}"
    );
    assert!(tests.iter().all(|t| t.get("cmd").is_some()), "{tests:?}");
}

#[test]
fn file_names_anchor_by_path_suffix_and_list_contained_symbols() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let (_, packet) = cli(&repo, "add a per-tool helper in sub/lib.go", &[]);
    assert_eq!(packet["outcome"], "ranked", "{packet}");
    let anchors = packet["anchors"].as_array().unwrap();
    assert_eq!(anchors.len(), 1, "{packet}");
    assert_eq!(anchors[0]["term"], "sub/lib.go");
    assert_eq!(anchors[0]["qualified"], "sub/lib.go");
    assert_eq!(anchors[0]["k"], "file");
    let candidates = packet["candidates"].as_array().unwrap();
    assert_eq!(candidates[0]["handle"], "sub/lib.go", "{packet}");
    let contained: Vec<&str> = candidates
        .iter()
        .filter(|c| c["why"]["channels"][0] == "file")
        .map(|c| c["qualified"].as_str().unwrap())
        .collect();
    assert_eq!(contained, ["Helper"], "{packet}");
    let intents = packet["unresolved_intents"].as_array().unwrap();
    assert!(
        !intents.iter().any(|v| v == "sub/lib.go" || v == "per-tool"),
        "{intents:?}"
    );
    assert!(
        packet["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "sinter show sub/lib.go"),
        "{packet}"
    );

    // `lib.go` fits two indexed files: neither is guessed.
    let (_, packet) = cli(&repo, "add a helper in lib.go", &[]);
    assert!(packet["anchors"].as_array().unwrap().is_empty(), "{packet}");
    let ambiguous = &packet["gaps"]["ambiguous_files"][0];
    assert_eq!(ambiguous["term"], "lib.go", "{packet}");
    assert_eq!(
        ambiguous["candidates"],
        serde_json::json!(["lib.go", "sub/lib.go"]),
        "{packet}"
    );
}

#[test]
fn ranking_terms_skip_extensions_filler_and_hyphen_fragments() {
    let dir = tempfile::tempdir().unwrap();
    let repo = build_repo(dir.path());
    let (_, packet) = cli(
        &repo,
        "make Base honor a per-tool default budget in lib_test.go",
        &[],
    );
    let matched: Vec<&str> = packet["candidates"]
        .as_array()
        .unwrap()
        .iter()
        // Anchors state their own term; only `ask`'s scored hits rank on terms.
        .filter(|c| !c["score"].is_null())
        .flat_map(|c| c["why"]["matched"].as_array().into_iter().flatten())
        .filter_map(|v| v.as_str())
        .collect();
    assert!(!matched.is_empty(), "{packet}");
    for junk in ["go", "rs", "per", "honor", "make", "lib_test.go"] {
        assert!(!matched.contains(&junk), "ranked on `{junk}`: {matched:?}");
    }
}
