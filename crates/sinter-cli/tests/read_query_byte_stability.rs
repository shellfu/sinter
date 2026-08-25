//! Read verbs are reads: an identical query on an unchanged tree leaves
//! `.sinter/graph.redb` byte-identical, while a real edit is still picked
//! up by the same query's self-sync.

use std::process::Command;

fn sinter(repo: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .current_dir(repo)
        .output()
        .expect("run sinter");
    assert!(
        out.status.success(),
        "sinter {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn graph_bytes(repo: &std::path::Path) -> Vec<u8> {
    std::fs::read(repo.join(".sinter/graph.redb")).expect("read graph")
}

#[test]
fn repeated_read_queries_do_not_rewrite_the_graph() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/a.rs"),
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { alpha() }\n",
    )
    .unwrap();
    sinter(repo, &["build"]);

    // First query may still refresh stat stamps; from then on a read that
    // changes nothing must write nothing.
    sinter(repo, &["show", "alpha"]);
    let baseline = graph_bytes(repo);
    for verb in [
        vec!["show", "alpha"],
        vec!["query", "alpha"],
        vec!["affected", "alpha"],
        vec!["show", "alpha"],
    ] {
        sinter(repo, &verb);
        assert_eq!(
            graph_bytes(repo),
            baseline,
            "`sinter {verb:?}` rewrote the graph on an unchanged tree"
        );
    }

    // Self-sync is still live: an edit is picked up by a read verb.
    std::fs::write(
        repo.join("src/a.rs"),
        "pub fn alpha() -> u32 { 1 }\npub fn gamma() -> u32 { alpha() }\n",
    )
    .unwrap();
    let found = sinter(repo, &["show", "gamma"]);
    assert!(found.contains("gamma"), "{found}");
    assert_ne!(
        graph_bytes(repo),
        baseline,
        "an edited tree must move the graph"
    );

    // ...and settles again.
    let after_edit = graph_bytes(repo);
    sinter(repo, &["show", "gamma"]);
    assert_eq!(graph_bytes(repo), after_edit);
}
