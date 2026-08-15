use std::path::Path;

use anyhow::Result;

use crate::lookup::{Found, find_symbol, open_store, print_node};

/// `sinter query`: exact + trigram symbol search, content-bearing results.
pub fn run(repo: &Path, symbol: &str, limit: usize) -> Result<()> {
    let store = open_store(repo)?;
    match find_symbol(&store, symbol)? {
        Found::Exact(nodes) => {
            for node in nodes.iter().take(limit) {
                print_node(node);
            }
        }
        Found::Suggestions(nodes) if nodes.is_empty() => {
            println!("no matches for `{symbol}`");
        }
        Found::Suggestions(nodes) => {
            println!("no exact match for `{symbol}`; close names:");
            for node in nodes.iter().take(limit) {
                print_node(node);
            }
        }
    }
    Ok(())
}
