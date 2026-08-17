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
];

pub fn run(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;

    // One index.scip per repo and every indexer writes that same file, so
    // the dominant language by file count picks the indexer.
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
    let Some(&(lang, files)) = counts.first() else {
        bail!("no language files found under {}", repo.display());
    };
    let Some((_, argv, hint)) = INDEXERS.iter().find(|(l, ..)| *l == lang) else {
        bail!("no SCIP indexer known for {lang}");
    };
    if counts.len() > 1 {
        eprintln!(
            "multiple languages present; indexing the dominant one only ({lang}, {files} files)"
        );
    }

    eprintln!("running {}...", argv.join(" "));
    let status = Command::new(argv[0])
        .args(&argv[1..])
        .current_dir(&repo)
        .status();
    match status {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("{} is not on PATH — install it: {hint}", argv[0])
        }
        Err(e) => return Err(e).with_context(|| format!("run {}", argv[0])),
        Ok(s) if !s.success() => bail!("{} failed with {s}", argv[0]),
        Ok(_) => {}
    }
    if !repo.join("index.scip").exists() {
        bail!("{} succeeded but wrote no index.scip", argv[0]);
    }

    // SCIP binds at resolve time and only for files in the affected set, so
    // an index appearing over an already-built graph would never be read.
    // ponytail: full rebuild forces the re-resolve; hash index.scip into the
    // scan for an incremental re-resolve if the rebuild cost ever matters.
    let db = pipeline::db_path(&repo);
    if db.exists() {
        std::fs::remove_file(&db).context("reset graph for full re-resolve")?;
    }
    let report = pipeline::build(&repo, None)?;
    pipeline::print_report(&report);
    Ok(())
}
