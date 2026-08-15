use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use crate::lookup::{edge_filter, open_store, unique_symbol};

/// `sinter path`: how one symbol reaches another.
pub fn run(repo: &Path, from: &str, to: &str, evidence: &[String], certain: bool) -> Result<()> {
    let store = open_store(repo)?;
    let from_node = unique_symbol(&store, from)?;
    let to_node = unique_symbol(&store, to)?;
    let filter = edge_filter(evidence, certain)?;
    match store.shortest_path(&from_node.id, &to_node.id, &filter)? {
        None => println!(
            "no path {} -> {}",
            qualified_of(from_node.id.as_str()),
            qualified_of(to_node.id.as_str())
        ),
        Some(edges) => {
            print!("{}", qualified_of(from_node.id.as_str()));
            for edge in &edges {
                print!(
                    " -[{}/{}]-> {}",
                    edge.relation.as_str(),
                    edge.evidence.as_str(),
                    qualified_of(edge.dst.as_str())
                );
            }
            println!();
        }
    }
    Ok(())
}
