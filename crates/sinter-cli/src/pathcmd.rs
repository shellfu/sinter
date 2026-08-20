use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use sinter_store::EdgeFilter;

use crate::lookup::{open_store, unique_symbol};

/// `sinter path`: how one symbol reaches another. Ok(true) when a route
/// exists (grep-style exit codes).
pub fn run(repo: &Path, from: &str, to: &str, filter: &EdgeFilter, json: bool) -> Result<bool> {
    let store = open_store(repo)?;
    let from_node = unique_symbol(&store, from)?;
    let to_node = unique_symbol(&store, to)?;
    let path = store.shortest_path(&from_node.id, &to_node.id, filter)?;
    let root = crate::pipeline::discover_root(repo);
    if json {
        // Same shape as the MCP `path` tool.
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "found": path.is_some(),
                "steps": path.iter().flatten().map(|e| serde_json::json!({
                    "from": qualified_of(e.src.as_str()),
                    "to": qualified_of(e.dst.as_str()),
                    "relation": e.relation.as_str(),
                    "evidence": e.evidence.as_str(),
                    "site": crate::render::site_json(&root, e),
                })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(path.is_some());
    }
    match path {
        None => {
            println!(
                "no path {} -> {}",
                qualified_of(from_node.id.as_str()),
                qualified_of(to_node.id.as_str())
            );
            explain_miss(&store, &root, &from_node, &to_node, filter)?;
            if let Some(note) = crate::scip::stale_note(repo) {
                println!("{note}");
            }
            Ok(false)
        }
        Some(edges) => {
            print!("{}", qualified_of(from_node.id.as_str()));
            for edge in &edges {
                // Each hop names where it is written: `at file:line`.
                let site = crate::render::site_location(&root, edge)
                    .map(|s| format!(" at {s}"))
                    .unwrap_or_default();
                print!(
                    " -[{}/{}{}]-> {}",
                    edge.relation.as_str(),
                    edge.evidence.as_str(),
                    site,
                    qualified_of(edge.dst.as_str())
                );
            }
            println!();
            Ok(true)
        }
    }
}

/// `sinter path --workspace`: shortest cross-repo path with per-step
/// member, relation, and evidence.
pub fn run_workspace(
    manifest: &std::path::Path,
    from: &str,
    to: &str,
    filter: &EdgeFilter,
) -> Result<bool> {
    let ws = crate::workspace::load(manifest)?;
    let (from_member, from_node) = crate::workspace::find_symbol(&ws, from)?;
    let (to_member, to_node) = crate::workspace::find_symbol(&ws, to)?;
    match crate::workspace::shortest_path(
        &ws,
        (&from_member, &from_node.id),
        (&to_member, &to_node.id),
        filter,
    )? {
        None => {
            println!(
                "no path {from_member}:{} -> {to_member}:{}",
                qualified_of(from_node.id.as_str()),
                qualified_of(to_node.id.as_str())
            );
            Ok(false)
        }
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
            Ok(true)
        }
    }
}

/// Why a path search came up empty, in the two numbers an agent needs
/// next: how far the forward search got, and which edges actually reach
/// the target (so the query can be rerun from the gap). Dynamic edges
/// excluded by the filter are named, since trait dispatch is the usual
/// missing hop.
fn explain_miss(
    store: &sinter_store::Store,
    root: &Path,
    from: &sinter_core::Node,
    to: &sinter_core::Node,
    filter: &EdgeFilter,
) -> Result<()> {
    let forward = store.dependencies(&from.id, filter, usize::MAX)?;
    println!(
        "  forward search from {} reached {} symbol(s)",
        qualified_of(from.id.as_str()),
        forward.len()
    );
    let inn = store.in_edges(&to.id)?;
    let mut reaching: Vec<&sinter_core::Edge> = inn
        .iter()
        .filter(|e| e.relation != sinter_core::Relation::Contains)
        .collect();
    let excluded = reaching.iter().filter(|e| !filter.admits(e)).count();
    reaching.retain(|e| filter.admits(e));
    if reaching.is_empty() {
        println!(
            "  nothing reaches {} under this filter",
            qualified_of(to.id.as_str())
        );
    } else {
        println!(
            "  {} is reached by ({}):",
            qualified_of(to.id.as_str()),
            reaching.len()
        );
        for e in reaching.iter().take(8) {
            let site = crate::render::site_location(root, e)
                .map(|s| format!(" at {s}"))
                .unwrap_or_default();
            println!(
                "    {} [{}/{}]{site}",
                qualified_of(e.src.as_str()),
                e.relation.as_str(),
                e.evidence.as_str()
            );
        }
        if reaching.len() > 8 {
            println!("    … (+{})", reaching.len() - 8);
        }
    }
    if excluded > 0 {
        println!("  {excluded} incoming edge(s) excluded by --evidence/--certain");
    }
    Ok(())
}
