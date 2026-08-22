//! `sinter impact <rev-range>`: changed symbols -> blast radius -> affected
//! tests. Line hunks come from `git diff -U0`; spans are matched against the
//! graph built from the working tree, so build before asking.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sinter_core::{Node, SymbolKind};
use sinter_resolve::qualified_of;
use sinter_store::EdgeFilter;

use crate::lookup::{open_current, open_store};

/// Per-collection output budget for agent-facing impact results. Computation
/// always remains complete; this limit applies only when rendering the three
/// potentially large symbol collections.
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
    /// Hunks come from the rev range but spans match the working-tree
    /// graph; uncommitted edits shift spans, so totals may include drift.
    pub working_tree_dirty: bool,
    /// Authoritative path-level ledger from `git diff --name-status`.
    /// Unlike `changed_symbols`, this never drops configuration, binary,
    /// deleted, renamed, or otherwise unindexed paths.
    pub changed_files: Vec<ChangedFile>,
    /// Entries from `git status --porcelain`, including untracked paths
    /// which Git does not include in a normal `git diff <rev>`.
    pub working_tree_changes: Vec<WorkingTreeChange>,
    /// Changed paths that could not be represented completely by the
    /// working-tree graph. A file can have mapped symbols and still appear
    /// here when deleted content has no current-tree symbol.
    pub unmapped_files: Vec<UnmappedFile>,
    pub changed_symbols: Vec<SymbolRef>,
    pub blast_radius: Vec<SymbolRef>,
    pub affected_tests: Vec<SymbolRef>,
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

#[derive(Serialize, Clone)]
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
        });
    }
    files.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.old_path.cmp(&b.old_path))
    });
    Ok(files)
}

fn changed_files(repo: &Path, rev_range: &str) -> Result<Vec<ChangedFile>> {
    let output = git_diff(repo, rev_range, &["--name-status", "-z", "--find-renames"])?;
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
    changes.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.old_path.cmp(&b.old_path))
    });
    Ok(changes)
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
    parse_working_tree_changes(&output.stdout)
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

fn unmapped(change: &ChangedFile, reason: UnmappedReason) -> UnmappedFile {
    UnmappedFile {
        path: change.path.clone(),
        old_path: change.old_path.clone(),
        git_status: change.git_status.clone(),
        reason,
    }
}

/// Test detection heuristic: conventional test files and names.
pub fn is_test(node: &Node) -> bool {
    let f = &node.file;
    f.ends_with("_test.go")
        || f.ends_with("_test.py")
        || f.ends_with("Tests.cs")
        || f.contains(".test.")
        || f.contains(".spec.")
        || f.starts_with("tests/")
        || f.contains("/tests/")
        || f.contains("/test/")
        || node.name.starts_with("test_")
        || node.name.starts_with("Test")
        // Rust `#[cfg(test)] mod tests` inline modules.
        || qualified_of(node.id.as_str()).split("::").any(|s| s == "tests")
}

pub fn compute(repo: &Path, rev_range: &str) -> Result<ImpactReport> {
    compute_filtered(repo, rev_range, &EdgeFilter::default())
}

pub fn compute_filtered(repo: &Path, rev_range: &str, filter: &EdgeFilter) -> Result<ImpactReport> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
    compute_with_store(&repo, rev_range, filter, &store)
}

pub(crate) fn compute_current(repo: &Path, rev_range: &str) -> Result<ImpactReport> {
    let repo = repo.canonicalize()?;
    let store = open_current(&repo)?;
    compute_with_store(&repo, rev_range, &EdgeFilter::default(), &store)
}

pub(crate) fn compute_with_store(
    repo: &Path,
    rev_range: &str,
    filter: &EdgeFilter,
    store: &sinter_store::Store,
) -> Result<ImpactReport> {
    let working_tree_changes = working_tree_changes(repo)?;
    let working_tree_dirty = !working_tree_changes.is_empty();
    let historical_endpoint_matches_head = historical_endpoint_matches_head(repo, rev_range)?;
    let mut changed_files = changed_files(repo, rev_range)?;
    // New-side hunks per file from Git. Name/status is deliberately a
    // separate command: patch text cannot represent config-only, binary,
    // deleted, pure-rename, or mode-only changes reliably.
    let patch = git_diff(repo, rev_range, &["-U0", "--no-color", "--find-renames"])?;
    let hunks = parse_hunks(&patch.stdout);

    // Changed symbols: nodes whose byte span overlaps a changed line range.
    // The BTreeMap de-duplicates nodes while preserving deterministic output.
    let mut changed_by_id: BTreeMap<String, Node> = BTreeMap::new();
    let mut unmapped_files = Vec::new();
    for change in &mut changed_files {
        let reason = if change.kind == FileChangeKind::Deleted {
            Some(UnmappedReason::Deleted)
        } else if change.kind == FileChangeKind::Unknown {
            Some(UnmappedReason::UnknownGitStatus)
        } else {
            let Some(facts) = store.facts(&change.path)? else {
                unmapped_files.push(unmapped(change, UnmappedReason::NotIndexed));
                continue;
            };
            let Ok(source) = std::fs::read_to_string(repo.join(&change.path)) else {
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
                unmapped_files.push(unmapped(change, UnmappedReason::NoContentHunks));
                continue;
            } else {
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
                for node in &facts.nodes {
                    if node.kind == SymbolKind::File {
                        continue;
                    }
                    let touched = ranges.iter().any(|hunk| {
                        byte_range(hunk.new_start, hunk.new_count).is_some_and(|(start, end)| {
                            node.span.start < end && start < node.span.end
                        })
                    });
                    if touched {
                        mapped_ids.insert(node.id.as_str().to_string());
                        changed_by_id.insert(node.id.as_str().to_string(), node.clone());
                    }
                }
                if mapped_ids.is_empty() {
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

    // Blast radius: union of dependents of every changed symbol.
    let mut radius: BTreeMap<String, Node> = BTreeMap::new();
    for node in &changed {
        for reached in store.dependents(&node.id, filter, 25)? {
            radius.insert(reached.node.id.as_str().to_string(), reached.node);
        }
    }
    for node in &changed {
        radius.remove(node.id.as_str());
    }

    let affected_tests: Vec<SymbolRef> = radius
        .values()
        .chain(changed.iter())
        .filter(|n| is_test(n))
        .map(symbol_ref)
        .collect();

    let mut partial_reasons = Vec::new();
    if !unmapped_files.is_empty() {
        partial_reasons.push("one_or_more_changed_files_are_not_fully_mapped");
    }
    if rev_range.contains("..") && working_tree_dirty {
        partial_reasons.push("historical_diff_uses_a_dirty_working_tree_graph");
    }
    if !historical_endpoint_matches_head {
        partial_reasons.push("historical_diff_endpoint_does_not_match_graph_head");
    }
    if working_tree_changes
        .iter()
        .any(|change| change.index_status == "untracked" || change.worktree_status == "untracked")
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
        working_tree_dirty,
        changed_files,
        working_tree_changes,
        unmapped_files,
        changed_symbols: changed.iter().map(symbol_ref).collect(),
        blast_radius: radius.values().map(symbol_ref).collect(),
        affected_tests,
    })
}

pub fn run(
    repo: &Path,
    rev_range: &str,
    manifest: Option<&Path>,
    evidence: &[String],
    certain: bool,
    limit: usize,
    json: bool,
) -> Result<()> {
    let filter = crate::lookup::edge_filter(evidence, certain)?;
    let mut report = compute_filtered(repo, rev_range, &filter)?;
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
        println!("  partial: {reason}");
    }
    if report.working_tree_dirty {
        println!(
            "  note: working tree has {} uncommitted path(s); see working_tree_changes in --json output",
            report.working_tree_changes.len()
        );
    }
    println!("changed files:");
    for file in &report.changed_files {
        if let Some(old) = &file.old_path {
            println!(
                "  {}  {} -> {}  ({} graph symbols)",
                file.git_status, old, file.path, file.mapped_symbols
            );
        } else {
            println!(
                "  {}  {}  ({} graph symbols)",
                file.git_status, file.path, file.mapped_symbols
            );
        }
    }
    if !report.unmapped_files.is_empty() {
        println!("unmapped files:");
        for file in &report.unmapped_files {
            println!("  {:?}  {}", file.reason, file.path);
        }
    }
    print_symbols("changed", &report.changed_symbols, limit);
    print_symbols("blast radius", &report.blast_radius, limit);
    print_symbols("affected tests", &report.affected_tests, limit);
    Ok(())
}

fn returned_count(total: usize, limit: usize) -> usize {
    if limit == 0 { total } else { total.min(limit) }
}

fn truncated_count(total: usize, limit: usize) -> usize {
    total - returned_count(total, limit)
}

fn print_symbols(label: &str, symbols: &[SymbolRef], limit: usize) {
    let returned = returned_count(symbols.len(), limit);
    let truncated = symbols.len() - returned;
    println!(
        "{label}: {returned} shown, {} total, {truncated} truncated",
        symbols.len()
    );
    for symbol in symbols.iter().take(returned) {
        println!("  {} {}  {}", symbol.kind, symbol.qualified, symbol.file);
    }
}

/// Also usable by the MCP server.
pub fn to_json(report: &ImpactReport, limit: usize) -> serde_json::Value {
    let totals = serde_json::json!({
        "changed_symbols": report.changed_symbols.len(),
        "blast_radius": report.blast_radius.len(),
        "affected_tests": report.affected_tests.len(),
    });
    let truncated = serde_json::json!({
        "changed_symbols": truncated_count(report.changed_symbols.len(), limit),
        "blast_radius": truncated_count(report.blast_radius.len(), limit),
        "affected_tests": truncated_count(report.affected_tests.len(), limit),
    });
    let mut value = serde_json::to_value(report).expect("impact report serializes");
    for (name, total) in [
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
        AnalysisStatus, FileChangeKind, ImpactReport, SymbolRef, UnmappedReason, changed_files,
        compute, is_test, to_json, working_tree_changes,
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
            changed_files: Vec::new(),
            working_tree_changes: Vec::new(),
            unmapped_files: Vec::new(),
            changed_symbols: symbols("changed", 3),
            blast_radius: symbols("blast", 4),
            affected_tests: symbols("test", 1),
        }
    }

    #[test]
    fn impact_budget_caps_each_collection_independently_and_preserves_totals() {
        let value = to_json(&report_for_budget(), 2);

        assert_eq!(value["limit"], 2);
        assert_eq!(value["changed_symbols"].as_array().unwrap().len(), 2);
        assert_eq!(value["blast_radius"].as_array().unwrap().len(), 2);
        assert_eq!(value["affected_tests"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["totals"],
            serde_json::json!({
                "changed_symbols": 3,
                "blast_radius": 4,
                "affected_tests": 1,
            })
        );
        assert_eq!(
            value["truncated"],
            serde_json::json!({
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
        assert_eq!(value["changed_symbols"].as_array().unwrap().len(), 3);
        assert_eq!(value["blast_radius"].as_array().unwrap().len(), 4);
        assert_eq!(value["affected_tests"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["truncated"],
            serde_json::json!({
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

        let files = changed_files(repo.path(), "HEAD~1..HEAD").expect("collect changed paths");
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
    fn working_tree_impact_remains_complete_for_tracked_edits() {
        let repo = repository();
        write(repo.path(), "src/lib.rs", "pub fn value() -> u32 { 2 }\n");
        crate::pipeline::build(repo.path(), None).expect("build fixture graph");

        let report = compute(repo.path(), "HEAD").expect("compute working-tree impact");

        assert_eq!(report.analysis_status, AnalysisStatus::Complete);
        assert!(report.working_tree_dirty);
        assert!(report.partial_reasons.is_empty());
    }
}
