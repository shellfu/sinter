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

    let full_started = Instant::now();
    let first = sinter(repo, &["build"]);
    let full_elapsed = full_started.elapsed();
    assert!(first.contains("300 scanned, 300 changed"), "{first}");

    // No-op rebuild: nothing re-extracted. Its duration is the measured
    // scan floor (spawn + walk + hash 300 files) every build pays.
    let noop_started = Instant::now();
    let noop = sinter(repo, &["build"]);
    let noop_elapsed = noop_started.elapsed();
    assert!(noop.contains("300 scanned, 0 changed"), "{noop}");

    // One-file edit: exactly one re-extracted. The timing gate compares
    // work ABOVE the scan floor: on hosts with slow file I/O (Windows CI)
    // the floor dominates both builds, so edit-vs-full wall clock ratios
    // are meaningless — a 0.5x gate failed there at 0.64x with nothing
    // regressed. Subtracting the no-op floor cancels the fixed cost on
    // every platform. What this must catch is the incremental path
    // silently redoing full-corpus work — then above-floor work converges
    // on the full build's. Absolute edit-latency budgets are the nightly
    // release-mode gate's job.
    std::fs::write(
        repo.join("src/m7.rs"),
        "pub fn f7() -> u32 { 77 }\npub fn g7() -> u32 { f7() }\n",
    )
    .unwrap();
    let started = Instant::now();
    let edit = sinter(repo, &["build"]);
    let elapsed = started.elapsed();
    assert!(edit.contains("300 scanned, 1 changed"), "{edit}");
    let edit_work = elapsed.saturating_sub(noop_elapsed);
    let full_work = full_elapsed.saturating_sub(noop_elapsed);
    assert!(
        edit_work < full_work.mul_f64(0.5).max(Duration::from_millis(250)),
        "incremental work {edit_work:?} vs full-build work {full_work:?} \
         (edit {elapsed:?}, full {full_elapsed:?}, scan floor {noop_elapsed:?})"
    );

    // Deletion is incremental too.
    std::fs::remove_file(repo.join("src/m8.rs")).unwrap();
    let removed = sinter(repo, &["build"]);
    assert!(removed.contains("0 changed, 1 removed"), "{removed}");
}
