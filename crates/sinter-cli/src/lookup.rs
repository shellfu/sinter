//! Symbol-argument resolution shared by every query-side command: exact
//! name, qualified suffix, node id, or trigram suggestions.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sinter_core::{Confidence, Evidence, Node, SymbolKey, SymbolKind};
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
    for node in candidates {
        writeln!(
            f,
            "  {}@{}  ({}, symbol_key {}, id {})",
            qualified_of(node.id.as_str()),
            node.file,
            node.kind.as_str(),
            node.symbol_key(),
            node.id
        )?;
    }
    Ok(())
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
    let store = Store::open(&path)?;
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
    let (symbol, file) = match symbol.rsplit_once('@') {
        Some((s, f)) if !s.is_empty() && !f.is_empty() => (s, Some(f)),
        _ => (symbol, None),
    };
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
    matches.sort_by(|a, b| a.id.cmp(&b.id));
    if matches.is_empty() {
        Ok(Found::Suggestions(store.search(symbol, 10)?))
    } else {
        Ok(Found::Exact(matches))
    }
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

/// Exactly one node or a listed-candidates error.
pub fn unique_symbol(store: &Store, symbol: &str) -> Result<Node> {
    match find_symbol(store, symbol)? {
        Found::Exact(mut nodes) if nodes.len() == 1 => Ok(nodes.remove(0)),
        Found::Exact(nodes) => Err(SymbolLookupError::Ambiguous {
            requested: symbol.to_string(),
            candidates: nodes,
        }
        .into()),
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
            let list: Vec<String> = nodes
                .iter()
                .map(|n| format!("  {}", qualified_of(n.id.as_str())))
                .collect();
            Err(NoMatch(format!(
                "no exact match for `{symbol}`; close names:\n{}",
                list.join("\n")
            ))
            .into())
        }
    }
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
            other => {
                bail!("unknown relation `{other}` (calls, uses, imports, implements, extends)")
            }
        });
    }
    Ok(Some(set))
}

/// Content-bearing one-node listing (R3): the reader should not need to
/// open the file.
pub fn print_node(node: &Node) {
    println!(
        "{} {}  {}:{}..{}",
        node.kind.as_str(),
        qualified_of(node.id.as_str()),
        node.file,
        node.span.start,
        node.span.end
    );
    if !node.signature.is_empty() {
        println!("    {}", node.signature);
    }
    if let Some(doc) = &node.doc {
        for line in doc.lines().take(3) {
            println!("    /// {line}");
        }
    }
}
