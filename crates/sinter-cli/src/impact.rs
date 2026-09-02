//! `sinter impact [rev-range]`: changed symbols -> blast radius -> affected
//! tests. Line hunks come from `git diff -U0`; spans are matched against the
//! graph built from the working tree, so build before asking. Without a
//! range the working tree is diffed against `HEAD`, untracked files included.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sinter_core::{CorpusScope, Node, SymbolKind};
use sinter_resolve::qualified_of;
use sinter_store::EdgeFilter;

use crate::lookup::{open_current, open_store};

/// Per-collection output budget for agent-facing impact results. Computation
/// always remains complete; rendering applies this limit independently to
/// every potentially large collection.
pub const DEFAULT_LIMIT: usize = 20;

#[derive(Serialize)]
pub struct ImpactReport {
    pub rev_range: String,
    /// Whether every file in the requested diff could be projected onto
    /// the current graph snapshot. `partial_reasons` explains every reason
    /// this is `partial`; agents must not treat a partial blast radius as a
    /// complete test-selection proof.
    pub analysis_status: AnalysisStatus,
    pub partial_reasons: Vec<&'static str>,
    /// Reason -> what an agent should do about it, for the reasons that
    /// change how the rest of the report should be read.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub partial_reason_notes: BTreeMap<&'static str, &'static str>,
    /// Hunks come from the rev range but spans match the working-tree
    /// graph; uncommitted edits shift spans, so totals may include drift.
    pub working_tree_dirty: bool,
    /// Authoritative path-level ledger from `git diff --name-status`.
    /// Unlike `changed_symbols`, this never drops configuration, binary,
    /// deleted, renamed, or otherwise unindexed paths.
    pub changed_files: Vec<ChangedFile>,
    /// Entries from `git status --porcelain`, including untracked paths
    /// which Git does not include in a normal `git diff <rev>`. Top-level
    /// derived roots excluded from the graph are excluded here too: they
    /// cannot make graph-relative impact analysis stale or incomplete.
    pub working_tree_changes: Vec<WorkingTreeChange>,
    /// Changed paths that could not be represented completely by the
    /// working-tree graph. A file can have mapped symbols and still appear
    /// here when deleted content has no current-tree symbol.
    pub unmapped_files: Vec<UnmappedFile>,
    pub changed_symbols: Vec<SymbolRef>,
    pub blast_radius: Vec<SymbolRef>,
    pub affected_tests: Vec<SymbolRef>,
    /// One entry per `--expect` symbol: which of its direct dependents this
    /// change set touched and which it still owes. Absent without `--expect`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub expect: Vec<ExpectReport>,
    /// Recommended commands that exercise the affected tests. Repository
    /// instructions win over anything listed here.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub validation: Vec<ValidationStep>,
    /// Node ids of every symbol the diff touched, tests included. Internal:
    /// `--expect` diffs its dependents against this, and unlike
    /// `changed_symbols` it is neither test-filtered nor name-keyed.
    #[serde(skip)]
    pub changed_ids: BTreeSet<String>,
    /// `file:qualified` -> runnable command, for affected tests only.
    #[serde(skip)]
    pub test_commands: BTreeMap<String, String>,
    /// `file:qualified` -> how a blast-radius entry was reached when only a
    /// constant/static edit reaches it (`uses/const`): a reader of a string
    /// is not a caller of a function.
    #[serde(skip)]
    pub via: BTreeMap<String, &'static str>,
    /// `file:qualified` -> number of same-named definitions folded into one
    /// changed row (`#[cfg]` twins).
    #[serde(skip)]
    pub variants: BTreeMap<String, usize>,
}

/// `--expect <SYMBOL>`: the unfinished-refactor check. Bare impact answers
/// "what did my edit reach"; this answers "what did the edit still miss".
#[derive(Serialize, Debug)]
pub struct ExpectReport {
    /// The requested symbol as resolved in the graph.
    pub symbol: String,
    pub file: String,
    /// The seed changed, but its signature and kind are identical at the
    /// range base: callers cannot be owed anything by a body-only edit.
    pub body_only: bool,
    /// How the symbol was resolved when the head graph alone could not do
    /// it (deleted/renamed symbol resolved at the range base).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Depth-1 dependents only. Transitive dependents would drown the
    /// signal: a refactor owes its callers, not their callers.
    pub direct_dependents: usize,
    pub changed_total: usize,
    pub untouched_total: usize,
    /// Direct dependents this change set already touched.
    pub changed: Vec<ExpectSite>,
    /// Direct dependents it did not touch: the sites a refactor of `symbol`
    /// probably still owes.
    pub untouched: Vec<ExpectSite>,
}

#[derive(Serialize, Clone, Debug, Eq, PartialEq)]
pub struct ExpectSite {
    pub qualified: String,
    pub kind: &'static str,
    /// `file:line`; the file alone when the line cannot be read.
    pub at: String,
    /// Admitted call sites from this dependent into the expected symbol
    /// (an edge counts once per site it kept, so a caller that calls the
    /// symbol three times ranks above one that calls it once).
    pub sites: usize,
}

#[derive(Serialize, Clone, Debug, Eq, PartialEq)]
pub struct ValidationStep {
    pub command: String,
    pub reason: String,
}

#[derive(Serialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisStatus {
    Complete,
    Partial,
}

#[derive(Serialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    Added,
    Copied,
    Deleted,
    Modified,
    Renamed,
    TypeChanged,
    Unmerged,
    Unknown,
}

#[derive(Serialize, Clone, Debug, Eq, PartialEq)]
pub struct ChangedFile {
    /// New/current path for additions, copies, and renames; the removed
    /// path for deletions.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// Exact Git status (`M`, `D`, `R100`, ...), retained alongside the
    /// normalized kind so agents do not lose rename/copy similarity data.
    pub git_status: String,
    pub kind: FileChangeKind,
    pub mapped_symbols: usize,
    /// Why this file contributes what it does to the blast radius.
    pub reason: String,
}

#[derive(Serialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UnmappedReason {
    Deleted,
    NotIndexed,
    Unreadable,
    NoContentHunks,
    NoSymbolOverlap,
    DeletedContentNotInCurrentGraph,
    UnknownGitStatus,
}

#[derive(Serialize, Clone, Debug, Eq, PartialEq)]
pub struct UnmappedFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub git_status: String,
    pub reason: UnmappedReason,
    /// Human explanation, same text as the `changed_files` entry.
    pub detail: String,
}

#[derive(Serialize, Clone, Debug, Eq, PartialEq)]
pub struct WorkingTreeChange {
    /// Current path; for a rename or copy, `old_path` is the source path.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// Git porcelain's index (`X`) and worktree (`Y`) states, normalized
    /// into stable names instead of exposing whitespace-sensitive bytes.
    pub index_status: &'static str,
    pub worktree_status: &'static str,
    pub conflicted: bool,
}

#[derive(Clone, Copy, Debug)]
struct Hunk {
    new_start: usize,
    new_count: usize,
    deleted_only: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct SymbolRef {
    pub qualified: String,
    pub kind: &'static str,
    pub file: String,
}

fn symbol_ref(node: &Node) -> SymbolRef {
    SymbolRef {
        qualified: qualified_of(node.id.as_str()).to_string(),
        kind: node.kind.as_str(),
        file: node.file.clone(),
    }
}

fn git_diff(repo: &Path, rev_range: &str, args: &[&str]) -> Result<Output> {
    if rev_range.trim().is_empty() || rev_range.starts_with('-') {
        bail!("rev range must be a non-empty revision expression, not a Git option");
    }
    let output = Command::new("git")
        .args(["-c", "diff.noprefix=false", "diff", "--no-ext-diff"])
        .args(args)
        .arg(rev_range)
        .arg("--")
        .current_dir(repo)
        .output()
        .context("run git diff")?;
    if output.status.success() {
        return Ok(output);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.to_lowercase().contains("not a git repository") {
        bail!("not a git repository — impact needs git history");
    }
    // Full git stderr can run to pages; the first line names the problem.
    bail!(
        "git diff {rev_range} failed: {}",
        stderr.lines().next().unwrap_or("").trim()
    );
}

fn historical_range_endpoint(rev_range: &str) -> Option<&str> {
    let (_, endpoint) = rev_range
        .split_once("...")
        .or_else(|| rev_range.split_once(".."))?;
    let endpoint = endpoint.trim();
    Some(if endpoint.is_empty() {
        "HEAD"
    } else {
        endpoint
    })
}

fn git_tree(repo: &Path, revision: &str) -> Result<String> {
    let treeish = format!("{revision}^{{tree}}");
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "--end-of-options"])
        .arg(&treeish)
        .current_dir(repo)
        .output()
        .with_context(|| format!("resolve Git tree for {revision}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git rev-parse {revision} failed: {}",
            stderr.lines().next().unwrap_or("").trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn historical_endpoint_matches_head(repo: &Path, rev_range: &str) -> Result<bool> {
    let Some(endpoint) = historical_range_endpoint(rev_range) else {
        return Ok(true);
    };
    Ok(git_tree(repo, endpoint)? == git_tree(repo, "HEAD")?)
}

/// `rev:path` content, `None` when the path does not exist at `rev`.
fn git_show(repo: &Path, rev: &str, path: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["show", "--end-of-options", &format!("{rev}:{path}")])
        .current_dir(repo)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The revision a range diffs *from*: `A` for `A..B`, the merge base for
/// `A...B`, the revision itself for a working-tree diff.
pub(crate) fn range_base(repo: &Path, rev_range: &str) -> String {
    if let Some((a, b)) = rev_range.split_once("...") {
        let b = if b.is_empty() { "HEAD" } else { b };
        let output = Command::new("git")
            .args(["merge-base", "--end-of-options", a, b])
            .current_dir(repo)
            .output();
        if let Ok(output) = output
            && output.status.success()
        {
            return String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
        return a.to_string();
    }
    match rev_range.split_once("..") {
        Some((a, _)) => a.to_string(),
        None => rev_range.to_string(),
    }
}

/// Whether `ancestor` is reachable from `descendant`.
pub(crate) fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(repo)
        .status()
        .is_ok_and(|status| status.success())
}

/// Definitions in `path` as it read at `rev`, extracted fresh: the graph
/// only knows the working tree.
fn nodes_at(repo: &Path, rev: &str, path: &str) -> Option<Vec<Node>> {
    let spec = sinter_extract::spec_for_path(path)?;
    let source = git_show(repo, rev, path)?;
    let mut extractor = sinter_extract::Extractor::new(spec).ok()?;
    Some(extractor.extract(path, &source).ok()?.nodes)
}

/// Nodes whose byte span overlaps a changed line range of `source`.
fn touched_nodes<'a>(source: &str, nodes: &'a [Node], ranges: &[Hunk]) -> Vec<&'a Node> {
    let mut line_starts = vec![0u64];
    for (i, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            line_starts.push(i as u64 + 1);
        }
    }
    let byte_range = |line: usize, count: usize| -> Option<(u64, u64)> {
        if line == 0 || count == 0 {
            return None;
        }
        let start = line_starts
            .get(line - 1)
            .copied()
            .unwrap_or(source.len() as u64);
        let end = line_starts
            .get(line - 1 + count)
            .copied()
            .unwrap_or(source.len() as u64);
        Some((start, end))
    };
    nodes
        .iter()
        .filter(|node| node.kind != SymbolKind::File)
        .filter(|node| {
            ranges.iter().any(|hunk| {
                byte_range(hunk.new_start, hunk.new_count)
                    .is_some_and(|(start, end)| node.span.start < end && start < node.span.end)
            })
        })
        .collect()
}

fn file_change_kind(status: &str) -> FileChangeKind {
    match status.as_bytes().first().copied() {
        Some(b'A') => FileChangeKind::Added,
        Some(b'C') => FileChangeKind::Copied,
        Some(b'D') => FileChangeKind::Deleted,
        Some(b'M') => FileChangeKind::Modified,
        Some(b'R') => FileChangeKind::Renamed,
        Some(b'T') => FileChangeKind::TypeChanged,
        Some(b'U') => FileChangeKind::Unmerged,
        _ => FileChangeKind::Unknown,
    }
}

fn path_string(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

fn parse_changed_files(raw: &[u8]) -> Result<Vec<ChangedFile>> {
    let fields: Vec<&[u8]> = raw
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect();
    let mut cursor = 0usize;
    let mut files = Vec::new();
    while cursor < fields.len() {
        let git_status = path_string(fields[cursor]);
        cursor += 1;
        let kind = file_change_kind(&git_status);
        let (old_path, path) = if matches!(kind, FileChangeKind::Copied | FileChangeKind::Renamed) {
            let old = fields
                .get(cursor)
                .context("git diff emitted a rename/copy without its old path")?;
            let new = fields
                .get(cursor + 1)
                .context("git diff emitted a rename/copy without its new path")?;
            cursor += 2;
            (Some(path_string(old)), path_string(new))
        } else {
            let path = fields
                .get(cursor)
                .context("git diff emitted a status without a path")?;
            cursor += 1;
            (None, path_string(path))
        };
        files.push(ChangedFile {
            path,
            old_path,
            git_status,
            kind,
            mapped_symbols: 0,
            reason: String::new(),
        });
    }
    files.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.old_path.cmp(&b.old_path))
    });
    Ok(files)
}

fn changed_files(repo: &Path, rev_range: &str, extra: &[&str]) -> Result<Vec<ChangedFile>> {
    let output = git_diff(
        repo,
        rev_range,
        &[extra, &["--name-status", "-z", "--find-renames"]].concat(),
    )?;
    parse_changed_files(&output.stdout)
}

fn status_name(status: u8) -> &'static str {
    match status {
        b' ' => "unmodified",
        b'M' => "modified",
        b'T' => "type_changed",
        b'A' => "added",
        b'D' => "deleted",
        b'R' => "renamed",
        b'C' => "copied",
        b'U' => "unmerged",
        b'?' => "untracked",
        b'!' => "ignored",
        _ => "unknown",
    }
}

fn is_conflict_status(index: u8, worktree: u8) -> bool {
    matches!(
        (index, worktree),
        (b'D', b'D')
            | (b'A', b'U')
            | (b'U', b'D')
            | (b'U', b'A')
            | (b'D', b'U')
            | (b'A', b'A')
            | (b'U', b'U')
    )
}

fn parse_working_tree_changes(raw: &[u8]) -> Result<Vec<WorkingTreeChange>> {
    let fields: Vec<&[u8]> = raw
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect();
    let mut cursor = 0usize;
    let mut changes = Vec::new();
    while cursor < fields.len() {
        let entry = fields[cursor];
        cursor += 1;
        if entry.len() < 4 || entry[2] != b' ' {
            bail!("git status emitted a malformed porcelain record");
        }
        let index = entry[0];
        let worktree = entry[1];
        let path = path_string(&entry[3..]);
        let old_path = if matches!(index, b'R' | b'C') || matches!(worktree, b'R' | b'C') {
            let old = fields
                .get(cursor)
                .context("git status emitted a rename/copy without its old path")?;
            cursor += 1;
            Some(path_string(old))
        } else {
            None
        };
        changes.push(WorkingTreeChange {
            path,
            old_path,
            index_status: status_name(index),
            worktree_status: status_name(worktree),
            conflicted: is_conflict_status(index, worktree),
        });
    }
    Ok(changes)
}

/// Paths the graph never maps: derived roots plus sinter's own `.sinter/` state.
fn is_tool_state(path: &str) -> bool {
    path == ".sinter" || path.starts_with(".sinter/") || crate::corpus::excluded(path)
}

fn is_untracked(change: &WorkingTreeChange) -> bool {
    change.index_status == "untracked" || change.worktree_status == "untracked"
}

fn sort_working_tree_changes(changes: &mut [WorkingTreeChange]) {
    changes.sort_by(|a, b| {
        b.conflicted
            .cmp(&a.conflicted)
            .then_with(|| is_untracked(a).cmp(&is_untracked(b)))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.old_path.cmp(&b.old_path))
    });
}

fn working_tree_changes(repo: &Path) -> Result<Vec<WorkingTreeChange>> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(repo)
        .output()
        .context("run git status")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git status failed: {}",
            stderr.lines().next().unwrap_or("").trim()
        );
    }
    let mut changes = parse_working_tree_changes(&output.stdout)?;
    changes.retain(|change| !is_tool_state(&change.path));
    sort_working_tree_changes(&mut changes);
    Ok(changes)
}

fn parse_range(token: &str, prefix: char) -> Option<(usize, usize)> {
    let range = token.strip_prefix(prefix)?;
    let mut parts = range.splitn(2, ',');
    let start = parts.next()?.parse().ok()?;
    let count = parts.next().map_or(Some(1), |value| value.parse().ok())?;
    Some((start, count))
}

fn parse_hunks(raw: &[u8]) -> BTreeMap<String, Vec<Hunk>> {
    let mut hunks: BTreeMap<String, Vec<Hunk>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in String::from_utf8_lossy(raw).lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            // Deletions ("+++ /dev/null") and quoted/unexpected paths must
            // clear the current file, never inherit the previous one.
            current = rest.strip_prefix("b/").map(str::to_string);
        } else if let (Some(file), Some(rest)) = (&current, line.strip_prefix("@@ ")) {
            let mut ranges = rest.split_whitespace();
            let old = ranges.next().and_then(|part| parse_range(part, '-'));
            let new = ranges.next().and_then(|part| parse_range(part, '+'));
            if let (Some((_, old_count)), Some((new_start, new_count))) = (old, new) {
                hunks.entry(file.clone()).or_default().push(Hunk {
                    new_start,
                    new_count,
                    deleted_only: old_count > 0 && new_count == 0,
                });
            }
        }
    }
    hunks
}

const NOT_INDEXED: &str = "not indexed (language unsupported / excluded)";

/// Dependents of a file's symbols living in other files: the edges the
/// blast radius will actually follow.
fn file_reason(
    store: &sinter_store::Store,
    change: &ChangedFile,
    nodes: &[Node],
) -> Result<String> {
    let ids: Vec<sinter_core::NodeId> = nodes.iter().map(|n| n.id.clone()).collect();
    let dependents = store
        .in_edges_many(&ids)?
        .values()
        .flatten()
        .filter(|e| e.relation != sinter_core::Relation::Contains)
        .filter(|e| {
            e.src
                .as_str()
                .split_once('#')
                .map_or(e.src.as_str(), |(f, _)| f)
                != change.path
        })
        .count();
    Ok(if change.kind == FileChangeKind::Added && dependents == 0 {
        "new file, 0 inbound edges (nothing references it yet)".to_string()
    } else {
        format!(
            "indexed, {} symbols, {dependents} dependents",
            change.mapped_symbols
        )
    })
}

/// Whether an unmapped file is a hole in the analysis. Manifests, lockfiles,
/// and other unsupported or excluded paths were never going to be in the
/// graph, so they cannot make it incomplete; a source file in an indexed
/// language that still failed to map can.
fn mapping_gap(file: &UnmappedFile) -> bool {
    // ponytail: `.sinterignore` exclusions are not consulted here; an ignored
    // source file still reads as a gap. Thread the ignore matcher through if
    // that ever misleads.
    let hidden = file.path.split('/').any(|segment| segment.starts_with('.'));
    file.reason != UnmappedReason::NotIndexed
        || (sinter_extract::spec_for_path(&file.path).is_some()
            && !hidden
            && !crate::corpus::excluded(&file.path))
}

fn unmapped(change: &ChangedFile, reason: UnmappedReason) -> UnmappedFile {
    UnmappedFile {
        path: change.path.clone(),
        old_path: change.old_path.clone(),
        git_status: change.git_status.clone(),
        detail: change.reason.clone(),
        reason,
    }
}

/// Only executable symbols can be tests; prose sections, files, fields and
/// modules match the name/path heuristics below far too easily.
fn test_capable_kind(node: &Node) -> bool {
    matches!(
        node.kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Class | SymbolKind::Struct
    )
}

/// Conventional test-file paths across the indexed languages.
fn test_path(f: &str) -> bool {
    f.ends_with("_test.go")
        || f.ends_with("_test.py")
        || f.ends_with("Tests.cs")
        || f.contains(".test.")
        || f.contains(".spec.")
        || f.starts_with("tests/")
        || f.contains("/tests/")
        || f.contains("/test/")
}

/// Test detection heuristic: conventional test files and names.
pub fn is_test(node: &Node) -> bool {
    if !test_capable_kind(node) {
        return false;
    }
    test_path(&node.file)
        || node.name.starts_with("test_")
        || node.name.starts_with("Test")
        // Rust `#[cfg(test)] mod tests` inline modules.
        || qualified_of(node.id.as_str()).split("::").any(|s| s == "tests")
}

/// Blast radius: union of transitive dependents of every changed symbol,
/// keyed by node id, minus the changed symbols themselves.
pub(crate) fn blast_radius(
    store: &sinter_store::Store,
    filter: &EdgeFilter,
    changed: &[Node],
) -> Result<BTreeMap<String, Node>> {
    let mut radius: BTreeMap<String, Node> = BTreeMap::new();
    for node in changed {
        for reached in store.dependents(&node.id, filter, 25)? {
            radius.insert(reached.node.id.as_str().to_string(), reached.node);
        }
    }
    for node in changed {
        radius.remove(node.id.as_str());
    }
    Ok(radius)
}

/// Affected-test selection shared by `impact` and `context`. Node scope
/// decides what is a test: an inline `#[cfg(test)] mod tests` in a
/// production file is affected-test material, not a changed production
/// symbol.
pub(crate) fn affected_tests(
    store: &sinter_store::Store,
    radius: &BTreeMap<String, Node>,
    changed: &[Node],
) -> Result<Vec<SymbolRef>> {
    let scope_index = store.scope_index()?;
    let is_test_node = |n: &Node| {
        test_capable_kind(n) && (scope_index.scope_of(n) == CorpusScope::Test || is_test(n))
    };
    Ok(radius
        .values()
        .chain(changed.iter())
        .filter(|n| is_test_node(n))
        .map(symbol_ref)
        .collect())
}

// --------------------------------------------------- runnable validation

/// Nearest ancestor `Cargo.toml` that declares a `[package]`, as
/// (package directory relative to `repo`, parsed manifest). A virtual
/// workspace manifest is not a package: the walk stops there with `None`.
fn package_of(repo: &Path, file: &Path) -> Option<(PathBuf, toml::Value)> {
    let mut dir = file.parent()?;
    loop {
        if let Ok(text) = std::fs::read_to_string(repo.join(dir).join("Cargo.toml")) {
            let manifest: toml::Value = text.parse().ok()?;
            manifest.get("package")?.get("name")?.as_str()?;
            return Some((dir.to_path_buf(), manifest));
        }
        dir = dir.parent()?;
    }
}

/// The single binary target of a package, or `None` when the manifest
/// declares several and the layout cannot pick one.
fn sole_bin(manifest: &toml::Value, package: &str, pkg_root: &Path) -> Option<String> {
    match manifest.get("bin").and_then(toml::Value::as_array) {
        Some(bins) => {
            let mains: Vec<&toml::Value> = bins
                .iter()
                .filter(|bin| {
                    bin.get("path")
                        .and_then(toml::Value::as_str)
                        .is_none_or(|path| path == "src/main.rs")
                })
                .collect();
            match mains.as_slice() {
                [only] => only.get("name")?.as_str().map(str::to_string),
                _ => None,
            }
        }
        // No `[[bin]]`: the default binary exists only if `src/main.rs` does,
        // and it is named after the package.
        None => pkg_root
            .join("src/main.rs")
            .is_file()
            .then(|| package.to_string()),
    }
}

/// Cargo target selector for `rel` (a package-relative path), plus the Rust
/// module prefix Cargo's test filter needs on top of the graph's qualified
/// name. `None` whenever the layout does not name exactly one target.
fn cargo_target(
    manifest: &toml::Value,
    package: &str,
    pkg_root: &Path,
    rel: &Path,
) -> Option<(String, String)> {
    let parts: Vec<&str> = rel
        .iter()
        .map(|part| part.to_str())
        .collect::<Option<_>>()?;
    let stem = |name: &str| name.strip_suffix(".rs").map(str::to_string);
    match parts.as_slice() {
        // `tests/<name>.rs` and `tests/<name>/main.rs` are integration test
        // crates; anything else under `tests/` is a helper module of one.
        ["tests", file] => Some((format!("--test {}", stem(file)?), String::new())),
        ["tests", name, "main.rs"] => Some((format!("--test {name}"), String::new())),
        ["tests", ..] => None,
        ["src", "bin", file] => Some((format!("--bin {}", stem(file)?), String::new())),
        ["src", "bin", name, "main.rs"] => Some((format!("--bin {name}"), String::new())),
        ["src", "main.rs"] => Some((
            format!("--bin {}", sole_bin(manifest, package, pkg_root)?),
            String::new(),
        )),
        ["src", "lib.rs"] => Some(("--lib".to_string(), String::new())),
        ["src", rest @ ..] if !rest.is_empty() => {
            // Unit tests compile into whichever target roots this module.
            let target = if pkg_root.join("src/lib.rs").is_file() {
                "--lib".to_string()
            } else {
                format!("--bin {}", sole_bin(manifest, package, pkg_root)?)
            };
            let mut module: Vec<String> = Vec::new();
            for (index, part) in rest.iter().enumerate() {
                if index + 1 == rest.len() {
                    if *part != "mod.rs" {
                        module.push(stem(part)?);
                    }
                } else {
                    module.push((*part).to_string());
                }
            }
            Some((target, module.join("::")))
        }
        // benches/, examples/, build.rs: real targets, but not test targets.
        _ => None,
    }
}

/// Runnable test command for one affected test node, e.g.
/// `cargo test -p sinter-cli --test integration -- module::name`.
///
/// `None` rather than a guess: a wrong command costs more than no command.
/// Doc tests are deliberately absent — they are not graph nodes, so nothing
/// here could name one without inventing it.
pub fn test_command(repo: &Path, node: &Node) -> Option<String> {
    let file = Path::new(&node.file);
    let qualified = qualified_of(node.id.as_str());
    let name = qualified.rsplit("::").next().unwrap_or(qualified);
    match file.extension()?.to_str()? {
        "rs" => {}
        "go" => {
            let dir = file.parent()?.to_str()?;
            let dir = if dir.is_empty() { "." } else { dir };
            return Some(crate::testcmd::test_command("go", dir, &node.file, name));
        }
        "py" => {
            return Some(crate::testcmd::test_command(
                "python", "", &node.file, qualified,
            ));
        }
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            let runner = if package_json_mentions(repo, file, "vitest") {
                "vitest"
            } else {
                "npm"
            };
            return Some(crate::testcmd::test_command(runner, "", &node.file, name));
        }
        // Other ecosystems need their own layout rules, and a fabricated
        // command is worse than silence.
        _ => return None,
    }
    let (pkg_dir, manifest) = package_of(repo, file)?;
    let package = manifest.get("package")?.get("name")?.as_str()?;
    let pkg_root = repo.join(&pkg_dir);
    let rel = file.strip_prefix(&pkg_dir).ok()?;
    let (target, module) = cargo_target(&manifest, package, &pkg_root, rel)?;
    let filter = if module.is_empty() {
        qualified.to_string()
    } else {
        format!("{module}::{qualified}")
    };
    Some(crate::testcmd::test_command(
        "rust", package, &target, &filter,
    ))
}

/// Whether the nearest `package.json` above `file` names `needle` anywhere
/// (a dependency or a script). Text search, not JSON parsing: the question
/// is only "is vitest around here".
fn package_json_mentions(repo: &Path, file: &Path, needle: &str) -> bool {
    let mut dir = file.parent();
    while let Some(current) = dir {
        if let Ok(text) = std::fs::read_to_string(repo.join(current).join("package.json")) {
            return text.contains(needle);
        }
        dir = current.parent();
    }
    false
}

fn symbol_key(file: &str, qualified: &str) -> String {
    format!("{file}:{qualified}")
}

/// One step per cargo *target*, not per test: running a target once beats
/// invoking cargo a hundred times with a name filter each.
fn validation_steps(commands: &BTreeMap<String, String>) -> Vec<ValidationStep> {
    let mut covered: BTreeMap<&str, usize> = BTreeMap::new();
    for command in commands.values() {
        let target = command.split(" -- ").next().unwrap_or(command);
        *covered.entry(target).or_default() += 1;
    }
    covered
        .into_iter()
        .map(|(command, tests)| ValidationStep {
            command: command.to_string(),
            reason: format!("direct affected test target, {tests} affected test(s)"),
        })
        .collect()
}

// ------------------------------------------------------- expected symbols

/// Resolve one `--expect` name. An ambiguous name prefers candidates in
/// changed files; what remains ambiguous is an error listing candidates,
/// never a quiet in-degree pick. A name absent from the head graph is
/// looked up at the range base, so a deleted or renamed symbol can still
/// be named: the returned note says so.
fn expect_target(
    repo: &Path,
    store: &sinter_store::Store,
    symbol: &str,
    report: &ImpactReport,
    base: &str,
) -> Result<(Node, Option<String>)> {
    use crate::lookup::{Found, SymbolLookupError, find_symbol};
    let changed_files: BTreeSet<&str> = report
        .changed_files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    if let Found::Exact(mut nodes) = find_symbol(store, symbol)? {
        if nodes.len() == 1 {
            return Ok((nodes.remove(0), None));
        }
        let mut in_changed: Vec<Node> = nodes
            .iter()
            .filter(|node| changed_files.contains(node.file.as_str()))
            .cloned()
            .collect();
        let twins = in_changed.windows(2).all(|pair| {
            pair[0].file == pair[1].file
                && pair[0].kind == pair[1].kind
                && qualified_of(pair[0].id.as_str()) == qualified_of(pair[1].id.as_str())
        });
        if !in_changed.is_empty() && twins {
            return Ok((in_changed.remove(0), None));
        }
        return Err(SymbolLookupError::Ambiguous {
            requested: symbol.to_string(),
            candidates: if in_changed.is_empty() {
                nodes
            } else {
                in_changed
            },
        }
        .into());
    }
    // Deleted or renamed by this change set: it exists at the base only.
    let name = symbol.split('@').next().unwrap_or(symbol);
    for file in &report.changed_files {
        let path = file.old_path.as_deref().unwrap_or(&file.path);
        let Some(nodes) = nodes_at(repo, base, path) else {
            continue;
        };
        if let Some(node) = nodes
            .into_iter()
            .find(|node| qualified_of(node.id.as_str()) == name || node.name == name)
        {
            return Ok((
                node,
                Some(format!(
                    "absent at head; resolved at {base}:{path} — dependents are the unresolved references still naming it"
                )),
            ));
        }
    }
    // Still nothing: surface the ordinary lookup error with its suggestions.
    crate::lookup::unique_symbol(store, symbol).map(|node| (node, None))
}

/// Whether the seed's declaration is identical at the range base: same
/// kind, same signature text. A body-only edit owes its callers nothing.
fn body_only(repo: &Path, base: &str, target: &Node) -> bool {
    nodes_at(repo, base, &target.file).is_some_and(|nodes| {
        nodes.iter().any(|node| {
            node.kind == target.kind
                && qualified_of(node.id.as_str()) == qualified_of(target.id.as_str())
                && node.signature == target.signature
        })
    })
}

/// For each `--expect` symbol: its direct dependents, split by whether this
/// change set touched them. Ranked by edge count into the expected symbol.
fn expect_reports(
    repo: &Path,
    store: &sinter_store::Store,
    filter: &EdgeFilter,
    expect: &[String],
    report: &ImpactReport,
    limit: usize,
) -> Result<Vec<ExpectReport>> {
    if expect.is_empty() {
        return Ok(Vec::new());
    }
    let base = range_base(repo, &report.rev_range);
    // A file node depends on the symbol by importing it. Editing any symbol
    // in that file is editing the import site, so the file counts as touched
    // — otherwise every file whose call sites were all updated still reports
    // its own import as owed.
    let changed_files: BTreeSet<&str> = report
        .changed_files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let mut reports = Vec::new();
    for symbol in expect {
        let (target, note) = expect_target(repo, store, symbol, report, &base)?;
        let site_of = |node: &Node, sites: usize| ExpectSite {
            qualified: qualified_of(node.id.as_str()).to_string(),
            kind: node.kind.as_str(),
            at: match crate::render::line_of(repo, &node.file, node.span.start) {
                Some(line) => format!("{}:{line}", node.file),
                None => node.file.clone(),
            },
            sites,
        };
        let (mut changed, mut untouched) = (Vec::new(), Vec::new());
        if note.is_some() {
            // No head node, so no edges: the dependents are whoever still
            // writes the old name.
            let mut by_enclosing: BTreeMap<String, usize> = BTreeMap::new();
            for unresolved in store.unresolved_details(None, Some(&target.name))? {
                if let Some(enclosing) = unresolved.reference.enclosing {
                    *by_enclosing
                        .entry(enclosing.as_str().to_string())
                        .or_default() += 1;
                }
            }
            for (id, sites) in by_enclosing {
                let Some(node) = store.node(&sinter_core::NodeId::new(&id))? else {
                    continue;
                };
                let site = site_of(&node, sites);
                if report.changed_ids.contains(&id) {
                    changed.push(site);
                } else {
                    untouched.push(site);
                }
            }
        } else {
            let mut edges: BTreeMap<String, usize> = BTreeMap::new();
            // Files whose import/re-export line reaches the target: a
            // dependent inside a changed one of those was reached through
            // an edited import, which is the touch a refactor owes it.
            // ponytail: file-granular; a per-edge path check if a changed
            // file that merely imports the target ever hides a real miss.
            let mut importing_files: BTreeSet<String> = BTreeSet::new();
            for edge in store.in_edges(&target.id)? {
                if filter.admits(&edge) {
                    *edges.entry(edge.src.as_str().to_string()).or_default() +=
                        (edge.sites_total as usize).max(1);
                }
                if edge.relation == sinter_core::Relation::Imports {
                    let src = edge.src.as_str();
                    importing_files.insert(src.split_once('#').map_or(src, |(f, _)| f).to_string());
                }
            }
            for reached in store.dependents(&target.id, filter, 1)? {
                let id = reached.node.id.as_str().to_string();
                let site = site_of(&reached.node, edges.get(&id).copied().unwrap_or(1));
                let file = reached.node.file.as_str();
                let touched = report.changed_ids.contains(&id)
                    || (changed_files.contains(file)
                        && (reached.node.kind == SymbolKind::File
                            || importing_files.contains(file)));
                if touched {
                    changed.push(site);
                } else {
                    untouched.push(site);
                }
            }
        }
        let rank =
            |a: &ExpectSite, b: &ExpectSite| b.sites.cmp(&a.sites).then_with(|| a.at.cmp(&b.at));
        changed.sort_by(rank);
        untouched.sort_by(rank);
        let (changed_total, untouched_total) = (changed.len(), untouched.len());
        changed.truncate(returned_count(changed_total, limit));
        untouched.truncate(returned_count(untouched_total, limit));
        let body_only = note.is_none()
            && report.changed_ids.contains(target.id.as_str())
            && body_only(repo, &base, &target);
        reports.push(ExpectReport {
            symbol: qualified_of(target.id.as_str()).to_string(),
            file: target.file.clone(),
            body_only,
            note,
            direct_dependents: changed_total + untouched_total,
            changed_total,
            untouched_total,
            changed,
            untouched,
        });
    }
    Ok(reports)
}

pub fn compute(repo: &Path, rev_range: &str) -> Result<ImpactReport> {
    compute_filtered(repo, rev_range, &EdgeFilter::default())
}

pub fn compute_filtered(repo: &Path, rev_range: &str, filter: &EdgeFilter) -> Result<ImpactReport> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
    compute_with_store(&repo, rev_range, filter, &store)
}

/// The MCP `impact` computation. One store handle covers both the diff pass
/// and `--expect`, the way `run` does it for the CLI: redb forbids a second
/// open of the same file, and the two passes must see one snapshot. An empty
/// `expect` leaves `ImpactReport::expect` empty, which serializes to nothing.
pub(crate) fn compute_current_with_expect(
    repo: &Path,
    rev_range: &str,
    expect: &[String],
    limit: usize,
) -> Result<ImpactReport> {
    let repo = repo.canonicalize()?;
    let store = open_current(&repo)?;
    let filter = EdgeFilter::default();
    let mut report = compute_with_store(&repo, rev_range, &filter, &store)?;
    report.expect = expect_reports(&repo, &store, &filter, expect, &report, limit)?;
    Ok(report)
}

pub(crate) fn compute_with_store(
    repo: &Path,
    rev_range: &str,
    filter: &EdgeFilter,
    store: &sinter_store::Store,
) -> Result<ImpactReport> {
    compute_with_store_mode(repo, rev_range, false, filter, store)
}

/// Untracked, non-ignored paths (`git ls-files --others --exclude-standard`).
fn untracked_files(repo: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .current_dir(repo)
        .output()
        .context("run git ls-files")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git ls-files failed: {}",
            stderr.lines().next().unwrap_or("").trim()
        );
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .map(path_string)
        .filter(|path| !is_tool_state(path))
        .collect())
}

/// `staged` diffs the index instead of the working tree (`git diff --cached`).
fn compute_with_store_mode(
    repo: &Path,
    rev_range: &str,
    staged: bool,
    filter: &EdgeFilter,
    store: &sinter_store::Store,
) -> Result<ImpactReport> {
    let working_tree_changes = working_tree_changes(repo)?;
    let working_tree_dirty = !working_tree_changes.is_empty();
    let historical = rev_range.contains("..");
    let historical_endpoint_matches_head = historical_endpoint_matches_head(repo, rev_range)?;
    let cached: &[&str] = if staged { &["--cached"] } else { &[] };
    let mut changed_files = changed_files(repo, rev_range, cached)?;
    changed_files.retain(|change| !is_tool_state(&change.path));
    // New-side hunks per file from Git. Name/status is deliberately a
    // separate command: patch text cannot represent config-only, binary,
    // deleted, pure-rename, or mode-only changes reliably.
    let patch = git_diff(
        repo,
        rev_range,
        &[cached, &["-U0", "--no-color", "--find-renames"]].concat(),
    )?;
    let mut hunks = parse_hunks(&patch.stdout);
    // Working-tree mode: untracked files are additions whose every line is a
    // hunk; Git's diff against a revision never lists them.
    let untracked_included = !historical && !staged;
    if untracked_included {
        for path in untracked_files(repo)? {
            let lines = std::fs::read_to_string(repo.join(&path))
                .map(|source| source.lines().count().max(1))
                .unwrap_or(1);
            hunks.insert(
                path.clone(),
                vec![Hunk {
                    new_start: 1,
                    new_count: lines,
                    deleted_only: false,
                }],
            );
            changed_files.push(ChangedFile {
                path,
                old_path: None,
                git_status: "??".to_string(),
                kind: FileChangeKind::Added,
                mapped_symbols: 0,
                reason: String::new(),
            });
        }
    }

    // Changed symbols: nodes whose byte span overlaps a changed line range.
    // The BTreeMap de-duplicates nodes while preserving deterministic output.
    let mut changed_by_id: BTreeMap<String, Node> = BTreeMap::new();
    let mut unmapped_files = Vec::new();
    for change in &mut changed_files {
        let reason = if change.kind == FileChangeKind::Deleted {
            change.reason = "deleted".to_string();
            Some(UnmappedReason::Deleted)
        } else if change.kind == FileChangeKind::Unknown {
            change.reason = "unknown git status".to_string();
            Some(UnmappedReason::UnknownGitStatus)
        } else {
            let Some(facts) = store.facts(&change.path)? else {
                change.reason = NOT_INDEXED.to_string();
                unmapped_files.push(unmapped(change, UnmappedReason::NotIndexed));
                continue;
            };
            let Ok(source) = std::fs::read_to_string(repo.join(&change.path)) else {
                change.reason = "unreadable".to_string();
                unmapped_files.push(unmapped(change, UnmappedReason::Unreadable));
                continue;
            };
            let ranges = hunks.get(&change.path).map(Vec::as_slice).unwrap_or(&[]);
            let mut mapped_ids = BTreeSet::new();

            if ranges.is_empty()
                && matches!(
                    change.kind,
                    FileChangeKind::Copied | FileChangeKind::Renamed
                )
            {
                // A pure rename/copy has no patch hunks but changes the
                // identity of every symbol in the file. Include the file
                // node too so import dependents remain visible.
                for node in &facts.nodes {
                    mapped_ids.insert(node.id.as_str().to_string());
                    changed_by_id.insert(node.id.as_str().to_string(), node.clone());
                }
            } else if ranges.is_empty() {
                change.mapped_symbols = 0;
                change.reason = "no content hunks".to_string();
                unmapped_files.push(unmapped(change, UnmappedReason::NoContentHunks));
                continue;
            } else {
                // Hunk lines address the range endpoint. When that is not
                // the tree the graph was built from, match against the
                // endpoint's own definitions and carry the result over by
                // name; anything that no longer exists falls back to the
                // file row.
                let endpoint_nodes = (!historical_endpoint_matches_head)
                    .then(|| {
                        let endpoint = historical_range_endpoint(rev_range)?;
                        let source = git_show(repo, endpoint, &change.path)?;
                        let nodes = nodes_at(repo, endpoint, &change.path)?;
                        Some((source, nodes))
                    })
                    .flatten();
                let mut file_fallback = false;
                match &endpoint_nodes {
                    Some((endpoint_source, endpoint_nodes)) => {
                        for touched in touched_nodes(endpoint_source, endpoint_nodes, ranges) {
                            let key = (qualified_of(touched.id.as_str()), touched.kind);
                            let mut matched = false;
                            for node in facts
                                .nodes
                                .iter()
                                .filter(|node| (qualified_of(node.id.as_str()), node.kind) == key)
                            {
                                matched = true;
                                mapped_ids.insert(node.id.as_str().to_string());
                                changed_by_id.insert(node.id.as_str().to_string(), node.clone());
                            }
                            file_fallback |= !matched;
                        }
                    }
                    None => {
                        for node in touched_nodes(&source, &facts.nodes, ranges) {
                            mapped_ids.insert(node.id.as_str().to_string());
                            changed_by_id.insert(node.id.as_str().to_string(), node.clone());
                        }
                    }
                }
                if mapped_ids.is_empty() || file_fallback {
                    // Imports and other file-level changes live outside a
                    // definition span. The file node is the graph's honest
                    // fallback and carries import dependents.
                    if let Some(node) = facts
                        .nodes
                        .iter()
                        .find(|node| node.kind == SymbolKind::File)
                    {
                        mapped_ids.insert(node.id.as_str().to_string());
                        changed_by_id.insert(node.id.as_str().to_string(), node.clone());
                    }
                }
            }
            change.mapped_symbols = mapped_ids.len();
            change.reason = file_reason(store, change, &facts.nodes)?;
            if ranges.iter().any(|hunk| hunk.deleted_only) {
                Some(UnmappedReason::DeletedContentNotInCurrentGraph)
            } else if mapped_ids.is_empty() {
                Some(UnmappedReason::NoSymbolOverlap)
            } else {
                None
            }
        };
        if let Some(reason) = reason {
            unmapped_files.push(unmapped(change, reason));
        }
    }
    let changed: Vec<Node> = changed_by_id.into_values().collect();
    // Node ids before the test filter: `--expect` asks whether a dependent
    // was edited, and an edited test is an edited dependent.
    let changed_ids: BTreeSet<String> = changed
        .iter()
        .map(|node| node.id.as_str().to_string())
        .collect();

    // A constant's readers are not a function's callers: reach them from
    // their own seeds so those rows can be marked and sorted last.
    let (const_seeds, code_seeds): (Vec<Node>, Vec<Node>) =
        changed.iter().cloned().partition(|node| {
            matches!(
                node.kind,
                SymbolKind::Constant | SymbolKind::Static | SymbolKind::Variable
            )
        });
    let mut radius = blast_radius(store, filter, &code_seeds)?;
    let mut via = BTreeMap::new();
    for (id, node) in blast_radius(store, filter, &const_seeds)? {
        if let std::collections::btree_map::Entry::Vacant(slot) = radius.entry(id) {
            via.insert(
                symbol_key(&node.file, qualified_of(slot.key())),
                "uses/const",
            );
            slot.insert(node);
        }
    }
    for id in &changed_ids {
        radius.remove(id);
    }

    let scope_index = store.scope_index()?;
    let is_test_node = |n: &Node| {
        test_capable_kind(n) && (scope_index.scope_of(n) == CorpusScope::Test || is_test(n))
    };
    // Nearest tests first: direct callers of a changed symbol, then the
    // changed symbols' own packages, then everything reached transitively.
    let mut direct: BTreeSet<String> = BTreeSet::new();
    for node in &changed {
        for reached in store.dependents(&node.id, filter, 1)? {
            direct.insert(symbol_key(
                &reached.node.file,
                qualified_of(reached.node.id.as_str()),
            ));
        }
    }
    let changed_packages: BTreeSet<PathBuf> = changed
        .iter()
        .map(|node| package_dir(repo, &node.file))
        .collect();
    let mut affected_tests = affected_tests(store, &radius, &changed)?;
    affected_tests.sort_by_cached_key(|test| {
        let distance = if direct.contains(&symbol_key(&test.file, &test.qualified)) {
            0
        } else if changed_packages.contains(&package_dir(repo, &test.file)) {
            1
        } else {
            2
        };
        (distance, test.file.clone(), test.qualified.clone())
    });
    let test_commands: BTreeMap<String, String> = radius
        .values()
        .chain(changed.iter())
        .filter(|node| is_test_node(node))
        .filter_map(|node| {
            let command = test_command(repo, node)?;
            Some((
                symbol_key(&node.file, qualified_of(node.id.as_str())),
                command,
            ))
        })
        .collect();
    let validation = validation_steps(&test_commands);

    // `#[cfg]` twins are one definition to a reader: fold same-named,
    // same-kind rows in one file and remember how many were folded.
    let mut variants: BTreeMap<String, usize> = BTreeMap::new();
    let mut changed_symbols: Vec<SymbolRef> = Vec::new();
    for node in changed.iter().filter(|n| !is_test_node(n)) {
        let key = symbol_key(&node.file, qualified_of(node.id.as_str()));
        let seen = variants.entry(key).or_default();
        *seen += 1;
        if *seen == 1 {
            changed_symbols.push(symbol_ref(node));
        }
    }
    variants.retain(|_, count| *count > 1);

    let mut partial_reasons = Vec::new();
    let mut partial_reason_notes = BTreeMap::new();
    if unmapped_files.iter().any(mapping_gap) {
        partial_reasons.push("one_or_more_changed_files_are_not_fully_mapped");
    }
    // Dirty paths the graph never indexes cannot have moved a span.
    let dirty_indexed = working_tree_changes.iter().any(|change| {
        sinter_extract::spec_for_path(&change.path).is_some()
            && !crate::corpus::excluded(&change.path)
    });
    if historical && dirty_indexed {
        partial_reasons.push("historical_diff_uses_a_dirty_working_tree_graph");
    }
    if !historical_endpoint_matches_head {
        let reason = "historical_diff_endpoint_does_not_match_graph_head";
        partial_reasons.push(reason);
        partial_reason_notes.insert(reason, "symbol attribution may be off; prefer file rows");
    }
    if !untracked_included
        && working_tree_changes.iter().any(|change| {
            change.index_status == "untracked" || change.worktree_status == "untracked"
        })
    {
        partial_reasons.push("untracked_files_are_not_included_in_git_diff");
    }
    if working_tree_changes.iter().any(|change| change.conflicted) {
        partial_reasons.push("working_tree_has_unmerged_paths");
    }
    let analysis_status = if partial_reasons.is_empty() {
        AnalysisStatus::Complete
    } else {
        AnalysisStatus::Partial
    };

    Ok(ImpactReport {
        rev_range: rev_range.to_string(),
        analysis_status,
        partial_reasons,
        partial_reason_notes,
        working_tree_dirty,
        changed_files,
        working_tree_changes,
        unmapped_files,
        changed_symbols,
        blast_radius: radius.values().map(symbol_ref).collect(),
        affected_tests,
        expect: Vec::new(),
        validation,
        changed_ids,
        test_commands,
        via,
        variants,
    })
}

/// Nearest ancestor directory carrying a package manifest of any supported
/// ecosystem, or the repository root. Coarser than `package_of` on purpose:
/// this only decides "same package as a changed file".
fn package_dir(repo: &Path, file: &str) -> PathBuf {
    let mut dir = Path::new(file).parent();
    while let Some(current) = dir {
        if [
            "Cargo.toml",
            "go.mod",
            "package.json",
            "pyproject.toml",
            "setup.py",
        ]
        .iter()
        .any(|manifest| repo.join(current).join(manifest).is_file())
        {
            return current.to_path_buf();
        }
        dir = current.parent();
    }
    PathBuf::new()
}

/// `rev_range` of `None` diffs the working tree (or, with `staged`, the
/// index) against `HEAD`.
#[allow(clippy::too_many_arguments)] // mirrors the clap subcommand one-to-one
pub fn run(
    repo: &Path,
    rev_range: Option<&str>,
    staged: bool,
    manifest: Option<&Path>,
    expect: &[String],
    evidence: &[String],
    certain: bool,
    limit: usize,
    full: bool,
    json: bool,
) -> Result<()> {
    let filter = crate::lookup::edge_filter(evidence, certain)?;
    let rev_range = rev_range.unwrap_or("HEAD");
    let mut report = {
        // One store handle for both passes: workspace mode below reopens
        // member stores, and redb forbids a second open of the same file.
        let repo = repo.canonicalize()?;
        let store = open_store(&repo)?;
        let mut report = compute_with_store_mode(&repo, rev_range, staged, &filter, &store)?;
        report.expect = expect_reports(&repo, &store, &filter, expect, &report, limit)?;
        report
    };
    report
        .validation
        .truncate(returned_count(report.validation.len(), limit));
    // Workspace mode: follow boundary links out of the changed member and
    // continue the blast radius inside the other members.
    if let Some(manifest) = manifest {
        let ws = crate::workspace::load(manifest)?;
        let repo_canon = repo.canonicalize()?;
        let member = ws
            .members
            .iter()
            .find(|(_, path)| **path == repo_canon)
            .map(|(name, _)| name.clone())
            .ok_or_else(|| anyhow::anyhow!("--repo is not a member of this workspace"))?;
        // Resolve changed symbols to node ids first, then drop the handle:
        // workspace traversal opens every member store itself, and redb
        // forbids a second open of the same file in-process.
        let changed_ids: Vec<sinter_core::NodeId> = {
            let store = open_store(&repo_canon)?;
            report
                .changed_symbols
                .iter()
                .filter_map(|c| {
                    crate::lookup::unique_symbol(&store, &c.qualified)
                        .ok()
                        .map(|n| n.id)
                })
                .collect()
        };
        let mut cross: std::collections::BTreeMap<String, SymbolRef> =
            std::collections::BTreeMap::new();
        for node_id in &changed_ids {
            for reached in crate::workspace::dependents(&ws, &member, node_id, &filter, 25)? {
                if reached.member == member {
                    continue; // local radius already counted
                }
                let key = format!("{}:{}", reached.member, reached.node.id.as_str());
                let mut sym = symbol_ref(&reached.node);
                sym.file = format!("{}:{}", reached.member, sym.file);
                if is_test(&reached.node) {
                    report.affected_tests.push(sym.clone());
                }
                cross.insert(key, sym);
            }
        }
        report.blast_radius.extend(cross.into_values());
    }
    if json {
        crate::agent_protocol::write_json(&to_json(&report, limit))?;
        return Ok(());
    }
    let status = match report.analysis_status {
        AnalysisStatus::Complete => "complete",
        AnalysisStatus::Partial => "partial",
    };
    println!(
        "impact {}: {status}; {} changed files, {} changed symbols, {} in blast radius, {} tests affected",
        report.rev_range,
        report.changed_files.len(),
        report.changed_symbols.len(),
        report.blast_radius.len(),
        report.affected_tests.len()
    );
    for reason in &report.partial_reasons {
        match report.partial_reason_notes.get(reason) {
            Some(note) => println!("  partial: {reason} ({note})"),
            None => println!("  partial: {reason}"),
        }
    }
    if report.working_tree_dirty {
        println!(
            "  note: working tree has {} uncommitted path(s); see working_tree_changes in --json output",
            report.working_tree_changes.len()
        );
    }
    for expected in &report.expect {
        if let Some(note) = &expected.note {
            println!("expect {}: {note}", expected.symbol);
        }
        if expected.body_only {
            println!(
                "expect {} ({}): body-only change; callers unaffected ({} direct dependents)",
                expected.symbol, expected.file, expected.direct_dependents
            );
            continue;
        }
        println!(
            "expect {} ({}): {} direct dependents; {} changed, {} expected but untouched",
            expected.symbol,
            expected.file,
            expected.direct_dependents,
            expected.changed_total,
            expected.untouched_total
        );
        print_expect_sites(
            "  expected but untouched",
            &expected.untouched,
            expected.untouched_total,
        );
        print_expect_sites("  changed", &expected.changed, expected.changed_total);
    }
    // With `--expect` the owed sites are the answer; the radius is context
    // and gets a short preview unless `--full` asks for it.
    let radius_limit = if !report.expect.is_empty() && !full {
        if limit == 0 { 5 } else { limit.min(5) }
    } else {
        limit
    };
    if !report.validation.is_empty() {
        println!("validation (recommended; repository instructions take precedence):");
        for step in &report.validation {
            println!("  {}  — {}", step.command, step.reason);
        }
    }
    print_symbols_with_commands(
        "affected tests",
        &report.affected_tests,
        radius_limit,
        &report.test_commands,
    );
    print_blast_radius(&report, &filter, radius_limit);
    print_changed("changed", &report.changed_symbols, limit, &report.variants);
    println!("changed files: {}", report.changed_files.len());
    for file in &report.changed_files {
        let path = match &file.old_path {
            Some(old) => format!("{old} -> {}", file.path),
            None => file.path.clone(),
        };
        // Drop the long-form explanations: `(5 symbols, 12 dependents)`,
        // `(not indexed)`, `(new file, 0 inbound edges)`.
        let reason = file
            .reason
            .strip_prefix("indexed, ")
            .unwrap_or(&file.reason);
        let reason = reason.split(" (").next().unwrap_or(reason);
        println!("  {}  {path}  ({reason})", file.git_status);
    }
    print_unmapped(&report.unmapped_files);
    let capped = [
        report.changed_symbols.len(),
        by_file(&report.blast_radius, &report.via).len(),
        report.affected_tests.len(),
    ]
    .iter()
    .map(|&total| truncated_count(total, limit))
    .sum::<usize>();
    if capped > 0 {
        println!(
            "{capped} row(s) truncated by the default cap · `sinter impact --limit 0` returns all"
        );
    }
    Ok(())
}

/// One blast-radius row per file: `(file, symbols in it, first names,
/// reached only through a constant)`. Production files first, then test
/// harness files; inside each, rows reached only via `uses/const` sink
/// below real code dependents; busiest first after that. A file row is
/// what a reader acts on; 500 symbol rows are not.
fn by_file<'a>(
    symbols: &'a [SymbolRef],
    via: &BTreeMap<String, &'static str>,
) -> Vec<(&'a str, usize, Vec<&'a str>, bool)> {
    let mut files: BTreeMap<&str, (usize, Vec<&str>, bool)> = BTreeMap::new();
    for symbol in symbols {
        let entry = files.entry(&symbol.file).or_insert((0, Vec::new(), true));
        entry.0 += 1;
        entry.2 &= via.contains_key(&symbol_key(&symbol.file, &symbol.qualified));
        // The file node is the row itself; naming it again says nothing.
        if entry.1.len() < 3 && symbol.kind != "file" {
            entry.1.push(&symbol.qualified);
        }
    }
    let mut rows: Vec<_> = files
        .into_iter()
        .map(|(file, (count, names, const_only))| (file, count, names, const_only))
        .collect();
    rows.sort_by(|a, b| {
        test_path(a.0)
            .cmp(&test_path(b.0))
            .then_with(|| a.3.cmp(&b.3))
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.0.cmp(b.0))
    });
    rows
}

fn print_blast_radius(report: &ImpactReport, filter: &EdgeFilter, limit: usize) {
    if report.blast_radius.is_empty() && !report.changed_symbols.is_empty() {
        let scope = filter.scopes.as_ref().map_or("all".to_string(), |set| {
            set.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",")
        });
        println!("blast radius empty: changed symbols have no dependents in scope {scope}");
        return;
    }
    let rows = by_file(&report.blast_radius, &report.via);
    let returned = returned_count(rows.len(), limit);
    println!(
        "blast radius: {} symbols in {} files, {returned} files shown, {} truncated",
        report.blast_radius.len(),
        rows.len(),
        rows.len() - returned
    );
    for (file, count, names, const_only) in rows.iter().take(returned) {
        let more = count - names.len();
        let mut names = names.join(", ");
        if more > 0 {
            names.push_str(&format!(", +{more} more"));
        }
        if *const_only {
            names.push_str("  [uses/const]");
        }
        println!("  {count:>4}  {file}  {names}");
    }
}

/// One line per unmapped reason: `unmapped: a, b (not indexed)`.
fn print_unmapped(files: &[UnmappedFile]) {
    let mut by_reason: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for file in files {
        let reason = serde_json::to_value(file.reason)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.replace('_', " ")))
            .unwrap_or_default();
        by_reason.entry(reason).or_default().push(&file.path);
    }
    for (reason, paths) in by_reason {
        println!("unmapped: {} ({reason})", paths.join(", "));
    }
}

fn print_expect_sites(label: &str, sites: &[ExpectSite], total: usize) {
    println!("{label}: {} shown, {total} total", sites.len());
    for site in sites {
        println!(
            "    {}x {} {}  {}",
            site.sites, site.kind, site.qualified, site.at
        );
    }
}

fn returned_count(total: usize, limit: usize) -> usize {
    if limit == 0 { total } else { total.min(limit) }
}

fn truncated_count(total: usize, limit: usize) -> usize {
    total - returned_count(total, limit)
}

/// `print_symbols` plus the runnable command for each symbol that has one.
fn print_symbols_with_commands(
    label: &str,
    symbols: &[SymbolRef],
    limit: usize,
    commands: &BTreeMap<String, String>,
) {
    let returned = returned_count(symbols.len(), limit);
    println!(
        "{label}: {returned} shown, {} total, {} truncated",
        symbols.len(),
        symbols.len() - returned
    );
    for symbol in symbols.iter().take(returned) {
        match commands.get(&symbol_key(&symbol.file, &symbol.qualified)) {
            Some(command) => println!("  {command}"),
            None => println!("  {} {}  {}", symbol.kind, symbol.qualified, symbol.file),
        }
    }
}

fn print_changed(
    label: &str,
    symbols: &[SymbolRef],
    limit: usize,
    variants: &BTreeMap<String, usize>,
) {
    let returned = returned_count(symbols.len(), limit);
    let truncated = symbols.len() - returned;
    println!(
        "{label}: {returned} shown, {} total, {truncated} truncated",
        symbols.len()
    );
    for symbol in symbols.iter().take(returned) {
        match variants.get(&symbol_key(&symbol.file, &symbol.qualified)) {
            Some(n) => println!(
                "  {} {}  {}  [{n} cfg variants]",
                symbol.kind, symbol.qualified, symbol.file
            ),
            None => println!("  {} {}  {}", symbol.kind, symbol.qualified, symbol.file),
        }
    }
}

/// Also usable by the MCP server.
pub fn to_json(report: &ImpactReport, limit: usize) -> serde_json::Value {
    let totals = serde_json::json!({
        "changed_files": report.changed_files.len(),
        "working_tree_changes": report.working_tree_changes.len(),
        "unmapped_files": report.unmapped_files.len(),
        "changed_symbols": report.changed_symbols.len(),
        "blast_radius": report.blast_radius.len(),
        "affected_tests": report.affected_tests.len(),
    });
    let truncated = serde_json::json!({
        "changed_files": truncated_count(report.changed_files.len(), limit),
        "working_tree_changes": truncated_count(report.working_tree_changes.len(), limit),
        "unmapped_files": truncated_count(report.unmapped_files.len(), limit),
        "changed_symbols": truncated_count(report.changed_symbols.len(), limit),
        "blast_radius": truncated_count(report.blast_radius.len(), limit),
        "affected_tests": truncated_count(report.affected_tests.len(), limit),
    });
    let mut value = serde_json::to_value(report).expect("impact report serializes");
    for (name, total) in [
        ("changed_files", report.changed_files.len()),
        ("working_tree_changes", report.working_tree_changes.len()),
        ("unmapped_files", report.unmapped_files.len()),
        ("changed_symbols", report.changed_symbols.len()),
        ("blast_radius", report.blast_radius.len()),
        ("affected_tests", report.affected_tests.len()),
    ] {
        value[name]
            .as_array_mut()
            .expect("impact symbol collection serializes as an array")
            .truncate(returned_count(total, limit));
    }
    value["limit"] = serde_json::json!(limit);
    value["totals"] = totals;
    value["truncated"] = truncated;
    value
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::{
        AnalysisStatus, ChangedFile, FileChangeKind, ImpactReport, SymbolRef, UnmappedFile,
        UnmappedReason, WorkingTreeChange, changed_files, compute, compute_with_store_mode,
        expect_reports, is_test, test_command, to_json, working_tree_changes,
    };
    use sinter_core::{Node, NodeId, Span, SymbolKind};
    use tempfile::TempDir;

    fn node(id: &str, name: &str, file: &str) -> Node {
        Node {
            id: NodeId::new(id),
            kind: SymbolKind::Function,
            name: name.to_string(),
            file: file.to_string(),
            span: Span { start: 0, end: 1 },
            signature: String::new(),
            doc: None,
        }
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(repo: &Path, path: &str, contents: &str) {
        let path = repo.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(path, contents).expect("write fixture");
    }

    fn repository() -> TempDir {
        let repo = TempDir::new().expect("temp repo");
        git(repo.path(), &["init", "-q"]);
        git(
            repo.path(),
            &["config", "user.email", "impact@example.test"],
        );
        git(repo.path(), &["config", "user.name", "Impact Test"]);
        write(repo.path(), ".gitignore", ".sinter/\n");
        write(
            repo.path(),
            "Cargo.toml",
            "[package]\nname = \"impact-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 1 }\n");
        write(repo.path(), "deleted.rs", "pub fn removed() {}\n");
        write(repo.path(), "old.rs", "pub fn renamed() {}\n");
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "base"]);
        repo
    }

    fn symbols(prefix: &str, count: usize) -> Vec<SymbolRef> {
        (0..count)
            .map(|index| SymbolRef {
                qualified: format!("{prefix}_{index}"),
                kind: "function",
                file: format!("{prefix}_{index}.rs"),
            })
            .collect()
    }

    fn report_for_budget() -> ImpactReport {
        ImpactReport {
            rev_range: "HEAD~1..HEAD".to_string(),
            analysis_status: AnalysisStatus::Complete,
            partial_reasons: Vec::new(),
            working_tree_dirty: false,
            changed_files: (0..3)
                .map(|index| ChangedFile {
                    path: format!("changed-file-{index}"),
                    old_path: None,
                    git_status: "M".to_string(),
                    kind: FileChangeKind::Modified,
                    mapped_symbols: 1,
                    reason: String::new(),
                })
                .collect(),
            working_tree_changes: (0..4)
                .map(|index| WorkingTreeChange {
                    path: format!("working-tree-{index}"),
                    old_path: None,
                    index_status: "unmodified",
                    worktree_status: "modified",
                    conflicted: false,
                })
                .collect(),
            unmapped_files: vec![UnmappedFile {
                path: "unmapped-file".to_string(),
                old_path: None,
                git_status: "M".to_string(),
                reason: UnmappedReason::NoSymbolOverlap,
                detail: String::new(),
            }],
            changed_symbols: symbols("changed", 3),
            blast_radius: symbols("blast", 4),
            affected_tests: symbols("test", 1),
            expect: Vec::new(),
            validation: Vec::new(),
            changed_ids: Default::default(),
            test_commands: Default::default(),
            partial_reason_notes: Default::default(),
            via: Default::default(),
            variants: Default::default(),
        }
    }

    #[test]
    fn impact_budget_caps_each_collection_independently_and_preserves_totals() {
        let value = to_json(&report_for_budget(), 2);

        assert_eq!(value["limit"], 2);
        assert_eq!(value["changed_files"].as_array().unwrap().len(), 2);
        assert_eq!(value["working_tree_changes"].as_array().unwrap().len(), 2);
        assert_eq!(value["unmapped_files"].as_array().unwrap().len(), 1);
        assert_eq!(value["changed_symbols"].as_array().unwrap().len(), 2);
        assert_eq!(value["blast_radius"].as_array().unwrap().len(), 2);
        assert_eq!(value["affected_tests"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["totals"],
            serde_json::json!({
                "changed_files": 3,
                "working_tree_changes": 4,
                "unmapped_files": 1,
                "changed_symbols": 3,
                "blast_radius": 4,
                "affected_tests": 1,
            })
        );
        assert_eq!(
            value["truncated"],
            serde_json::json!({
                "changed_files": 1,
                "working_tree_changes": 2,
                "unmapped_files": 0,
                "changed_symbols": 1,
                "blast_radius": 2,
                "affected_tests": 0,
            })
        );
    }

    #[test]
    fn zero_impact_budget_returns_every_entry() {
        let value = to_json(&report_for_budget(), 0);

        assert_eq!(value["limit"], 0);
        assert_eq!(value["changed_files"].as_array().unwrap().len(), 3);
        assert_eq!(value["working_tree_changes"].as_array().unwrap().len(), 4);
        assert_eq!(value["unmapped_files"].as_array().unwrap().len(), 1);
        assert_eq!(value["changed_symbols"].as_array().unwrap().len(), 3);
        assert_eq!(value["blast_radius"].as_array().unwrap().len(), 4);
        assert_eq!(value["affected_tests"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["truncated"],
            serde_json::json!({
                "changed_files": 0,
                "working_tree_changes": 0,
                "unmapped_files": 0,
                "changed_symbols": 0,
                "blast_radius": 0,
                "affected_tests": 0,
            })
        );
    }

    #[test]
    fn detects_conventional_test_files_and_names() {
        // One case per convention.
        for (id, name, file) in [
            ("a_test.go#f", "f", "a_test.go"),                  // Go
            ("a_test.py#f", "f", "a_test.py"),                  // Python file
            ("FooTests.cs#F", "F", "FooTests.cs"),              // C#
            ("app.test.ts#f", "f", "app.test.ts"),              // JS/TS .test.
            ("app.spec.js#f", "f", "app.spec.js"),              // JS/TS .spec.
            ("tests/x.rs#f", "f", "tests/x.rs"),                // tests/ dir
            ("crate/tests/x.rs#f", "f", "crate/tests/x.rs"),    // nested tests/
            ("pkg/test/x.py#f", "f", "pkg/test/x.py"),          // test/ dir
            ("m.py#test_f", "test_f", "m.py"),                  // test_ prefix
            ("M.go#TestF", "TestF", "M.go"),                    // Test prefix
            ("src/lib.rs#tests::works", "works", "src/lib.rs"), // #[cfg(test)] mod
        ] {
            assert!(is_test(&node(id, name, file)), "{file} {name}");
        }
    }

    #[test]
    fn plain_symbols_are_not_tests() {
        for (id, name, file) in [
            ("src/lib.rs#build", "build", "src/lib.rs"),
            ("src/attest.rs#attest", "attest", "src/attest.rs"),
            ("contest.py#run", "run", "contest.py"),
        ] {
            assert!(!is_test(&node(id, name, file)), "{file} {name}");
        }
    }

    #[test]
    fn impact_reports_commit_range_paths_including_config_deletion_and_rename() {
        let repo = repository();
        write(
            repo.path(),
            "Cargo.toml",
            "[package]\nname = \"impact-fixture\"\nversion = \"0.2.0\"\nedition = \"2024\"\n",
        );
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 2 }\n");
        fs::remove_file(repo.path().join("deleted.rs")).expect("delete fixture");
        git(repo.path(), &["mv", "old.rs", "renamed.rs"]);
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "mixed changes"]);

        let files = changed_files(repo.path(), "HEAD~1..HEAD", &[]).expect("collect changed paths");
        assert!(
            files
                .iter()
                .any(|file| { file.path == "Cargo.toml" && file.kind == FileChangeKind::Modified })
        );
        assert!(
            files
                .iter()
                .any(|file| file.path == "deleted.rs" && file.kind == FileChangeKind::Deleted)
        );
        assert!(files.iter().any(|file| {
            file.path == "renamed.rs"
                && file.old_path.as_deref() == Some("old.rs")
                && file.kind == FileChangeKind::Renamed
        }));

        crate::pipeline::build(repo.path(), None).expect("build fixture graph");
        let report = compute(repo.path(), "HEAD~1..HEAD").expect("compute impact");
        assert_eq!(report.analysis_status, AnalysisStatus::Partial);
        assert_eq!(report.changed_files.len(), 4);
        assert!(report.unmapped_files.iter().any(|file| {
            file.path == "Cargo.toml" && file.reason == UnmappedReason::NotIndexed
        }));
        assert!(
            report.unmapped_files.iter().any(|file| {
                file.path == "deleted.rs" && file.reason == UnmappedReason::Deleted
            })
        );
        assert!(
            report
                .changed_files
                .iter()
                .any(|file| { file.path == "renamed.rs" && file.mapped_symbols > 0 })
        );
        assert!(
            report
                .changed_symbols
                .iter()
                .any(|symbol| symbol.file == "src/lib.rs")
        );
    }

    #[test]
    fn impact_reports_staged_unstaged_renamed_deleted_and_untracked_states() {
        let repo = repository();
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 2 }\n");
        git(repo.path(), &["add", "src/lib.rs"]);
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 3 }\n");
        fs::remove_file(repo.path().join("deleted.rs")).expect("delete fixture");
        git(repo.path(), &["mv", "old.rs", "renamed.rs"]);
        write(repo.path(), "untracked.rs", "pub fn untracked() {}\n");
        write(
            repo.path(),
            "graphify-out/cache/derived.json",
            "generated\n",
        );

        let changes = working_tree_changes(repo.path()).expect("collect working tree state");
        let modified = changes
            .iter()
            .find(|change| change.path == "src/lib.rs")
            .expect("modified path");
        assert_eq!(modified.index_status, "modified");
        assert_eq!(modified.worktree_status, "modified");
        let deleted = changes
            .iter()
            .find(|change| change.path == "deleted.rs")
            .expect("deleted path");
        assert_eq!(deleted.index_status, "unmodified");
        assert_eq!(deleted.worktree_status, "deleted");
        let renamed = changes
            .iter()
            .find(|change| change.path == "renamed.rs")
            .expect("renamed path");
        assert_eq!(renamed.old_path.as_deref(), Some("old.rs"));
        assert_eq!(renamed.index_status, "renamed");
        let untracked = changes
            .iter()
            .find(|change| change.path == "untracked.rs")
            .expect("untracked path");
        assert_eq!(untracked.index_status, "untracked");
        assert_eq!(untracked.worktree_status, "untracked");
        assert!(
            changes
                .iter()
                .all(|change| !change.path.starts_with("graphify-out/")),
            "derived roots must not affect graph-relative working tree state"
        );
        assert!(
            changes
                .iter()
                .position(|change| change.path == "untracked.rs")
                .is_some_and(|position| {
                    changes[..position]
                        .iter()
                        .all(|change| !super::is_untracked(change))
                }),
            "tracked changes must win the bounded output budget: {changes:#?}"
        );
    }

    #[test]
    fn historical_impact_is_partial_when_endpoint_is_not_graph_head() {
        let repo = repository();
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 2 }\n");
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-qm", "second"]);
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 3 }\n");
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-qm", "third"]);
        crate::pipeline::build(repo.path(), None).expect("build fixture graph");

        let report = compute(repo.path(), "HEAD~2..HEAD~1").expect("compute historical impact");

        assert_eq!(report.analysis_status, AnalysisStatus::Partial);
        assert!(
            report
                .partial_reasons
                .contains(&"historical_diff_endpoint_does_not_match_graph_head")
        );
        assert!(!report.working_tree_dirty);
    }

    #[test]
    fn historical_impact_is_complete_when_endpoint_is_graph_head() {
        let repo = repository();
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 2 }\n");
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-qm", "second"]);
        crate::pipeline::build(repo.path(), None).expect("build fixture graph");

        let report = compute(repo.path(), "HEAD~1..HEAD").expect("compute current impact");

        assert_eq!(report.analysis_status, AnalysisStatus::Complete);
        assert!(report.partial_reasons.is_empty());
    }

    #[test]
    fn manifest_and_lockfile_changes_are_unmapped_but_not_partial() {
        let repo = repository();
        write(repo.path(), "Cargo.lock", "# lockfile\n");
        git(repo.path(), &["add", "Cargo.lock"]);
        git(repo.path(), &["commit", "-qm", "lock"]);
        crate::pipeline::build(repo.path(), None).expect("build fixture graph");

        let report = compute(repo.path(), "HEAD~1..HEAD").expect("compute lockfile impact");

        assert_eq!(report.analysis_status, AnalysisStatus::Complete);
        assert!(report.partial_reasons.is_empty());
        assert!(report.unmapped_files.iter().any(|file| {
            file.path == "Cargo.lock" && file.reason == UnmappedReason::NotIndexed
        }));
    }

    #[test]
    fn unindexed_non_source_paths_are_never_mapping_gaps() {
        let unmapped = |path: &str, reason: UnmappedReason| UnmappedFile {
            path: path.to_string(),
            old_path: None,
            git_status: "M".to_string(),
            reason,
            detail: String::new(),
        };
        for path in [
            ".claude/hooks/sinter-first.sh",
            "harness/eval/agent-flows.json",
            "crates/sinter-extract/queries/rust.scm",
            ".cursor/rules/sinter.mdc",
            "Cargo.lock",
        ] {
            assert!(
                !super::mapping_gap(&unmapped(path, UnmappedReason::NotIndexed)),
                "{path}"
            );
        }
        assert!(super::mapping_gap(&unmapped(
            "src/lib.rs",
            UnmappedReason::NotIndexed
        )));
        assert!(super::mapping_gap(&unmapped(
            "notes.json",
            UnmappedReason::NoSymbolOverlap
        )));
    }

    #[test]
    fn blast_radius_rolls_up_by_file_busiest_first() {
        let mut radius = symbols("a", 4);
        radius.iter_mut().for_each(|s| s.file = "a.rs".to_string());
        radius[3].kind = "file";
        radius.extend(symbols("b", 1));
        let rows = super::by_file(&radius, &Default::default());
        assert_eq!(rows[0].0, "a.rs");
        assert_eq!(rows[0].1, 4);
        assert_eq!(rows[0].2, vec!["a_0", "a_1", "a_2"]);
        assert_eq!(rows[1], ("b_0.rs", 1, vec!["b_0"], false));
    }

    #[test]
    fn blast_radius_sorts_test_files_and_const_readers_last() {
        // Busiest file is a test harness; a big const-only reader file;
        // one small production caller. Production first, const-only next,
        // tests last.
        let mut radius = symbols("harness", 6);
        radius
            .iter_mut()
            .for_each(|s| s.file = "tests/golden.rs".to_string());
        let mut readers = symbols("reader", 3);
        readers
            .iter_mut()
            .for_each(|s| s.file = "src/readers.rs".to_string());
        radius.extend(readers);
        radius.extend(symbols("caller", 1));
        let via: std::collections::BTreeMap<String, &'static str> = (0..3)
            .map(|i| (format!("src/readers.rs:reader_{i}"), "uses/const"))
            .collect();
        let rows = super::by_file(&radius, &via);
        let order: Vec<(&str, bool)> = rows.iter().map(|r| (r.0, r.3)).collect();
        assert_eq!(
            order,
            [
                ("caller_0.rs", false),
                ("src/readers.rs", true),
                ("tests/golden.rs", false)
            ]
        );
    }

    #[test]
    fn historical_range_attributes_symbols_from_the_endpoint_tree() {
        // Commit 2 edits `value`; commit 3 adds `later`, which overlaps the
        // line hunk of commit 2 in today's tree. Attribution must follow
        // the endpoint's own definitions, never the head layout.
        let repo = repository();
        write(
            repo.path(),
            "src/lib.rs",
            "pub fn value() -> u32 {\n    2\n}\n",
        );
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-qm", "second"]);
        write(
            repo.path(),
            "src/lib.rs",
            "pub fn later() -> u32 {\n    9\n}\n\npub fn value() -> u32 {\n    2\n}\n",
        );
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-qm", "third"]);
        crate::pipeline::build(repo.path(), None).expect("build fixture graph");

        let report = compute(repo.path(), "HEAD~2..HEAD~1").expect("compute historical impact");

        let names: Vec<&str> = report
            .changed_symbols
            .iter()
            .map(|s| s.qualified.as_str())
            .collect();
        assert_eq!(names, ["value"], "{names:?}");
        assert_eq!(
            report
                .partial_reason_notes
                .get("historical_diff_endpoint_does_not_match_graph_head")
                .copied(),
            Some("symbol attribution may be off; prefer file rows")
        );
        let json = to_json(&report, 0);
        assert!(json["partial_reason_notes"].is_object(), "{json}");
    }

    #[test]
    fn historical_symbol_missing_at_head_falls_back_to_the_file_row() {
        let repo = repository();
        write(
            repo.path(),
            "src/lib.rs",
            "pub fn value() -> u32 { 1 }\npub fn gone() -> u32 { 2 }\n",
        );
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-qm", "add gone"]);
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 1 }\n");
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-qm", "remove gone"]);
        crate::pipeline::build(repo.path(), None).expect("build fixture graph");

        let report = compute(repo.path(), "HEAD~2..HEAD~1").expect("compute historical impact");

        assert!(
            report
                .changed_symbols
                .iter()
                .all(|s| s.qualified != "value"),
            "{:?}",
            report.changed_symbols
        );
        assert!(
            report
                .changed_ids
                .iter()
                .any(|id| id.ends_with("src/lib.rs")),
            "file row expected: {:?}",
            report.changed_ids
        );
    }

    #[test]
    fn cfg_twins_fold_into_one_changed_row() {
        let repo = repository();
        write(
            repo.path(),
            "src/lib.rs",
            "#[cfg(unix)]\npub fn value() -> u32 { 2 }\n#[cfg(not(unix))]\npub fn value() -> u32 { 3 }\n",
        );
        crate::pipeline::build(repo.path(), None).expect("build fixture graph");

        let report = compute(repo.path(), "HEAD").expect("compute working-tree impact");

        let rows: Vec<&str> = report
            .changed_symbols
            .iter()
            .map(|s| s.qualified.as_str())
            .collect();
        assert_eq!(rows, ["value"]);
        assert_eq!(report.variants.get("src/lib.rs:value"), Some(&2));
    }

    #[test]
    fn dirty_unindexed_paths_do_not_make_a_historical_diff_partial() {
        let repo = repository();
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 2 }\n");
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-qm", "second"]);
        crate::pipeline::build(repo.path(), None).expect("build fixture graph");
        write(repo.path(), "notes.json", "{}\n");
        write(repo.path(), "Cargo.lock", "# lock\n");

        let report = compute(repo.path(), "HEAD~1..HEAD").expect("compute historical impact");

        assert!(report.working_tree_dirty);
        assert!(
            !report
                .partial_reasons
                .contains(&"historical_diff_uses_a_dirty_working_tree_graph"),
            "{:?}",
            report.partial_reasons
        );
    }

    #[test]
    fn working_tree_impact_remains_complete_for_tracked_edits() {
        let repo = repository();
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 2 }\n");
        crate::pipeline::build(repo.path(), None).expect("build fixture graph");

        let report = compute(repo.path(), "HEAD").expect("compute working-tree impact");

        assert_eq!(report.analysis_status, AnalysisStatus::Complete);
        assert!(report.working_tree_dirty);
        assert!(report.partial_reasons.is_empty());
    }

    #[test]
    fn markdown_section_is_never_an_affected_test() {
        let mut section = node("docs/testing.md::Testing", "Testing", "docs/testing.md");
        section.kind = SymbolKind::Section;
        assert!(!is_test(&section));
        let mut section = node("tests/README.md::Setup", "Setup", "tests/README.md");
        section.kind = SymbolKind::Section;
        assert!(!is_test(&section));
        assert!(is_test(&node(
            "tests/a.rs::tests::works",
            "works",
            "tests/a.rs"
        )));
    }

    #[test]
    fn sinter_state_is_not_a_working_tree_change() {
        let repo = repository();
        write(repo.path(), ".sinter/graph.redb", "");
        write(repo.path(), "src/new.rs", "pub fn fresh() -> u32 { 4 }\n");
        let changes = working_tree_changes(repo.path()).expect("git status");
        assert!(
            changes.iter().all(|c| !c.path.starts_with(".sinter/")),
            "{changes:?}"
        );
        assert!(changes.iter().any(|c| c.path == "src/new.rs"));
    }

    #[test]
    fn changed_files_explain_their_blast_radius_contribution() {
        let repo = repository();
        write(repo.path(), "src/new.rs", "pub fn fresh() -> u32 { 4 }\n");
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 2 }\n");
        write(repo.path(), "notes.txt", "not code\n");
        crate::pipeline::build(repo.path(), None).expect("build fixture graph");
        let store = crate::lookup::open_store(repo.path()).expect("open store");
        let report = compute_with_store_mode(
            repo.path(),
            "HEAD",
            false,
            &sinter_store::EdgeFilter::default(),
            &store,
        )
        .expect("compute working-tree impact");
        let reason = |path: &str| {
            report
                .changed_files
                .iter()
                .find(|file| file.path == path)
                .unwrap_or_else(|| panic!("{path} listed"))
                .reason
                .clone()
        };
        assert_eq!(
            reason("src/new.rs"),
            "new file, 0 inbound edges (nothing references it yet)"
        );
        assert_eq!(reason("src/lib.rs"), "indexed, 1 symbols, 0 dependents");
        assert_eq!(reason("notes.txt"), super::NOT_INDEXED);
        let unmapped = report
            .unmapped_files
            .iter()
            .find(|file| file.path == "notes.txt")
            .expect("unindexed file is unmapped");
        assert_eq!(unmapped.detail, super::NOT_INDEXED);
        let json = to_json(&report, 0);
        assert_eq!(
            json["changed_files"][0]["reason"]
                .as_str()
                .map(str::is_empty),
            Some(false)
        );
    }

    #[test]
    fn working_tree_impact_includes_untracked_files_and_stays_complete() {
        let repo = repository();
        write(repo.path(), "src/new.rs", "pub fn fresh() -> u32 { 4 }\n");
        crate::pipeline::build(repo.path(), None).expect("build fixture graph");
        let store = crate::lookup::open_store(repo.path()).expect("open store");

        let report = compute_with_store_mode(
            repo.path(),
            "HEAD",
            false,
            &sinter_store::EdgeFilter::default(),
            &store,
        )
        .expect("compute working-tree impact");
        assert_eq!(report.analysis_status, AnalysisStatus::Complete);
        let added = report
            .changed_files
            .iter()
            .find(|file| file.path == "src/new.rs")
            .expect("untracked file listed");
        assert_eq!(added.kind, FileChangeKind::Added);
        assert!(
            report
                .changed_symbols
                .iter()
                .any(|s| s.qualified.contains("fresh"))
        );

        let staged = compute_with_store_mode(
            repo.path(),
            "HEAD",
            true,
            &sinter_store::EdgeFilter::default(),
            &store,
        )
        .expect("compute staged impact");
        assert!(staged.changed_files.is_empty());
        assert!(
            staged
                .partial_reasons
                .contains(&"untracked_files_are_not_included_in_git_diff")
        );
    }

    // ------------------------------------------------------ --expect diff

    /// A library symbol with three callers, one of which the working tree
    /// has already edited.
    fn expect_repository() -> TempDir {
        let repo = repository();
        write(
            repo.path(),
            "src/lib.rs",
            "pub mod a;\npub mod b;\npub mod c;\npub fn target() -> u32 { 1 }\n",
        );
        for module in ["a", "b", "c"] {
            write(
                repo.path(),
                &format!("src/{module}.rs"),
                &format!("pub fn call_{module}() -> u32 {{ crate::target() }}\n"),
            );
        }
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "callers"]);
        repo
    }

    fn expect_for(repo: &Path, symbol: &str) -> super::ExpectReport {
        crate::pipeline::build(repo, None).expect("build fixture graph");
        let store = crate::lookup::open_store(repo).expect("open store");
        let filter = sinter_store::EdgeFilter::default();
        let report = compute_with_store_mode(repo, "HEAD", false, &filter, &store)
            .expect("compute working-tree impact");
        expect_reports(repo, &store, &filter, &[symbol.to_string()], &report, 0)
            .expect("expect report")
            .pop()
            .expect("one expect entry")
    }

    #[test]
    fn expect_reports_direct_dependents_the_change_set_never_touched() {
        let repo = expect_repository();
        write(
            repo.path(),
            "src/a.rs",
            "pub fn call_a() -> u32 { crate::target() + 1 }\n",
        );

        let expected = expect_for(repo.path(), "target");

        assert_eq!(expected.direct_dependents, 3);
        assert_eq!(expected.changed_total, 1);
        assert_eq!(expected.untouched_total, 2);
        let changed: Vec<&str> = expected
            .changed
            .iter()
            .map(|site| site.qualified.as_str())
            .collect();
        assert_eq!(changed, ["call_a"]);
        let untouched: Vec<&str> = expected
            .untouched
            .iter()
            .map(|site| site.qualified.as_str())
            .collect();
        assert_eq!(untouched, ["call_b", "call_c"]);
        assert!(
            expected.untouched.iter().all(|site| site.at.contains(':')),
            "every untouched site carries file:line: {:?}",
            expected.untouched
        );
    }

    #[test]
    fn expect_reports_nothing_untouched_once_every_dependent_is_edited() {
        let repo = expect_repository();
        for module in ["a", "b", "c"] {
            write(
                repo.path(),
                &format!("src/{module}.rs"),
                &format!("pub fn call_{module}() -> u32 {{ crate::target() + 1 }}\n"),
            );
        }

        let expected = expect_for(repo.path(), "target");

        assert_eq!(expected.changed_total, 3);
        assert_eq!(expected.untouched_total, 0);
        assert!(expected.untouched.is_empty());
    }

    #[test]
    fn expect_body_only_change_owes_callers_nothing() {
        let repo = expect_repository();
        write(
            repo.path(),
            "src/lib.rs",
            "pub mod a;\npub mod b;\npub mod c;\npub fn target() -> u32 { 2 }\n",
        );

        let expected = expect_for(repo.path(), "target");

        assert!(expected.body_only, "{expected:?}");
        assert_eq!(expected.direct_dependents, 3);
    }

    #[test]
    fn expect_signature_change_is_not_body_only() {
        let repo = expect_repository();
        write(
            repo.path(),
            "src/lib.rs",
            "pub mod a;\npub mod b;\npub mod c;\npub fn target(x: u32) -> u32 { x }\n",
        );

        let expected = expect_for(repo.path(), "target");

        assert!(!expected.body_only);
        assert_eq!(expected.untouched_total, 3);
    }

    #[test]
    fn expect_resolves_a_renamed_symbol_at_the_base_rev() {
        let repo = expect_repository();
        write(
            repo.path(),
            "src/lib.rs",
            "pub mod a;\npub mod b;\npub mod c;\npub fn renamed() -> u32 { 1 }\n",
        );
        write(
            repo.path(),
            "src/a.rs",
            "pub fn call_a() -> u32 { crate::renamed() }\n",
        );

        let expected = expect_for(repo.path(), "target");

        assert_eq!(expected.symbol, "target");
        assert!(
            expected
                .note
                .as_deref()
                .is_some_and(|n| n.contains("absent at head")),
            "{expected:?}"
        );
        let untouched: Vec<&str> = expected
            .untouched
            .iter()
            .map(|site| site.qualified.as_str())
            .collect();
        assert_eq!(untouched, ["call_b", "call_c"]);
    }

    #[test]
    fn expect_prefers_candidates_in_changed_files_and_errors_when_still_ambiguous() {
        let repo = expect_repository();
        write(
            repo.path(),
            "src/b.rs",
            "pub fn call_b() -> u32 { crate::target() }\npub fn twin() -> u32 { 1 }\n",
        );
        write(
            repo.path(),
            "src/c.rs",
            "pub fn call_c() -> u32 { crate::target() }\npub fn twin() -> u32 { 2 }\n",
        );
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "twins"]);
        write(
            repo.path(),
            "src/b.rs",
            "pub fn call_b() -> u32 { crate::target() }\npub fn twin() -> u32 { 3 }\n",
        );

        let expected = expect_for(repo.path(), "twin");
        assert_eq!(expected.file, "src/b.rs");

        write(
            repo.path(),
            "src/c.rs",
            "pub fn call_c() -> u32 { crate::target() }\npub fn twin() -> u32 { 4 }\n",
        );
        crate::pipeline::build(repo.path(), None).expect("rebuild");
        let store = crate::lookup::open_store(repo.path()).expect("open store");
        let filter = sinter_store::EdgeFilter::default();
        let report = compute_with_store_mode(repo.path(), "HEAD", false, &filter, &store)
            .expect("compute working-tree impact");
        let error = expect_reports(
            repo.path(),
            &store,
            &filter,
            &["twin".to_string()],
            &report,
            0,
        )
        .expect_err("two changed candidates stay ambiguous");
        assert!(error.to_string().contains("ambiguous"), "{error}");
    }

    #[test]
    fn expect_is_absent_from_json_until_requested() {
        let value = to_json(&report_for_budget(), 0);
        assert!(value.get("expect").is_none());
        assert!(value.get("validation").is_none());
    }

    #[test]
    fn validation_collapses_per_test_commands_onto_their_cargo_target() {
        let commands: std::collections::BTreeMap<String, String> = [
            ("a", "cargo test -p one --lib -- x::a"),
            ("b", "cargo test -p one --lib -- x::b"),
            ("c", "cargo test -p one --test surface -- c"),
        ]
        .into_iter()
        .map(|(key, command)| (key.to_string(), command.to_string()))
        .collect();

        let steps = super::validation_steps(&commands);

        assert_eq!(
            steps
                .iter()
                .map(|step| (step.command.as_str(), step.reason.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    "cargo test -p one --lib",
                    "direct affected test target, 2 affected test(s)"
                ),
                (
                    "cargo test -p one --test surface",
                    "direct affected test target, 1 affected test(s)"
                ),
            ]
        );
    }

    // -------------------------------------------------- runnable commands

    fn cargo_repo(manifest: &str, files: &[&str]) -> TempDir {
        let repo = TempDir::new().expect("temp repo");
        write(repo.path(), "Cargo.toml", manifest);
        for file in files {
            write(repo.path(), file, "\n");
        }
        repo
    }

    const LIB_MANIFEST: &str = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n";

    #[test]
    fn test_command_derives_lib_bin_and_integration_targets() {
        let repo = cargo_repo(
            LIB_MANIFEST,
            &[
                "src/lib.rs",
                "src/impact.rs",
                "src/ask/mod.rs",
                "src/ask/rank.rs",
            ],
        );
        for (id, file, expected) in [
            (
                "src/impact.rs#tests::works",
                "src/impact.rs",
                "cargo test -p fixture --lib -- impact::tests::works",
            ),
            (
                "src/ask/mod.rs#tests::works",
                "src/ask/mod.rs",
                "cargo test -p fixture --lib -- ask::tests::works",
            ),
            (
                "src/ask/rank.rs#tests::works",
                "src/ask/rank.rs",
                "cargo test -p fixture --lib -- ask::rank::tests::works",
            ),
            (
                "src/lib.rs#tests::works",
                "src/lib.rs",
                "cargo test -p fixture --lib -- tests::works",
            ),
        ] {
            assert_eq!(
                test_command(repo.path(), &node(id, "works", file)).as_deref(),
                Some(expected),
                "{file}"
            );
        }

        let repo = cargo_repo(LIB_MANIFEST, &["src/lib.rs", "tests/surface.rs"]);
        assert_eq!(
            test_command(
                repo.path(),
                &node("tests/surface.rs#works", "works", "tests/surface.rs")
            )
            .as_deref(),
            Some("cargo test -p fixture --test surface -- works")
        );

        // A binary-only package with a renamed `[[bin]]`: unit tests belong
        // to that binary, not to a lib that does not exist.
        let repo = cargo_repo(
            "[package]\nname = \"sinter-io\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"sinter\"\npath = \"src/main.rs\"\n",
            &["src/main.rs", "src/impact.rs"],
        );
        assert_eq!(
            test_command(
                repo.path(),
                &node("src/impact.rs#tests::works", "works", "src/impact.rs")
            )
            .as_deref(),
            Some("cargo test -p sinter-io --bin sinter -- impact::tests::works")
        );
    }

    #[test]
    fn test_command_covers_go_python_and_javascript() {
        let repo = cargo_repo("", &["pkg/retry/retry_test.go", "tests/test_x.py"]);
        assert_eq!(
            test_command(
                repo.path(),
                &node(
                    "pkg/retry/retry_test.go#TestBackoff",
                    "TestBackoff",
                    "pkg/retry/retry_test.go"
                )
            )
            .as_deref(),
            Some("go test ./pkg/retry -run '^TestBackoff$'")
        );
        assert_eq!(
            test_command(
                repo.path(),
                &node("tests/test_x.py#TestX::test_y", "test_y", "tests/test_x.py")
            )
            .as_deref(),
            Some("pytest tests/test_x.py::TestX::test_y")
        );
        let js = node("src/a.test.ts#adds", "adds", "src/a.test.ts");
        assert_eq!(
            test_command(repo.path(), &js).as_deref(),
            Some("npm test -- src/a.test.ts")
        );
        write(
            repo.path(),
            "package.json",
            "{\"devDependencies\": {\"vitest\": \"1\"}}",
        );
        assert_eq!(
            test_command(repo.path(), &js).as_deref(),
            Some("npx vitest run src/a.test.ts -t 'adds'")
        );
    }

    #[test]
    fn test_command_finds_the_owning_workspace_member() {
        let repo = cargo_repo(
            "[workspace]\nmembers = [\"crates/one\"]\n",
            &["crates/one/src/lib.rs", "crates/one/src/deep/nest.rs"],
        );
        write(
            repo.path(),
            "crates/one/Cargo.toml",
            "[package]\nname = \"one\"\nversion = \"0.1.0\"\n",
        );
        assert_eq!(
            test_command(
                repo.path(),
                &node(
                    "crates/one/src/deep/nest.rs#tests::works",
                    "works",
                    "crates/one/src/deep/nest.rs"
                )
            )
            .as_deref(),
            Some("cargo test -p one --lib -- deep::nest::tests::works")
        );
    }

    #[test]
    fn test_command_is_none_when_the_layout_does_not_name_one_target() {
        // No manifest anywhere above the file.
        let bare = TempDir::new().expect("temp repo");
        write(bare.path(), "src/lib.rs", "\n");
        assert_eq!(
            test_command(bare.path(), &node("src/lib.rs#tests::x", "x", "src/lib.rs")),
            None
        );

        // A virtual workspace root is not a package.
        let virtual_root = cargo_repo("[workspace]\nmembers = []\n", &["src/lib.rs"]);
        assert_eq!(
            test_command(
                virtual_root.path(),
                &node("src/lib.rs#tests::x", "x", "src/lib.rs")
            ),
            None
        );

        let repo = cargo_repo(
            LIB_MANIFEST,
            &[
                "src/lib.rs",
                "tests/common/mod.rs",
                "benches/bench.rs",
                "notes.md",
            ],
        );
        for (id, file) in [
            // A helper module of an integration crate, not a target itself.
            ("tests/common/mod.rs#helper", "tests/common/mod.rs"),
            ("benches/bench.rs#bench", "benches/bench.rs"),
            // Not Rust: the layout says nothing about how to run it.
            ("notes.md::Testing", "notes.md"),
        ] {
            assert_eq!(
                test_command(repo.path(), &node(id, "x", file)),
                None,
                "{file}"
            );
        }

        // Two binaries and no lib: nothing picks one.
        let ambiguous = cargo_repo(
            "[package]\nname = \"two\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"a\"\n\n[[bin]]\nname = \"b\"\n",
            &["src/bin/a.rs", "src/bin/b.rs", "src/shared.rs"],
        );
        assert_eq!(
            test_command(
                ambiguous.path(),
                &node("src/shared.rs#tests::x", "x", "src/shared.rs")
            ),
            None
        );
        // ...but a `src/bin/<name>.rs` names its own target.
        assert_eq!(
            test_command(
                ambiguous.path(),
                &node("src/bin/a.rs#tests::x", "x", "src/bin/a.rs")
            )
            .as_deref(),
            Some("cargo test -p two --bin a -- tests::x")
        );
    }
}
