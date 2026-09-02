use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use sinter_store::EdgeFilter;

use crate::lookup::{
    SymbolLookupError, candidate_labels, candidates_in, ensure_snapshot, ensure_snapshot_token,
    open_store,
};

/// Ambiguous endpoints are tried pairwise up to this many pairs.
const MAX_PAIRS: usize = 16;

/// Frontier rows reported on a miss.
const MAX_FRONTIER: usize = 5;

/// Incoming edges of the target reported on a miss: enough to name the
/// hop that is missing, not the target's whole caller list.
const MAX_REACHED_BY: usize = 5;

/// `sinter path`: how one symbol reaches another. Ok(true) when a route
/// exists (grep-style exit codes). `k` routes are node-disjoint: each
/// avoids the interior symbols of the routes before it.
#[allow(clippy::too_many_arguments)] // mirrors the clap subcommand one-to-one
pub fn run(
    repo: &Path,
    from: &str,
    to: &str,
    filter: &EdgeFilter,
    json: bool,
    if_snapshot: Option<&str>,
    full_coverage: bool,
    k: usize,
) -> Result<bool> {
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let froms = candidates_in(&store, from, filter.scopes.as_ref())?;
    let tos = candidates_in(&store, to, filter.scopes.as_ref())?;
    let ambiguous = froms.len() > 1 || tos.len() > 1;
    // Ambiguous endpoints: any connecting pair answers the question.
    let mut pairs = froms
        .iter()
        .flat_map(|f| tos.iter().map(move |t| (f, t)))
        .take(MAX_PAIRS);
    let mut found = None;
    let mut first = None;
    for (f, t) in &mut pairs {
        let path = store.shortest_path(&f.id, &t.id, filter)?;
        if path.is_some() {
            found = Some((f.clone(), t.clone(), path));
            break;
        }
        first.get_or_insert((f.clone(), t.clone(), path));
    }
    let (from_node, to_node, path) = found.or(first).expect("candidate lists are non-empty");
    if ambiguous && path.is_none() {
        if json {
            let (requested, candidates) = if froms.len() > 1 {
                (from, froms)
            } else {
                (to, tos)
            };
            return Err(SymbolLookupError::Ambiguous {
                requested: requested.to_string(),
                candidates,
            }
            .into());
        }
        println!("not proven: no path {from} -> {to} observed for any candidate pair");
        for (name, nodes) in [(from, &froms), (to, &tos)] {
            if nodes.len() > 1 {
                println!("  `{name}` candidates:");
                // Rendered as a list: each label is unique within it, so
                // one can be pasted straight back as the next selector.
                for label in candidate_labels(nodes) {
                    println!("    {label}");
                }
            }
        }
        println!("  snapshot: {snapshot}");
        let root = crate::pipeline::discover_root(repo);
        print_actionable(&root, &explain_miss(&store, &from_node, &to_node, filter)?);
        return Ok(false);
    }
    if ambiguous && !json {
        println!(
            "from: {}@{}  to: {}@{}",
            qualified_of(from_node.id.as_str()),
            from_node.file,
            qualified_of(to_node.id.as_str()),
            to_node.file
        );
    }
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
    let paths = match &path {
        Some(first) if k > 1 => {
            disjoint_paths(&store, &from_node.id, &to_node.id, filter, first, k)?
        }
        Some(first) => vec![first.clone()],
        None => Vec::new(),
    };
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        path.iter().flatten().map(|edge| edge.confidence),
        miss.as_ref()
            .map_or(0, |miss| miss.unresolved_matching_target),
    );
    // Gaps scoped to the files the found route runs through: unresolved
    // refs there mean a shorter or different route may be missing. A miss
    // carries its own gap evidence (`miss.unresolved_matching_target`).
    let radius = match &path {
        Some(edges) => Some(crate::coverage::radius_unresolved(
            &root,
            &store,
            std::iter::once(from_node.file.as_str()).chain(
                edges
                    .iter()
                    .flat_map(|e| [e.src.as_str(), e.dst.as_str()])
                    .map(|id| id.split_once('#').map_or(id, |(file, _)| file)),
            ),
        )?),
        None => None,
    };
    if json {
        // Same shape as the MCP `path` tool.
        let steps_json = |edges: &[sinter_core::Edge]| {
            edges
                .iter()
                .map(|e| {
                    let mut row = serde_json::json!({
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
                    });
                    crate::render::add_sites(&mut row, &root, e);
                    row
                })
                .collect::<Vec<_>>()
        };
        let mut out = serde_json::json!({
            "status": if path.is_some() { "found" } else { "not_proven" },
            "snapshot": snapshot,
            "found": path.is_some(),
            "steps": steps_json(paths.first().map_or(&[][..], Vec::as_slice)),
        });
        if k > 1 {
            out["paths"] =
                serde_json::json!(paths.iter().map(|p| steps_json(p)).collect::<Vec<_>>());
        }
        if let Some(miss) = &miss {
            out["miss"] = miss_json(&root, miss);
            if !miss.excluded_edges.is_empty() {
                out["reason"] = serde_json::json!("filter_excluded");
            }
        }
        out["coverage"] = crate::coverage::coverage_json(
            &root,
            &store,
            filter,
            evidence,
            path.is_some(),
            full_coverage,
        )?;
        if let Some(radius) = radius {
            crate::coverage::attach_radius(&mut out["coverage"], radius);
        }
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
            crate::coverage::print_footer(&root, &store, filter, evidence, false, Some(&snapshot))?;
            Ok(false)
        }
        Some(_) => {
            for edges in &paths {
                print!("{}", qualified_of(from_node.id.as_str()));
                for edge in edges {
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
            }
            if k > 1 && paths.len() < k {
                println!(
                    "  {} node-disjoint route(s); no further route avoids them",
                    paths.len()
                );
            }
            if let Some(note) = radius.and_then(crate::coverage::radius_note) {
                println!("{note}");
            }
            crate::coverage::print_footer(&root, &store, filter, evidence, true, Some(&snapshot))?;
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

/// Routes after the first, each avoiding the interior symbols of every
/// route before it (a route sharing all but one hop with the first tells
/// an agent nothing new). Stops at the first miss, so fewer than `k` is
/// the honest count.
fn disjoint_paths(
    store: &sinter_store::Store,
    from: &sinter_core::NodeId,
    to: &sinter_core::NodeId,
    filter: &EdgeFilter,
    first: &[sinter_core::Edge],
    k: usize,
) -> Result<Vec<Vec<sinter_core::Edge>>> {
    let scopes = store.scope_index()?;
    let mut banned: std::collections::HashSet<sinter_core::NodeId> =
        std::collections::HashSet::new();
    let mut paths = vec![first.to_vec()];
    while paths.len() < k {
        let last = paths.last().expect("one route is always present");
        let interior = last.iter().map(|e| e.dst.clone()).filter(|id| id != to);
        // A direct edge has no interior to avoid: the next search would
        // only find it again.
        let mut grew = false;
        for id in interior {
            grew |= banned.insert(id);
        }
        if !grew {
            break;
        }
        let Some(path) = path_avoiding(store, &scopes, from, to, filter, &banned)? else {
            break;
        };
        paths.push(path);
    }
    Ok(paths)
}

/// Breadth-first shortest route over outgoing edges that never enters a
/// banned node. Same admission rules as the store's `shortest_path`,
/// minus the file-start containment seeding (a file seed has one route).
fn path_avoiding(
    store: &sinter_store::Store,
    scopes: &sinter_store::ScopeIndex,
    from: &sinter_core::NodeId,
    to: &sinter_core::NodeId,
    filter: &EdgeFilter,
    banned: &std::collections::HashSet<sinter_core::NodeId>,
) -> Result<Option<Vec<sinter_core::Edge>>> {
    let mut prev: std::collections::HashMap<sinter_core::NodeId, sinter_core::Edge> =
        std::collections::HashMap::new();
    let mut seen = std::collections::HashSet::from([from.clone()]);
    let mut queue = std::collections::VecDeque::from([from.clone()]);
    while let Some(current) = queue.pop_front() {
        if &current == to {
            let mut path = Vec::new();
            let mut at = to.clone();
            while &at != from {
                let edge = prev[&at].clone();
                at = edge.src.clone();
                path.push(edge);
            }
            path.reverse();
            return Ok(Some(path));
        }
        for edge in store.out_edges(&current)? {
            let id = edge.dst.as_str();
            let file = id.split_once('#').map_or(id, |(file, _)| file);
            if !filter.admits(&edge)
                || !filter.admits_scope(scopes.scope_of_id(id, file))
                || banned.contains(&edge.dst)
                || !seen.insert(edge.dst.clone())
            {
                continue;
            }
            prev.insert(edge.dst.clone(), edge.clone());
            queue.push_back(edge.dst);
        }
    }
    Ok(None)
}

/// Why a path search came up empty, in the two numbers an agent needs
/// next: how far the forward search got, and which edges actually reach
/// the target (so the query can be rerun from the gap). Dynamic edges
/// excluded by the filter are counted, since trait dispatch is the usual
/// missing hop.
pub struct Miss {
    pub forward_reached: usize,
    /// Admitted incoming edges of the target, capped at `MAX_REACHED_BY`;
    /// `reached_by_total` is the uncapped count.
    pub reached_by: Vec<sinter_core::Edge>,
    pub reached_by_total: usize,
    pub excluded_by_filter: usize,
    pub unresolved_matching_target: usize,
    /// Where the forward search actually stopped, nearest the target
    /// first (shared file-path prefix, then depth). Bounded.
    pub closest_frontier: Vec<sinter_store::Reached>,
    /// Incoming edges the filter refused, counted per refusing attribute.
    pub excluded_edges: std::collections::BTreeMap<&'static str, usize>,
    /// Runnable next operations implied by the two above. Never
    /// speculative: each one is only emitted when the miss data proves it
    /// would change the answer.
    pub suggested_retries: Vec<Retry>,
}

/// One runnable follow-up operation. `remove` drops a flag from the
/// original invocation, `add` adds one; both are absent for a retry that
/// only changes the endpoints.
pub struct Retry {
    pub arguments: [String; 2],
    pub remove: Option<&'static str>,
    pub add: Option<String>,
    pub note: Option<&'static str>,
}

/// How many leading path segments two files share — the only "closeness"
/// measure available without a second traversal.
fn shared_prefix(a: &str, b: &str) -> usize {
    a.split('/')
        .zip(b.split('/'))
        .take_while(|(x, y)| x == y)
        .count()
}

/// Which restriction refused this edge: the attribute the filter objected
/// to, and the flag that carries it. `scope_blocked` is the target's own
/// corpus scope sitting outside `--scope`, which blocks the final hop
/// whatever the edge looks like. Evidence kind `scope` is reported as
/// `scope-evidence` so it never merges with the corpus-scope bucket.
fn refusal(
    filter: &EdgeFilter,
    edge: &sinter_core::Edge,
    scope_blocked: bool,
) -> Option<(&'static str, &'static str)> {
    if filter
        .relations
        .as_ref()
        .is_some_and(|allowed| !allowed.contains(&edge.relation))
    {
        return Some((edge.relation.as_str(), "relations"));
    }
    if filter
        .evidence
        .as_ref()
        .is_some_and(|allowed| !allowed.contains(&edge.evidence))
    {
        let reason = match edge.evidence {
            sinter_core::Evidence::Scope => "scope-evidence",
            other => other.as_str(),
        };
        return Some((reason, "evidence"));
    }
    if filter.min_confidence == Some(sinter_core::Confidence::Certain)
        && edge.confidence != sinter_core::Confidence::Certain
    {
        return Some(("inferred", "certain"));
    }
    scope_blocked.then_some(("scope", "scope"))
}

/// Retries implied by the miss. `blocking` holds the flags that refused an
/// incoming edge of the target: every route in runs through one of those
/// edges, so lifting the flag is the one retry that can admit it.
/// `frontier` is a reached symbol inside the target's own file — the
/// search got to the boundary but not through it.
fn derive_retries(
    from: &str,
    to: &str,
    blocking: &std::collections::BTreeSet<&'static str>,
    target_scope: sinter_core::CorpusScope,
    frontier: Option<&str>,
) -> Vec<Retry> {
    let pair = || [from.to_string(), to.to_string()];
    let mut out = Vec::new();
    for flag in ["certain", "evidence", "relations"] {
        if blocking.contains(flag) {
            out.push(Retry {
                arguments: pair(),
                remove: Some(flag),
                add: None,
                note: None,
            });
        }
    }
    if blocking.contains("scope") {
        out.push(Retry {
            arguments: pair(),
            remove: None,
            add: Some(format!("--scope {}", target_scope.as_str())),
            note: Some("target is out of the selected scope"),
        });
    }
    if let Some(frontier) = frontier {
        out.push(Retry {
            arguments: [from.to_string(), frontier.to_string()],
            remove: None,
            add: None,
            note: Some("reached the target's file, not the target"),
        });
    }
    out
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
    // The traversal filters entered nodes by scope, so a target outside
    // the selection is unreachable however its incoming edges look.
    let target_scope = store.scope_index()?.scope_of(to);
    let scope_blocked = !filter.admits_scope(target_scope);
    let mut excluded_edges = std::collections::BTreeMap::new();
    let mut blocking = std::collections::BTreeSet::new();
    for edge in &candidates {
        let Some((reason, flag)) = refusal(filter, edge, scope_blocked) else {
            continue;
        };
        *excluded_edges.entry(reason).or_default() += 1;
        // A refused incoming edge is an edge into the target: lifting the
        // flag that refused it is the only thing that can admit that hop.
        blocking.insert(flag);
    }
    let mut reached_by: Vec<sinter_core::Edge> = candidates
        .into_iter()
        .filter(|e| filter.admits(e))
        .cloned()
        .collect();
    let reached_by_total = reached_by.len();
    reached_by.truncate(MAX_REACHED_BY);
    let mut forward = forward;
    forward.sort_by(|a, b| {
        shared_prefix(&b.node.file, &to.file)
            .cmp(&shared_prefix(&a.node.file, &to.file))
            .then(a.depth.cmp(&b.depth))
            .then(a.node.name.cmp(&b.node.name))
    });
    forward.truncate(MAX_FRONTIER);
    let frontier = forward
        .first()
        .filter(|reached| reached.node.file == to.file)
        .map(|reached| qualified_of(reached.node.id.as_str()));
    let suggested_retries = derive_retries(
        qualified_of(from.id.as_str()),
        qualified_of(to.id.as_str()),
        &blocking,
        target_scope,
        frontier,
    );
    Ok(Miss {
        forward_reached,
        reached_by,
        reached_by_total,
        excluded_by_filter,
        unresolved_matching_target,
        closest_frontier: forward,
        excluded_edges,
        suggested_retries,
    })
}

/// Same shape for `--json` and the MCP `path` tool.
pub fn miss_json(root: &Path, miss: &Miss) -> serde_json::Value {
    let mut out = serde_json::json!({
        "forward_reached": miss.forward_reached,
        "reached_by": miss.reached_by.iter().map(|e| {
            let mut row = serde_json::json!({
                "from": qualified_of(e.src.as_str()),
                "relation": e.relation.as_str(),
                "evidence": e.evidence.as_str(),
                "site": crate::render::site_json(root, e),
            });
            crate::render::add_sites(&mut row, root, e);
            row
        }).collect::<Vec<_>>(),
        "reached_by_total": miss.reached_by_total,
        "excluded_by_filter": miss.excluded_by_filter,
        "unresolved_matching_target": miss.unresolved_matching_target,
        // Always present, empty or not: an agent reads them without probing.
        "closest_frontier": miss
            .closest_frontier
            .iter()
            .map(|reached| {
                serde_json::json!({
                    "symbol": qualified_of(reached.node.id.as_str()),
                    "site": match crate::render::line_of(root, &reached.node.file, reached.node.span.start) {
                        Some(line) => format!("{}:{line}", reached.node.file),
                        None => reached.node.file.clone(),
                    },
                    "depth": reached.depth,
                })
            })
            .collect::<Vec<_>>(),
        "suggested_retries": miss
            .suggested_retries
            .iter()
            .map(|retry| {
                let mut entry = serde_json::json!({
                    "operation": "path",
                    "arguments": retry.arguments,
                });
                if let Some(remove) = retry.remove {
                    entry["remove"] = serde_json::json!([remove]);
                }
                if let Some(add) = &retry.add {
                    entry["add"] = serde_json::json!([add]);
                }
                if let Some(note) = retry.note {
                    entry["note"] = serde_json::json!(note);
                }
                entry
            })
            .collect::<Vec<_>>(),
    });
    if !miss.excluded_edges.is_empty() {
        out["excluded_edges"] = serde_json::json!(miss.excluded_edges);
    }
    out
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
            miss.reached_by_total
        );
        for e in &miss.reached_by {
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
        if miss.reached_by_total > miss.reached_by.len() {
            println!(
                "    … (+{}); `sinter show {}` lists them all",
                miss.reached_by_total - miss.reached_by.len(),
                qualified_of(to.id.as_str())
            );
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
    print_actionable(root, miss);
}

/// The machine-shaped half of a miss: where the search stopped, what the
/// filter refused, and what to run next.
fn print_actionable(root: &Path, miss: &Miss) {
    if miss.closest_frontier.is_empty() {
        println!("  closest frontier: none (the forward search admitted no edge)");
    } else {
        println!("  closest frontier ({}):", miss.closest_frontier.len());
        for reached in &miss.closest_frontier {
            let line = crate::render::line_of(root, &reached.node.file, reached.node.span.start);
            println!(
                "    {} [d{}] at {}",
                qualified_of(reached.node.id.as_str()),
                reached.depth,
                crate::render::location(root, &reached.node.file, line)
            );
        }
    }
    if !miss.excluded_edges.is_empty() {
        let counts: Vec<String> = miss
            .excluded_edges
            .iter()
            .map(|(reason, count)| format!("{reason}={count}"))
            .collect();
        println!("  excluded edges: {}", counts.join(" "));
        println!("  reason: filter_excluded");
    }
    if miss.suggested_retries.is_empty() {
        println!("  suggested retries: none (no flag or endpoint change is implied by the miss)");
    }
    for retry in &miss.suggested_retries {
        let flag = match (retry.remove, &retry.add) {
            (Some(remove), _) => format!("  drop={remove}"),
            (None, Some(add)) => format!("  add={add}"),
            (None, None) => String::new(),
        };
        let note = retry
            .note
            .map(|note| format!("  # {note}"))
            .unwrap_or_default();
        println!(
            "  retry: path {} {}{flag}{note}",
            retry.arguments[0], retry.arguments[1]
        );
    }
}

fn name_tail_matches(written: &str, name: &str) -> bool {
    let tail = written.rsplit("::").next().unwrap_or(written);
    let tail = tail.rsplit(['/', '.']).next().unwrap_or(tail);
    tail == name
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use sinter_core::{Confidence, CorpusScope, Edge, Evidence, NodeId, Relation};
    use sinter_store::EdgeFilter;

    use super::{Retry, derive_retries, refusal, shared_prefix};

    fn edge(evidence: Evidence, relation: Relation) -> Edge {
        Edge {
            src: NodeId::new("a.rs#a@0"),
            dst: NodeId::new("b.rs#b@0"),
            relation,
            evidence,
            confidence: evidence.confidence(),
            site: None,
            extra_sites: Vec::new(),
            sites_total: 0,
        }
    }

    fn blocking(flags: &[&'static str]) -> BTreeSet<&'static str> {
        flags.iter().copied().collect()
    }

    fn shapes(retries: &[Retry]) -> Vec<(String, String, Option<&'static str>, Option<String>)> {
        retries
            .iter()
            .map(|r| {
                (
                    r.arguments[0].clone(),
                    r.arguments[1].clone(),
                    r.remove,
                    r.add.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn nothing_implied_suggests_nothing() {
        let retries = derive_retries("A", "B", &blocking(&[]), CorpusScope::Production, None);
        assert!(retries.is_empty());
    }

    #[test]
    fn certain_refusing_an_incoming_edge_suggests_dropping_certain() {
        let retries = derive_retries(
            "A",
            "B",
            &blocking(&["certain"]),
            CorpusScope::Production,
            None,
        );
        assert_eq!(
            shapes(&retries),
            vec![("A".into(), "B".into(), Some("certain"), None)]
        );
    }

    #[test]
    fn out_of_scope_target_suggests_widening_to_its_scope() {
        let retries = derive_retries("A", "B", &blocking(&["scope"]), CorpusScope::Test, None);
        assert_eq!(
            shapes(&retries),
            vec![(
                "A".into(),
                "B".into(),
                None,
                Some("--scope test".to_string())
            )]
        );
    }

    #[test]
    fn frontier_in_the_target_file_suggests_the_shorter_path() {
        let retries = derive_retries(
            "A",
            "B",
            &blocking(&[]),
            CorpusScope::Production,
            Some("Frontier"),
        );
        assert_eq!(
            shapes(&retries),
            vec![("A".into(), "Frontier".into(), None, None)]
        );
        assert!(retries[0].note.is_some());
    }

    #[test]
    fn every_blocking_flag_yields_its_own_retry() {
        let retries = derive_retries(
            "A",
            "B",
            &blocking(&["certain", "evidence", "relations", "scope"]),
            CorpusScope::Vendor,
            Some("F"),
        );
        assert_eq!(retries.len(), 5);
    }

    #[test]
    fn certain_refuses_inferred_edges_by_confidence() {
        let filter = EdgeFilter {
            min_confidence: Some(Confidence::Certain),
            ..Default::default()
        };
        assert_eq!(
            refusal(&filter, &edge(Evidence::Dynamic, Relation::Calls), false),
            Some(("inferred", "certain"))
        );
        assert_eq!(
            refusal(&filter, &edge(Evidence::Scip, Relation::Calls), false),
            None
        );
    }

    #[test]
    fn evidence_and_relation_refusals_name_the_edge_attribute() {
        let filter = EdgeFilter {
            evidence: Some(BTreeSet::from([Evidence::Structural])),
            ..Default::default()
        };
        assert_eq!(
            refusal(&filter, &edge(Evidence::Dynamic, Relation::Calls), false),
            Some(("dynamic", "evidence"))
        );
        // Evidence kind `scope` never merges with the corpus-scope bucket.
        assert_eq!(
            refusal(&filter, &edge(Evidence::Scope, Relation::Calls), false),
            Some(("scope-evidence", "evidence"))
        );
        let filter = EdgeFilter {
            relations: Some(BTreeSet::from([Relation::Calls])),
            ..Default::default()
        };
        assert_eq!(
            refusal(&filter, &edge(Evidence::Scip, Relation::Implements), false),
            Some(("implements", "relations"))
        );
    }

    #[test]
    fn an_admitted_edge_into_an_out_of_scope_target_is_refused_by_scope() {
        let filter = EdgeFilter::default();
        assert_eq!(
            refusal(&filter, &edge(Evidence::Scip, Relation::Calls), true),
            Some(("scope", "scope"))
        );
    }

    #[test]
    fn frontier_ranks_by_shared_file_prefix() {
        let target = "crates/a/src/x.rs";
        let mut files = ["crates/b/src/y.rs", "crates/a/src/z.rs", "other.rs"];
        files.sort_by_key(|f| std::cmp::Reverse(shared_prefix(f, target)));
        assert_eq!(files[0], "crates/a/src/z.rs");
    }

    #[test]
    fn excluded_counts_key_on_the_refusing_attribute() {
        let filter = EdgeFilter {
            min_confidence: Some(Confidence::Certain),
            ..Default::default()
        };
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for e in [
            edge(Evidence::Dynamic, Relation::Calls),
            edge(Evidence::Import, Relation::Uses),
            edge(Evidence::Scip, Relation::Calls),
        ] {
            if let Some((reason, _)) = refusal(&filter, &e, false) {
                *counts.entry(reason).or_default() += 1;
            }
        }
        assert_eq!(counts, BTreeMap::from([("inferred", 2)]));
    }
}
