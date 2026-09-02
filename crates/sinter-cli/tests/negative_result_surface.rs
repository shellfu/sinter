//! What a zero-row answer must say. An audit that reads "0 dependents" as
//! "no callers" is reading a verdict sinter never issued: every empty
//! traversal is `not_proven`, and every surface that renders one has to
//! carry that word plus the command that settles it.

use std::process::Command;

fn sinter(repo: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
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

/// A repo with one symbol nothing calls and nothing it calls: the shape the
/// audit relied on ("no production callers for X").
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/main.rs"),
        "fn orphan_wipe() {}\nfn main() {}\n",
    )
    .unwrap();
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    dir
}

#[test]
fn an_empty_affected_names_the_command_that_settles_it() {
    let dir = fixture();
    let repo = dir.path();

    let (found, out) = sinter(repo, &["affected", "orphan_wipe"]);
    assert!(!found, "{out}");
    assert!(out.contains("not proven: 0 dependents"), "{out}");
    assert!(
        out.contains("verify: sinter unresolved --name orphan_wipe"),
        "{out}"
    );

    let (_, out) = sinter(repo, &["affected", "orphan_wipe", "--json"]);
    let value: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(value["status"], "not_proven");
    assert_eq!(value["verify_with"], "sinter unresolved --name orphan_wipe");
    // Nothing about the verdict weakened by adding the next action.
    assert_eq!(value["coverage"]["status"], "not_proven");
    assert_eq!(value["coverage"]["conclusive"], false);
}

#[test]
fn an_empty_deps_names_the_command_that_settles_it() {
    let dir = fixture();
    let repo = dir.path();

    let (found, out) = sinter(repo, &["deps", "orphan_wipe"]);
    assert!(!found, "{out}");
    assert!(out.contains("not proven: 0 dependencies"), "{out}");
    assert!(
        out.contains("verify: sinter unresolved --name orphan_wipe"),
        "{out}"
    );

    let (_, out) = sinter(repo, &["deps", "orphan_wipe", "--json"]);
    let value: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(value["status"], "not_proven");
    assert_eq!(value["verify_with"], "sinter unresolved --name orphan_wipe");
    assert_eq!(value["coverage"]["conclusive"], false);
}

#[test]
fn grep_over_a_dependent_free_symbol_still_searches_its_own_file() {
    let dir = fixture();
    let repo = dir.path();

    // The bounding traversal is empty, but the seed's definition file is
    // part of its radius: the search covers it and finds the definition.
    let (found, out) = sinter(
        repo,
        &["grep", "orphan_wipe", "--within", "affected(orphan_wipe)"],
    );
    assert!(found, "{out}");
    assert!(out.contains("bound 1 file"), "{out}");

    let (_, out) = sinter(
        repo,
        &[
            "grep",
            "orphan_wipe",
            "--within",
            "affected(orphan_wipe)",
            "--json",
        ],
    );
    let value: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(value["status"], "found");
    assert_eq!(value["files_in_bound"], 1);

    // A non-empty bound is a real search: it keeps its search-shaped answer
    // and does not grow a coverage envelope it did not earn.
    let (found, out) = sinter(
        repo,
        &[
            "grep",
            "orphan_wipe",
            "--within",
            "file(src/main.rs)",
            "--json",
        ],
    );
    assert!(found, "{out}");
    let value: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(value["status"], "found");
    assert!(value.get("coverage").is_none(), "{value}");
}
