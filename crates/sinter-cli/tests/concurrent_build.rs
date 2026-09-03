//! A rebuild must never take the repository away from readers.
//!
//! The outage this guards: a full rebuild held redb's exclusive write
//! lock for ~2 minutes on a 1,130-file repository, and every query in
//! every other process queued behind it with no output. Builds now write
//! a side file and swap it in, so a reader answers from the previous
//! graph while the rebuild runs.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn sinter(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .arg(repo)
        .output()
        .expect("run sinter")
}

/// `query` names its repository with `--repo`, not a trailing path.
fn query(repo: &Path, symbol: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["query", symbol, "--json", "--repo"])
        .arg(repo)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .output()
        .expect("run sinter query")
}

/// Big enough that a full build is measured in seconds, so the reader
/// below is genuinely racing it rather than winning by scheduling luck.
fn write_corpus(repo: &Path, modules: usize) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    for i in 0..modules {
        let mut source = String::new();
        for f in 0..25 {
            source.push_str(&format!(
                "pub fn m{i}_f{f}() -> u32 {{ {f} }}\npub fn m{i}_g{f}() -> u32 {{ m{i}_f{f}() }}\n"
            ));
        }
        std::fs::write(repo.join("src").join(format!("m{i}.rs")), source).unwrap();
    }
}

/// Append to every module so the next build is a full one.
fn touch_all(repo: &Path, modules: usize) {
    for i in 0..modules {
        let path = repo.join("src").join(format!("m{i}.rs"));
        let mut source = std::fs::read_to_string(&path).unwrap();
        source.push_str(&format!("pub fn touched{i}() -> u32 {{ {i} }}\n"));
        std::fs::write(&path, source).unwrap();
    }
}

#[test]
fn reader_answers_from_the_previous_graph_while_a_rebuild_runs() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    write_corpus(repo, 400);
    assert!(sinter(repo, &["build"]).status.success());

    // Force a full rebuild: every file changed.
    touch_all(repo, 400);

    let mut builder = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["build"])
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .arg(repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn builder");

    // Let the builder get past its scan and into the write phase.
    std::thread::sleep(Duration::from_millis(600));
    let started = Instant::now();
    let reader = query(repo, "m7_f3");
    let latency = started.elapsed();
    let _ = builder.wait();

    assert!(
        reader.status.success(),
        "reader failed during rebuild: {}",
        String::from_utf8_lossy(&reader.stderr)
    );
    assert!(
        String::from_utf8_lossy(&reader.stdout).contains("m7_f3"),
        "reader served no answer from the previous graph"
    );
    assert!(
        latency < Duration::from_secs(1),
        "reader waited {latency:?} for a concurrent rebuild"
    );
}

#[test]
fn two_concurrent_builds_leave_one_consistent_graph() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    // Long enough that the second process is certain to find the first
    // still building rather than already finished.
    write_corpus(repo, 600);
    assert!(sinter(repo, &["build"]).status.success());
    touch_all(repo, 600);

    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_sinter"))
            .args(["build"])
            .env("HOME", repo)
            .env("USERPROFILE", repo)
            .arg(repo)
            .output()
            .expect("run sinter build")
    };
    let (left, right) = std::thread::scope(|scope| {
        let a = scope.spawn(spawn);
        let b = scope.spawn(spawn);
        (a.join().unwrap(), b.join().unwrap())
    });

    for out in [&left, &right] {
        assert!(
            out.status.success(),
            "concurrent build failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // Exactly one did the work; the other yielded to it and served the
    // graph that was already there rather than rebuilding it in parallel.
    // (Either process may be the winner, so this asserts the pair.)
    let yielded = [&left, &right]
        .iter()
        .filter(|out| String::from_utf8_lossy(&out.stdout).contains("another sinter process"))
        .count();
    assert_eq!(yielded, 1, "expected exactly one build to yield");

    // The surviving graph answers, and no side file was left behind.
    let answer = query(repo, "m3_g4");
    assert!(
        answer.status.success() && String::from_utf8_lossy(&answer.stdout).contains("m3_g4"),
        "graph unusable after concurrent builds: {}",
        String::from_utf8_lossy(&answer.stderr)
    );
    let leftovers: Vec<_> = std::fs::read_dir(repo.join(".sinter"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".build-") || n == "build.lock")
        .collect();
    assert!(leftovers.is_empty(), "left behind {leftovers:?}");
}

#[test]
fn stale_lock_from_a_dead_process_is_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    write_corpus(repo, 4);
    std::fs::create_dir_all(repo.join(".sinter")).unwrap();
    // A pid no process can hold, stamped as if taken just now: only the
    // liveness check can free this build.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        repo.join(".sinter/build.lock"),
        format!("4294967290 {now}\n"),
    )
    .unwrap();

    let out = sinter(repo, &["build"]);
    assert!(
        out.status.success(),
        "build refused to reclaim a dead owner's lock: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("another sinter process"),
        "build yielded to a dead owner"
    );
    assert!(!repo.join(".sinter/build.lock").exists());
}
