use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use sinter_store::EdgeFilter;

use crate::lookup::{ensure_snapshot, open_store, unique_symbol_in};
use crate::render::node_json;

/// `sinter deps`: forward blast radius — everything the symbol transitively
/// depends on (calls, uses, imports, ...), cross-file. Ok(true) when any
/// dependency was found (grep-style exit codes).
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
    let node = unique_symbol_in(&store, symbol, filter.scopes.as_ref())?;
    let mut reached = store.dependencies(&node.id, filter, max_depth)?;
    let scopes = store.scope_index()?;
    let scope_of = |node: &sinter_core::Node| scopes.scope_of(node);
    let total = reached.len();
    let root = crate::pipeline::discover_root(repo);
    // Honest-empty signal: unresolved refs inside this definition mean the
    // dependency list may be incomplete, never authoritative.
    let unresolved = store
        .references_in(&node.file)?
        .iter()
        .filter(|r| r.enclosing.as_ref() == Some(&node.id))
        .count();
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        reached.iter().map(|item| item.via.confidence),
        unresolved,
    );
    if json {
        // Same shape as the MCP `deps` tool (terse entries, like affected).
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
            "unresolved_refs_in_symbol": unresolved,
            "by_file": pairs,
            "dependencies": entries,
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
            "not proven: 0 dependencies observed for {} ({})",
            qualified_of(node.id.as_str()),
            node.file
        );
    } else {
        println!(
            "{} dependencies of {} ({})",
            total,
            qualified_of(node.id.as_str()),
            node.file
        );
    }
    // Same tree rendering as affected, keyed by the node each dependency
    // was reached from (via.src).
    let mut children: std::collections::HashMap<&str, Vec<&sinter_store::Reached>> =
        std::collections::HashMap::new();
    for r in &reached {
        children.entry(r.via.src.as_str()).or_default().push(r);
    }
    let mut stack: Vec<(&sinter_store::Reached, usize)> = Vec::new();
    let mut roots: Vec<&&sinter_store::Reached> = Vec::new();
    // A file start seeds through its contained symbols, so roots are every
    // reached node whose parent was never itself reached.
    let reached_ids: std::collections::HashSet<&str> =
        reached.iter().map(|r| r.node.id.as_str()).collect();
    for (parent, kids) in &children {
        if !reached_ids.contains(parent) {
            roots.extend(kids.iter());
        }
    }
    roots.sort_by_key(|r| r.node.id.as_str());
    for r in roots.iter().rev() {
        stack.push((r, 1));
    }
    while let Some((r, depth)) = stack.pop() {
        // Site is in the *parent's* file (via.src), so it appends after
        // the evidence instead of replacing the dependency's file.
        let site = crate::render::site_location(&root, &r.via)
            .map(|s| format!("  at {s}"))
            .unwrap_or_default();
        println!(
            "  {}{} {}  {}  [{}/{}]{}",
            "  ".repeat(depth - 1),
            qualified_of(r.node.id.as_str()),
            r.node.kind.as_str(),
            r.node.file,
            r.via.relation.as_str(),
            r.via.evidence.as_str(),
            site,
        );
        if let Some(kids) = children.get(r.node.id.as_str()) {
            for kid in kids.iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }
    if total > limit {
        println!(
            "{} more dependencies below cutoff · `sinter deps --limit {}` to widen",
            total - limit,
            total,
        );
    }
    if unresolved > 0 {
        println!(
            "  note: {unresolved} unresolved ref(s) inside {} — dependencies may be missing; {}",
            node.name,
            crate::coverage::unresolved_hint(&root)
        );
    }
    crate::coverage::print_footer(&root, &store, filter, evidence, total > 0, Some(&snapshot))?;
    Ok(total > 0)
}
