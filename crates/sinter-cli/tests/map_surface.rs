//! Acceptance for `sinter map`: a bounded structural inventory with module
//! counts, dependency hubs, doc entry points, and an honest health envelope.

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

    // Dependency hubs: `start` is called from two files.
    assert!(
        out.contains("Dependency hubs (non-containment in-degree"),
        "{out}"
    );
    let hub_section = &out[out.find("Dependency hubs").unwrap()..];
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

    // Map states what keeps the graph from complete, in one line.
    assert!(out.contains("health: "), "{out}");
    assert!(out.contains("user gaps"), "{out}");
    assert!(out.contains("partial-syntax files"), "{out}");

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
    assert_eq!(parsed["orientation"]["kind"], "repository_inventory");
    assert_eq!(
        parsed["orientation"]["hub_metric"],
        "cross_module_in_degree_then_non_contains_in_degree"
    );
    assert_eq!(
        parsed["orientation"]["claim_boundary"],
        "structural_evidence_not_runtime_architecture"
    );
    let modules = parsed["modules"].as_array().expect("modules array");
    assert!(
        modules
            .iter()
            .any(|m| m["path"] == "core" && m["files"] == 1),
        "{parsed}"
    );
    assert!(
        modules
            .iter()
            .any(|m| m["path"] == "util" && m["files"] == 2),
        "{parsed}"
    );
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
    assert_eq!(parsed["health"]["status"], "partial", "{parsed}");
    assert!(
        parsed["health"]["compiler_index"]["state"].is_string(),
        "{parsed}"
    );
    assert!(
        parsed["health"]["graph"]["actionable_unresolved"].is_u64(),
        "{parsed}"
    );
    assert!(
        parsed["health"]["limitations"].as_array().is_some(),
        "{parsed}"
    );
}

#[test]
fn default_map_excludes_golden_fixture_hubs_but_all_scope_recovers_them() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fixture(repo);
    std::fs::create_dir_all(repo.join("harness/golden/case")).unwrap();
    std::fs::write(
        repo.join("harness/golden/case/check.ts"),
        "export function check(): number { return 1; }\n",
    )
    .unwrap();
    for name in ["one", "two", "three"] {
        std::fs::write(
            repo.join(format!("harness/golden/case/{name}.ts")),
            format!(
                "import {{ check }} from './check';\nexport function {name}(): number {{ return check(); }}\n"
            ),
        )
        .unwrap();
    }
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (ok, default) = sinter(repo, &["map", "--json"]);
    assert!(ok, "{default}");
    let default: serde_json::Value = serde_json::from_str(&default).unwrap();
    assert_eq!(
        default["scope"],
        serde_json::json!(["production", "test", "docs"])
    );
    assert!(
        default["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|module| module["path"] != "harness"),
        "{default}"
    );
    assert!(
        default["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hub| hub["file"] != "harness/golden/case/check.ts"),
        "{default}"
    );

    let (ok, all) = sinter(repo, &["map", "--json", "--scope", "all"]);
    assert!(ok, "{all}");
    let all: serde_json::Value = serde_json::from_str(&all).unwrap();
    assert!(
        all["modules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|module| module["path"] == "harness"),
        "{all}"
    );
    assert!(
        all["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hub| hub["file"] == "harness/golden/case/check.ts" && hub["scope"] == "fixture"),
        "{all}"
    );

    let (ok, shown) = sinter(repo, &["show", "check", "--json"]);
    assert!(ok, "{shown}");
    let shown: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(shown["symbol"]["scope"], "fixture", "{shown}");
}

#[test]
fn repository_override_can_promote_an_exception_into_default_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("harness/golden/production-tool")).unwrap();
    std::fs::write(
        repo.join("harness/golden/production-tool/main.ts"),
        "export function shippedTool(): number { return 1; }\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".sinter.toml"),
        r#"
[[scope.override]]
pattern = "harness/golden/production-tool/**"
scope = "production"
"#,
    )
    .unwrap();
    let (ok, build) = sinter(repo, &["build"]);
    assert!(ok, "{build}");
    let (ok, output) = sinter(repo, &["ask", "shipped tool", "--json"]);
    assert!(ok, "{output}");
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    let hit = &output["topics"][0]["hits"][0];
    assert_eq!(hit["name"], "shippedTool", "{output}");
    assert_eq!(hit["scope"], "production", "{output}");
}
