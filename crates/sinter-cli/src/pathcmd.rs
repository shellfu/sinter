use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use sinter_store::EdgeFilter;

use crate::lookup::{ensure_snapshot, ensure_snapshot_token, open_store, unique_symbol};

/// `sinter path`: how one symbol reaches another. Ok(true) when a route
/// exists (grep-style exit codes).
pub fn run(
    repo: &Path,
    from: &str,
    to: &str,
    filter: &EdgeFilter,
    json: bool,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let from_node = unique_symbol(&store, from)?;
    let to_node = unique_symbol(&store, to)?;
    let path = store.shortest_path(&from_node.id, &to_node.id, filter)?;
    let scopes = store.file_scopes()?;
    let scope_of_id = |id: &sinter_core::NodeId| {
        let file = id
            .as_str()
            .split_once('#')
            .map_or(id.as_str(), |(file, _)| file);
        scopes
            .get(file)
            .copied()
            .unwrap_or_else(|| sinter_core::CorpusScope::classify_path(file))
    };
    let root = crate::pipeline::discover_root(repo);
    let miss = path
        .is_none()
        .then(|| explain_miss(&store, &from_node, &to_node, filter))
        .transpose()?;
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        path.iter().flatten().map(|edge| edge.confidence),
        miss.as_ref()
            .map_or(0, |miss| miss.unresolved_matching_target),
    );
    if json {
        // Same shape as the MCP `path` tool.
        let mut out = serde_json::json!({
            "status": if path.is_some() { "found" } else { "not_proven" },
            "snapshot": snapshot,
            "found": path.is_some(),
            "steps": path.iter().flatten().map(|e| serde_json::json!({
                "from": qualified_of(e.src.as_str()),
                "to": qualified_of(e.dst.as_str()),
                "from_scope": scope_of_id(&e.src).as_str(),
                "to_scope": scope_of_id(&e.dst).as_str(),
                "relation": e.relation.as_str(),
                "evidence": e.evidence.as_str(),
                "confidence": match e.confidence {
                    sinter_core::Confidence::Certain => "certain",
                    sinter_core::Confidence::Inferred => "possible",
                },
                "site": crate::render::site_json(&root, e),
            })).collect::<Vec<_>>(),
        });
        if let Some(miss) = &miss {
            out["miss"] = miss_json(&root, miss);
        }
        out["coverage"] =
            crate::coverage::traversal_json(&root, &store, filter, evidence, path.is_some())?;
        crate::agent_protocol::write_json(&out)?;
        return Ok(path.is_some());
    }
    match path {
        None => {
            println!(
                "not proven: no path {} -> {} observed",
                qualified_of(from_node.id.as_str()),
                qualified_of(to_node.id.as_str())
            );
            print_miss(
                &root,
                &from_node,
                &to_node,
                miss.as_ref().expect("a missing path has miss evidence"),
            );
            println!("  snapshot: {snapshot}");
            crate::coverage::print_traversal(&root, &store, filter, evidence, false)?;
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
            println!("  snapshot: {snapshot}");
            crate::coverage::print_traversal(&root, &store, filter, evidence, true)?;
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
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let ws = crate::workspace::load(manifest)?;
    let snapshot = crate::workspace::snapshot_token(&ws)?;
    ensure_snapshot_token(if_snapshot, &snapshot)?;
    let (from_member, from_node) = crate::workspace::find_symbol(&ws, from)?;
    let (to_member, to_node) = crate::workspace::find_symbol(&ws, to)?;
    let path = crate::workspace::shortest_path(
        &ws,
        (&from_member, &from_node.id),
        (&to_member, &to_node.id),
        filter,
    )?;
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        path.iter()
            .flatten()
            .map(|(_, _, _, evidence, _, _)| evidence.confidence()),
        0,
    );
    match path {
        None => {
            println!(
                "not proven: no path {from_member}:{} -> {to_member}:{} observed",
                qualified_of(from_node.id.as_str()),
                qualified_of(to_node.id.as_str())
            );
            println!("  snapshot: {snapshot}");
            crate::coverage::print_workspace_traversal(&ws, filter, evidence, false)?;
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
            println!("  snapshot: {snapshot}");
            crate::coverage::print_workspace_traversal(&ws, filter, evidence, true)?;
            Ok(true)
        }
    }
}

/// Why a path search came up empty, in the two numbers an agent needs
/// next: how far the forward search got, and which edges actually reach
/// the target (so the query can be rerun from the gap). Dynamic edges
/// excluded by the filter are counted, since trait dispatch is the usual
/// missing hop.
pub struct Miss {
    pub forward_reached: usize,
    pub reached_by: Vec<sinter_core::Edge>,
    pub excluded_by_filter: usize,
    pub unresolved_matching_target: usize,
}

pub fn explain_miss(
    store: &sinter_store::Store,
    from: &sinter_core::Node,
    to: &sinter_core::Node,
    filter: &EdgeFilter,
) -> Result<Miss> {
    let forward = store.dependencies(&from.id, filter, usize::MAX)?;
    let forward_reached = forward.len();
    let mut reachable = std::collections::HashSet::from([from.id.clone()]);
    reachable.extend(forward.iter().map(|reached| reached.node.id.clone()));
    let mut files = std::collections::BTreeSet::from([from.file.as_str()]);
    files.extend(forward.iter().map(|reached| reached.node.file.as_str()));
    let mut unresolved_matching_target = 0;
    for file in files {
        unresolved_matching_target += store
            .references_in(file)?
            .iter()
            .filter(|reference| {
                reference
                    .enclosing
                    .as_ref()
                    .is_some_and(|enclosing| reachable.contains(enclosing))
                    && name_tail_matches(&reference.name, &to.name)
            })
            .count();
    }
    let inn = store.in_edges(&to.id)?;
    let candidates: Vec<&sinter_core::Edge> = inn
        .iter()
        .filter(|e| e.relation != sinter_core::Relation::Contains)
        .collect();
    let excluded_by_filter = candidates.iter().filter(|e| !filter.admits(e)).count();
    let reached_by = candidates
        .into_iter()
        .filter(|e| filter.admits(e))
        .cloned()
        .collect();
    Ok(Miss {
        forward_reached,
        reached_by,
        excluded_by_filter,
        unresolved_matching_target,
    })
}

/// Same shape for `--json` and the MCP `path` tool.
pub fn miss_json(root: &Path, miss: &Miss) -> serde_json::Value {
    serde_json::json!({
        "forward_reached": miss.forward_reached,
        "reached_by": miss.reached_by.iter().map(|e| serde_json::json!({
            "from": qualified_of(e.src.as_str()),
            "relation": e.relation.as_str(),
            "evidence": e.evidence.as_str(),
            "site": crate::render::site_json(root, e),
        })).collect::<Vec<_>>(),
        "excluded_by_filter": miss.excluded_by_filter,
        "unresolved_matching_target": miss.unresolved_matching_target,
    })
}

fn print_miss(root: &Path, from: &sinter_core::Node, to: &sinter_core::Node, miss: &Miss) {
    println!(
        "  forward search from {} reached {} symbol(s)",
        qualified_of(from.id.as_str()),
        miss.forward_reached
    );
    if miss.reached_by.is_empty() {
        println!(
            "  nothing reaches {} under this filter",
            qualified_of(to.id.as_str())
        );
    } else {
        println!(
            "  {} is reached by ({}):",
            qualified_of(to.id.as_str()),
            miss.reached_by.len()
        );
        for e in miss.reached_by.iter().take(8) {
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
        if miss.reached_by.len() > 8 {
            println!("    … (+{})", miss.reached_by.len() - 8);
        }
    }
    if miss.excluded_by_filter > 0 {
        println!(
            "  {} incoming edge(s) excluded by --evidence/--certain",
            miss.excluded_by_filter
        );
    }
    if miss.unresolved_matching_target > 0 {
        println!(
            "  {} unresolved ref(s) on the forward frontier name `{}` — the path may be missing; `sinter scip` would bind them",
            miss.unresolved_matching_target, to.name,
        );
    }
}

fn name_tail_matches(written: &str, name: &str) -> bool {
    let tail = written.rsplit("::").next().unwrap_or(written);
    let tail = tail.rsplit(['/', '.']).next().unwrap_or(tail);
    tail == name
}
