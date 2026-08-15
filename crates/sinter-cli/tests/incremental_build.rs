//! R4 gate: a one-file edit re-extracts one file and finishes fast; an
//! unchanged corpus does no work.

use std::process::Command;
use std::time::{Duration, Instant};

fn sinter(repo: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .arg(repo)
        .output()
        .expect("run sinter");
    assert!(
        out.status.success(),
        "sinter {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn one_file_edit_is_incremental_and_fast() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    for i in 0..300 {
        std::fs::write(
            repo.join("src").join(format!("m{i}.rs")),
            format!("pub fn f{i}() -> u32 {{ {i} }}\npub fn g{i}() -> u32 {{ f{i}() }}\n"),
        )
        .unwrap();
    }

    let first = sinter(repo, &["build"]);
    assert!(first.contains("300 scanned, 300 changed"), "{first}");

    // No-op rebuild: nothing re-extracted.
    let noop = sinter(repo, &["build"]);
    assert!(noop.contains("300 scanned, 0 changed"), "{noop}");

    // One-file edit: exactly one re-extracted, well under the 1s budget
    // (budget is for 1M-LOC repos; this corpus must be far faster).
    std::fs::write(
        repo.join("src/m7.rs"),
        "pub fn f7() -> u32 { 77 }\npub fn g7() -> u32 { f7() }\n",
    )
    .unwrap();
    let started = Instant::now();
    let edit = sinter(repo, &["build"]);
    let elapsed = started.elapsed();
    assert!(edit.contains("300 scanned, 1 changed"), "{edit}");
    assert!(
        elapsed < Duration::from_secs(1),
        "incremental update took {elapsed:?}"
    );

    // Deletion is incremental too.
    std::fs::remove_file(repo.join("src/m8.rs")).unwrap();
    let removed = sinter(repo, &["build"]);
    assert!(removed.contains("0 changed, 1 removed"), "{removed}");
}
