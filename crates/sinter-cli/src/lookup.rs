//! Symbol-argument resolution shared by every query-side command: exact
//! name, qualified suffix, node id, or trigram suggestions.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use anyhow::{Context, Result, bail};
use sinter_core::{
    Confidence, CorpusScope, Evidence, Node, NodeId, Relation, SymbolKey, SymbolKind,
};
use sinter_resolve::qualified_of;
use sinter_store::{EdgeFilter, Store};

use crate::pipeline;

/// Valid query, no results. Read commands exit 1 (grep-style) when the
/// error chain carries this; every other error is exit 2.
#[derive(Debug)]
pub struct NoMatch(pub String);

impl std::fmt::Display for NoMatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoMatch {}

/// Machine-classifiable failures from an agent-provided symbol handle.
#[derive(Debug)]
pub enum SymbolLookupError {
    Ambiguous {
        requested: String,
        candidates: Vec<Node>,
    },
    Relocated {
        requested: String,
        candidates: Vec<Node>,
    },
    StaleSnapshot {
        expected: String,
        actual: String,
    },
}

impl SymbolLookupError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Ambiguous { .. } => "ambiguous_symbol",
            Self::Relocated { .. } => "relocated_handle",
            Self::StaleSnapshot { .. } => "stale_snapshot",
        }
    }

    pub fn candidates(&self) -> &[Node] {
        match self {
            Self::Ambiguous { candidates, .. } | Self::Relocated { candidates, .. } => candidates,
            Self::StaleSnapshot { .. } => &[],
        }
    }

    pub fn snapshots(&self) -> Option<(&str, &str)> {
        match self {
            Self::StaleSnapshot { expected, actual } => Some((expected, actual)),
            _ => None,
        }
    }
}

impl std::fmt::Display for SymbolLookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ambiguous {
                requested,
                candidates,
            } => {
                writeln!(f, "`{requested}` is ambiguous — choose a candidate")?;
                write_candidates(f, candidates)
            }
            Self::Relocated {
                requested,
                candidates,
            } => {
                writeln!(
                    f,
                    "snapshot-local node id `{requested}` moved — use its stable symbol key or a current candidate"
                )?;
                write_candidates(f, candidates)
            }
            Self::StaleSnapshot { expected, actual } => write!(
                f,
                "graph snapshot changed (expected `{expected}`, current `{actual}`)"
            ),
        }
    }
}

impl std::error::Error for SymbolLookupError {}

fn write_candidates(f: &mut std::fmt::Formatter<'_>, candidates: &[Node]) -> std::fmt::Result {
    for label in candidate_labels(candidates) {
        writeln!(f, "  {label}")?;
    }
    Ok(())
}

/// The narrowest handle that tells each candidate apart from every other
/// candidate in its own list. Rungs, first one unique across the whole list
/// wins per candidate:
///
/// 0. `Name` — the bare qualified name;
/// 1. `Name@file`;
/// 2. `Name@file:line` — two candidates in one file;
/// 3. the snapshot id — no line available (see [`line_of`]).
///
/// `first` is the lowest rung a caller will print: candidate listings start
/// at 1 because a candidate line that omits its file is useless to the
/// reader even when the name alone is unambiguous.
///
/// Every rung is a form [`find_symbol`] parses back. The returned selectors
/// are unique by construction: rung shapes are disjoint (rungs 1-2 add an
/// `@`, rung 3 an `#`, and qualified names carry neither), a rung is only
/// taken when no sibling produces the same string at it, and rung 3 is the
/// store's primary key, so the last rung always terminates the ladder.
fn selectors_from(
    nodes: &[Node],
    first: u8,
    line_of: impl Fn(&Node) -> Option<usize>,
) -> Vec<String> {
    let lines: Vec<Option<usize>> = nodes.iter().map(line_of).collect();
    let rung = |i: usize, n: u8| -> Option<String> {
        let node = &nodes[i];
        let qualified = qualified_of(node.id.as_str());
        match n {
            0 => Some(qualified.to_string()),
            1 => Some(format!("{qualified}@{}", node.file)),
            2 => lines[i].map(|line| format!("{qualified}@{}:{line}", node.file)),
            _ => Some(node.id.as_str().to_string()),
        }
    };
    (0..nodes.len())
        .map(|i| {
            (first..=3)
                .find_map(|n| {
                    let selector = rung(i, n)?;
                    (0..nodes.len())
                        .all(|j| j == i || rung(j, n).as_deref() != Some(selector.as_str()))
                        .then_some(selector)
                })
                .unwrap_or_else(|| nodes[i].id.as_str().to_string())
        })
        .collect()
}

/// `Name@path/to/file.rs[:line]` per candidate: the bare selector an agent
/// pastes back as `symbol`, narrowed by [`selectors_from`].
pub fn candidate_selectors(nodes: &[Node]) -> Vec<String> {
    selectors_from(nodes, 1, line_of)
}

/// `Name@path/to/file.rs (kind)` per candidate — the form an agent pastes
/// back, narrowed by [`selectors_from`] so no two lines of one listing carry
/// the same handle. No symbol keys: nobody types those.
pub fn candidate_labels(nodes: &[Node]) -> Vec<String> {
    selectors_from(nodes, 1, line_of)
        .into_iter()
        .zip(nodes)
        .map(|(selector, node)| format!("{selector} ({})", node.kind.as_str()))
        .collect()
}

/// Comma-joined `Name@file` list for one-line notes, disambiguated within
/// the list the same way [`candidate_labels`] is.
pub fn short_list(nodes: &[Node]) -> String {
    const SHOWN: usize = 5;
    let all = selectors_from(nodes, 1, line_of);
    if all.len() <= SHOWN {
        return all.join(", ");
    }
    format!("{}, +{} more", all[..SHOWN].join(", "), all.len() - SHOWN)
}

/// 1-based start line of a node, when the repository is reachable.
///
/// The store carries no repo root and candidate rendering runs inside a
/// `Display` impl with nowhere to pass one, so the root is discovered from
/// the process working directory and trusted only when it actually holds a
/// graph.
///
/// ponytail: cwd-derived root. A process querying a graph outside its own
/// working directory reads no line and its selectors fall back to the
/// snapshot id — still unique, just longer. Thread a root through
/// `SymbolLookupError` if that case ever needs lines.
fn line_of(node: &Node) -> Option<usize> {
    let root = pipeline::discover_root(&std::env::current_dir().ok()?);
    pipeline::db_path(&root).exists().then_some(())?;
    crate::render::line_of(&root, &node.file, node.span.start)
}

pub fn open_store(repo: &Path) -> Result<Store> {
    let repo = pipeline::discover_root(repo);
    let repo = repo
        .canonicalize()
        .with_context(|| format!("repo path {}", repo.display()))?;
    let path = pipeline::db_path(&repo);
    if !path.exists() {
        bail!("no graph at {} — run `sinter build` first", path.display());
    }
    // Freshness lives at the one-shot query boundary. The MCP server owns
    // an event-driven generation and calls open_current after synchronizing.
    pipeline::build(&repo, None)?;
    open_current(&repo)
}

/// Open an already-synchronized graph. This is deliberately crate-private:
/// only the MCP freshness owner may bypass the one-shot query scan.
pub(crate) fn open_current(repo: &Path) -> Result<Store> {
    let repo = pipeline::discover_root(repo);
    let repo = repo
        .canonicalize()
        .with_context(|| format!("repo path {}", repo.display()))?;
    let path = pipeline::db_path(&repo);
    if !path.exists() {
        bail!("no graph at {} — run `sinter build` first", path.display());
    }
    // Reads take a shared lock and write nothing; only an unclean
    // shutdown needs the writable handle, which repairs on open.
    let store = Store::open_read_only(&path).or_else(|_| Store::open(&path))?;
    // A 0-node graph answers every query with "no match" — say what is
    // actually wrong instead.
    if store.node_count()? == 0 {
        bail!(
            "graph at {} is empty — was `sinter build` run in the right directory?",
            path.display()
        );
    }
    Ok(store)
}

/// Nodes matching a symbol argument: full node id, exact name, or qualified
/// suffix (`Config::new`), optionally narrowed by a file-path suffix
/// (`run@init.rs`, `new@cli/src/config.rs`) — the disambiguator an agent
/// can derive from the candidate list without copying a byte-offset id.
/// Empty result falls back to fuzzy suggestions.
pub enum Found {
    Exact(Vec<Node>),
    /// A snapshot-local id no longer exists, but its semantic location has
    /// current candidates. Never promoted to an exact binding implicitly.
    Relocated(Vec<Node>),
    Suggestions(Vec<Node>),
}

pub fn find_symbol(store: &Store, symbol: &str) -> Result<Found> {
    if symbol.starts_with(SymbolKey::PREFIX) {
        let key = SymbolKey::parse(symbol.to_string())
            .ok_or_else(|| anyhow::anyhow!("invalid stable symbol key `{symbol}`"))?;
        let (kind, file, qualified) = key.parts().expect("validated symbol key");
        let name = if kind == SymbolKind::File {
            file
        } else {
            qualified.rsplit("::").next().unwrap_or(qualified)
        };
        let mut matches: Vec<Node> = store
            .nodes_named(name)?
            .into_iter()
            .filter(|node| node.symbol_key() == key)
            .collect();
        matches.sort_by(|a, b| a.id.cmp(&b.id));
        return if matches.is_empty() {
            Ok(Found::Suggestions(Vec::new()))
        } else {
            Ok(Found::Exact(matches))
        };
    }
    if symbol.contains('#') {
        if let Some(node) = store.node(&sinter_core::NodeId::new(symbol))? {
            return Ok(Found::Exact(vec![node]));
        }
        let relocated = relocation_candidates(store, symbol)?;
        if !relocated.is_empty() {
            return Ok(Found::Relocated(relocated));
        }
        return Ok(Found::Suggestions(Vec::new()));
    }
    let (symbol, file, line) = split_handle(symbol);
    let name = symbol.rsplit("::").next().unwrap_or(symbol);
    let mut matches: Vec<Node> = store
        .nodes_named(name)?
        .into_iter()
        .filter(|n| {
            let q = qualified_of(n.id.as_str());
            (q == symbol || q.ends_with(&format!("::{symbol}")))
                && file.is_none_or(|f| n.file == f || n.file.ends_with(&format!("/{f}")))
        })
        .collect();
    // A free `parse` must not be shadowed by `Parser::parse`: when the
    // qualified name matches exactly, suffix matches are only noise.
    if matches
        .iter()
        .any(|n| qualified_of(n.id.as_str()) == symbol)
    {
        matches.retain(|n| qualified_of(n.id.as_str()) == symbol);
    }
    if let Some(line) = line
        && matches.len() > 1
    {
        matches = narrow_to_line(matches, line, line_of);
    }
    matches.sort_by(|a, b| a.id.cmp(&b.id));
    if matches.is_empty() {
        Ok(Found::Suggestions(store.search(symbol, 10)?))
    } else {
        Ok(Found::Exact(matches))
    }
}

/// A symbol argument split into (symbol, file suffix, line). `Name@file`
/// parses exactly as it always did; `:line` is only ever read off the file
/// part, so a qualified name's `::`, a Windows drive letter (`C:\src\x.rs`)
/// and a node id's own `@offset` are never mistaken for one. Node ids are
/// answered before this is reached — they carry a `#`.
fn split_handle(handle: &str) -> (&str, Option<&str>, Option<usize>) {
    match handle.rsplit_once('@') {
        Some((symbol, file)) if !symbol.is_empty() && !file.is_empty() => {
            let (file, line) = split_line(file);
            (symbol, Some(file), line)
        }
        _ => (handle, None, None),
    }
}

/// `src/client.rs:709` → (`src/client.rs`, 709). A colon that is not
/// followed by digits belongs to the path.
fn split_line(file: &str) -> (&str, Option<usize>) {
    match file.rsplit_once(':') {
        Some((path, line)) if !path.is_empty() => match line.parse() {
            Ok(line) => (path, Some(line)),
            Err(_) => (file, None),
        },
        _ => (file, None),
    }
}

/// Narrow same-file candidates to the one starting at, or nearest below,
/// `want`; when every candidate starts after it, the nearest above. A set
/// with no readable lines is returned untouched — `Name@file` behaviour.
fn narrow_to_line(
    matches: Vec<Node>,
    want: usize,
    line_of: impl Fn(&Node) -> Option<usize>,
) -> Vec<Node> {
    let lines: Vec<Option<usize>> = matches.iter().map(line_of).collect();
    let Some(best) = lines
        .iter()
        .flatten()
        .filter(|line| **line <= want)
        .max()
        .or_else(|| lines.iter().flatten().min())
        .copied()
    else {
        return matches;
    };
    matches
        .into_iter()
        .zip(lines)
        .filter_map(|(node, line)| (line == Some(best)).then_some(node))
        .collect()
}

fn relocation_candidates(store: &Store, id: &str) -> Result<Vec<Node>> {
    let Some((file, rest)) = id.split_once('#') else {
        return Ok(Vec::new());
    };
    let Some((qualified, offset)) = rest.rsplit_once('@') else {
        return Ok(Vec::new());
    };
    if offset.parse::<u64>().is_err() || qualified.is_empty() {
        return Ok(Vec::new());
    }
    let name = qualified.rsplit("::").next().unwrap_or(qualified);
    let mut candidates: Vec<Node> = store
        .nodes_named(name)?
        .into_iter()
        .filter(|node| node.file == file && node.id.qualified() == qualified)
        .collect();
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(candidates)
}

/// Enforce an optimistic graph-snapshot precondition and return the current
/// token for response projection.
pub fn ensure_snapshot(store: &Store, expected: Option<&str>) -> Result<String> {
    let actual = store.snapshot_token()?;
    ensure_snapshot_token(expected, &actual)?;
    Ok(actual)
}

pub fn ensure_snapshot_token(expected: Option<&str>, actual: &str) -> Result<()> {
    if let Some(expected) = expected
        && expected != actual
    {
        return Err(SymbolLookupError::StaleSnapshot {
            expected: expected.to_string(),
            actual: actual.to_string(),
        }
        .into());
    }
    Ok(())
}

/// Exactly one node or a listed-candidates error. A name ambiguous only
/// because of fixture/test/vendor copies resolves to the lone
/// production/docs candidate (the same default scope as `sinter map`).
pub fn unique_symbol(store: &Store, symbol: &str) -> Result<Node> {
    unique_symbol_in(store, symbol, None)
}

/// `unique_symbol` with an explicit preferred scope set (`None` = the agent
/// default). Candidates outside it only break ties, never hide results:
/// `--scope all` or an `@file` suffix still reaches them.
pub fn unique_symbol_in(
    store: &Store,
    symbol: &str,
    scopes: Option<&BTreeSet<CorpusScope>>,
) -> Result<Node> {
    resolve_symbol_in(store, symbol, scopes).map(|r| r.node)
}

/// A symbol that resolved to one node, plus the same-name candidates the
/// tie-break set aside (empty when the name was unique). Verbs that print
/// a card show these so a quiet pick is never mistaken for a unique one.
pub struct Resolved {
    pub node: Node,
    pub ignored: Vec<Node>,
    /// Why the others lost: `in-degree`, `language`, or the scope kinds
    /// (`fixture/test`). Empty when nothing was ignored.
    pub reason: String,
}

impl Resolved {
    /// The handle a follow-up should paste: the bare qualified name when
    /// it was unique, otherwise the shortest selector (`Name@file`, or
    /// `Name@file:line`) that beats every ignored candidate.
    pub fn selector(&self) -> String {
        if self.ignored.is_empty() {
            return qualified_of(self.node.id.as_str()).to_string();
        }
        let mut all = Vec::with_capacity(self.ignored.len() + 1);
        all.push(self.node.clone());
        all.extend(self.ignored.iter().cloned());
        selectors_from(&all, 1, line_of).swap_remove(0)
    }
}

/// [`unique_symbol_in`] that also reports which candidates were ignored.
pub fn resolve_symbol_in(
    store: &Store,
    symbol: &str,
    scopes: Option<&BTreeSet<CorpusScope>>,
) -> Result<Resolved> {
    let Narrowed {
        mut keep,
        ignored,
        reason,
    } = narrowed(store, symbol, scopes)?;
    if keep.len() > 1 {
        return Err(SymbolLookupError::Ambiguous {
            requested: symbol.to_string(),
            candidates: keep,
        }
        .into());
    }
    Ok(Resolved {
        node: keep.remove(0),
        ignored,
        reason,
    })
}

/// The best-tier candidates for a symbol: one node when it resolves, or
/// the list an `Ambiguous` error would carry. `path` tries every pair of
/// these before giving up.
pub fn candidates_in(
    store: &Store,
    symbol: &str,
    scopes: Option<&BTreeSet<CorpusScope>>,
) -> Result<Vec<Node>> {
    narrowed(store, symbol, scopes).map(|n| n.keep)
}

struct Narrowed {
    keep: Vec<Node>,
    ignored: Vec<Node>,
    reason: String,
}

/// Exact matches split into the survivors and the candidates the scope
/// tiering and tie-break dropped. Emits the "N other ignored" note once, so
/// every caller (text stderr or the JSON `warnings` field) sees it.
fn narrowed(
    store: &Store,
    symbol: &str,
    scopes: Option<&BTreeSet<CorpusScope>>,
) -> Result<Narrowed> {
    match find_symbol(store, symbol)? {
        Found::Exact(nodes) if nodes.len() == 1 => Ok(Narrowed {
            keep: nodes,
            ignored: Vec::new(),
            reason: String::new(),
        }),
        Found::Exact(nodes) => {
            let preferred = scopes
                .cloned()
                .unwrap_or_else(|| crate::corpus::ScopeSelection::agent_default().as_set());
            let scope_index = store.scope_index()?;
            let (mut keep, mut ignored) =
                select_tier(nodes, &preferred, |n| scope_index.scope_of(n));
            let mut reason = {
                let mut kinds: Vec<&str> = ignored
                    .iter()
                    .map(|n| scope_index.scope_of(n).as_str())
                    .collect();
                kinds.sort_unstable();
                kinds.dedup();
                kinds.join("/")
            };
            if keep.len() > 1 {
                let dominant = dominant_language(&store.file_scopes()?);
                let ids: Vec<NodeId> = keep.iter().map(|n| n.id.clone()).collect();
                let in_edges = store.in_edges_many(&ids)?;
                let in_degree = |n: &Node| {
                    in_edges.get(&n.id).map_or(0, |edges| {
                        edges
                            .iter()
                            .filter(|e| e.relation != Relation::Contains)
                            .count()
                    })
                };
                let (kept, dropped, why) = break_ties(keep, dominant.as_deref(), in_degree);
                keep = kept;
                if let Some(why) = why {
                    ignored = dropped;
                    reason = why.to_string();
                }
            }
            if keep.len() == 1 && !ignored.is_empty() {
                crate::agent_protocol::warn(format!(
                    "{} other `{symbol}` ignored ({reason}): {}",
                    ignored.len(),
                    short_list(&ignored)
                ));
            }
            Ok(Narrowed {
                keep,
                ignored,
                reason,
            })
        }
        Found::Relocated(nodes) => Err(SymbolLookupError::Relocated {
            requested: symbol.to_string(),
            candidates: nodes,
        }
        .into()),
        Found::Suggestions(nodes) if nodes.is_empty() => Err(NoMatch(format!(
            "no symbol matches `{symbol}` — try `sinter ask \"{symbol}\"` for concept search"
        ))
        .into()),
        Found::Suggestions(nodes) => {
            let list: Vec<String> = selectors_from(&nodes, 0, line_of)
                .into_iter()
                .map(|selector| format!("  {selector}"))
                .collect();
            Err(NoMatch(format!(
                "no exact match for `{symbol}`; close names:\n{}",
                list.join("\n")
            ))
            .into())
        }
    }
}

/// Split same-name candidates into (best tier, the rest). Tiers, first
/// non-empty wins: preferred production/docs, other preferred scopes,
/// other hand-written scopes (test/fixture/example), then generated/vendor.
fn select_tier(
    nodes: Vec<Node>,
    preferred: &BTreeSet<CorpusScope>,
    scope_of: impl Fn(&Node) -> CorpusScope,
) -> (Vec<Node>, Vec<Node>) {
    let tier = |n: &Node| {
        let scope = scope_of(n);
        let shipped = matches!(scope, CorpusScope::Production | CorpusScope::Docs);
        match (preferred.contains(&scope), shipped) {
            (true, true) => 0,
            (true, false) => 1,
            (false, _) if matches!(scope, CorpusScope::Generated | CorpusScope::Vendor) => 3,
            (false, _) => 2,
        }
    };
    let best = nodes.iter().map(&tier).min().unwrap_or(0);
    nodes.into_iter().partition(|n| tier(n) == best)
}

/// Language name (`spec_for_path`) of the file a node lives in.
fn language_of(node: &Node) -> Option<&'static str> {
    sinter_extract::spec_for_path(&node.file).map(|spec| spec.name)
}

/// The language with the most production-scoped files, if any.
fn dominant_language(file_scopes: &HashMap<String, CorpusScope>) -> Option<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (file, scope) in file_scopes {
        if *scope == CorpusScope::Production
            && let Some(spec) = sinter_extract::spec_for_path(file)
        {
            *counts.entry(spec.name).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(name, _)| name.to_string())
}

/// Narrow a same-tier tie: first to candidates in the repository's
/// dominant language, then to the highest non-containment in-degree.
/// Returns (kept, dropped, reason); reason is `None` when nothing was
/// dropped. Ties that survive both steps are returned intact.
fn break_ties(
    nodes: Vec<Node>,
    dominant: Option<&str>,
    in_degree: impl Fn(&Node) -> usize,
) -> (Vec<Node>, Vec<Node>, Option<&'static str>) {
    let mut dropped = Vec::new();
    let mut reason = None;
    let mut keep = nodes;
    if let Some(dominant) = dominant
        && keep.len() > 1
        && keep.iter().any(|n| language_of(n) == Some(dominant))
    {
        let (same, other): (Vec<Node>, Vec<Node>) = keep
            .into_iter()
            .partition(|n| language_of(n) == Some(dominant));
        keep = same;
        if !other.is_empty() {
            dropped.extend(other);
            reason = Some("language");
        }
    }
    if keep.len() > 1 {
        let best = keep.iter().map(&in_degree).max().unwrap_or(0);
        let (top, rest): (Vec<Node>, Vec<Node>) =
            keep.into_iter().partition(|n| in_degree(n) == best);
        keep = top;
        if !rest.is_empty() {
            dropped.extend(rest);
            reason = Some("in-degree");
        }
    }
    (keep, dropped, reason)
}

/// One place a symbol not defined in this repo is referenced: the
/// enclosing definition (or file) and how many refs it holds.
pub struct ExternalSite {
    pub file: String,
    pub enclosing: Option<String>,
    pub refs: usize,
}

/// Reference sites for a symbol the corpus does not define — dependency
/// blast radius at the repo boundary ("what here touches tokio::spawn").
/// Qualified queries must match the written path's tail; bare names match
/// the final segment.
pub fn external_sites(store: &Store, symbol: &str) -> Result<Vec<ExternalSite>> {
    let tail = symbol.rsplit([':', '/', '.']).next().unwrap_or(symbol);
    if tail.is_empty() {
        return Ok(Vec::new());
    }
    let matches = |written: &str| {
        written == symbol
            || (written.ends_with(symbol)
                && written[..written.len() - symbol.len()]
                    .chars()
                    .next_back()
                    .is_some_and(|c| !c.is_alphanumeric() && c != '_'))
    };
    let files = store.ref_files(&BTreeSet::from([tail.to_string()]))?;
    let mut sites: std::collections::BTreeMap<(String, Option<String>), usize> =
        std::collections::BTreeMap::new();
    for file in files {
        for r in store.references_in(&file)? {
            let written = r.path.as_deref().unwrap_or(&r.name);
            if matches(written) || matches(&r.name) {
                let enclosing = r.enclosing.map(|id| qualified_of(id.as_str()).to_string());
                *sites.entry((r.file, enclosing)).or_default() += 1;
            }
        }
    }
    Ok(sites
        .into_iter()
        .map(|((file, enclosing), refs)| ExternalSite {
            file,
            enclosing,
            refs,
        })
        .collect())
}

/// --evidence / --certain flags to an EdgeFilter.
pub fn edge_filter(evidence: &[String], certain: bool) -> Result<EdgeFilter> {
    let evidence = if evidence.is_empty() {
        None
    } else {
        let mut set = BTreeSet::new();
        for e in evidence {
            set.insert(match e.as_str() {
                "structural" => Evidence::Structural,
                "scope" => Evidence::Scope,
                "import" => Evidence::Import,
                "scip" => Evidence::Scip,
                "declared" => Evidence::Declared,
                "dynamic" => Evidence::Dynamic,
                other => bail!("unknown evidence kind `{other}`"),
            });
        }
        Some(set)
    };
    Ok(EdgeFilter {
        evidence,
        min_confidence: certain.then_some(Confidence::Certain),
        relations: None,
        scopes: None,
    })
}

/// --relations names to the traversal's relation set; empty = all.
pub fn relation_set(relations: &[String]) -> Result<Option<BTreeSet<sinter_core::Relation>>> {
    if relations.is_empty() {
        return Ok(None);
    }
    let mut set = BTreeSet::new();
    for r in relations {
        set.insert(match r.as_str() {
            "calls" => sinter_core::Relation::Calls,
            "uses" => sinter_core::Relation::Uses,
            "imports" => sinter_core::Relation::Imports,
            "implements" => sinter_core::Relation::Implements,
            "extends" => sinter_core::Relation::Extends,
            "reads" => sinter_core::Relation::Reads,
            "writes" => sinter_core::Relation::Writes,
            "creates" => sinter_core::Relation::Creates,
            "alters" => sinter_core::Relation::Alters,
            "drops" => sinter_core::Relation::Drops,
            other => {
                bail!(
                    "unknown relation `{other}` (calls, uses, imports, implements, extends, reads, writes, creates, alters, drops)"
                )
            }
        });
    }
    Ok(Some(set))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sinter_core::Span;

    fn node(file: &str, kind: SymbolKind) -> Node {
        Node {
            id: NodeId::new(format!("{file}#Widget@10")),
            kind,
            name: "Widget".into(),
            file: file.into(),
            span: Span { start: 10, end: 20 },
            signature: String::new(),
            doc: None,
        }
    }

    fn pick(files: &[&str]) -> (Vec<String>, Vec<String>) {
        let nodes = files.iter().map(|f| node(f, SymbolKind::Struct)).collect();
        let preferred = BTreeSet::from([CorpusScope::Production, CorpusScope::Test]);
        let (keep, rest) = select_tier(nodes, &preferred, |n| CorpusScope::classify_path(&n.file));
        let names = |v: Vec<Node>| v.into_iter().map(|n| n.file).collect();
        (names(keep), names(rest))
    }

    #[test]
    fn lone_production_candidate_wins_over_test_copies() {
        let (keep, rest) = pick(&["src/a.rs", "tests/a.rs", "fixtures/a.rs"]);
        assert_eq!(keep, ["src/a.rs"]);
        assert_eq!(rest, ["tests/a.rs", "fixtures/a.rs"]);
    }

    #[test]
    fn several_production_candidates_stay_ambiguous_without_test_noise() {
        let (keep, rest) = pick(&["crates/a/src/lib.rs", "crates/b/src/lib.rs", "tests/x.rs"]);
        assert_eq!(keep, ["crates/a/src/lib.rs", "crates/b/src/lib.rs"]);
        assert_eq!(rest, ["tests/x.rs"]);
    }

    #[test]
    fn generated_and_vendor_lose_to_hand_written() {
        let (keep, rest) = pick(&["generated/a.rs", "vendor/a.rs", "tests/a.rs"]);
        assert_eq!(keep, ["tests/a.rs"]);
        assert_eq!(rest, ["generated/a.rs", "vendor/a.rs"]);
        let (keep, _) = pick(&["generated/a.rs", "vendor/a.rs"]);
        assert_eq!(keep, ["generated/a.rs", "vendor/a.rs"]);
    }

    #[test]
    fn relation_filter_accepts_sql_data_flow() {
        let filter = relation_set(&[
            "reads".into(),
            "writes".into(),
            "creates".into(),
            "alters".into(),
            "drops".into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            filter,
            BTreeSet::from([
                Relation::Reads,
                Relation::Writes,
                Relation::Creates,
                Relation::Alters,
                Relation::Drops,
            ])
        );
    }

    fn files(v: Vec<Node>) -> Vec<String> {
        v.into_iter().map(|n| n.file).collect()
    }

    #[test]
    fn dominant_language_breaks_production_tie() {
        let nodes = vec![
            node("proto/event.proto", SymbolKind::Struct),
            node("src/event.rs", SymbolKind::Enum),
        ];
        let (keep, rest, why) = break_ties(nodes, Some("rust"), |_| 0);
        assert_eq!(files(keep), ["src/event.rs"]);
        assert_eq!(files(rest), ["proto/event.proto"]);
        assert_eq!(why, Some("language"));
    }

    #[test]
    fn in_degree_breaks_same_language_tie() {
        let nodes = vec![
            node("crates/a/src/lib.rs", SymbolKind::Struct),
            node("crates/b/src/lib.rs", SymbolKind::Struct),
        ];
        let (keep, rest, why) = break_ties(nodes, Some("rust"), |n| {
            usize::from(n.file.starts_with("crates/b"))
        });
        assert_eq!(files(keep), ["crates/b/src/lib.rs"]);
        assert_eq!(files(rest), ["crates/a/src/lib.rs"]);
        assert_eq!(why, Some("in-degree"));
    }

    #[test]
    fn fully_tied_candidates_stay_ambiguous() {
        let nodes = vec![
            node("crates/a/src/main.rs", SymbolKind::Function),
            node("crates/b/src/main.rs", SymbolKind::Function),
        ];
        let (keep, rest, why) = break_ties(nodes, Some("rust"), |_| 3);
        assert_eq!(keep.len(), 2);
        assert!(rest.is_empty());
        assert_eq!(why, None);
    }

    #[test]
    fn dominant_language_counts_production_files_only() {
        let scopes = HashMap::from([
            ("a.proto".to_string(), CorpusScope::Production),
            ("b.proto".to_string(), CorpusScope::Production),
            ("src/x.rs".to_string(), CorpusScope::Production),
            ("tests/y.rs".to_string(), CorpusScope::Test),
            ("tests/z.rs".to_string(), CorpusScope::Test),
        ]);
        assert_eq!(dominant_language(&scopes).as_deref(), Some("proto"));
    }

    fn node_at(file: &str, offset: u64) -> Node {
        Node {
            id: NodeId::new(format!("{file}#Widget@{offset}")),
            kind: SymbolKind::Method,
            name: "Widget".into(),
            file: file.into(),
            span: Span {
                start: offset,
                end: offset + 10,
            },
            signature: String::new(),
            doc: None,
        }
    }

    /// Stand-in for a repository read: one line per ten bytes.
    fn tenths(node: &Node) -> Option<usize> {
        Some(node.span.start as usize / 10)
    }

    #[test]
    fn handle_grammar_keeps_the_legacy_file_form() {
        assert_eq!(split_handle("Client::run"), ("Client::run", None, None));
        assert_eq!(
            split_handle("Client::run@src/client.rs"),
            ("Client::run", Some("src/client.rs"), None)
        );
        assert_eq!(
            split_handle("Client::run@src/client.rs:709"),
            ("Client::run", Some("src/client.rs"), Some(709))
        );
        // A drive letter, a colon with no digits, and a bare `@` are paths
        // and names, not line numbers.
        assert_eq!(
            split_handle(r"run@C:\src\client.rs"),
            ("run", Some(r"C:\src\client.rs"), None)
        );
        assert_eq!(split_line("src/client.rs:"), ("src/client.rs:", None));
        assert_eq!(
            split_line("src/client.rs:head"),
            ("src/client.rs:head", None)
        );
        assert_eq!(split_handle("@src/x.rs"), ("@src/x.rs", None, None));
    }

    #[test]
    fn rendered_line_selector_parses_back_to_its_own_candidate() {
        let nodes = vec![node_at("src/client.rs", 100), node_at("src/client.rs", 400)];
        let rendered = selectors_from(&nodes, 1, tenths);
        assert_eq!(
            rendered,
            ["Widget@src/client.rs:10", "Widget@src/client.rs:40"]
        );
        assert_eq!(
            split_handle(&rendered[1]),
            ("Widget", Some("src/client.rs"), Some(40))
        );
        let (_, _, line) = split_handle(&rendered[1]);
        assert_eq!(
            narrow_to_line(nodes, line.unwrap(), tenths)
                .into_iter()
                .map(|n| n.span.start)
                .collect::<Vec<_>>(),
            [400]
        );
    }

    #[test]
    fn selectors_stop_at_the_narrowest_disambiguating_rung() {
        let across = vec![node_at("src/a.rs", 10), node_at("src/b.rs", 10)];
        // Distinct files: never reach for the line even though one is known.
        assert_eq!(
            selectors_from(&across, 1, tenths),
            ["Widget@src/a.rs", "Widget@src/b.rs"]
        );
        // Rung 0 is only offered to callers that ask for it.
        assert_eq!(selectors_from(&across, 0, tenths).len(), 2);
        // No lines readable: fall through to the snapshot ids.
        let same = vec![node_at("src/a.rs", 10), node_at("src/a.rs", 900)];
        assert_eq!(
            selectors_from(&same, 1, |_| None),
            ["src/a.rs#Widget@10", "src/a.rs#Widget@900"]
        );
    }

    #[test]
    fn no_two_candidates_in_one_list_share_a_selector() {
        let lists = [
            vec![node_at("src/a.rs", 10), node_at("src/b.rs", 10)],
            vec![node_at("src/a.rs", 10), node_at("src/a.rs", 900)],
            vec![
                node_at("src/a.rs", 10),
                node_at("src/a.rs", 20),
                node_at("src/b.rs", 10),
            ],
        ];
        for nodes in lists {
            for rendered in [
                selectors_from(&nodes, 0, |_| None),
                selectors_from(&nodes, 1, |_| None),
                selectors_from(&nodes, 0, tenths),
                selectors_from(&nodes, 1, tenths),
                candidate_labels(&nodes),
            ] {
                let distinct: BTreeSet<&String> = rendered.iter().collect();
                assert_eq!(distinct.len(), rendered.len(), "{rendered:?}");
            }
        }
    }

    #[test]
    fn line_narrows_to_the_candidate_at_or_nearest_below() {
        let candidates = || {
            vec![
                node_at("src/a.rs", 100),
                node_at("src/a.rs", 700),
                node_at("src/a.rs", 1200),
            ]
        };
        let starts = |v: Vec<Node>| v.into_iter().map(|n| n.span.start).collect::<Vec<_>>();
        assert_eq!(starts(narrow_to_line(candidates(), 70, tenths)), [700]);
        assert_eq!(starts(narrow_to_line(candidates(), 75, tenths)), [700]);
        // Every candidate below the asked-for line: take the nearest.
        assert_eq!(starts(narrow_to_line(candidates(), 3, tenths)), [100]);
        // Repository unreadable: no narrowing, `Name@file` behaviour.
        assert_eq!(
            starts(narrow_to_line(candidates(), 70, |_| None)),
            [100, 700, 1200]
        );
    }

    #[test]
    fn ambiguous_listing_is_name_at_file_kind_only() {
        let err = SymbolLookupError::Ambiguous {
            requested: "Widget".into(),
            candidates: vec![node("no/such/a.rs", SymbolKind::Enum)],
        };
        let text = err.to_string();
        assert!(text.contains("  Widget@no/such/a.rs (enum)"), "{text}");
        assert!(!text.contains("symbol_key"), "{text}");
        assert!(!text.contains(" id "), "{text}");
    }
}
