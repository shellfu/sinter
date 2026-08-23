use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use sinter_store::EdgeFilter;

use crate::lookup::{ensure_snapshot, ensure_snapshot_token, open_store, unique_symbol_in};
use crate::render::node_json;

/// `sinter affected`: reverse blast radius — everything transitively
/// depending on the symbol, cross-file. Ok(true) when any dependent (or
/// external reference site) was found (grep-style exit codes).
pub fn run(
    repo: &Path,
    symbol: &str,
    filter: &EdgeFilter,
    max_depth: usize,
    limit: usize,
    json: bool,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let root = crate::pipeline::discover_root(repo);
    let node = match unique_symbol_in(&store, symbol, filter.scopes.as_ref()) {
        Ok(node) => node,
        // Not defined here — dependency blast radius at the repo boundary
        // is still an answer: every site referencing the external symbol.
        Err(e) if e.is::<crate::lookup::NoMatch>() => {
            let sites = crate::lookup::external_sites(&store, symbol)?;
            if sites.is_empty() {
                return Err(e);
            }
            if json {
                // Same shape as the MCP `affected` tool's external answer.
                let unresolved: usize = sites.iter().map(|site| site.refs).sum();
                let mut out = serde_json::json!({
                    "status": "found",
                    "snapshot": snapshot,
                    "external": true,
                    "note": "symbol is not defined in this repo; sites reference it (dependency blast radius at the repo boundary)",
                    "sites": sites.iter().map(|s| serde_json::json!({
                        "enclosing": s.enclosing,
                        "file": s.file,
                        "refs": s.refs,
                    })).collect::<Vec<_>>(),
                });
                out["coverage"] = crate::coverage::traversal_json(
                    &root,
                    &store,
                    filter,
                    crate::coverage::TraversalEvidence {
                        unresolved,
                        ..Default::default()
                    },
                    true,
                )?;
                crate::agent_protocol::write_json(&out)?;
                return Ok(true);
            }
            let total: usize = sites.iter().map(|s| s.refs).sum();
            println!(
                "`{symbol}` is not defined in this repo; {total} reference(s) at {} site(s):",
                sites.len()
            );
            for s in &sites {
                println!(
                    "  {}  {}  ({} ref(s))",
                    s.enclosing.as_deref().unwrap_or("<file scope>"),
                    s.file,
                    s.refs
                );
            }
            crate::coverage::print_footer(
                &root,
                &store,
                filter,
                crate::coverage::TraversalEvidence {
                    unresolved: total,
                    ..Default::default()
                },
                true,
                Some(&snapshot),
            )?;
            return Ok(true);
        }
        Err(e) => return Err(e),
    };
    let mut reached = store.dependents(&node.id, filter, max_depth)?;
    let scopes = store.scope_index()?;
    let scope_of = |node: &sinter_core::Node| scopes.scope_of(node);
    let total = reached.len();
    // File `use` lines are dependents too, but they are not callers: count
    // them separately so "N direct" means N symbols that actually use it.
    let is_import = |r: &sinter_store::Reached| r.via.relation == sinter_core::Relation::Imports;
    let callers: Vec<&sinter_store::Reached> = reached
        .iter()
        .filter(|r| r.depth == 1 && !is_import(r))
        .collect();
    let direct = callers.len();
    let direct_files = callers
        .iter()
        .map(|r| r.node.file.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let importing_files = reached
        .iter()
        .filter(|r| r.depth == 1 && is_import(r))
        .map(|r| r.node.file.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let unresolved = store.unresolved_named(&node.name)?;
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        reached.iter().map(|item| item.via.confidence),
        unresolved,
    );
    if json {
        // Same shape as the MCP `affected` tool (terse entries).
        let entries: Vec<serde_json::Value> = reached
            .iter()
            .take(limit)
            .map(|r| {
                let mut entry = serde_json::json!({
                    "s": qualified_of(r.node.id.as_str()),
                    "k": r.node.kind.as_str(),
                    "f": r.node.file,
                    "scope": scope_of(&r.node).as_str(),
                    "e": format!("{}/{}", r.via.relation.as_str(), r.via.evidence.as_str()),
                    "c": match r.via.confidence {
                        sinter_core::Confidence::Certain => "certain",
                        sinter_core::Confidence::Inferred => "possible",
                    },
                    "d": r.depth,
                });
                let site = crate::render::site_json(&root, &r.via);
                if !site.is_null() {
                    entry["site"] = site;
                }
                entry
            })
            .collect();
        let mut counts = std::collections::HashMap::<String, u64>::new();
        for r in &reached {
            *counts.entry(r.node.file.clone()).or_default() += 1;
        }
        let mut pairs: Vec<_> = counts.into_iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        pairs.truncate(10);
        let mut symbol_json = node_json(&node);
        symbol_json["scope"] = serde_json::json!(scope_of(&node).as_str());
        let mut out = serde_json::json!({
            "status": if total > 0 { "found" } else { "not_proven" },
            "symbol": symbol_json,
            "snapshot": snapshot,
            "total": total,
            "direct": direct,
            "direct_files": direct_files,
            "importing_files": importing_files,
            "unresolved_refs_matching_name": unresolved,
            "scip_evidence_available": crate::pipeline::scip_index_path(&root).is_some(),
            "by_file": pairs,
            "dependents": entries,
        });
        if total > limit {
            out["truncated"] = serde_json::json!(total - limit);
        }
        out["coverage"] =
            crate::coverage::traversal_json(&root, &store, filter, evidence, total > 0)?;
        crate::agent_protocol::write_json(&out)?;
        return Ok(total > 0);
    }
    reached.truncate(limit);
    if total == 0 {
        println!(
            "not proven: 0 dependents observed for {} ({})",
            qualified_of(node.id.as_str()),
            node.file,
        );
    } else {
        let imports = if importing_files > 0 {
            format!("; {importing_files} file(s) import it")
        } else {
            String::new()
        };
        println!(
            "{} dependents of {} ({}): {direct} direct in {direct_files} file(s){imports}, {} transitive",
            total,
            qualified_of(node.id.as_str()),
            node.file,
            total - direct - importing_files,
        );
    }
    // Render as a real tree: each dependent indents under the node it
    // actually reaches (via.dst), not under whatever BFS printed last.
    let mut children: std::collections::HashMap<&str, Vec<&sinter_store::Reached>> =
        std::collections::HashMap::new();
    for r in &reached {
        children.entry(r.via.dst.as_str()).or_default().push(r);
    }
    let mut stack: Vec<(&sinter_store::Reached, usize)> = Vec::new();
    if let Some(roots) = children.get(node.id.as_str()) {
        for r in roots.iter().rev() {
            stack.push((r, 1));
        }
    }
    while let Some((r, depth)) = stack.pop() {
        // The call site (in the dependent's own file) replaces the bare
        // file — "depends on it at file:line", not just "depends on it".
        let place =
            crate::render::site_location(&root, &r.via).unwrap_or_else(|| r.node.file.clone());
        println!(
            "  {}{} {}  {}  [{}/{}]",
            "  ".repeat(depth - 1),
            qualified_of(r.node.id.as_str()),
            r.node.kind.as_str(),
            place,
            r.via.relation.as_str(),
            r.via.evidence.as_str(),
        );
        if let Some(kids) = children.get(r.node.id.as_str()) {
            for kid in kids.iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }
    if total > limit {
        println!(
            "{} more dependents below cutoff · `sinter affected --limit {}` to widen",
            total - limit,
            total,
        );
    }
    if unresolved > 0 {
        println!(
            "  note: {unresolved} unresolved ref(s) also name `{}` — dependents may be missing; {}",
            node.name,
            crate::coverage::unresolved_hint(&root)
        );
    }
    crate::coverage::print_footer(&root, &store, filter, evidence, total > 0, Some(&snapshot))?;
    Ok(total > 0)
}

/// `sinter affected --workspace`: cross-repo blast radius over member
/// stores plus boundary links.
pub fn run_workspace(
    manifest: &std::path::Path,
    symbol: &str,
    filter: &EdgeFilter,
    max_depth: usize,
    limit: usize,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let ws = crate::workspace::load(manifest)?;
    let snapshot = crate::workspace::snapshot_token(&ws)?;
    ensure_snapshot_token(if_snapshot, &snapshot)?;
    let (member, node) = crate::workspace::find_symbol(&ws, symbol)?;
    let mut reached = crate::workspace::dependents(&ws, &member, &node.id, filter, max_depth)?;
    let total = reached.len();
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        reached.iter().map(|item| item.evidence.confidence()),
        0,
    );
    reached.truncate(limit);
    if total == 0 {
        println!(
            "not proven: 0 dependents observed for {member}:{} ({})",
            qualified_of(node.id.as_str()),
            node.file
        );
    } else {
        println!(
            "{} dependents of {member}:{} ({})",
            total,
            qualified_of(node.id.as_str()),
            node.file
        );
    }
    let mut children: std::collections::HashMap<(&str, &str), Vec<&crate::workspace::WsReached>> =
        std::collections::HashMap::new();
    for r in &reached {
        children
            .entry((r.parent.0.as_str(), r.parent.1.as_str()))
            .or_default()
            .push(r);
    }
    let mut stack: Vec<(&crate::workspace::WsReached, usize)> = Vec::new();
    if let Some(roots) = children.get(&(member.as_str(), node.id.as_str())) {
        for r in roots.iter().rev() {
            stack.push((r, 1));
        }
    }
    while let Some((r, depth)) = stack.pop() {
        println!(
            "  {}{}:{} {}  {}  [{}/{}]",
            "  ".repeat(depth - 1),
            r.member,
            qualified_of(r.node.id.as_str()),
            r.node.kind.as_str(),
            r.node.file,
            r.relation.as_str(),
            r.evidence.as_str(),
        );
        if let Some(kids) = children.get(&(r.member.as_str(), r.node.id.as_str())) {
            for kid in kids.iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }
    if total > limit {
        println!(
            "{} more dependents below cutoff · `sinter affected --limit {}` to widen",
            total - limit,
            total,
        );
    }
    crate::coverage::print_workspace_footer(&ws, filter, evidence, total > 0, Some(&snapshot))?;
    Ok(total > 0)
}
