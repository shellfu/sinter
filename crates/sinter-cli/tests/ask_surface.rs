//! Acceptance for `sinter ask` / `sinter show`.
//! The controller fixture models the reference failure case: component
//! classes, constructor-like functions named after the base concept, and a
//! documented controller class that must rank #1.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn path_with_sinter() -> std::ffi::OsString {
    let executable = Path::new(env!("CARGO_BIN_EXE_sinter"));
    let mut paths = vec![
        executable
            .parent()
            .expect("sinter binary directory")
            .to_path_buf(),
    ];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).expect("valid search path")
}

fn sinter(repo: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .current_dir(repo)
        // Hermetic HOME: the developer's real ~/.claude must not leak
        // stale-artifact nudges into captured output.
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .env("PATH", path_with_sinter())
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

/// Component classes + `character()` constructor-like noise + a documented
/// controller class. Mirrors the prototype session where the real answer
/// ranked 45th of 51.
fn controller_fixture(repo: &Path) {
    std::fs::create_dir_all(repo.join("player/traversal")).unwrap();
    std::fs::write(
        repo.join("player/character.ts"),
        r#"// Main player character controller: movement, traversal, input routing.
export class PlayerCharacterV2 {
  // Advances one frame.
  update(): void {
    climbFactory();
  }
}

// Builds a character record (constructor-style noise seed).
export function character(): number {
  return 1;
}
"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("player/traversal/climb.ts"),
        r#"import { character } from "../character";

// Climbing movement component.
export class ClimbComponentV2 {
  begin(): void {
    character();
  }
}

export function climbFactory(): number {
  return character();
}
"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("player/traversal/ledge.ts"),
        r#"// Ledge grab component.
export class LedgeComponentV2 {
  grab(): void {}
}
"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("tests/character.test.ts"),
        "export function character(): number {\n  return 2;\n}\n",
    )
    .ok();
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    std::fs::write(
        repo.join("tests/character.test.ts"),
        "export function testCharacter(): number {\n  return 2;\n}\n",
    )
    .unwrap();
}

#[test]
fn ask_ranks_documented_controller_first() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    controller_fixture(repo);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let started = Instant::now();
    let (ok, out) = sinter(repo, &["ask", "where is the character controller?"]);
    let elapsed = started.elapsed();
    assert!(ok, "{out}");

    // The documented controller class is hit #1; constructor noise is not
    // in the top 5 lines of ranked output.
    let first_hit = out
        .lines()
        .find(|l| l.starts_with("1. "))
        .expect("ranked output");
    assert!(first_hit.contains("PlayerCharacterV2"), "{out}");
    assert!(first_hit.contains("class"), "{out}");
    // Content-bearing: doc line and provenance present.
    assert!(out.contains("Main player character controller"), "{out}");
    assert!(out.contains("2/2 terms"), "{out}");
    // Constructor-style function must not outrank components.
    let ctor_rank = out
        .lines()
        .position(|l| l.contains("function character") || l.contains(" character    "));
    let class_rank = out.lines().position(|l| l.contains("PlayerCharacterV2"));
    if let (Some(c), Some(k)) = (ctor_rank, class_rank) {
        assert!(k < c, "constructor outranked controller:\n{out}");
    }
    // Perf gate (v1 scale) — includes process spawn. Nominal is ~66ms;
    // 1500ms still catches any real regression while riding out cold
    // shared CI runners (500ms flaked once under runner load).
    assert!(
        elapsed < Duration::from_millis(1500),
        "ask took {elapsed:?} end to end"
    );

    // Determinism: byte-identical output across runs.
    let (_, again) = sinter(repo, &["ask", "where is the character controller?"]);
    assert_eq!(out, again, "ask output not deterministic");
}

#[test]
fn ask_zero_hits_and_stopword_only() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    controller_fixture(repo);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (ok, out) = sinter(repo, &["ask", "zzqx flurble"]);
    assert!(!ok, "no-result ask must exit nonzero: {out}");
    assert!(out.contains("no match"), "{out}");

    let (ok, out) = sinter(repo, &["ask", "where is the?"]);
    assert!(!ok, "stopword-only question should fail with a hint");
    assert!(out.contains("no searchable terms"), "{out}");
}

#[test]
fn ask_json_is_compact_unless_explanation_is_requested() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    controller_fixture(repo);
    sinter(repo, &["build"]);
    let (ok, out) = sinter(repo, &["ask", "character controller", "--json"]);
    assert!(ok, "{out}");
    let compact: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    let first = &compact["topics"][0]["hits"][0];
    assert_eq!(first["name"], "PlayerCharacterV2");
    assert!(first["line"].as_u64().unwrap() > 0);
    assert!(first["matched"].as_array().unwrap().len() == 2);
    assert!(first["confidence"].is_string(), "{out}");
    assert!(
        first["id"].as_str().unwrap().starts_with("symbol:"),
        "{out}"
    );
    // Lean by default: scores, spans, ids, and calibration are --explain detail.
    for field in [
        "score",
        "span",
        "snapshot_id",
        "symbol_key",
        "score_breakdown",
        "calibration",
        "ranking_margin",
        "ranking_bucket",
        "abstain",
        "family_size",
        "roles",
    ] {
        assert!(first.get(field).is_none(), "{field} leaked: {out}");
    }
    let topic = &compact["topics"][0];
    assert!(topic["confidence"]["level"].is_string(), "{out}");
    assert!(topic["confidence"]["reason"].is_string(), "{out}");
    assert!(topic.get("advice").is_none(), "{out}");
    assert!(topic["confidence"].get("calibration").is_none(), "{out}");
    assert!(topic["verify_required"].is_boolean(), "{out}");

    let (ok, out) = sinter(
        repo,
        &["ask", "character controller", "--json", "--explain"],
    );
    assert!(ok, "{out}");
    let mut explained: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    let explained_first = &explained["topics"][0]["hits"][0];
    assert_eq!(
        explained_first["score"],
        explained_first["score_breakdown"]["final_score"]
    );
    assert!(explained_first.get("calibration").is_none(), "{out}");
    assert!(explained_first["span"]["end"].as_u64().unwrap() > 0);
    assert!(explained_first["snapshot_id"].is_string(), "{out}");
    assert!(
        explained["topics"][0]["confidence"]["calibration"].is_object(),
        "{out}"
    );
    assert!(explained["topics"][0]["advice"].is_string(), "{out}");

    // --explain only adds fields: every lean field is unchanged.
    for (topic, lean_topic) in explained["topics"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(compact["topics"].as_array().unwrap())
    {
        for (hit, lean_hit) in topic["hits"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip(lean_hit_list(lean_topic))
        {
            for (key, value) in lean_hit.as_object().unwrap() {
                assert_eq!(&hit[key], value, "--explain changed {key}: {out}");
            }
        }
        assert_eq!(topic["status"], lean_topic["status"]);
        assert_eq!(topic["verify_required"], lean_topic["verify_required"]);
        assert_eq!(
            topic["confidence"]["level"],
            lean_topic["confidence"]["level"]
        );
    }
}

fn lean_hit_list(topic: &serde_json::Value) -> &[serde_json::Value] {
    topic["hits"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// Abstain never hides the candidates: the text list and the JSON hits are
/// identical to the confident case, only the caveat line differs.
#[test]
fn ask_abstain_still_lists_candidates_and_prefers_production() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("tests/fixtures")).unwrap();
    std::fs::write(
        repo.join("src/policy.go"),
        "package policy\n\n// Decide turns a hook request into a policy decision.\nfunc Decide(r Request) Decision { return Decision{} }\n\nfunc Request() int { return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("tests/fixtures/fake.go"),
        "package fixtures\n\nfunc Request() int { return 2 }\n\nfunc Decision() int { return 3 }\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["ask", "request policy decision", "--scope", "all"]);
    assert!(ok, "{out}");
    assert!(out.lines().any(|l| l.starts_with("1. ")), "{out}");
    assert!(!out.contains("abstain:"), "{out}");
    let first = out.lines().find(|l| l.starts_with("1. ")).unwrap();
    assert!(
        first.contains("Decide"),
        "fixture outranked production:\n{out}"
    );
    let (ok, out) = sinter(
        repo,
        &["ask", "request policy decision", "--scope", "all", "--json"],
    );
    assert!(ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let topic = &v["topics"][0];
    assert!(topic["confidence"]["reason"].is_string(), "{out}");
    assert!(!topic["hits"].as_array().unwrap().is_empty(), "{out}");
    assert_eq!(topic["hits"][0]["name"], "Decide", "{out}");
}

#[test]
fn ask_singleton_abstains_instead_of_claiming_high_confidence() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(
        repo.join("single.go"),
        "package one\n\nfunc SingularQuartz() int { return 1 }\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["ask", "singular quartz", "--json"]);
    assert!(ok, "{out}");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    let topic = &value["topics"][0];
    assert_eq!(topic["status"], "abstain", "{out}");
    assert_eq!(topic["confidence"]["level"], "unrated", "{out}");
    assert_eq!(topic["confidence"]["reason"], "no_runner_up", "{out}");
    assert_eq!(topic["verify_required"], true, "{out}");
    let (ok, out) = sinter(repo, &["ask", "singular quartz", "--json", "--explain"]);
    assert!(ok, "{out}");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    let topic = &value["topics"][0];
    assert!(topic["ranking_margin"]["permille"].is_null(), "{out}");
    assert_eq!(
        topic["confidence"]["calibration"]["version"],
        "ask-holdout-2026-08-23.v2"
    );
}

/// Agent vocabulary: "cap ... output size" reaches a constant named with
/// budget/bytes through query synonyms, without a literal term match.
#[test]
fn ask_synonyms_reach_budget_constant_without_outranking_literal_matches() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(
        repo.join("protocol.rs"),
        "/// Default MCP byte budget. Tool results land in an agent's context window.\n\
         pub const MCP_DEFAULT_BUDGET_BYTES: usize = 8000;\n\n\
         /// Register the sinter server in every client's MCP config.\n\
         pub fn mcp_server_register() {}\n\n\
         /// Cap the output size of one text block.\n\
         pub fn cap_output_size(text: &str) -> String { text.to_owned() }\n\n\
         /// Serve requests over stdio.\n\
         pub fn serve() {}\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(
        repo,
        &[
            "ask",
            "how does the MCP server cap output size",
            "--json",
            "--limit",
            "3",
        ],
    );
    assert!(ok, "{out}");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    let names = value["topics"][0]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(
        names.contains(&"MCP_DEFAULT_BUDGET_BYTES".to_owned()),
        "synonyms must reach the budget constant: {names:?}\n{out}"
    );
    // A literal match on the same terms still outranks the synonym match.
    assert_eq!(names[0], "cap_output_size", "{names:?}\n{out}");

    // Text --explain: calibration under the confidence line, one
    // `explain:` line per hit; neither leaks without the flag.
    let (ok, plain) = sinter(repo, &["ask", "cap output size"]);
    assert!(ok, "{plain}");
    assert!(
        !plain.contains("explain:") && !plain.contains("calibration:"),
        "{plain}"
    );
    let (ok, explained) = sinter(repo, &["ask", "cap output size", "--explain"]);
    assert!(ok, "{explained}");
    assert!(explained.contains("calibration:"), "{explained}");
    assert!(explained.contains("explain: channels"), "{explained}");
    assert!(explained.contains("· margin "), "{explained}");
}

/// The machine contract is transport-stable: CLI `--json` is precisely the
/// MCP envelope's data payload, every advertised input is closed, every tool
/// declares a versioned output, and ambiguity is structured error data.
#[test]
fn agent_protocol_matches_cli_and_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    controller_fixture(repo);
    std::fs::write(
        repo.join("duplicate_a.ts"),
        "export function collide(): void {}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("duplicate_b.ts"),
        "export function collide(): void {}\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (ok, cli) = sinter(repo, &["ask", "character controller", "--json"]);
    assert!(ok, "{cli}");
    assert!(!cli.contains("\n  "), "agent JSON must be compact: {cli}");
    let cli: serde_json::Value = serde_json::from_str(&cli).unwrap();
    let (ok, cli_explained) = sinter(
        repo,
        &["ask", "character controller", "--json", "--explain"],
    );
    assert!(ok, "{cli_explained}");
    let cli_explained: serde_json::Value = serde_json::from_str(&cli_explained).unwrap();
    let (ok, cli_query) = sinter(
        repo,
        &["query", "PlayerCharacterV2", "--limit", "5", "--json"],
    );
    assert!(ok, "{cli_query}");
    let cli_query: serde_json::Value = serde_json::from_str(&cli_query).unwrap();
    let (ok, ambiguous_cli) = sinter(repo, &["show", "collide", "--json"]);
    assert!(!ok, "ambiguous symbol must not succeed: {ambiguous_cli}");
    let ambiguous_cli: serde_json::Value = serde_json::from_str(&ambiguous_cli).unwrap();
    assert_eq!(ambiguous_cli["error"]["code"], "ambiguous_symbol");
    assert_eq!(
        ambiguous_cli["error"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["serve", "--repo", repo.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn MCP server");
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list"}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"ask","arguments":{{"question":"character controller"}}}}}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"show","arguments":{{"symbol":"collide"}}}}}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"query","arguments":{{"symbol":"PlayerCharacterV2","limit":5}}}}}}"#).unwrap();
        // `explain` payloads exceed MCP's default 8000-byte budget; lift it
        // so the parity check compares against the unbudgeted CLI output.
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{{"name":"ask","arguments":{{"question":"character controller","explain":true,"budget_bytes":0}}}}}}"#).unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let responses: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    for tool in responses[0]["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{tool}");
        assert!(tool.get("outputSchema").is_none(), "{tool}");
    }
    let ask_tool = responses[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "ask")
        .unwrap();
    assert_eq!(
        ask_tool["inputSchema"]["properties"]["explain"]["type"],
        "boolean"
    );
    assert_eq!(
        ask_tool["inputSchema"]["properties"]["explain"]["default"],
        false
    );
    let structured = &responses[1]["result"]["structuredContent"];
    assert_eq!(structured["protocol"], "sinter.agent.v1");
    assert_eq!(structured["operation"], "ask");
    assert_eq!(structured["data"], cli);
    assert!(
        structured["data"]["topics"][0]["hits"][0]
            .get("score_breakdown")
            .is_none()
    );
    assert!(
        structured["data"]["topics"][0]["hits"][0]
            .get("calibration")
            .is_none()
    );

    // Ambiguity is a tool outcome (`isError`), not a JSON-RPC error, and
    // its candidates are the `Name@file` selectors an agent pastes back;
    // the CLI keeps full node objects for the same error.
    assert!(responses[2].get("error").is_none(), "{}", responses[2]);
    assert_eq!(responses[2]["result"]["isError"], true);
    let ambiguity = &responses[2]["result"]["structuredContent"];
    assert_eq!(ambiguity["protocol"], "sinter.agent.v1");
    assert_eq!(ambiguity["error"]["code"], "ambiguous_symbol");
    assert_eq!(
        ambiguity["error"]["message"],
        ambiguous_cli["error"]["message"]
    );
    let candidates = ambiguity["error"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(
        candidates
            .iter()
            .all(|c| c.as_str().is_some_and(|c| c.contains('@'))),
        "{ambiguity}"
    );

    let query = &responses[3]["result"]["structuredContent"];
    assert_eq!(query["protocol"], "sinter.agent.v1");
    assert_eq!(query["operation"], "query");
    assert_eq!(query["outcome"]["status"], "complete");
    assert_eq!(query["data"], cli_query);
    assert_eq!(query["data"]["scope"], cli_query["scope"]);
    assert_eq!(query["data"]["snapshot"], cli_query["snapshot"]);
    for transport_field in ["protocol", "operation", "outcome"] {
        assert!(query["data"].get(transport_field).is_none());
    }

    let explained = &responses[4]["result"]["structuredContent"];
    assert_eq!(explained["protocol"], "sinter.agent.v1");
    assert_eq!(explained["operation"], "ask");
    assert_eq!(explained["data"], cli_explained);
    assert!(explained["data"]["topics"][0]["hits"][0]["score_breakdown"].is_object());
    assert!(
        explained["data"]["topics"][0]["hits"][0]
            .get("calibration")
            .is_none()
    );
}

/// Skaffold-trial finding #2: embedded third-party source must not outrank
/// project code. A vendored class matching every term still loses to the
/// project's own documented symbol.
#[test]
fn ask_dampens_vendored_paths() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    controller_fixture(repo);
    std::fs::create_dir_all(repo.join("vendor/thirdlib")).unwrap();
    std::fs::write(
        repo.join("vendor/thirdlib/character.ts"),
        r#"// Vendored character controller helper for character controller demos.
export class CharacterControllerShim {
  control(): void {}
}
"#,
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["ask", "where is the character controller?"]);
    assert!(ok, "{out}");
    let first = out.lines().find(|l| l.starts_with("1. ")).unwrap();
    assert!(
        first.contains("PlayerCharacterV2"),
        "vendored shim outranked project code:\n{out}"
    );
}

/// Analysis products are outside the semantic corpus. A generated memory
/// graph can repeat the query terms thousands of times; it must never
/// become an `ask` candidate or affect source navigation.
#[test]
fn ask_excludes_derived_analysis_corpora() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("graphify-out")).unwrap();
    std::fs::create_dir_all(repo.join("memory")).unwrap();
    std::fs::write(
        repo.join("src/harness.rs"),
        "/// Runtime policy harness.\npub struct CedarPolicyHarness;\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("graphify-out/harness.md"),
        "# Runtime Policy Harness\n\nGenerated analysis memory.\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("memory/harness.md"),
        "# Runtime Policy Harness\n\nDerived agent memory.\n",
    )
    .unwrap();

    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["ask", "runtime policy harness", "--limit", "20"]);
    assert!(ok, "{out}");
    assert!(out.contains("CedarPolicyHarness"), "{out}");
    assert!(!out.contains("graphify-out"), "{out}");
    assert!(!out.contains("memory/harness.md"), "{out}");
}

/// Skaffold-trial finding #3: weak verbs ("work") are soft stopwords —
/// dropped when real terms remain, so they cannot inflate term coverage on
/// unrelated symbols.
#[test]
fn ask_drops_weak_verbs_when_real_terms_remain() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/cache.ts"),
        r#"// Incremental cache invalidation for derived state.
export class CacheInvalidator {
  invalidate(): void {}
}
"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("src/workspace.ts"),
        r#"// Workspace working-set utilities for workers at work.
export class WorkspaceWorker {
  work(): void {}
}
"#,
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["ask", "how does the cache invalidation work?"]);
    assert!(ok, "{out}");
    // "work" must be dropped: terms are cache + invalidation only.
    assert!(out.contains("2 terms"), "{out}");
    let first = out.lines().find(|l| l.starts_with("1. ")).unwrap();
    assert!(first.contains("CacheInvalidator"), "{out}");
    // A question that is ONLY weak verbs keeps them (soft, not hard).
    let (ok, out) = sinter(repo, &["ask", "work"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("WorkspaceWorker") || out.contains("work"),
        "{out}"
    );
}

/// Black Lantern finding: trigram closeness to ONE term must not grant
/// name credit for every term — a symbol close to "controller" but with no
/// "character" anywhere cannot claim 2/2 coverage.
#[test]
fn ask_trigram_credit_is_per_term() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    controller_fixture(repo);
    std::fs::write(
        repo.join("player/gym.ts"),
        r#"// Gym control action registry.
export class GymControlAction {
  act(): void {}
}
"#,
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(
        repo,
        &["ask", "where is the character controller?", "--limit", "10"],
    );
    assert!(ok, "{out}");
    let first = out.lines().find(|l| l.starts_with("1. ")).unwrap();
    assert!(first.contains("PlayerCharacterV2"), "{out}");
    for line in out.lines() {
        let ranked = line.get(..3).is_some_and(|p| p.ends_with(". "));
        if ranked && line.contains("GymControlAction") {
            assert!(
                line.contains("1/2 terms"),
                "trigram closeness credited across terms:\n{out}"
            );
        }
    }
}

/// Design §6: on every basic golden fixture, asking for the fixture's
/// primary symbol must rank its defining node first.
#[test]
fn ask_finds_primaries_across_golden_fixtures() {
    let fixtures = [
        ("rust-basic", "compute", "function compute"),
        ("go-basic", "Greet", "function Greet"),
        ("python-basic", "greet", "greet"),
        ("typescript-basic", "greet", "greet"),
        ("bash-basic", "greet", "function greet"),
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../harness/golden/fixtures");
    for (fixture, symbol, expect) in fixtures {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        copy_tree(&root.join(fixture), repo);
        let (ok, out) = sinter(repo, &["build"]);
        assert!(ok, "{fixture}: {out}");
        let (ok, out) = sinter(repo, &["ask", symbol]);
        assert!(ok, "{fixture}: {out}");
        let first = out
            .lines()
            .find(|l| l.starts_with("1. "))
            .unwrap_or_else(|| panic!("{fixture}: no hits\n{out}"));
        assert!(first.contains(expect), "{fixture}: top hit wrong\n{out}");
    }
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.path().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else if entry.file_name() != "expected.json" {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

#[test]
fn show_card_and_ambiguity() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    controller_fixture(repo);
    sinter(repo, &["build"]);

    let (ok, out) = sinter(repo, &["show", "PlayerCharacterV2"]);
    assert!(ok, "{out}");
    assert!(out.contains("class PlayerCharacterV2"), "{out}");
    assert!(out.contains("contains (1)"), "{out}");
    // `sinter unresolved` owns that question; the card no longer pays for it.
    assert!(!out.contains("unresolved refs in this file:"), "{out}");
    assert!(out.contains("Next: sinter affected"), "{out}");
    // No excerpt without --body.
    assert!(!out.contains("  | "), "{out}");

    // --body: bounded excerpt of the definition, fenced with `  | `.
    let (ok, body) = sinter(repo, &["show", "PlayerCharacterV2", "--body"]);
    assert!(ok, "{body}");
    let excerpt: Vec<&str> = body.lines().filter(|l| l.starts_with("  | ")).collect();
    assert!(!excerpt.is_empty(), "no excerpt in --body card\n{body}");
    assert!(excerpt.len() <= 10, "excerpt unbounded\n{body}");
    assert!(
        excerpt[0].contains("class PlayerCharacterV2"),
        "excerpt is not the definition source\n{body}"
    );
    // --context-lines caps it further.
    let (ok, two) = sinter(
        repo,
        &[
            "show",
            "PlayerCharacterV2",
            "--body",
            "--context-lines",
            "2",
        ],
    );
    assert!(ok, "{two}");
    let shown: Vec<&str> = two
        .lines()
        .filter(|l| l.starts_with("  | ") && !l.starts_with("  | …"))
        .collect();
    assert_eq!(shown.len(), 2, "{two}");
    // The cut is announced, with the flag that shows everything.
    assert!(
        two.contains("  | … 4 more lines (--context-lines 0 for all)"),
        "{two}"
    );
    // `0` is the whole span, reported as uncut.
    let (ok, whole) = sinter(
        repo,
        &[
            "show",
            "PlayerCharacterV2",
            "--json",
            "--body",
            "--context-lines",
            "0",
        ],
    );
    assert!(ok, "{whole}");
    let v: serde_json::Value = serde_json::from_str(&whole).unwrap();
    assert_eq!(v["excerpt_truncated"], false, "{whole}");
    assert_eq!(v["excerpt"].as_str().unwrap().lines().count(), 6, "{whole}");

    // JSON: `excerpt` only with --body.
    let (ok, plain) = sinter(repo, &["show", "PlayerCharacterV2", "--json"]);
    assert!(ok, "{plain}");
    assert!(!plain.contains("\"excerpt\""), "{plain}");
    let (ok, with_body) = sinter(repo, &["show", "PlayerCharacterV2", "--json", "--body"]);
    assert!(ok, "{with_body}");
    assert!(with_body.contains("\"excerpt\""), "{with_body}");
    let v: serde_json::Value = serde_json::from_str(&with_body).unwrap();
    assert_eq!(v["excerpt_truncated"], false, "{with_body}");
    assert_eq!(v["excerpt_total_lines"], 6, "{with_body}");
    let (ok, cut) = sinter(
        repo,
        &[
            "show",
            "PlayerCharacterV2",
            "--json",
            "--body",
            "--context-lines",
            "2",
        ],
    );
    assert!(ok, "{cut}");
    let v: serde_json::Value = serde_json::from_str(&cut).unwrap();
    assert_eq!(v["excerpt_truncated"], true, "{cut}");
    assert_eq!(v["excerpt_total_lines"], 6, "{cut}");

    // File-node card.
    let (ok, out) = sinter(repo, &["show", "player/traversal/climb.ts"]);
    assert!(ok, "{out}");
    assert!(out.contains("file player/traversal/climb.ts"), "{out}");
    assert!(out.contains("contains"), "{out}");

    // Ambiguous: `character` names a function in two files -> list, no guess.
    let (ok, out) = sinter(repo, &["show", "begin"]);
    assert!(ok, "{out}"); // unique method — fine
    let (ok, out) = sinter(repo, &["show", "nonexistent_thing_xyz"]);
    assert!(!ok, "{out}");
}

/// Family boost: a class whose doc lacks one term still outranks its own
/// methods when several of them match both terms — the class is the
/// concept the hits share (design §1c, Black Lantern case).
#[test]
fn ask_family_boost_surfaces_parent() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/pawn.ts"),
        r#"// The player character pawn: locomotion and traversal.
export class PlayerPawn {
  // Refreshes the character controller control mode.
  refreshControlMode(): void {}
  // Reads the character controller mode.
  getControlMode(): void {}
  // Applies character controller input routing.
  routeControllerInput(): void {}
}
"#,
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["ask", "where is the character controller?"]);
    assert!(ok, "{out}");
    let first = out.lines().find(|l| l.starts_with("1. ")).unwrap();
    assert!(
        first.contains("class PlayerPawn"),
        "parent class not surfaced above its matching members:\n{out}"
    );
    assert!(first.contains("family"), "family channel missing:\n{out}");
}

/// Family boost across header/impl: out-of-class definitions name their
#[test]
fn ask_test_question_reaches_test_scope_and_code_outranks_prose() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    std::fs::write(
        repo.join("src/impact.rs"),
        "/// Selects affected tests for a change.\npub fn affected_tests() {}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("tests/impact_surface.rs"),
        "#[test]\nfn impact_selects_affected_tests() {}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("README.md"),
        "# Impact\n\nImpact selects affected tests. Impact selects affected tests for every change, and affected tests run first.\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(
        repo,
        &["ask", "tests proving impact selects affected tests"],
    );
    assert!(ok, "{out}");
    let first = out.lines().find(|l| l.starts_with("1. ")).unwrap();
    assert!(
        first.contains("impact_selects_affected_tests"),
        "test symbol outside the default scope not surfaced:\n{out}"
    );
    assert!(!out.contains("abstain"), "{out}");
    let (ok, out) = sinter(repo, &["ask", "fn that selects affected tests", "--json"]);
    assert!(ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let hits = v["topics"][0]["hits"].as_array().unwrap();
    assert_ne!(hits[0]["kind"], "section", "prose outranks code:\n{out}");
    assert!(
        hits.iter()
            .all(|h| h["doc"].as_str().is_none_or(|d| d.chars().count() <= 201)),
        "{out}"
    );
}

/// class in their qualified prefix — that syntactic link counts as family
/// even though their structural parent is the impl file.
#[test]
fn ask_family_boost_crosses_header_impl() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("pawn")).unwrap();
    std::fs::write(
        repo.join("pawn/pawn.h"),
        r#"// The player character pawn.
class GAME_API APawn2
{
public:
    void RefreshControlMode();
    void RouteControllerInput();
};
"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("pawn/pawn.cpp"),
        r#"#include "pawn/pawn.h"

// Refreshes the character controller mode.
void APawn2::RefreshControlMode() {}

// Routes character controller input.
void APawn2::RouteControllerInput() {}
"#,
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["ask", "where is the character controller?"]);
    assert!(ok, "{out}");
    let first = out.lines().find(|l| l.starts_with("1. ")).unwrap();
    assert!(
        first.contains("class APawn2"),
        "class not surfaced above impl-side members:\n{out}"
    );
}

/// `sinter install` writes the embedded skill card; rerunning refreshes it.
#[test]
fn install_writes_skill_card() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("skills/sinter");
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["install", "--dir"])
        .arg(&target)
        .output()
        .unwrap();
    assert!(out.status.success());
    let card = std::fs::read_to_string(target.join("SKILL.md")).unwrap();
    assert!(card.contains("name: sinter"), "frontmatter missing");
    for command in [
        "sinter map",
        "sinter ask",
        "sinter query",
        "sinter show",
        "sinter affected",
        "sinter deps",
        "sinter path",
        "sinter unresolved",
        "sinter impact",
        "sinter overlap",
        "sinter workspace",
        "sinter ensure",
        "sinter doctor",
        "sinter scip",
    ] {
        assert!(card.contains(command), "installed card lost {command}");
    }
    assert!(
        card.find("sinter map") < card.find("sinter ask"),
        "map must be the first unfamiliar-repo route"
    );
    for contract in ["--explain", "--limit 0", "not_proven"] {
        assert!(card.contains(contract), "installed card lost {contract}");
    }
    assert!(
        card.contains("each\nclass at most once per session")
            && card.contains("Calls without a session ID remain"),
        "hook session behavior missing from installed card: {card}"
    );
}

/// `sinter doctor`: no graph -> exit 1 naming the fix; after build -> exit 0
/// (skill-card check is environment-dependent, so only repo checks assert).
#[test]
fn doctor_diagnoses_and_clears() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();

    let (ok, out) = sinter(repo, &["doctor"]);
    assert!(!ok, "{out}");
    assert!(out.contains("run `sinter build`"), "{out}");

    sinter(repo, &["build"]);
    let (_, out) = sinter(repo, &["doctor"]);
    assert!(out.contains("graph fresh"), "{out}");
    assert!(out.contains("graph schema"), "{out}");

    std::fs::write(repo.join("a.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();
    let (ok, out) = sinter(repo, &["doctor"]);
    assert!(!ok, "{out}");
    assert!(out.contains("graph stale: 1 changed"), "{out}");
}

/// `sinter install --mcp` merges into every project config without
/// clobbering other servers. The generated command is checkout-portable and
/// initial registration cannot make Codex startup depend on the server.
#[test]
fn install_mcp_merges_project_config() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(
        repo.join(".mcp.json"),
        r#"{"mcpServers": {"other": {"command": "other-tool"}}}"#,
    )
    .unwrap();
    let skills = dir.path().join("skills");
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["install", "--dir"])
        .arg(&skills)
        .args(["--mcp", "--repo"])
        .arg(repo)
        .output()
        .unwrap();
    assert!(out.status.success());
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(repo.join(".mcp.json")).unwrap()).unwrap();
    assert!(
        cfg["mcpServers"]["other"].is_object(),
        "clobbered existing server"
    );
    let command = cfg["mcpServers"]["sinter"]["command"]
        .as_str()
        .expect("sinter MCP command");
    assert_eq!(command, "sinter");

    let cursor: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(repo.join(".cursor/mcp.json")).unwrap())
            .unwrap();
    assert_eq!(cursor["mcpServers"]["sinter"]["command"], command);

    let codex: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".codex/config.toml")).unwrap()).unwrap();
    assert_eq!(
        codex["mcp_servers"]["sinter"]["command"].as_str(),
        Some(command)
    );
    assert_eq!(
        codex["mcp_servers"]["sinter"]["required"].as_bool(),
        Some(false)
    );

    let args = cfg["mcpServers"]["sinter"]["args"]
        .as_array()
        .expect("sinter MCP args")
        .iter()
        .map(|arg| arg.as_str().expect("string MCP arg"));
    let mut launched = Command::new(command)
        .args(args)
        .env(
            "PATH",
            Path::new(env!("CARGO_BIN_EXE_sinter"))
                .parent()
                .expect("sinter binary directory"),
        )
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("launch generated MCP command without PATH");
    writeln!(
        launched.stdin.as_mut().expect("piped stdin"),
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
    )
    .unwrap();
    drop(launched.stdin.take());
    let handshake = launched.wait_with_output().unwrap();
    assert!(handshake.status.success());
    assert!(
        String::from_utf8_lossy(&handshake.stdout).contains("\"result\""),
        "MCP initialize did not return a result"
    );

    sinter(repo, &["build"]);
    let (_, out) = sinter(repo, &["doctor"]);
    assert!(out.contains("MCP server registered"), "{out}");
}

/// Multi-assistant install: one embedded card body, thin per-target
/// writers. AGENTS.md merge preserves surrounding content and is
/// idempotent; cursor gets its own rule file.
#[test]
fn install_for_cursor_and_agents() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(
        repo.join("AGENTS.md"),
        "# Existing house rules\n\nKeep these.\n",
    )
    .unwrap();
    let skills = dir.path().join("skills");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_sinter"))
            .args(["install", "all", "--dir"])
            .arg(&skills)
            .args(["--repo"])
            .arg(repo)
            .env("HOME", dir.path())
            .env("USERPROFILE", dir.path())
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    assert!(run().status.success()); // idempotent

    let agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert!(
        agents.contains("Existing house rules"),
        "clobbered host content"
    );
    assert_eq!(
        agents.matches("BEGIN sinter").count(),
        1,
        "block duplicated"
    );
    assert!(agents.contains("sinter ask"), "routing missing");

    let rule = std::fs::read_to_string(repo.join(".cursor/rules/sinter.mdc")).unwrap();
    assert!(rule.contains("alwaysApply"), "cursor frontmatter missing");
    assert!(rule.contains("sinter ask"), "routing missing");
    // Two tiers, one source: skill and cursor rule share the full card
    // (loaded on demand); AGENTS.md gets the compact always-in-context
    // block. Both are embedded in the binary — no per-assistant forks —
    // and a unit test pins their command surfaces together.
    let skill = std::fs::read_to_string(skills.join("SKILL.md")).unwrap();
    let body_line = "never treat it as stale-proof";
    assert!(skill.contains(body_line) && rule.contains(body_line));
    assert!(!agents.contains(body_line), "AGENTS.md got the full card");
    for verb in [
        "sinter map",
        "sinter ask",
        "sinter query",
        "sinter show",
        "sinter affected",
        "sinter deps",
        "sinter path",
        "sinter unresolved",
        "sinter impact",
        "sinter overlap",
        "sinter workspace",
        "sinter ensure",
        "sinter doctor",
        "sinter scip",
    ] {
        assert!(
            agents.contains(verb) && skill.contains(verb),
            "{verb} missing"
        );
    }
    for text in [&agents, &skill] {
        assert!(text.find("sinter map") < text.find("sinter ask"), "{text}");
        for contract in ["--explain", "--limit 0", "not_proven"] {
            assert!(text.contains(contract), "{contract} missing: {text}");
        }
    }
}

/// Git hooks install appends to an existing hook rather than clobbering
/// it, and rerunning is a no-op.
#[test]
fn hooks_install_preserves_existing_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo)
        .output()
        .unwrap();
    let hook = repo.join(".git/hooks/post-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\necho user-hook\n").unwrap();

    let (ok, out) = sinter(repo, &["hooks", "install"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["hooks", "install"]); // idempotent
    assert!(ok, "{out}");
    assert!(out.contains("already installed"), "{out}");

    let content = std::fs::read_to_string(&hook).unwrap();
    assert!(
        content.contains("echo user-hook"),
        "clobbered user hook:\n{content}"
    );
    assert_eq!(
        content.matches("sinter build").count(),
        1,
        "duplicated:\n{content}"
    );
}

/// `sinter version` reports version, schema, and language packs; the
/// number matches the clap --version flag (one source of truth).
#[test]
fn version_subcommand_matches_flag() {
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let sub = run(&["version"]);
    let flag = run(&["--version"]);
    assert!(sub.contains(env!("CARGO_PKG_VERSION")), "{sub}");
    assert!(flag.contains(env!("CARGO_PKG_VERSION")), "{flag}");
    assert!(sub.contains("graph schema v"), "{sub}");
    assert!(sub.contains("rust"), "{sub}");
}

/// `sinter init` onboards a repo end to end: graph built, hooks installed,
/// AGENTS.md block + MCP registered, doctor clean — and every write lands
/// inside the repo. HOME points at a temp dir so a leak outside the
/// project is caught rather than silently landing in the real user
/// environment.
#[test]
fn init_onboards_repo() {
    let home = tempfile::tempdir().unwrap();
    // Reproduce the real failure: an existing graph at ~/.sinter must not
    // capture a first-time init of a nested Git repository.
    std::fs::create_dir_all(home.path().join(".sinter")).unwrap();
    let repo = home.path().join("work/brg-runtime-gateway");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .output()
        .unwrap();

    let init = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
            .args(args)
            .current_dir(&repo)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("PATH", path_with_sinter())
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
    };
    let (ok, out) = init(&["init"]);
    assert!(ok, "{out}");
    // Project first: a plain init must not reach into the home directory.
    assert!(
        !home.path().join(".claude/skills/sinter/SKILL.md").exists(),
        "init wrote the machine-wide skill card without --global: {out}"
    );
    // Every write is disclosed before it happens.
    assert!(out.contains("sinter init —"), "{out}");
    assert!(out.contains("this repo"), "{out}");
    assert!(out.contains("pass --global"), "{out}");
    assert!(out.contains("symbols,"), "{out}");
    assert!(repo.join(".sinter/graph.redb").exists(), "{out}");
    assert!(
        !home.path().join(".sinter/graph.redb").exists(),
        "init built the ancestor home graph: {out}"
    );
    assert!(
        std::fs::read_to_string(repo.join(".git/hooks/post-commit"))
            .unwrap()
            .contains("sinter build"),
        "{out}"
    );
    assert!(
        std::fs::read_to_string(repo.join("AGENTS.md"))
            .unwrap()
            .contains("BEGIN sinter"),
        "{out}"
    );
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(repo.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["sinter"]["command"], "sinter");
    assert!(out.contains("== doctor =="), "{out}");
    // A project-scoped install is a complete install: the absent global
    // skill card must not be reported as a problem to fix.
    assert!(out.contains("0 graph problem(s)"), "{out}");
    // Idempotent: second init changes nothing and still succeeds.
    let (_, again) = init(&["init"]);
    let agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert_eq!(agents.matches("BEGIN sinter").count(), 1, "{agents}");
    assert!(again.contains("== doctor =="), "{again}");

    // --global is the opt-in that reaches the machine.
    let (ok, out) = init(&["init", "--global"]);
    assert!(ok, "{out}");
    assert!(
        home.path().join(".claude/skills/sinter/SKILL.md").exists(),
        "{out}"
    );
    assert!(
        std::fs::read_to_string(home.path().join(".claude/settings.json"))
            .unwrap()
            .contains("sinter-first."),
        "{out}"
    );
}

/// Agent-safe setup creates only derived graph state. It must not acquire the
/// broader authority that full `init` intentionally exercises.
#[test]
fn ensure_builds_graph_without_installing_integrations() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo)
        .output()
        .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["ensure"])
        .current_dir(repo)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()
        .expect("run sinter ensure");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(repo.join(".sinter/graph.redb").exists());
    for absent in [
        "AGENTS.md",
        "CLAUDE.md",
        ".mcp.json",
        ".cursor",
        ".codex",
        ".claude",
        ".git/hooks/post-commit",
    ] {
        assert!(
            !repo.join(absent).exists(),
            "ensure must not create {absent}"
        );
    }
    assert!(
        !home.path().join(".claude/skills/sinter/SKILL.md").exists(),
        "ensure must not modify global agent configuration"
    );
}

/// init must not execute repository-selected indexer binaries without
/// consent: non-interactive default skips them; --scip runs them.
#[cfg(unix)]
#[test]
fn init_runs_indexers_only_with_consent() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    // Fake rust-analyzer first on PATH: records execution, produces nothing.
    let marker = bin.path().join("executed");
    std::fs::write(
        bin.path().join("rust-analyzer"),
        format!("#!/bin/sh\ntouch {}\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        bin.path().join("rust-analyzer"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let sinter_path = path_with_sinter();
    let path_env = std::env::join_paths(
        std::iter::once(bin.path().to_path_buf()).chain(std::env::split_paths(&sinter_path)),
    )
    .unwrap();

    let init = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_sinter"))
            .args(args)
            .current_dir(repo)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("PATH", &path_env)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run sinter")
    };

    let out = init(&["init"]);
    assert!(out.status.success());
    assert!(
        !marker.exists(),
        "non-interactive init executed an indexer without consent"
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("not run — pass --scip"), "{text}");

    init(&["init", "--scip"]);
    assert!(marker.exists(), "--scip must run the indexer");
}

/// Enforcement hook file this platform installs (bash on unix,
/// PowerShell on Windows) — mirrors `install::PLATFORM_HOOK`.
const HOOK_FILE: &str = if cfg!(windows) {
    "sinter-first.ps1"
} else {
    "sinter-first.sh"
};
/// The variant the platform must NOT install.
const OTHER_HOOK_FILE: &str = if cfg!(windows) {
    "sinter-first.sh"
} else {
    "sinter-first.ps1"
};

/// `install enforce` writes the hook script and merges the three
/// settings entries idempotently, preserving unrelated settings and hooks.
#[test]
fn install_enforce_is_idempotent_and_preserving() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".git")).unwrap();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::write(
        home.path().join(".claude/settings.json"),
        r#"{"model":"opus","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"other-tool hook"}]}]}}"#,
    )
    .unwrap();

    let run = || {
        let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
            .args(["install", "enforce", "--global"])
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .output()
            .expect("run sinter");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run();
    run();

    assert!(home.path().join(".claude/hooks").join(HOOK_FILE).exists());
    assert!(
        !home
            .path()
            .join(".claude/hooks")
            .join(OTHER_HOOK_FILE)
            .exists(),
        "only the platform's hook variant is installed"
    );
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap(),
    )
    .expect("settings stay valid JSON");
    assert_eq!(settings["model"], "opus", "unrelated settings preserved");
    let text = settings.to_string();
    assert!(text.contains("other-tool hook"), "existing hooks preserved");
    assert!(text.contains(HOOK_FILE), "{text}");
    // Trailing quote anchors each mode ("grep" is a prefix of "greptool").
    for marker in [" prompt\"", " grep\"", " greptool\""] {
        assert_eq!(
            text.matches(marker).count(),
            1,
            "{marker} must appear exactly once after two installs"
        );
    }
    assert!(
        !text.contains("permissionDecision"),
        "enforcement must never carry a permission decision"
    );

    // Doctor accepts the platform's variant and never demands the other.
    let doctor = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["doctor"])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()
        .expect("run doctor");
    let out = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        out.contains("enforcement hooks installed and current"),
        "doctor must accept the {HOOK_FILE} enforcement install: {out}"
    );
}

/// Default enforce scope is the repo: script and settings land under
/// <repo>/.claude with a relative command (committable, teammate-portable).
#[test]
fn install_enforce_defaults_to_repo_scope() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["install", "enforce"])
        .current_dir(repo.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()
        .expect("run sinter");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(repo.path().join(".claude/hooks").join(HOOK_FILE).exists());
    let settings = std::fs::read_to_string(repo.path().join(".claude/settings.json")).unwrap();
    let relative_cmd = if cfg!(windows) {
        "& '.claude/hooks/sinter-first.ps1' prompt".to_string()
    } else {
        "bash .claude/hooks/sinter-first.sh prompt".to_string()
    };
    assert!(
        settings.contains(&relative_cmd),
        "repo scope must use a relative command: {settings}"
    );
    assert!(
        !home.path().join(".claude/hooks").join(HOOK_FILE).exists(),
        "repo scope must not touch the global home"
    );
}

/// init then uninit round-trips: every managed artifact is gone, and
/// pre-existing user content (AGENTS.md prose, foreign hooks, other MCP
/// servers) survives untouched.
#[test]
fn uninit_reverses_init_and_preserves_user_content() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(repo.join("AGENTS.md"), "# My rules\n\nKeep me.\n").unwrap();
    std::fs::write(
        repo.join(".mcp.json"),
        r#"{"mcpServers":{"other":{"command":"other"}}}"#,
    )
    .unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo)
        .output()
        .unwrap();

    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
            .args(args)
            .current_dir(repo)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("PATH", path_with_sinter())
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run sinter");
        assert!(
            out.status.success(),
            "sinter {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init"]);
    assert!(repo.join(".sinter/graph.redb").exists());
    assert!(repo.join(".claude/hooks").join(HOOK_FILE).exists());
    run(&["uninit"]);

    for gone in [
        ".sinter",
        ".claude/hooks/sinter-first.sh",
        ".claude/hooks/sinter-first.ps1",
        ".claude/settings.json",
        ".codex/config.toml",
        ".git/hooks/post-commit",
    ] {
        assert!(!repo.join(gone).exists(), "{gone} should be removed");
    }
    let agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert!(agents.contains("Keep me."), "{agents}");
    assert!(!agents.contains("sinter"), "{agents}");
    let mcp = std::fs::read_to_string(repo.join(".mcp.json")).unwrap();
    assert!(mcp.contains("other"), "{mcp}");
    assert!(!mcp.contains("sinter"), "{mcp}");
}

/// Codex field report: verbose multi-topic questions ("what documentation
/// describes X, Y, or comparisons to Z") diluted term coverage and let a
/// filler-word name hit rank #1. Scaffolding terms are soft-dropped and a
/// weak top hit is called out instead of passing as an answer.
#[test]
fn ask_verbose_question_drops_scaffolding_and_flags_weak_match() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    controller_fixture(repo);
    std::fs::write(
        repo.join("player/describe.ts"),
        "// Describe helper.\nexport function describeThing(): number {\n  return 1;\n}\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (ok, out) = sinter(
        repo,
        &[
            "ask",
            "What documentation describes dashboard usability, operator experience, or comparisons to k9s?",
        ],
    );
    // No topic matched: grep-style exit 1, output still printed.
    assert!(!ok, "{out}");
    // Scaffolding terms are gone from the term list...
    let header = out.lines().next().unwrap_or_default();
    for filler in ["documentation", "describes", "comparisons"] {
        assert!(!header.contains(filler), "{filler} survived: {out}");
    }
    // ...so the filler-named function cannot ride them to the top, and a
    // barely-covering top hit is flagged rather than presented as an answer.
    if out.contains("1. ") {
        assert!(out.contains("weak match"), "{out}");
    }
}

/// Multi-topic questions split into clauses (on `,`/`;`/" or ") and each
/// clause is answered under its own heading; single-topic output is
/// untouched, and multi-clause output is deterministic.
#[test]
fn ask_multi_topic_groups_hits_per_clause() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    controller_fixture(repo);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let question = "where is the character controller, or the ledge grab?";
    let (ok, out) = sinter(repo, &["ask", question]);
    assert!(ok, "{out}");
    assert!(out.contains("Best matches (2 topics):"), "{out}");
    let controller_at = out.find("## character controller").expect(&out);
    let ledge_at = out.find("## ledge grab").expect(&out);
    assert!(
        controller_at < ledge_at,
        "clause order not preserved:\n{out}"
    );
    // Each topic's hit lands in its own section.
    let controller_section = &out[controller_at..ledge_at];
    let ledge_section = &out[ledge_at..];
    assert!(controller_section.contains("PlayerCharacterV2"), "{out}");
    assert!(ledge_section.contains("LedgeComponentV2"), "{out}");
    // Per-clause coverage, not diluted whole-question coverage.
    assert!(controller_section.contains("2/2 terms"), "{out}");

    // Determinism: byte-identical across runs.
    let (_, again) = sinter(repo, &["ask", question]);
    assert_eq!(out, again, "multi-clause output not deterministic");

    // Single-topic output has no clause scaffolding.
    let (ok, single) = sinter(repo, &["ask", "where is the character controller?"]);
    assert!(ok, "{single}");
    assert!(!single.contains("## "), "{single}");
    assert!(single.contains("Best matches (2 terms"), "{single}");

    // --json multi-clause: explicit per-topic results with a strict global
    // budget and an independently actionable safety decision.
    let (ok, js) = sinter(repo, &["ask", question, "--json", "--limit", "3"]);
    assert!(ok, "{js}");
    let parsed: serde_json::Value = serde_json::from_str(&js).expect("valid json");
    let topics = parsed["topics"].as_array().expect("topics");
    assert_eq!(topics.len(), 2, "{js}");
    assert!(parsed["returned"].as_u64().unwrap() <= 3, "{js}");
    assert!(
        topics
            .iter()
            .all(|topic| topic["confidence"]["level"].is_string()),
        "{js}"
    );
    assert!(
        topics
            .iter()
            .all(|topic| topic["verify_required"].is_boolean()),
        "{js}"
    );
    assert!(
        topics
            .iter()
            .any(|topic| topic["topic"] == "character controller"
                && topic["hits"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|hit| hit["name"] == "PlayerCharacterV2")),
        "{js}"
    );
    assert!(
        topics.iter().any(|topic| topic["topic"] == "ledge grab"),
        "{js}"
    );
}

/// The Codex field-report question: five topics in one sentence. Each
/// clause gets its own heading, scaffolding words (documentation,
/// describe, comparisons) never become topic labels, and topics the graph
/// cannot answer say "no match" instead of surfacing filler hits.
#[test]
fn ask_codex_shaped_question_splits_sanely() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    controller_fixture(repo);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");

    let (ok, out) = sinter(
        repo,
        &[
            "ask",
            "What product decisions or documentation describe dashboard usability, \
             operator experience, terminal layout, keyboard controls, or comparisons to k9s?",
        ],
    );
    // No topic matched: grep-style exit 1, output still printed.
    assert!(!ok, "{out}");
    assert!(out.contains("topics):"), "did not split: {out}");
    // Soft scaffolding stripped from clause labels.
    assert!(out.contains("## dashboard usability"), "{out}");
    assert!(out.contains("## keyboard controls"), "{out}");
    for filler in ["## documentation", "describe", "comparisons"] {
        assert!(!out.contains(filler), "{filler} leaked into topics: {out}");
    }
    // The controller fixture has none of these topics: honesty required.
    assert!(out.contains("no match"), "{out}");
}

/// Strict-mode hook behavior, pipe-tested against the real bash script:
/// first matching search of a session is denied with a sinter redirect,
/// the retry gets one advisory nudge, later searches are silent, a new
/// session is denied again, and a missing session_id never denies or
/// silently loses the nudge.
#[cfg(unix)]
#[test]
fn strict_hook_denies_first_search_then_nudges() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/skill/sinter-first.sh");
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join(".sinter")).unwrap();
    std::fs::write(repo.path().join(".sinter/graph.redb"), "").unwrap();
    let tmp = tempfile::tempdir().unwrap(); // marker files live here

    let run = |mode: &str, stdin: &str| -> String {
        use std::io::Write;
        let mut child = Command::new("bash")
            .args([script, mode])
            .current_dir(repo.path())
            .env("TMPDIR", tmp.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn hook");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().expect("run hook");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let input = |sid: Option<&str>| match sid {
        Some(sid) => format!(r#"{{"session_id":"{sid}","tool_input":{{"command":"rg foo"}}}}"#),
        None => r#"{"tool_input":{"command":"rg foo"}}"#.to_string(),
    };

    // First search of session X: a valid deny, and never an allow.
    let first = run("grep-strict", &input(Some("sess-x")));
    let json: serde_json::Value = serde_json::from_str(&first).expect("deny must be valid JSON");
    assert_eq!(
        json["hookSpecificOutput"]["permissionDecision"], "deny",
        "{first}"
    );
    // "allowed" may appear in prose; the JSON value "allow" never may.
    assert!(!first.contains("\"allow\""), "{first}");
    assert!(
        first.contains("sinter ask")
            && first.contains("affected")
            && first.contains("path")
            && first.contains("impact"),
        "deny reason must name the sinter commands: {first}"
    );

    // Retry in the same session: nudge, no deny.
    let second = run("grep-strict", &input(Some("sess-x")));
    assert!(second.contains("additionalContext"), "{second}");
    assert!(!second.contains("deny"), "{second}");

    // Once both strict denial and the search nudge have been seen, later
    // searches in the same session stay out of the agent's context.
    let third = run("grep-strict", &input(Some("sess-x")));
    assert!(third.is_empty(), "{third}");

    // A different session gets its own first-search deny.
    let other = run("grep-strict", &input(Some("sess-y")));
    assert!(other.contains("\"permissionDecision\":\"deny\""), "{other}");

    // No session_id to scope a marker: never deny, nudge only.
    let anon = run("grep-strict", &input(None));
    assert!(anon.contains("additionalContext"), "{anon}");
    assert!(!anon.contains("deny"), "{anon}");
    let anon_again = run("grep-strict", &input(None));
    assert!(anon_again.contains("additionalContext"), "{anon_again}");
    assert!(!anon_again.contains("deny"), "{anon_again}");

    // Git archaeology (`git log -S`) stays advisory even in strict mode.
    let git = run(
        "grep-strict",
        r#"{"session_id":"sess-git","tool_input":{"command":"git log -S open_store"}}"#,
    );
    assert!(git.contains("additionalContext"), "{git}");
    assert!(!git.contains("deny"), "{git}");
    let git_again = run(
        "grep-strict",
        r#"{"session_id":"sess-git","tool_input":{"command":"git log -G foo"}}"#,
    );
    assert!(git_again.is_empty(), "{git_again}");

    // greptool-strict follows the same lifecycle.
    let gt_first = run("greptool-strict", r#"{"session_id":"sess-gt"}"#);
    assert!(
        gt_first.contains("\"permissionDecision\":\"deny\""),
        "{gt_first}"
    );
    let gt_second = run("greptool-strict", r#"{"session_id":"sess-gt"}"#);
    assert!(gt_second.contains("additionalContext"), "{gt_second}");
    assert!(!gt_second.contains("deny"), "{gt_second}");
    let gt_third = run("greptool-strict", r#"{"session_id":"sess-gt"}"#);
    assert!(gt_third.is_empty(), "{gt_third}");
}

/// Advisory hooks deduplicate each class per session. Shell recursive search
/// and the Grep tool intentionally share one search class, while git
/// archaeology and the prompt router have independent lifecycles. Everyday
/// commands and subagent spawns never nudge.
#[cfg(unix)]
#[test]
fn advisory_hook_nudges_are_session_deduplicated() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/skill/sinter-first.sh");
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join(".sinter")).unwrap();
    std::fs::write(repo.path().join(".sinter/graph.redb"), "").unwrap();
    let tmp = tempfile::tempdir().unwrap();

    let run = |mode: &str, stdin: &str| -> String {
        use std::io::Write;
        let mut child = Command::new("bash")
            .args([script, mode])
            .current_dir(repo.path())
            .env("TMPDIR", tmp.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn hook");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().expect("run hook");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let shell_search = |sid: Option<&str>| match sid {
        Some(sid) => format!(r#"{{"session_id":"{sid}","tool_input":{{"command":"rg foo"}}}}"#),
        None => r#"{"tool_input":{"command":"rg foo"}}"#.to_string(),
    };

    let search = run("grep", &shell_search(Some("shared-search")));
    assert!(search.contains("additionalContext"), "{search}");
    let same_class = run("greptool", r#"{"session_id":"shared-search"}"#);
    assert!(same_class.is_empty(), "{same_class}");
    let other_search = run("greptool", r#"{"session_id":"other-search"}"#);
    assert!(other_search.contains("additionalContext"), "{other_search}");

    let git_input =
        r#"{"session_id":"git-session","tool_input":{"command":"git log -S open_store"}}"#;
    assert!(run("grep", git_input).contains("additionalContext"));
    assert!(run("grep", git_input).is_empty());
    let other_git = r#"{"session_id":"other-git","tool_input":{"command":"git log -G foo"}}"#;
    assert!(run("grep", other_git).contains("additionalContext"));

    // Quiet by default: everyday commands and subagent spawns never nudge.
    for cmd in [
        "git status",
        "git log --oneline",
        "git diff",
        "cargo check",
        "ls -la",
        "cat README.md",
        "sed -n 1,5p x",
    ] {
        let input = format!(r#"{{"session_id":"quiet","tool_input":{{"command":"{cmd}"}}}}"#);
        assert!(run("grep", &input).is_empty(), "{cmd} must be silent");
    }
    assert!(
        run(
            "task",
            r#"{"session_id":"task-session","tool_input":{"prompt":"find callers"}}"#
        )
        .is_empty()
    );
    for cmd in [
        "grep -rn open_store crates",
        "find . -name '*.rs'",
        "ag foo",
    ] {
        let input =
            format!(r#"{{"session_id":"search-{cmd}","tool_input":{{"command":"{cmd}"}}}}"#);
        assert!(
            run("grep", &input).contains("additionalContext"),
            "{cmd} must nudge"
        );
    }

    // The prompt router fires once per session.
    let prompt = r#"{"session_id":"prompt-session"}"#;
    assert!(run("prompt", prompt).contains("sinter graph"));
    assert!(run("prompt", prompt).is_empty());

    // Without a usable session ID, hooks cannot safely deduplicate. They keep
    // nudging rather than silently disappearing, but never deny.
    for mode in ["grep", "greptool"] {
        let input = if mode == "grep" {
            shell_search(None)
        } else {
            r#"{"tool_input":{}}"#.to_string()
        };
        for _ in 0..2 {
            let out = run(mode, &input);
            assert!(out.contains("additionalContext"), "{mode}: {out}");
            assert!(!out.contains("permissionDecision"), "{mode}: {out}");
        }
    }

    // Raw session IDs never become path components. The marker name contains
    // only the advisory class and a digest.
    let hostile =
        r#"{"session_id":"../../outside session","tool_input":{"command":"git log -S x"}}"#;
    assert!(run("grep", hostile).contains("additionalContext"));
    let marker_dir = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("sinter-hooks-"))
        })
        .expect("per-user hook marker directory");
    for marker in std::fs::read_dir(marker_dir).unwrap() {
        let name = marker.unwrap().file_name().to_string_lossy().into_owned();
        assert!(!name.contains("outside") && !name.contains(".."), "{name}");
        assert!(
            name.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-'),
            "{name}"
        );
    }
}

/// SECURITY INVARIANT regression guard: neither hook script may ever
/// contain the JSON for permissionDecision "allow" — that would
/// auto-approve an entire Bash command, destructive parts included.
#[test]
fn hook_scripts_share_compact_agent_routing() {
    let scripts = [
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/skill/sinter-first.sh"
        ))
        .unwrap(),
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/skill/sinter-first.ps1"
        ))
        .unwrap(),
    ];
    for body in scripts {
        for route in [
            "sinter context",
            "sinter ask",
            "query/show",
            "affected/deps/path",
            "assert no-callers",
            "unresolved",
            "not_proven",
            "grep --within",
            "impact",
        ] {
            assert!(body.contains(route), "hook lost `{route}`: {body}");
        }
        assert!(
            !body.contains("--explain") && !body.contains("--limit 0"),
            "durable-card diagnostics leaked into always-on hook copy"
        );
    }
}

#[test]
fn hook_scripts_never_emit_allow() {
    for script in ["/skill/sinter-first.sh", "/skill/sinter-first.ps1"] {
        let body =
            std::fs::read_to_string(format!("{}{script}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert!(
            !body.contains("permissionDecision\":\"allow"),
            "{script} must never emit an allow decision"
        );
    }
}

/// `install enforce --strict` writes the -strict grep entries; rerunning
/// without --strict switches the same slots back (no duplicates either
/// way), and doctor accepts both variants as current.
#[test]
fn install_enforce_strict_switches_slots() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".git")).unwrap();
    // Doctor exits nonzero here (no graph in the temp home) — only the
    // enforcement finding matters, so success is asserted per call site.
    let run = |args: &[&str], must_succeed: bool| {
        let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
            .args(args)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path())
            .output()
            .expect("run sinter");
        if must_succeed {
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let settings = || std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();

    run(&["install", "enforce", "--global", "--strict"], true);
    let text = settings();
    for marker in [" grep-strict\"", " greptool-strict\""] {
        assert_eq!(text.matches(marker).count(), 1, "{marker}: {text}");
    }
    assert!(!text.contains(" grep\""), "non-strict entry left: {text}");

    // Doctor accepts the strict variant as installed and current.
    let doctor = run(&["doctor"], false);
    assert!(
        doctor.contains("enforcement hooks installed and current"),
        "{doctor}"
    );

    // Rerunning without --strict replaces the same slots back.
    run(&["install", "enforce", "--global"], true);
    let text = settings();
    assert!(!text.contains("-strict"), "strict entry left: {text}");
    for marker in [" grep\"", " greptool\""] {
        assert_eq!(text.matches(marker).count(), 1, "{marker}: {text}");
    }
    let doctor = run(&["doctor"], false);
    assert!(
        doctor.contains("enforcement hooks installed and current"),
        "{doctor}"
    );
}

/// Domain-blind ranking: three common terms in a harness doc must not beat
/// the one symbol that carries the rare domain word (`cedar`). The harness
/// functions never mention it and pay the rarest-term penalty.
fn domain_fixture(repo: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let mut harness = String::new();
    for i in 0..6 {
        harness.push_str(&format!(
            "def run_harness_{i}(events):\n    \"\"\"Adjudicate trajectory events against the configured policies.\"\"\"\n    return events\n\n"
        ));
    }
    std::fs::write(repo.join("src/harness.py"), harness).unwrap();
    std::fs::write(
        repo.join("src/engine.py"),
        r#"class CedarPolicyEngine:
    """Cedar engine."""

    def check(self, request):
        """Decide a request against cedar policies."""
        return record_trajectory(request)


def record_trajectory(events):
    """Store trajectory events for later replay."""
    return events
"#,
    )
    .unwrap();
}

#[test]
fn ask_rare_domain_term_beats_common_terms() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    domain_fixture(repo);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(
        repo,
        &["ask", "adjudicate trajectory events against cedar policies"],
    );
    assert!(ok, "{out}");
    let first = out.lines().find(|l| l.starts_with("1. ")).unwrap();
    assert!(
        first.contains("CedarPolicyEngine::check"),
        "harness outranked the cedar engine:\n{out}"
    );
}

#[test]
fn ask_relational_question_names_the_connected_pair() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    domain_fixture(repo);
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(repo, &["ask", "cedar engine check, record trajectory"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("connects: CedarPolicyEngine::check -> record_trajectory"),
        "{out}"
    );
    let (ok, out) = sinter(
        repo,
        &["ask", "cedar engine check, record trajectory", "--json"],
    );
    assert!(ok, "{out}");
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        json["connects"][0],
        "CedarPolicyEngine::check -> record_trajectory"
    );
}
