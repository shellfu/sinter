use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use sinter_core::Node;
use sinter_store::Store;

use crate::corpus::ScopeSelection;
use crate::lookup::{Found, ensure_snapshot, find_symbol, open_store};
use crate::render::{line_of, location, node_json};

/// Wide trigram net before local ranking: the store cuts at `limit`
/// *before* its majority filter, so a tight limit drops good names.
const SUGGESTION_POOL: usize = 64;

fn trigrams(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

/// Close names, best first: prefix match, then shared trigrams, then the
/// shorter name (a closer edit), then id for determinism.
fn rank_suggestions(store: &Store, symbol: &str) -> Result<Vec<Node>> {
    let query = symbol.to_lowercase();
    let grams = trigrams(&query);
    let mut nodes = store.search(symbol, SUGGESTION_POOL)?;
    let key = |n: &Node| {
        let name = n.name.to_lowercase();
        let shared = grams.iter().filter(|g| name.contains(g.as_str())).count();
        (!name.starts_with(&query), usize::MAX - shared, name.len())
    };
    nodes.sort_by(|a, b| key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id)));
    Ok(nodes)
}

/// `file:LINE (start..end)` header plus signature and doc, matching `show`.
fn print_node(repo: &Path, node: &Node) {
    let line = line_of(repo, &node.file, node.span.start);
    println!(
        "{} {}  {} ({}..{})",
        node.kind.as_str(),
        qualified_of(node.id.as_str()),
        location(repo, &node.file, line),
        node.span.start,
        node.span.end
    );
    if !node.signature.is_empty() {
        println!("    {}", node.signature);
    }
    if let Some(doc) = &node.doc {
        for l in doc.lines().take(3) {
            println!("    /// {l}");
        }
    }
}

/// `sinter query`: exact + trigram symbol search, content-bearing results.
/// Ok(true) when anything matched (grep-style exit codes).
pub fn run(
    repo: &Path,
    symbol: &str,
    limit: usize,
    json: bool,
    if_snapshot: Option<&str>,
    scopes: &ScopeSelection,
) -> Result<bool> {
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let (resolution, exact, mut nodes) = match find_symbol(&store, symbol)? {
        Found::Exact(nodes) => ("exact", true, nodes),
        Found::Relocated(nodes) => ("relocated", false, nodes),
        Found::Suggestions(_) => ("suggestions", false, rank_suggestions(&store, symbol)?),
    };
    let scope_index = store.scope_index()?;
    scopes.narrow(&mut nodes, &scope_index);
    if json {
        // Same shape as the MCP `query` tool.
        crate::agent_protocol::write_json(&serde_json::json!({
            "exact": exact,
            "resolution": resolution,
            "snapshot": snapshot,
            "scope": scopes.json(),
            "results": nodes.iter().take(limit).map(|node| {
                let mut value = node_json(node);
                value["scope"] = serde_json::json!(scope_index.scope_of(node).as_str());
                value
            }).collect::<Vec<_>>(),
        }))?;
        return Ok(!nodes.is_empty());
    }
    if nodes.is_empty() {
        println!("no matches for `{symbol}`");
    } else if exact {
        for node in nodes.iter().take(limit) {
            print_node(repo, node);
        }
    } else if resolution == "relocated" {
        println!("snapshot-local id `{symbol}` moved; current candidate(s):");
        for node in nodes.iter().take(limit) {
            print_node(repo, node);
        }
    } else {
        println!("no exact match for `{symbol}`; close names:");
        for node in nodes.iter().take(limit) {
            print_node(repo, node);
        }
    }
    Ok(!nodes.is_empty())
}
