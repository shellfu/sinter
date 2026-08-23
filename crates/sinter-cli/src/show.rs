//! `sinter show <symbol>`: the "I found it, now orient me" card — grouped,
//! capped, evidence-tagged. One bounded screen, never a BFS dump.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::{Edge, Evidence, Node, Relation};
use sinter_resolve::qualified_of;
use sinter_store::{EdgeFilter, Store};

use crate::lookup::{ensure_snapshot, open_store, unique_symbol_in};
use crate::render::{ellipsize, line_of, location, node_json, site_json, site_location};

/// Rows shown per relation group before collapsing to `… (+N)`.
pub const DEFAULT_LIMIT: usize = 20;

/// The symbol's edges after `--relations` / `--scope`: outgoing first,
/// incoming second. Scope applies to the far end of each edge; contains
/// edges survive only when no relation restriction was given.
pub fn edges(store: &Store, node: &Node, filter: &EdgeFilter) -> Result<(Vec<Edge>, Vec<Edge>)> {
    let scopes = store.scope_index()?;
    let keep = |e: &Edge, other: &str| {
        filter
            .relations
            .as_ref()
            .is_none_or(|set| set.contains(&e.relation))
            && filter.scopes.as_ref().is_none_or(|set| {
                let file = other.split_once('#').map_or(other, |(f, _)| f);
                set.contains(&scopes.scope_of_id(other, file))
            })
    };
    let out = store
        .out_edges(&node.id)?
        .into_iter()
        .filter(|e| keep(e, e.dst.as_str()))
        .collect();
    let inn = store
        .in_edges(&node.id)?
        .into_iter()
        .filter(|e| keep(e, e.src.as_str()))
        .collect();
    Ok((out, inn))
}

/// `outgoing`/`incoming` arrays capped at `limit` per relation, plus
/// `totals` and (only when something was cut) `truncated` per group —
/// the same convention as `affected`. Shared by the CLI and MCP `show`.
pub fn edges_json(
    repo: &Path,
    store: &Store,
    node: &Node,
    filter: &EdgeFilter,
    limit: usize,
) -> Result<Value> {
    let (out, inn) = edges(store, node, filter)?;
    let mut totals = json!({});
    let mut truncated = json!({});
    let mut direction = |name: &str, edges: &[Edge], other: fn(&Edge) -> &str| -> Vec<Value> {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        let mut rows = Vec::new();
        for e in edges {
            let n = seen.entry(e.relation.as_str()).or_default();
            *n += 1;
            if *n <= limit {
                rows.push(json!({
                    "symbol": qualified_of(other(e)),
                    "relation": e.relation.as_str(),
                    "evidence": e.evidence.as_str(),
                    "site": site_json(repo, e),
                }));
            }
        }
        for (rel, n) in seen {
            totals[name][rel] = json!(n);
            if n > limit {
                truncated[name][rel] = json!(n - limit);
            }
        }
        rows
    };
    let outgoing = direction("outgoing", &out, |e| e.dst.as_str());
    let incoming = direction("incoming", &inn, |e| e.src.as_str());
    let mut v = json!({"outgoing": outgoing, "incoming": incoming, "totals": totals});
    if truncated.as_object().is_some_and(|m| !m.is_empty()) {
        v["truncated"] = truncated;
    }
    Ok(v)
}

/// Join `shown` exemplars and collapse the rest to `… (+N) · --limit`.
fn listed(shown: Vec<String>, total: usize) -> String {
    let mut out = shown.join(", ");
    if total > shown.len() {
        out.push_str(&format!(", … (+{}) · --limit", total - shown.len()));
    }
    out
}

fn short(id: &str) -> &str {
    let q = qualified_of(id);
    q.rsplit("::").next().unwrap_or(q)
}

fn names(edges: &[&Edge], end: fn(&Edge) -> &str, limit: usize) -> String {
    listed(
        edges
            .iter()
            .take(limit)
            .map(|e| short(end(e)).to_string())
            .collect(),
        edges.len(),
    )
}

fn evidence_tally(edges: &[&Edge]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in edges {
        *counts.entry(e.evidence.as_str()).or_default() += 1;
    }
    counts
        .iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Ok(true) when the symbol resolved (grep-style exit codes).
pub fn run(
    repo: &Path,
    symbol: &str,
    filter: &EdgeFilter,
    limit: usize,
    json: bool,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let node = unique_symbol_in(&store, symbol, filter.scopes.as_ref())?;
    let scope = store.file_scope(&node.file)?;
    if json {
        // Same shape as the MCP `show` tool.
        let mut out = edges_json(&repo, &store, &node, filter, limit)?;
        let mut symbol_json = node_json(&node);
        symbol_json["scope"] = json!(scope.as_str());
        out["symbol"] = symbol_json;
        out["snapshot"] = json!(snapshot);
        crate::agent_protocol::write_json(&out)?;
        return Ok(true);
    }
    let line = line_of(&repo, &node.file, node.span.start);

    println!(
        "{} {}    {} ({}..{}) [{scope}]",
        node.kind.as_str(),
        qualified_of(node.id.as_str()),
        location(&repo, &node.file, line),
        node.span.start,
        node.span.end,
    );
    if let Some(doc) = &node.doc {
        for l in doc.lines().take(3) {
            println!("  /// {l}");
        }
    }
    if !node.signature.is_empty() {
        println!("  {}", ellipsize(&node.signature, 110));
    }
    println!();

    let (out, inn) = edges(&store, &node, filter)?;

    let group =
        |rel: Relation| -> Vec<&Edge> { out.iter().filter(|e| e.relation == rel).collect() };
    let contains = group(Relation::Contains);
    if !contains.is_empty() {
        println!(
            "contains ({})    {}",
            contains.len(),
            names(&contains, |e| e.dst.as_str(), limit)
        );
    }
    let extends = group(Relation::Extends);
    if !extends.is_empty() {
        println!(
            "extends          {}    [{}]",
            names(&extends, |e| e.dst.as_str(), limit),
            evidence_tally(&extends)
        );
    }
    let imports = group(Relation::Imports);
    if !imports.is_empty() {
        println!(
            "imports ({})     {}    [{}]",
            imports.len(),
            names(&imports, |e| e.dst.as_str(), limit),
            evidence_tally(&imports)
        );
    }

    let implements = group(Relation::Implements);
    if !implements.is_empty() {
        println!(
            "implements       {}    [{}]",
            names(&implements, |e| e.dst.as_str(), limit),
            evidence_tally(&implements)
        );
    }
    // Implementors are the answer to "who is behind this trait", not
    // dependents of it — listed by name, kept out of the used-by tally.
    let implementors: Vec<&Edge> = inn
        .iter()
        .filter(|e| e.relation == Relation::Implements)
        .collect();
    if !implementors.is_empty() {
        println!(
            "implemented by ({})    {}    [{}]",
            implementors.len(),
            names(&implementors, |e| e.src.as_str(), limit),
            evidence_tally(&implementors)
        );
    }

    // used by: incoming non-contains, non-implements edges grouped by
    // source file.
    let dependents: Vec<&Edge> = inn
        .iter()
        .filter(|e| !matches!(e.relation, Relation::Contains | Relation::Implements))
        .collect();
    if !dependents.is_empty() {
        // Per src file: edge count plus one representative call site (the
        // smallest span start — matches the stored representative).
        let mut per_file: BTreeMap<&str, (usize, Option<u64>)> = BTreeMap::new();
        for e in &dependents {
            let file = e
                .src
                .as_str()
                .split_once('#')
                .map_or(e.src.as_str(), |(f, _)| f);
            let entry = per_file.entry(file).or_default();
            entry.0 += 1;
            if let Some(span) = e.site {
                entry.1 = Some(entry.1.map_or(span.start, |s| s.min(span.start)));
            }
        }
        println!(
            "used by ({} files, {} edges)",
            per_file.len(),
            dependents.len()
        );
        let mut rows: Vec<(&str, (usize, Option<u64>))> = per_file.into_iter().collect();
        rows.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(b.0)));
        for (file, (count, site)) in rows.iter().take(limit) {
            let line = site.and_then(|byte| line_of(&repo, file, byte));
            println!("  {}   {count} edges", location(&repo, file, line));
        }
        if rows.len() > limit {
            println!("  … (+{} files) · --limit", rows.len() - limit);
        }
    }

    // Dynamic call edges fan a trait method out to its implementations.
    // Short names collide by construction (every impl is `speak`), so
    // these are listed qualified, apart from the direct calls.
    let dispatches: Vec<&Edge> = out
        .iter()
        .filter(|e| e.relation == Relation::Calls && e.evidence == Evidence::Dynamic)
        .collect();
    if !dispatches.is_empty() {
        let shown = dispatches
            .iter()
            .take(limit)
            .map(|e| qualified_of(e.dst.as_str()).to_string())
            .collect();
        println!(
            "dispatches to ({})    {}",
            dispatches.len(),
            listed(shown, dispatches.len())
        );
    }

    // One row per relation: a `uses` edge (type reference) is never a call.
    for (label, rel) in [("calls", Relation::Calls), ("uses", Relation::Uses)] {
        let edges: Vec<&Edge> = out
            .iter()
            .filter(|e| e.relation == rel && e.evidence != Evidence::Dynamic)
            .collect();
        if edges.is_empty() {
            continue;
        }
        // Exemplars carry their site (`name (file:line)`) so "A calls B"
        // comes with "at file:line" instead of forcing a follow-up grep.
        let shown = edges
            .iter()
            .take(limit)
            .map(|e| {
                let name = short(e.dst.as_str());
                match site_location(&repo, e) {
                    Some(site) => format!("{name} ({site})"),
                    None => name.to_string(),
                }
            })
            .collect();
        println!(
            "{:<16} {}    [{}]",
            format!("{label} ({})", edges.len()),
            listed(shown, edges.len()),
            evidence_tally(&edges)
        );
    }

    let unresolved = store.references_in(&node.file)?.len();
    println!("unresolved refs in this file: {unresolved}");
    println!();
    println!(
        "Next: sinter affected {} --max-depth 3",
        qualified_of(node.id.as_str())
    );
    Ok(true)
}
