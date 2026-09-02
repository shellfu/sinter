//! `--budget-bytes` / `budget_bytes`: serialized agent output never exceeds
//! the budget, and what was cut is recoverable through `next_cursor`.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn sinter(repo: &Path, args: &[&str]) -> (bool, Vec<u8>) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .output()
        .expect("run sinter");
    if out.status.success() {
        (true, out.stdout)
    } else {
        let mut both = out.stdout;
        both.extend_from_slice(b"\n--- stderr ---\n");
        both.extend_from_slice(&out.stderr);
        (false, both)
    }
}

fn fixture(repo: &Path) {
    std::fs::write(
        repo.join("hub.ts"),
        "/** Hub service wiring. */\nexport class Hub {\n  run(): void {}\n}\n",
    )
    .unwrap();
    // Enough documented doc-section hits that a ten-hit `ask` overflows.
    for n in 0..12 {
        let section: String = (0..40)
            .map(|i| format!("Skill card hook number {i} wires hub {n} into the agent flow.\n"))
            .collect();
        std::fs::write(
            repo.join(format!("doc{n}.md")),
            format!("# Skill card hooks {n}\n\n{section}"),
        )
        .unwrap();
    }
    for i in 0..60 {
        std::fs::write(
            repo.join(format!("user{i}.ts")),
            format!("import {{ Hub }} from './hub';\n/** user {i} */\nexport function use{i}(): void {{ new Hub().run(); }}\n"),
        )
        .unwrap();
    }
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{}", String::from_utf8_lossy(&out));
}

/// `ask`/`affected`/`impact` may already carry an integer `truncated`
/// (their own limit); the budget cut is then flagged as `budget_truncated`.
fn cut(v: &serde_json::Value) -> bool {
    v["truncated"] == true || v["budget_truncated"] == true
}

fn budgeted(repo: &Path, args: &[&str], budget: usize) -> serde_json::Value {
    let (ok, out) = sinter(repo, args);
    assert!(ok, "{}", String::from_utf8_lossy(&out));
    assert!(
        out.len() <= budget,
        "{} bytes > budget {budget}: {}",
        out.len(),
        String::from_utf8_lossy(&out)
    );
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    // Stamped only when the budget changed something; every call here overflows.
    assert_eq!(value["budget_bytes"], budget, "{value}");
    value
}

#[test]
fn ask_respects_budget_and_exposes_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);
    let (_, full) = sinter(
        repo,
        &["ask", "skill card hooks", "--json", "--limit", "10"],
    );
    assert!(full.len() > 2000, "fixture must overflow: {}", full.len());

    let value = budgeted(
        repo,
        &[
            "ask",
            "skill card hooks",
            "--json",
            "--budget-bytes",
            "1200",
        ],
        1200,
    );
    let hits = value["topics"][0]["hits"].as_array().unwrap();
    assert!(!hits.is_empty());
    assert!(hits[0]["doc"].as_str().unwrap().len() <= 420);
    assert!(value["totals"].is_object(), "{value}");
    assert_eq!(value["next_cursor"].is_number(), cut(&value), "{value}");
}

#[test]
fn affected_respects_budget_and_cursor_recovers_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);
    let (_, full) = sinter(repo, &["affected", "Hub", "--json"]);
    let full: serde_json::Value = serde_json::from_slice(&full).unwrap();
    let total = full["dependents"].as_array().unwrap().len();

    let page = budgeted(
        repo,
        &["affected", "Hub", "--json", "--budget-bytes", "4000"],
        4000,
    );
    assert!(cut(&page), "{page}");
    assert_eq!(page["totals"]["dependents"], total);
    let kept = page["dependents"].as_array().unwrap().len();
    assert!(kept > 0 && kept < total);
    let cursor = page["next_cursor"].as_u64().unwrap() as usize;
    assert_eq!(cursor, kept);

    let rest = budgeted(
        repo,
        &[
            "affected",
            "Hub",
            "--json",
            "--budget-bytes",
            "4000",
            "--offset",
            &cursor.to_string(),
        ],
        4000,
    );
    let mut seen = kept + rest["dependents"].as_array().unwrap().len();
    let mut next = rest["next_cursor"].as_u64();
    while let Some(c) = next {
        let (_, out) = sinter(
            repo,
            &[
                "affected",
                "Hub",
                "--json",
                "--budget-bytes",
                "4000",
                "--offset",
                &c.to_string(),
            ],
        );
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        seen += v["dependents"].as_array().unwrap().len();
        next = v["next_cursor"].as_u64();
    }
    assert_eq!(seen, total, "pages must cover every dependent");
}

#[test]
fn mcp_defaults_to_8000_bytes_and_accepts_budget_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);
    let mut child = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["serve", "--repo", repo.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list"}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"affected","arguments":{{"symbol":"Hub"}}}}}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"affected","arguments":{{"symbol":"Hub","budget_bytes":3000}}}}}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"affected","arguments":{{"symbol":"Hub","budget_bytes":0}}}}}}"#).unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    let lines: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .collect();
    let list: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    for tool in list["result"]["tools"].as_array().unwrap() {
        assert!(
            tool["inputSchema"]["properties"]["budget_bytes"].is_object(),
            "{}",
            tool["name"]
        );
        assert!(
            tool["inputSchema"]["properties"]["cursor"].is_object(),
            "{}",
            tool["name"]
        );
    }
    let result =
        |line: &str| serde_json::from_str::<serde_json::Value>(line).unwrap()["result"].clone();
    let default = result(lines[1]);
    assert!(serde_json::to_string(&default).unwrap().len() <= 8000);
    // Stamped only when the default budget had to cut something.
    assert!(
        default["structuredContent"]["data"]["budget_bytes"]
            .as_u64()
            .is_none_or(|budget| budget == 8000),
        "{default}"
    );
    // The tool's own `limit` left rows behind: the page says so.
    assert_eq!(
        default["structuredContent"]["data"]["next_cursor"], 50,
        "{default}"
    );
    assert_eq!(
        default["structuredContent"]["outcome"]["reason"],
        "limit_reached"
    );
    let small = result(lines[2]);
    assert!(serde_json::to_string(&small).unwrap().len() <= 3000);
    assert!(cut(&small["structuredContent"]["data"]));
    let unlimited = result(lines[3]);
    assert!(unlimited["structuredContent"]["data"]["budget_bytes"].is_null());
    assert!(!cut(&unlimited["structuredContent"]["data"]));
}
