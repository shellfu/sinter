//! `sinter scip`: run the repo's SCIP indexer, then rebuild so the
//! compiler-grade evidence lands. Indexer choice is toolchain policy, so
//! the table lives here in the binary, never in LanguageSpec.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::pipeline;

/// (language, argv, install hint). Every indexer writes `index.scip` at
/// the repo root when run from there.
const INDEXERS: &[(&str, &[&str], &str)] = &[
    (
        "rust",
        &["rust-analyzer", "scip", ".", "--output", "index.scip"],
        "rustup component add rust-analyzer",
    ),
    (
        "go",
        &["scip-go"],
        "go install github.com/sourcegraph/scip-go/cmd/scip-go@latest",
    ),
    (
        "typescript",
        &["scip-typescript", "index"],
        "npm install -g @sourcegraph/scip-typescript",
    ),
    (
        "python",
        &["scip-python", "index", ".", "--output", "index.scip"],
        "npm install -g @sourcegraph/scip-python",
    ),
    (
        "cpp",
        &["scip-clang", "--compdb-path=compile_commands.json"],
        "download a release from https://github.com/sourcegraph/scip-clang",
    ),
    (
        "javascript",
        &["scip-typescript", "index", "--infer-tsconfig"],
        "npm install -g @sourcegraph/scip-typescript",
    ),
    (
        "java",
        &["scip-java", "index"],
        "https://sourcegraph.github.io/scip-java/ (coursier install scip-java)",
    ),
    (
        "csharp",
        &["scip-dotnet", "index"],
        "dotnet tool install --global scip-dotnet",
    ),
    // c: scip-clang covers C via compile_commands.json under the cpp row.
    // sql/bash/proto: no SCIP indexers exist.
];

pub fn run(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;

    let hashes = pipeline::scan_hashes(&repo, &HashMap::new())?;
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for (file, _) in &hashes {
        let lang = sinter_extract::spec_for_path(file)
            .expect("scan yields language-matched files only")
            .name;
        match counts.iter_mut().find(|(l, _)| *l == lang) {
            Some((_, n)) => *n += 1,
            None => counts.push((lang, 1)),
        }
    }
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    if counts.is_empty() {
        bail!("no language files found under {}", repo.display());
    }

    // Every indexer insists on writing index.scip at the repo root; each
    // run is renamed aside immediately and the merge lands in .sinter/
    // (derived state, gitignored) so the repo root stays clean.
    let scratch = repo.join(".sinter");
    std::fs::create_dir_all(&scratch)?;
    let root_index = repo.join("index.scip");
    let final_index = scratch.join("index.scip");
    let mut produced: Vec<std::path::PathBuf> = Vec::new();
    for &(lang, files) in &counts {
        let Some((_, argv, hint)) = INDEXERS.iter().find(|(l, ..)| *l == lang) else {
            eprintln!("{lang}: no SCIP indexer exists — skipped ({files} files)");
            continue;
        };
        eprintln!("{lang}: running {}...", argv.join(" "));
        let status = Command::new(argv[0])
            .args(&argv[1..])
            .current_dir(&repo)
            .status();
        match status {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("{lang}: {} is not on PATH — install it: {hint}", argv[0]);
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("run {}", argv[0])),
            Ok(s) if !s.success() => {
                eprintln!("{lang}: {} failed with {s} — skipped", argv[0]);
                continue;
            }
            Ok(_) => {}
        }
        if !root_index.exists() {
            eprintln!("{lang}: {} succeeded but wrote no index.scip", argv[0]);
            continue;
        }
        let aside = scratch.join(format!("index-{lang}.scip"));
        std::fs::rename(&root_index, &aside)?;
        produced.push(aside);
    }
    if produced.is_empty() {
        bail!("no SCIP index produced for any language");
    }
    let parts: Vec<&Path> = produced.iter().map(std::path::PathBuf::as_path).collect();
    sinter_resolve::merge_index_files(&parts, &final_index)?;
    for aside in &produced {
        let _ = std::fs::remove_file(aside);
    }

    // The build notices the index fingerprint changed and re-resolves the
    // corpus without re-extracting; no db reset needed.
    let report = pipeline::build(&repo, None)?;
    pipeline::print_report(&report);
    Ok(())
}
