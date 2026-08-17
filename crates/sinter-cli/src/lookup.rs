//! Symbol-argument resolution shared by every query-side command: exact
//! name, qualified suffix, node id, or trigram suggestions.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sinter_core::{Confidence, Evidence, Node};
use sinter_resolve::qualified_of;
use sinter_store::{EdgeFilter, Store};

use crate::pipeline;

pub fn open_store(repo: &Path) -> Result<Store> {
    let repo = pipeline::discover_root(repo);
    let repo = repo
        .canonicalize()
        .with_context(|| format!("repo path {}", repo.display()))?;
    let path = pipeline::db_path(&repo);
    if !path.exists() {
        bail!("no graph at {} — run `sinter build` first", path.display());
    }
    // Freshness lives at the query boundary: every CLI command and MCP tool
    // syncs before answering, so agents never read a stale graph between
    // commits (hooks cover commit-time; this covers uncommitted edits).
    // A fresh corpus is a scan-floor no-op.
    pipeline::build(&repo, None)?;
    Ok(Store::open(&path)?)
}

/// Nodes matching a symbol argument: full node id, exact name, or qualified
/// suffix (`Config::new`). Empty result falls back to fuzzy suggestions.
pub enum Found {
    Exact(Vec<Node>),
    Suggestions(Vec<Node>),
}

pub fn find_symbol(store: &Store, symbol: &str) -> Result<Found> {
    if symbol.contains('#') {
        if let Some(node) = store.node(&sinter_core::NodeId::new(symbol))? {
            return Ok(Found::Exact(vec![node]));
        }
        return Ok(Found::Suggestions(Vec::new()));
    }
    let name = symbol.rsplit("::").next().unwrap_or(symbol);
    let mut matches: Vec<Node> = store
        .nodes_named(name)?
        .into_iter()
        .filter(|n| {
            let q = qualified_of(n.id.as_str());
            q == symbol || q.ends_with(&format!("::{symbol}"))
        })
        .collect();
    matches.sort_by(|a, b| a.id.cmp(&b.id));
    if matches.is_empty() {
        Ok(Found::Suggestions(store.search(symbol, 10)?))
    } else {
        Ok(Found::Exact(matches))
    }
}

/// Exactly one node or a listed-candidates error.
pub fn unique_symbol(store: &Store, symbol: &str) -> Result<Node> {
    match find_symbol(store, symbol)? {
        Found::Exact(mut nodes) if nodes.len() == 1 => Ok(nodes.remove(0)),
        Found::Exact(nodes) => {
            let list: Vec<String> = nodes
                .iter()
                .map(|n| format!("  {} ({} in {})", n.id.as_str(), n.kind.as_str(), n.file))
                .collect();
            bail!(
                "`{symbol}` is ambiguous — qualify it or pass a node id:\n{}",
                list.join("\n")
            )
        }
        Found::Suggestions(nodes) if nodes.is_empty() => bail!("no symbol matches `{symbol}`"),
        Found::Suggestions(nodes) => {
            let list: Vec<String> = nodes
                .iter()
                .map(|n| format!("  {}", qualified_of(n.id.as_str())))
                .collect();
            bail!(
                "no exact match for `{symbol}`; close names:\n{}",
                list.join("\n")
            )
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
                other => bail!("unknown evidence kind `{other}`"),
            });
        }
        Some(set)
    };
    Ok(EdgeFilter {
        evidence,
        min_confidence: certain.then_some(Confidence::Certain),
    })
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
