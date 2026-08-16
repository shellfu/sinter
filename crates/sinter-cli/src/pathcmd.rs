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

/// `sinter path --workspace`: shortest cross-repo path with per-step
/// member, relation, and evidence.
pub fn run_workspace(
    manifest: &std::path::Path,
    from: &str,
    to: &str,
    evidence: &[String],
    certain: bool,
) -> Result<()> {
    let ws = crate::workspace::load(manifest)?;
    let (from_member, from_node) = crate::workspace::find_symbol(&ws, from)?;
    let (to_member, to_node) = crate::workspace::find_symbol(&ws, to)?;
    let filter = crate::lookup::edge_filter(evidence, certain)?;
    match crate::workspace::shortest_path(
        &ws,
        (&from_member, &from_node.id),
        (&to_member, &to_node.id),
        &filter,
    )? {
        None => println!(
            "no path {from_member}:{} -> {to_member}:{}",
            qualified_of(from_node.id.as_str()),
            qualified_of(to_node.id.as_str())
        ),
        Some(steps) => {
            print!("{from_member}:{}", qualified_of(from_node.id.as_str()));
            for (_, _, rel, evid, dst_member, dst_id) in &steps {
                print!(
                    " -[{}/{}]-> {dst_member}:{}",
                    rel.as_str(),
                    evid.as_str(),
                    qualified_of(dst_id)
                );
            }
            println!();
        }
    }
    Ok(())
}
