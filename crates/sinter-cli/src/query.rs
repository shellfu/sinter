use std::path::Path;

use anyhow::Result;

use crate::lookup::{Found, find_symbol, open_store, print_node};
use crate::render::node_json;

/// `sinter query`: exact + trigram symbol search, content-bearing results.
/// Ok(true) when anything matched (grep-style exit codes).
pub fn run(repo: &Path, symbol: &str, limit: usize, json: bool) -> Result<bool> {
    let store = open_store(repo)?;
    let (exact, nodes) = match find_symbol(&store, symbol)? {
        Found::Exact(nodes) => (true, nodes),
        Found::Suggestions(nodes) => (false, nodes),
    };
    if json {
        // Same shape as the MCP `query` tool.
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "exact": exact,
                "results": nodes.iter().take(limit).map(node_json).collect::<Vec<_>>(),
            }))?
        );
        return Ok(!nodes.is_empty());
    }
    if nodes.is_empty() {
        println!("no matches for `{symbol}`");
    } else if exact {
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
