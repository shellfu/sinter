//! Crash-window gate: a build that dies after per-file derivation commits
//! but before the resolution pass must be fully repaired by the next
//! build — no edge may stay lost.

use std::process::Command;

use sinter_extract::{Extractor, spec_for_path};
use sinter_store::Store;

fn sinter(repo: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
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
fn interrupted_build_recovers_all_edges() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("util")).unwrap();
    // b's only binding into util is a module-level import whose ref name
    // is the module path (`example.com/fix/util`) — no def name of
    // util.go — so the name-refs index can never rediscover b.go after
    // util.go changes. Recovery depends entirely on the persisted
    // pending delta (mirrors the store-level test
    // `pending_delta_survives_crash_between_update_and_resolution`).
    std::fs::write(repo.join("go.mod"), "module example.com/fix\n\ngo 1.22\n").unwrap();
    std::fs::write(
        repo.join("util/util.go"),
        "package util\n\nfunc Backoff(n int) int { return n }\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("b.go"),
        "package main\n\nimport \"example.com/fix/util\"\n\nfunc Other() int { return 2 }\n",
    )
    .unwrap();
    sinter(repo, &["build"]);

    let db = repo.join(".sinter/graph.redb");
    let baseline = {
        let store = Store::open(&db).unwrap();
        store.edge_count().unwrap()
    };
    assert!(baseline > 0);

    // Simulate the crash: re-run exactly the first phase of a build for
    // util.go (per-file derivation, which tears down b's binding into
    // util and clears util's hash stamp), then die before any resolution
    // commits.
    {
        let store = Store::open(&db).unwrap();
        let spec = spec_for_path("util/util.go").unwrap();
        let mut extractor = Extractor::new(spec).unwrap();
        let source = std::fs::read_to_string(repo.join("util/util.go")).unwrap();
        let facts = extractor.extract("util/util.go", &source).unwrap();
        store.update_files(&[facts], &[]).unwrap();
        let lost = store.edge_count().unwrap();
        assert!(lost < baseline, "crash simulation removed no edges");
    }

    // The next build must repair everything the interrupted one tore down.
    sinter(repo, &["build"]);
    let recovered = {
        let store = Store::open(&db).unwrap();
        store.edge_count().unwrap()
    };
    assert_eq!(recovered, baseline, "edges lost across the crash window");
}
