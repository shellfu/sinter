//! Acceptance for `sinter map`: one-screen orientation — module tree,
//! hub symbols, doc entry points — terse, deterministic, --json-able.

use std::path::Path;
use std::process::Command;

fn sinter(repo: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .current_dir(repo)
        // Hermetic HOME: no stale-artifact nudges in captured output.
        .env("HOME", repo)
        .env("USERPROFILE", repo)
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

/// Two modules (util depends on core) plus a sectioned README and a doc.
fn fixture(repo: &Path) {
    std::fs::create_dir_all(repo.join("core")).unwrap();
    std::fs::create_dir_all(repo.join("util")).unwrap();
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::write(
        repo.join("core/engine.ts"),
        r#"// Core engine.
export class Engine {
  tick(): void {}
}

export function start(): number {
  return 1;
}
"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("util/loader.ts"),
        r#"import { start } from "../core/engine";

export function load(): number {
  return start();
}
"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("util/boot.ts"),
        r#"import { start } from "../core/engine";

export function boot(): number {
  return start();
}
"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("README.md"),
        "# Overview\n\nWhat this is.\n\n## Details\n\nSmall print.\n",
    )
    .unwrap();
    std::fs::write(repo.join("docs/design.md"), "# Design\n\nWhy it is so.\n").unwrap();
}

#[test]
fn map_shows_modules_hubs_and_docs() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (ok, out) = sinter(repo, &["map"]);
    assert!(ok, "{out}");

    // Module tree with per-module node counts.
    assert!(out.contains("Modules"), "{out}");
    assert!(out.contains("core/"), "{out}");
    assert!(out.contains("util/"), "{out}");

    // Hubs: `start` is called from two files — most depended-on.
    assert!(out.contains("Hubs (most depended-on)"), "{out}");
    let hub_section = &out[out.find("Hubs").unwrap()..];
    assert!(hub_section.contains("start"), "{out}");
    assert!(hub_section.contains("core/engine.ts:"), "{out}");

    // Doc entry points: level-1 sections of README.md and docs/*.md;
    // level-2 headings are not entry points.
    assert!(out.contains("Docs"), "{out}");
    let docs_section = &out[out.find("Docs").unwrap()..];
    assert!(docs_section.contains("README.md"), "{out}");
    assert!(docs_section.contains("Overview"), "{out}");
    assert!(docs_section.contains("docs/design.md"), "{out}");
    assert!(docs_section.contains("Design"), "{out}");
    assert!(!docs_section.contains("Details"), "{out}");

    // Next-step hints, like every other orientation verb.
    assert!(out.contains("Next: sinter ask"), "{out}");

    // Determinism: byte-identical across runs.
    let (_, again) = sinter(repo, &["map"]);
    assert_eq!(out, again, "map output not deterministic");
}

#[test]
fn map_json_is_valid_and_structured() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["map", "--json"])
        .current_dir(repo)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .output()
        .expect("run sinter");
    assert!(out.status.success());
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("valid json");

    assert!(parsed["nodes"].as_u64().unwrap() > 0);
    let modules = parsed["modules"].as_array().expect("modules array");
    assert!(modules.iter().any(|m| m["path"] == "core"), "{parsed}");
    assert!(modules.iter().any(|m| m["path"] == "util"), "{parsed}");
    let hubs = parsed["hubs"].as_array().expect("hubs array");
    assert!(!hubs.is_empty(), "{parsed}");
    assert!(hubs[0]["in_degree"].as_u64().unwrap() > 0, "{parsed}");
    assert!(
        hubs[0]["file"].is_string() && hubs[0]["line"].is_u64(),
        "{parsed}"
    );
    let docs = parsed["docs"].as_array().expect("docs array");
    assert!(
        docs.iter().any(|d| d["file"] == "README.md"
            && d["sections"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == "Overview")),
        "{parsed}"
    );
}
