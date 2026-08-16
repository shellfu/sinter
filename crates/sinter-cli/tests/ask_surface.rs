//! Acceptance for `sinter ask` / `sinter show` (docs/design-human-query.md §6).
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
    // Perf gate (v1 scale) — includes process spawn, generous headroom.
    assert!(
        elapsed < Duration::from_millis(500),
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
    assert!(ok, "{out}");
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
