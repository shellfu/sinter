//! `sinter scip`: run the repo's SCIP indexer, then rebuild so the
//! compiler-grade evidence lands. Indexer choice is toolchain policy, so
//! the table lives here in the binary, never in LanguageSpec.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::pipeline;

/// (language, argv, install hint). Every indexer writes `index.scip` in
/// the project directory it is run from.
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
    // sql/bash/proto/markdown: no SCIP indexers exist.
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Staleness {
    Fresh,
    Missing,
    /// How many source/configuration inputs are newer than the index.
    Stale(usize),
}

/// The doctor's staleness notion, reimplemented here so `check` needs
/// neither a graph nor an indexer: mtime of every language file and project
/// configuration/lock input vs the index's mtime.
pub fn staleness(repo: &Path) -> Staleness {
    let Some(index) = pipeline::scip_index_path(repo) else {
        return Staleness::Missing;
    };
    let Ok(index_mtime) = std::fs::metadata(&index).and_then(|m| m.modified()) else {
        return Staleness::Missing;
    };
    let mut newer = 0;
    for entry in ignore::WalkBuilder::new(repo).build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = sinter_core::rel_display(entry.path().strip_prefix(repo).unwrap_or(entry.path()));
        if rel.starts_with(".sinter/")
            || crate::corpus::excluded(&rel)
            || (sinter_extract::spec_for_path(&rel).is_none()
                && !is_project_marker(file_name(&rel)))
        {
            continue;
        }
        if entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .is_some_and(|m| m > index_mtime)
        {
            newer += 1;
        }
    }
    if newer == 0 {
        Staleness::Fresh
    } else {
        Staleness::Stale(newer)
    }
}

/// Languages present in the corpus for which Sinter knows a compiler
/// indexer. This is intentionally inventory-only: coverage reporting must
/// never execute a build tool.
pub fn indexable_languages(repo: &Path) -> Vec<String> {
    let Ok(inventory) = inventory(repo) else {
        return Vec::new();
    };
    let mut languages: Vec<String> = inventory
        .counts
        .into_iter()
        .filter_map(|(language, _)| {
            let indexer = if language == "c" { "cpp" } else { language };
            INDEXERS
                .iter()
                .any(|(candidate, ..)| *candidate == indexer)
                .then(|| language.to_string())
        })
        .collect();
    languages.sort();
    languages.dedup();
    languages
}

/// `sinter scip check`: the CI guard. Exit 0 when the index exists and
/// no source/configuration input is newer; exit 1 otherwise. Never runs an
/// indexer.
pub fn check(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;
    match staleness(&repo) {
        Staleness::Fresh => {
            println!("index fresh");
            Ok(())
        }
        Staleness::Missing => bail!("no SCIP index at .sinter/index.scip"),
        Staleness::Stale(n) => {
            bail!("index stale: {n} source/config input(s) newer than the index")
        }
    }
}

/// Bare `sinter scip`: index only when `check` would fail, so the command
/// is idempotent and a CI cache hit costs one directory walk. `--force`
/// routes to `run` instead.
pub fn run_if_stale(repo: &Path) -> Result<()> {
    let canon = repo.canonicalize()?;
    match staleness(&canon) {
        Staleness::Fresh => {
            println!("index fresh — nothing to do (--force to reindex)");
            Ok(())
        }
        Staleness::Missing | Staleness::Stale(_) => run(repo),
    }
}

pub fn run(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;

    let Inventory {
        mut counts,
        markers,
    } = inventory(&repo)?;
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    if counts.is_empty() {
        bail!("no language files found under {}", repo.display());
    }

    // Indexers insist on writing index.scip in their working directory.
    // Run once per configured project root, rebase nested document paths,
    // then merge into .sinter/ (derived state, gitignored).
    let scratch = repo.join(".sinter");
    std::fs::create_dir_all(&scratch)?;
    let final_index = scratch.join("index.scip");
    let mut produced: Vec<std::path::PathBuf> = Vec::new();
    let mut ran = HashSet::new();
    let typescript_roots = project_roots("typescript", &markers);
    let mut jobs = Vec::new();
    for &(source_lang, files) in &counts {
        // C and C++ share scip-clang. A configured TypeScript project may
        // cover JavaScript in the same root, so avoid indexing that exact
        // project twice while preserving independent JavaScript packages.
        let lang = if source_lang == "c" {
            "cpp"
        } else {
            source_lang
        };
        if !INDEXERS.iter().any(|(l, ..)| *l == lang) {
            eprintln!("{source_lang}: no SCIP indexer exists — skipped ({files} files)");
            continue;
        }
        let mut roots = project_roots(lang, &markers);
        if lang == "javascript" {
            roots.retain(|root| !typescript_roots.contains(root));
            if roots.is_empty() && !typescript_roots.is_empty() {
                eprintln!(
                    "javascript: covered by configured TypeScript project(s) ({files} files)"
                );
                continue;
            }
        }
        if roots.is_empty() {
            eprintln!(
                "{source_lang}: skipped {files} files — no {} found",
                config_hint(lang)
            );
            continue;
        }
        for root in roots {
            if ran.insert((lang.to_string(), root.clone())) {
                jobs.push((lang, root));
            }
        }
    }

    for (job_index, (lang, project_root)) in jobs.into_iter().enumerate() {
        let (_, argv, hint) = INDEXERS
            .iter()
            .find(|(candidate, ..)| *candidate == lang)
            .expect("jobs only contain known indexers");
        let project = if project_root.is_empty() {
            repo.clone()
        } else {
            repo.join(&project_root)
        };
        let root_index = project.join("index.scip");
        if root_index.exists() {
            eprintln!(
                "{lang} ({project_root}): refusing to overwrite pre-existing {} — skipped",
                root_index.display()
            );
            continue;
        }
        let label = if project_root.is_empty() {
            "."
        } else {
            &project_root
        };
        eprintln!("{lang} ({label}): running {}...", argv.join(" "));
        let status = Command::new(argv[0])
            .args(&argv[1..])
            .current_dir(&project)
            .status();
        match status {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("{lang}: {} is not on PATH — install it: {hint}", argv[0]);
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("run {}", argv[0])),
            Ok(s) if !s.success() => {
                let _ = std::fs::remove_file(&root_index);
                eprintln!("{lang}: {} failed with {s} — skipped", argv[0]);
                continue;
            }
            Ok(_) => {}
        }
        if !root_index.exists() {
            eprintln!("{lang}: {} succeeded but wrote no index.scip", argv[0]);
            continue;
        }
        sinter_resolve::prefix_index_paths(&root_index, &project_root)?;
        let aside = scratch.join(format!("index-{lang}-{job_index}.scip"));
        std::fs::rename(&root_index, &aside)?;
        produced.push(aside);
    }
    if produced.is_empty() {
        bail!("no SCIP index produced for any configured language project");
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

/// One cheap repository walk: count source languages and remember the full
/// repo-relative paths of project markers. Unlike the old
/// `scan_hashes(..., empty)`, this never reads or hashes every source file
/// before invoking the actual indexer.
struct Inventory {
    counts: Vec<(&'static str, usize)>,
    markers: HashSet<String>,
}

fn inventory(repo: &Path) -> Result<Inventory> {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    let mut markers = HashSet::new();
    for entry in ignore::WalkBuilder::new(repo).build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let rel = sinter_core::rel_display(entry.path().strip_prefix(repo).unwrap_or(entry.path()));
        if rel.starts_with(".sinter/") || crate::corpus::excluded(&rel) {
            continue;
        }
        if is_project_marker(file_name(&rel)) {
            markers.insert(rel.clone());
        }
        let Some(lang) = sinter_extract::spec_for_path(&rel).map(|spec| spec.name) else {
            continue;
        };
        match counts.iter_mut().find(|(l, _)| *l == lang) {
            Some((_, n)) => *n += 1,
            None => counts.push((lang, 1)),
        }
    }
    Ok(Inventory { counts, markers })
}

fn project_roots(lang: &str, markers: &HashSet<String>) -> Vec<String> {
    let named = |predicate: &dyn Fn(&str) -> bool| {
        let mut roots: Vec<String> = markers
            .iter()
            .filter(|path| predicate(file_name(path)))
            .map(|path| parent(path).to_string())
            .collect();
        roots.sort();
        roots.dedup();
        roots
    };
    match lang {
        "rust" => outermost(named(&|name| name == "Cargo.toml")),
        "go" => {
            let workspaces = named(&|name| name == "go.work");
            if workspaces.is_empty() {
                named(&|name| name == "go.mod")
            } else {
                outermost(workspaces)
            }
        }
        "typescript" => outermost(named(&|name| {
            name.starts_with("tsconfig") && name.ends_with(".json")
        })),
        "javascript" => named(&|name| name == "package.json" || name == "jsconfig.json"),
        "python" => named(&|name| {
            matches!(
                name,
                "pyproject.toml" | "setup.py" | "setup.cfg" | "requirements.txt" | "Pipfile"
            )
        }),
        "cpp" => named(&|name| name == "compile_commands.json"),
        "java" => outermost(named(&|name| {
            matches!(name, "pom.xml" | "build.gradle" | "build.gradle.kts")
        })),
        "csharp" => {
            let solutions = named(&|name| name.ends_with(".sln"));
            if solutions.is_empty() {
                outermost(named(&|name| name.ends_with(".csproj")))
            } else {
                outermost(solutions)
            }
        }
        _ => vec![String::new()],
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_project_marker(name: &str) -> bool {
    matches!(
        name,
        "Cargo.toml"
            | "Cargo.lock"
            | "go.mod"
            | "go.work"
            | "go.sum"
            | "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "jsconfig.json"
            | "pyproject.toml"
            | "poetry.lock"
            | "uv.lock"
            | "setup.py"
            | "setup.cfg"
            | "requirements.txt"
            | "Pipfile"
            | "compile_commands.json"
            | "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
    ) || (name.starts_with("tsconfig") && name.ends_with(".json"))
        || name.ends_with(".sln")
        || name.ends_with(".csproj")
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn contains(parent: &str, child: &str) -> bool {
    parent.is_empty() || parent == child || child.starts_with(&format!("{parent}/"))
}

/// Keep configured roots that are not already covered by a shallower
/// workspace/project marker.
fn outermost(mut roots: Vec<String>) -> Vec<String> {
    roots.sort_by_key(|root| {
        (
            root.split('/').filter(|part| !part.is_empty()).count(),
            root.clone(),
        )
    });
    let mut kept: Vec<String> = Vec::new();
    for root in roots {
        if !kept.iter().any(|parent| contains(parent, &root)) {
            kept.push(root);
        }
    }
    kept
}

fn config_hint(lang: &str) -> &'static str {
    match lang {
        "rust" => "Cargo.toml",
        "go" => "go.mod or go.work",
        "typescript" => "tsconfig*.json",
        "javascript" => "package.json or jsconfig.json",
        "python" => "Python project configuration",
        "cpp" => "compile_commands.json",
        "java" => "Maven or Gradle project configuration",
        "csharp" => ".sln or .csproj",
        _ => "project configuration",
    }
}
