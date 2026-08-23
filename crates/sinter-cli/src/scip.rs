//! `sinter scip`: run the repo's SCIP indexer, then rebuild so the
//! compiler-grade evidence lands. Indexer choice is toolchain policy, so
//! the table lives here in the binary, never in LanguageSpec.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexingRecommendation {
    pub argv: Vec<String>,
    pub working_directory: String,
}

/// A configured source project Sinter can index, together with the facts an
/// agent needs before deciding whether to run an indexer. Recommendations are
/// present only when the required executable is currently available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexingProject {
    pub root: String,
    pub marker: String,
    pub languages: Vec<String>,
    pub source_files: usize,
    pub indexer: String,
    pub indexer_available: bool,
    pub freshness: &'static str,
    pub stale_inputs: usize,
    pub status: &'static str,
    pub risk: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<IndexingRecommendation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
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
    let mut walker = ignore::WalkBuilder::new(repo);
    walker.add_custom_ignore_filename(".sinterignore");
    for entry in walker.build().flatten() {
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

/// Configured projects for which Sinter knows a compiler indexer. Discovery
/// reads project markers and source paths but never executes a build tool.
pub fn indexing_projects(repo: &Path) -> Vec<IndexingProject> {
    let Ok(inventory) = inventory(repo) else {
        return Vec::new();
    };
    indexing_projects_with(repo, &inventory, executable_available)
}

/// Languages belonging to configured projects for which Sinter knows a
/// compiler indexer. An isolated source file without the corresponding
/// project marker is intentionally absent.
pub fn indexable_languages(repo: &Path) -> Vec<String> {
    let mut languages = BTreeSet::new();
    for project in indexing_projects(repo) {
        languages.extend(project.languages);
    }
    languages.into_iter().collect()
}

/// Source languages that have a known SCIP indexer but are not inside a
/// detected project for that indexer. These explain a coverage gap, but must
/// never produce a run recommendation.
pub fn unconfigured_indexable_languages(repo: &Path) -> Vec<String> {
    let Ok(inventory) = inventory(repo) else {
        return Vec::new();
    };
    let configured: BTreeSet<String> = indexing_projects_with(repo, &inventory, |_| false)
        .into_iter()
        .flat_map(|project| project.languages)
        .collect();
    let mut unconfigured = BTreeSet::new();
    for &(language, _) in &inventory.counts {
        let indexer = if language == "c" { "cpp" } else { language };
        if INDEXERS.iter().any(|(candidate, ..)| *candidate == indexer)
            && !configured.contains(language)
        {
            unconfigured.insert(language.to_string());
        }
    }
    unconfigured.into_iter().collect()
}

fn indexing_projects_with(
    repo: &Path,
    inventory: &Inventory,
    available: impl Fn(&str) -> bool,
) -> Vec<IndexingProject> {
    let staleness = staleness(repo);
    let (freshness, stale_inputs) = match staleness {
        Staleness::Fresh => ("fresh", 0),
        Staleness::Missing => ("missing", 0),
        Staleness::Stale(count) => ("stale", count),
    };
    let typescript_roots = configured_project_roots("typescript", inventory);
    let mut projects = Vec::new();
    for &(indexer_language, argv, install_hint) in INDEXERS {
        let mut roots = configured_project_roots(indexer_language, inventory);
        if indexer_language == "javascript" {
            roots.retain(|root| !typescript_roots.contains(root));
        }
        for root in roots {
            let languages = project_languages(indexer_language, &root, &inventory.sources);
            if languages.is_empty() {
                continue;
            }
            let indexer_available = available(argv[0]);
            let status = match (staleness, indexer_available) {
                (Staleness::Fresh, _) => "indexed_fresh",
                (_, false) => "indexer_unavailable",
                (Staleness::Missing, true) => "ready_to_index",
                (Staleness::Stale(_), true) => "ready_to_refresh",
            };
            let recommendation = (indexer_available && staleness != Staleness::Fresh).then(|| {
                IndexingRecommendation {
                    argv: vec!["sinter".to_string(), "scip".to_string()],
                    working_directory: ".".to_string(),
                }
            });
            let marker = project_marker(indexer_language, &root, &inventory.markers)
                .unwrap_or_else(|| config_hint(indexer_language).to_string());
            let source_files = inventory
                .sources
                .iter()
                .filter(|(language, path)| {
                    languages.iter().any(|candidate| candidate == language) && contains(&root, path)
                })
                .count();
            projects.push(IndexingProject {
                root: display_root(&root).to_string(),
                marker,
                languages,
                source_files,
                indexer: argv[0].to_string(),
                indexer_available,
                freshness,
                stale_inputs,
                status,
                risk: "executes_repository_controlled_build_or_index_configuration",
                recommendation,
                install_hint: (!indexer_available).then(|| install_hint.to_string()),
            });
        }
    }
    projects.sort_by(|left, right| {
        (&left.root, &left.indexer, &left.languages).cmp(&(
            &right.root,
            &right.indexer,
            &right.languages,
        ))
    });
    projects
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
        sources,
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
    let project_inventory = Inventory {
        counts: counts.clone(),
        markers,
        sources,
    };
    let typescript_roots = configured_project_roots("typescript", &project_inventory);
    let mut jobs = Vec::new();
    if !counts.iter().any(|(language, _)| *language == "typescript") {
        for root in &typescript_roots {
            if ran.insert(("typescript".to_string(), root.clone())) {
                jobs.push(("typescript", root.clone()));
            }
        }
    }
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
        let mut roots = configured_project_roots(lang, &project_inventory);
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
    // corpus without re-extracting; no db reset needed. That re-resolve
    // covers the whole corpus, so it reports its phases — indexing is
    // already the slowest thing a user runs.
    let progress = crate::progress::Progress::stderr();
    let report = pipeline::build_with(&repo, None, &mut |phase| {
        crate::progress::render(&progress, phase)
    })?;
    drop(progress);
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
    sources: Vec<(&'static str, String)>,
}

fn inventory(repo: &Path) -> Result<Inventory> {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    let mut markers = HashSet::new();
    let mut sources = Vec::new();
    let mut walker = ignore::WalkBuilder::new(repo);
    walker.add_custom_ignore_filename(".sinterignore");
    for entry in walker.build() {
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
        sources.push((lang, rel));
        match counts.iter_mut().find(|(l, _)| *l == lang) {
            Some((_, n)) => *n += 1,
            None => counts.push((lang, 1)),
        }
    }
    Ok(Inventory {
        counts,
        markers,
        sources,
    })
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
            let mut roots = named(&|name| name == "go.work");
            roots.extend(named(&|name| name == "go.mod"));
            outermost(roots)
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
            let mut roots = named(&|name| name.ends_with(".sln"));
            roots.extend(named(&|name| name.ends_with(".csproj")));
            outermost(roots)
        }
        _ => vec![String::new()],
    }
}

fn configured_project_roots(lang: &str, inventory: &Inventory) -> Vec<String> {
    project_roots(lang, &inventory.markers)
        .into_iter()
        .filter(|root| !project_languages(lang, root, &inventory.sources).is_empty())
        .collect()
}

fn project_languages(
    indexer_language: &str,
    root: &str,
    sources: &[(&'static str, String)],
) -> Vec<String> {
    let mut languages = BTreeSet::new();
    for &(language, ref path) in sources {
        let covered = match indexer_language {
            "typescript" => matches!(language, "typescript" | "javascript"),
            "cpp" => matches!(language, "c" | "cpp"),
            other => language == other,
        };
        if covered && contains(root, path) {
            languages.insert(language.to_string());
        }
    }
    languages.into_iter().collect()
}

fn project_marker(lang: &str, root: &str, markers: &HashSet<String>) -> Option<String> {
    let mut matches: Vec<&String> = markers
        .iter()
        .filter(|path| parent(path) == root && marker_matches(lang, file_name(path)))
        .collect();
    matches.sort_by_key(|path| (marker_priority(lang, file_name(path)), path.as_str()));
    matches.first().map(|path| (*path).clone())
}

fn marker_matches(lang: &str, name: &str) -> bool {
    match lang {
        "rust" => name == "Cargo.toml",
        "go" => matches!(name, "go.work" | "go.mod"),
        "typescript" => name.starts_with("tsconfig") && name.ends_with(".json"),
        "javascript" => matches!(name, "package.json" | "jsconfig.json"),
        "python" => matches!(
            name,
            "pyproject.toml" | "setup.py" | "setup.cfg" | "requirements.txt" | "Pipfile"
        ),
        "cpp" => name == "compile_commands.json",
        "java" => matches!(name, "pom.xml" | "build.gradle" | "build.gradle.kts"),
        "csharp" => name.ends_with(".sln") || name.ends_with(".csproj"),
        _ => false,
    }
}

fn marker_priority(lang: &str, name: &str) -> usize {
    if (lang == "go" && name == "go.work") || (lang == "csharp" && name.ends_with(".sln")) {
        0
    } else {
        1
    }
}

fn display_root(root: &str) -> &str {
    if root.is_empty() { "." } else { root }
}

fn executable_available(program: &str) -> bool {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 {
        return is_executable(candidate);
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| {
        let candidate = directory.join(program);
        if is_executable(&candidate) {
            return true;
        }
        #[cfg(windows)]
        {
            for extension in ["exe", "cmd", "bat", "com"] {
                if is_executable(&candidate.with_extension(extension)) {
                    return true;
                }
            }
        }
        false
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
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

#[cfg(test)]
mod tests {
    use super::{indexing_projects_with, inventory};

    #[test]
    fn isolated_language_file_does_not_borrow_an_unrelated_project_marker() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join("apps/web")).unwrap();
        std::fs::create_dir_all(repo.join("fixtures")).unwrap();
        std::fs::write(repo.join("apps/web/tsconfig.json"), "{}").unwrap();
        std::fs::write(
            repo.join("fixtures/example.ts"),
            "export const value = 1;\n",
        )
        .unwrap();

        let inventory = inventory(repo).unwrap();
        let projects = indexing_projects_with(repo, &inventory, |_| true);

        assert!(projects.is_empty(), "{projects:#?}");
    }

    #[test]
    fn project_report_names_root_marker_indexer_and_source_languages() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join("apps/web")).unwrap();
        std::fs::write(repo.join("apps/web/tsconfig.json"), "{}").unwrap();
        std::fs::write(repo.join("apps/web/index.ts"), "export const value = 1;\n").unwrap();
        std::fs::write(
            repo.join("apps/web/runtime.js"),
            "export const runtime = true;\n",
        )
        .unwrap();

        let inventory = inventory(repo).unwrap();
        let projects = indexing_projects_with(repo, &inventory, |_| true);

        assert_eq!(projects.len(), 1, "{projects:#?}");
        let project = &projects[0];
        assert_eq!(project.root, "apps/web");
        assert_eq!(project.marker, "apps/web/tsconfig.json");
        assert_eq!(project.indexer, "scip-typescript");
        assert_eq!(project.languages, ["javascript", "typescript"]);
        assert_eq!(project.source_files, 2);
    }

    #[test]
    fn recommendation_requires_an_available_indexer() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("lib.rs"), "pub fn value() {}\n").unwrap();
        let inventory = inventory(repo).unwrap();

        let unavailable = indexing_projects_with(repo, &inventory, |_| false);
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].status, "indexer_unavailable");
        assert!(unavailable[0].recommendation.is_none());
        assert!(unavailable[0].install_hint.is_some());

        let available = indexing_projects_with(repo, &inventory, |_| true);
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].status, "ready_to_index");
        assert_eq!(
            available[0].recommendation.as_ref().unwrap().argv,
            ["sinter", "scip"]
        );
        assert!(available[0].install_hint.is_none());
    }
}
