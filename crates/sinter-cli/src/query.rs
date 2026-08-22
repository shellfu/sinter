use std::path::Path;

use anyhow::Result;

use crate::corpus::ScopeSelection;
use crate::lookup::{Found, ensure_snapshot, find_symbol, open_store, print_node};
use crate::render::node_json;

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
        Found::Suggestions(nodes) => ("suggestions", false, nodes),
    };
    let file_scopes = store.file_scopes()?;
    nodes.retain(|node| {
        scopes.contains(
            file_scopes
                .get(&node.file)
                .copied()
                .unwrap_or_else(|| sinter_core::CorpusScope::classify_path(&node.file)),
        )
    });
    if json {
        // Same shape as the MCP `query` tool.
        crate::agent_protocol::write_json(&serde_json::json!({
            "exact": exact,
            "resolution": resolution,
            "snapshot": snapshot,
            "scope": scopes.json(),
            "results": nodes.iter().take(limit).map(|node| {
                let mut value = node_json(node);
                value["scope"] = serde_json::json!(file_scopes
                    .get(&node.file)
                    .copied()
                    .unwrap_or_else(|| sinter_core::CorpusScope::classify_path(&node.file))
                    .as_str());
                value
            }).collect::<Vec<_>>(),
        }))?;
        return Ok(!nodes.is_empty());
    }
    if nodes.is_empty() {
        println!("no matches for `{symbol}`");
    } else if exact {
        for node in nodes.iter().take(limit) {
            print_node(node);
        }
    } else if resolution == "relocated" {
        println!("snapshot-local id `{symbol}` moved; current candidate(s):");
        for node in nodes.iter().take(limit) {
            print_node(node);
        }
    } else {
        println!("no exact match for `{symbol}`; close names:");
        for node in nodes.iter().take(limit) {
            print_node(node);
        }
    }
    Ok(!nodes.is_empty())
}
