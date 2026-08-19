//! Acceptance for `sinter ask` / `sinter show`.
//! The controller fixture models the reference failure case: component
//! classes, constructor-like functions named after the base concept, and a
//! documented controller class that must rank #1.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn sinter(repo: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .current_dir(repo)
        // Hermetic HOME: the developer's real ~/.claude must not leak
        // stale-artifact nudges into captured output.
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
fn ask_json_carries_span_and_score() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    controller_fixture(repo);
    sinter(repo, &["build"]);
    let (ok, out) = sinter(repo, &["ask", "character controller", "--json"]);
    assert!(ok, "{out}");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    let first = &parsed.as_array().expect("array")[0];
    assert_eq!(first["name"], "PlayerCharacterV2");
    assert!(first["span"]["end"].as_u64().unwrap() > 0);
    assert!(first["score"].as_i64().unwrap() > 0);
    assert!(first["matched"].as_array().unwrap().len() == 2);
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
    assert!(out.contains("unresolved refs in this file:"), "{out}");
    assert!(out.contains("Next: sinter affected"), "{out}");

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
    assert!(card.contains("sinter ask"), "routing missing");
    assert!(
        !card.to_lowercase().contains("retry"),
        "no orchestration in prose"
    );
}

/// `sinter doctor`: no graph -> exit 1 naming the fix; after build -> exit 0
/// (skill-card check is environment-dependent, so only repo checks assert).
#[test]
fn doctor_diagnoses_and_clears() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
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

/// `sinter install --mcp` merges into .mcp.json without clobbering other
/// servers; doctor reports the registration.
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
    assert_eq!(cfg["mcpServers"]["sinter"]["command"], "sinter");

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
        "sinter ask",
        "sinter affected",
        "sinter path",
        "sinter impact",
    ] {
        assert!(
            agents.contains(verb) && skill.contains(verb),
            "{verb} missing"
        );
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
/// AGENTS.md block + MCP registered, doctor clean. HOME points at a temp
/// dir so the global skill-card write never touches the real user
/// environment (hermetic under any sandbox).
#[test]
fn init_onboards_repo() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::write(repo.join("a.rs"), "pub fn f() {}\n").unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo)
        .output()
        .unwrap();

    let init = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
            .args(args)
            .current_dir(repo)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
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
    assert!(
        home.path().join(".claude/skills/sinter/SKILL.md").exists(),
        "{out}"
    );
    assert!(out.contains("== build =="), "{out}");
    assert!(repo.join(".sinter/graph.redb").exists(), "{out}");
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
    // Idempotent: second init changes nothing and still succeeds.
    let (_, again) = init(&["init"]);
    assert!(again.contains("0 changed"), "{again}");
    let agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert_eq!(agents.matches("BEGIN sinter").count(), 1, "{agents}");
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
    let path_env = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

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
    assert!(text.contains("skipped: non-interactive"), "{text}");

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

    // --json multi-clause: flat array, each hit tagged with its topic.
    let (ok, js) = sinter(repo, &["ask", question, "--json"]);
    assert!(ok, "{js}");
    let parsed: serde_json::Value = serde_json::from_str(&js).expect("valid json");
    let hits = parsed.as_array().expect("array");
    assert!(!hits.is_empty(), "{js}");
    assert!(hits.iter().all(|h| h["topic"].is_string()), "{js}");
    assert!(
        hits.iter()
            .any(|h| h["topic"] == "character controller" && h["name"] == "PlayerCharacterV2"),
        "{js}"
    );
    assert!(hits.iter().any(|h| h["topic"] == "ledge grab"), "{js}");
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
/// the retry (and every later search) gets the advisory nudge, a new
/// session is denied again, and a missing session_id never denies.
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

    // A different session gets its own first-search deny.
    let other = run("grep-strict", &input(Some("sess-y")));
    assert!(other.contains("\"permissionDecision\":\"deny\""), "{other}");

    // No session_id to scope a marker: never deny, nudge only.
    let anon = run("grep-strict", &input(None));
    assert!(anon.contains("additionalContext"), "{anon}");
    assert!(!anon.contains("deny"), "{anon}");

    // Git archaeology stays advisory even in strict mode.
    let git = run(
        "grep-strict",
        r#"{"session_id":"sess-git","tool_input":{"command":"git log --oneline"}}"#,
    );
    assert!(git.contains("additionalContext"), "{git}");
    assert!(!git.contains("deny"), "{git}");

    // greptool-strict follows the same lifecycle.
    let gt_first = run("greptool-strict", r#"{"session_id":"sess-gt"}"#);
    assert!(
        gt_first.contains("\"permissionDecision\":\"deny\""),
        "{gt_first}"
    );
    let gt_second = run("greptool-strict", r#"{"session_id":"sess-gt"}"#);
    assert!(gt_second.contains("additionalContext"), "{gt_second}");
    assert!(!gt_second.contains("deny"), "{gt_second}");
}

/// SECURITY INVARIANT regression guard: neither hook script may ever
/// contain the JSON for permissionDecision "allow" — that would
/// auto-approve an entire Bash command, destructive parts included.
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
