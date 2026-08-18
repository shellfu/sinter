//! D29 gate: SCIP monikers whose definition is outside the corpus become
//! dependency-surface nodes (`dep:<package>@<version>`), so "what breaks
//! if I bump tokio" is answerable — and the surface survives no-op builds
//! but refreshes fully when the index regenerates.

use std::process::Command;

use protobuf::Message;
use scip::types::{Document, Index, Occurrence};

fn sinter(repo: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run sinter");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// index.scip with one external reference occurrence on `spawn` in
/// `tokio::task::spawn()` inside src/main.rs.
fn write_index(repo: &std::path::Path, version: &str) {
    let index = Index {
        documents: vec![Document {
            relative_path: "src/main.rs".to_string(),
            occurrences: vec![
                Occurrence {
                    // fn caller() { tokio::task::spawn(); }
                    //                            ^27..32
                    range: vec![0, 27, 32],
                    symbol: format!("rust-analyzer cargo tokio {version} task/spawn()."),
                    symbol_roles: 0,
                    ..Default::default()
                },
                Occurrence {
                    range: vec![0, 14, 19],
                    symbol: format!("rust-analyzer cargo tokio {version} task/"),
                    symbol_roles: 0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::write(repo.join("index.scip"), index.write_to_bytes().unwrap()).unwrap();
}

#[test]
fn dependency_surface_binds_survives_noop_and_refreshes() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/main.rs"),
        "fn caller() { tokio::task::spawn(); }\nfn main() { caller(); }\n",
    )
    .unwrap();
    write_index(repo, "1.0.0");

    // Build: the external ref binds to a synthesized dep node and the
    // report says so on its own line — resolution honesty (D29).
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("dependency surface: 1 refs bound to 1 external symbols across 1 packages"),
        "{out}"
    );

    // The dep node is a first-class query result with its pseudo-file.
    let (ok, out) = sinter(repo, &["query", "spawn"]);
    assert!(ok, "{out}");
    assert!(out.contains("dep:tokio@1.0.0"), "{out}");
    assert!(out.contains("tokio::task::spawn"), "{out}");

    // Blast radius: the in-repo caller is a dependent, certain, scip.
    let (ok, out) = sinter(repo, &["affected", "tokio::task::spawn"]);
    assert!(ok, "{out}");
    assert!(out.contains("dep:tokio@1.0.0"), "{out}");
    assert!(out.contains("caller"), "{out}");
    assert!(out.contains("scip"), "{out}");
    // Honest-note machinery: the ref is bound, so no missing-dependents
    // note may fire for it.
    assert!(!out.contains("note:"), "{out}");

    // No-op rebuild must not tear the surface down (dep pseudo-files are
    // never on disk; the scan cannot see them as removed).
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    assert!(out.contains("0 changed, 0 removed"), "{out}");
    let (ok, out) = sinter(repo, &["affected", "spawn"]);
    assert!(ok, "{out}");
    assert!(out.contains("caller"), "{out}");

    // Index regeneration (fingerprint change) fully refreshes the surface:
    // the bumped tokio replaces the old package node.
    write_index(repo, "2.0.0");
    let (ok, out) = sinter(repo, &["build"]);
    assert!(ok, "{out}");
    assert!(out.contains("dependency surface: 1 refs"), "{out}");
    let (ok, out) = sinter(repo, &["query", "spawn"]);
    assert!(ok, "{out}");
    assert!(out.contains("dep:tokio@2.0.0"), "{out}");
    assert!(!out.contains("dep:tokio@1.0.0"), "{out}");
}
