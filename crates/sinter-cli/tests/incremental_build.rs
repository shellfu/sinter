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

/// mtime-gated hashing: a touch (new mtime, identical content) re-hashes
/// once, reports nothing changed, and refreshes the stored stamp so the
/// next scan is stat-only again — while a real edit that preserves file
/// length still comes out as changed.
#[test]
fn touched_but_unchanged_file_stays_clean_and_restamps() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let content = "pub fn f() -> u32 { 1 }\n";
    std::fs::write(repo.join("src/a.rs"), content).unwrap();
    std::fs::write(repo.join("src/b.rs"), "pub fn g() -> u32 { 2 }\n").unwrap();
    let first = sinter(repo, &["build"]);
    assert!(first.contains("2 scanned, 2 changed"), "{first}");

    // Touch: rewrite with identical content, forcing a new mtime.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(repo.join("src/a.rs"), content).unwrap();
    let touched = sinter(repo, &["build"]);
    assert!(touched.contains("2 scanned, 0 changed"), "{touched}");
    // That build refreshed the stored stamp: the next build is a clean
    // stat-only pass and must still report nothing changed.
    let clean = sinter(repo, &["build"]);
    assert!(clean.contains("2 scanned, 0 changed"), "{clean}");

    // Same length, different bytes, new mtime: the stat gate must miss
    // (mtime moved), re-hash, and surface the edit.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(repo.join("src/a.rs"), "pub fn f() -> u32 { 9 }\n").unwrap();
    let edited = sinter(repo, &["build"]);
    assert!(edited.contains("2 scanned, 1 changed"), "{edited}");
}

/// A package rename in Cargo.toml changes module roots without touching any
/// source file. The build must re-resolve the corpus (dropping edges bound
/// through the old root), not report a no-op that leaves stale dependents.
#[test]
fn manifest_rename_invalidates_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../harness/golden/fixtures/rust-workspace-crates/crates");
    for (src, dst) in [("util", "util"), ("app", "app")] {
        let out = repo.join("crates").join(dst).join("src");
        std::fs::create_dir_all(&out).unwrap();
        for entry in std::fs::read_dir(fixture.join(src).join("src")).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), out.join(entry.file_name())).unwrap();
        }
        std::fs::copy(
            fixture.join(src).join("Cargo.toml"),
            repo.join("crates").join(dst).join("Cargo.toml"),
        )
        .unwrap();
    }

    sinter(repo, &["build"]);
    let before = sinter(repo, &["affected", "double", "--repo"]);
    assert!(before.contains("main"), "expected dependents: {before}");

    // Rename the package; imports still say `acme_util`, so nothing binds.
    let manifest = repo.join("crates/util/Cargo.toml");
    let renamed = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("acme-util", "renamed-util");
    std::fs::write(&manifest, renamed).unwrap();

    let rebuild = sinter(repo, &["build"]);
    assert!(
        !rebuild.contains(" 0 files re-resolved"),
        "manifest change must re-resolve: {rebuild}"
    );
    let after = sinter(repo, &["affected", "double", "--repo"]);
    assert!(
        !after.contains("main"),
        "stale dependents survived the rename: {after}"
    );
}

/// Commands resolve the graph root like git resolves .git: running from a
/// subdirectory finds the repo's graph instead of reporting none (or worse,
/// building a nested one).
#[test]
fn subdirectory_invocation_discovers_graph_root() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src/deep")).unwrap();
    std::fs::write(repo.join("src/deep/m.rs"), "pub fn f() {}\n").unwrap();
    sinter(repo, &["build"]);

    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["query", "f"])
        .current_dir(repo.join("src/deep"))
        .output()
        .expect("run sinter");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("src/deep/m.rs"),
        "query from subdirectory missed the root graph"
    );
    assert!(
        !repo.join("src/deep/.sinter").exists(),
        "subdirectory build created a nested graph"
    );
}
