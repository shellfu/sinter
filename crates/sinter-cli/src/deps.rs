use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use sinter_store::EdgeFilter;

use crate::lookup::{ensure_snapshot, open_store, unique_symbol_in};
use crate::render::node_json;

/// `sinter deps`: forward blast radius — everything the symbol transitively
/// depends on (calls, uses, imports, ...), cross-file. Ok(true) when any
/// dependency was found (grep-style exit codes).
#[allow(clippy::too_many_arguments)] // mirrors the clap subcommand one-to-one
pub fn run(
    repo: &Path,
    symbol: &str,
    filter: &EdgeFilter,
    max_depth: usize,
    limit: usize,
    json: bool,
    if_snapshot: Option<&str>,
    full_coverage: bool,
    include_tests: bool,
) -> Result<bool> {
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let node = unique_symbol_in(&store, symbol, filter.scopes.as_ref())?;
    let scopes = store.scope_index()?;
    let scope_of = |node: &sinter_core::Node| scopes.scope_of(node);
    let is_test = |node: &sinter_core::Node| crate::prune::is_test_scope(scope_of(node));
    let pruned = crate::prune::prune(
        store.dependencies(&node.id, filter, max_depth)?,
        |r| &r.via.src,
        &crate::prune::Rules {
            keep_tests: include_tests,
            drop_file_rows: filter.relations.is_some(),
            hub_fan_in: None,
            is_test: &is_test,
        },
    );
    let mut reached = pruned.rows;
    let tests_hidden = if include_tests { 0 } else { pruned.tests };
    let total = reached.len();
    let filter_excluded = total == 0
        && tests_hidden == 0
        && crate::prune::filter_excluded(&store, &node.id, filter, max_depth, true)?;
    let root = crate::pipeline::discover_root(repo);
    // Honest-empty signal: unresolved refs inside this definition mean the
    // dependency list may be incomplete, never authoritative.
    let inside: Vec<_> = store
        .unresolved_details_in(&node.file)?
        .into_iter()
        .filter(|r| r.reference.enclosing.as_ref() == Some(&node.id))
        .collect();
    let unresolved = inside.len();
    let inside = crate::coverage::tally(&root, &store, &inside)?;
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        reached.iter().map(|item| item.via.confidence),
        unresolved,
    );
    // Gaps scoped to the files this radius actually touched.
    let radius = crate::coverage::radius_unresolved(
        &root,
        &store,
        std::iter::once(node.file.as_str()).chain(reached.iter().map(|r| r.node.file.as_str())),
    )?;
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
            "max_depth": max_depth,
            "unresolved_refs_in_symbol": unresolved,
            "actionable_unresolved_in_symbol": inside.actionable,
            "by_file": pairs,
            "dependencies": entries,
        });
        if total > limit {
            out["truncated"] = serde_json::json!(total - limit);
        }
        if total == 0 {
            out["verify_with"] =
                serde_json::json!(format!("sinter unresolved --name {}", node.name));
        }
        if filter_excluded {
            out["reason"] = serde_json::json!("filter_excluded");
        }
        if tests_hidden > 0 {
            out["tests_hidden"] = serde_json::json!(tests_hidden);
        }
        out["coverage"] = crate::coverage::coverage_json(
            &root,
            &store,
            filter,
            evidence,
            total > 0,
            full_coverage,
        )?;
        crate::coverage::attach_radius(&mut out["coverage"], radius);
        crate::agent_protocol::write_json(&out)?;
        return Ok(total > 0);
    }
    reached.truncate(limit);
    let hidden = if tests_hidden > 0 {
        format!(", tests: {tests_hidden} (--include-tests)")
    } else {
        String::new()
    };
    if total == 0 {
        println!(
            "not proven: 0 dependencies observed for {} ({}){hidden}",
            qualified_of(node.id.as_str()),
            node.file
        );
        if filter_excluded {
            println!(
                "  reason: filter excluded them (--max-depth 0 / --certain / --evidence / --relations); drop the flag to see them"
            );
        }
        // A blind graph and a leaf symbol look identical here; `unresolved`
        // is the verb that tells them apart.
        println!("  verify: sinter unresolved --name {}", node.name);
    } else {
        println!(
            "{} dependencies of {} ({}){hidden}",
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
    if max_depth == 1 && total > 0 {
        println!("  {total} direct; --max-depth 3 to widen");
    }
    if inside.actionable > 0 {
        println!(
            "  note: {} unresolved ref(s) inside {} — dependencies may be missing; {}",
            inside.actionable,
            node.name,
            crate::coverage::unresolved_hint(&root)
        );
    }
    if let Some(note) = crate::coverage::radius_note(radius) {
        println!("{note}");
    }
    crate::coverage::print_footer(&root, &store, filter, evidence, total > 0, Some(&snapshot))?;
    Ok(total > 0)
}

/// `sinter deps --workspace`: cross-repo forward blast radius over member
/// stores plus boundary links. Single-seed, matching the CLI's one-symbol
/// `deps` argument; text only, since `--json` conflicts with `--workspace`.
pub fn run_workspace(
    manifest: &Path,
    symbol: &str,
    filter: &EdgeFilter,
    max_depth: usize,
    limit: usize,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let ws = crate::workspace::load(manifest)?;
    let snapshot = crate::workspace::snapshot_token(&ws)?;
    crate::lookup::ensure_snapshot_token(if_snapshot, &snapshot)?;
    let (member, node) = crate::workspace::find_symbol(&ws, symbol)?;
    let mut reached = crate::workspace::dependencies(&ws, &member, &node.id, filter, max_depth)?;
    let total = reached.len();
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        reached.iter().map(|item| item.evidence.confidence()),
        0,
    );
    reached.truncate(limit);
    let label = format!(
        "{member}:{} ({})",
        qualified_of(node.id.as_str()),
        node.file
    );
    if total == 0 {
        println!("not proven: 0 dependencies observed for {label}");
    } else {
        println!("{total} dependencies of {label}");
    }
    // Same parent-keyed tree as `affected --workspace`: BFS order alone
    // misattributes children.
    let mut children: std::collections::HashMap<(&str, &str), Vec<&crate::workspace::WsReached>> =
        std::collections::HashMap::new();
    for item in &reached {
        children
            .entry((item.parent.0.as_str(), item.parent.1.as_str()))
            .or_default()
            .push(item);
    }
    let mut stack: Vec<(&crate::workspace::WsReached, usize)> = Vec::new();
    if let Some(roots) = children.get(&(member.as_str(), node.id.as_str())) {
        for item in roots.iter().rev() {
            stack.push((item, 1));
        }
    }
    while let Some((item, depth)) = stack.pop() {
        println!(
            "  {}{}:{} {}  {}  [{}/{}]",
            "  ".repeat(depth - 1),
            item.member,
            qualified_of(item.node.id.as_str()),
            item.node.kind.as_str(),
            item.node.file,
            item.relation.as_str(),
            item.evidence.as_str(),
        );
        if let Some(kids) = children.get(&(item.member.as_str(), item.node.id.as_str())) {
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
    crate::coverage::print_workspace_footer(&ws, filter, evidence, total > 0, Some(&snapshot))?;
    Ok(total > 0)
}
