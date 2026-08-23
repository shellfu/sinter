//! `sinter show <symbol>`: the "I found it, now orient me" card — grouped,
//! capped, evidence-tagged. One bounded screen, never a BFS dump.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use sinter_core::{Edge, Evidence, Relation};
use sinter_resolve::qualified_of;

use crate::lookup::{ensure_snapshot, open_store, unique_symbol};
use crate::render::{ellipsize, line_of, location, node_json, site_json, site_location};

const EXEMPLARS: usize = 8;

fn names(edges: &[&Edge], end: fn(&Edge) -> &str) -> String {
    let shown: Vec<&str> = edges
        .iter()
        .take(EXEMPLARS)
        .map(|e| {
            let q = qualified_of(end(e));
            q.rsplit("::").next().unwrap_or(q)
        })
        .collect();
    let mut out = shown.join(", ");
    if edges.len() > EXEMPLARS {
        out.push_str(&format!(", … (+{})", edges.len() - EXEMPLARS));
    }
    out
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
pub fn run(repo: &Path, symbol: &str, json: bool, if_snapshot: Option<&str>) -> Result<bool> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let node = unique_symbol(&store, symbol)?;
    let scope = store.file_scope(&node.file)?;
    if json {
        // Same shape as the MCP `show` tool.
        let edge_json = |e: &Edge, other: &str| {
            serde_json::json!({
                "symbol": qualified_of(other),
                "relation": e.relation.as_str(),
                "evidence": e.evidence.as_str(),
                "site": site_json(&repo, e),
            })
        };
        let out: Vec<serde_json::Value> = store
            .out_edges(&node.id)?
            .iter()
            .map(|e| edge_json(e, e.dst.as_str()))
            .collect();
        let inn: Vec<serde_json::Value> = store
            .in_edges(&node.id)?
            .iter()
            .map(|e| edge_json(e, e.src.as_str()))
            .collect();
        let mut symbol_json = node_json(&node);
        symbol_json["scope"] = serde_json::json!(scope.as_str());
        crate::agent_protocol::write_json(&serde_json::json!({
            "symbol": symbol_json,
            "snapshot": snapshot,
            "outgoing": out,
            "incoming": inn,
        }))?;
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

    let out = store.out_edges(&node.id)?;
    let inn = store.in_edges(&node.id)?;

    let group =
        |rel: Relation| -> Vec<&Edge> { out.iter().filter(|e| e.relation == rel).collect() };
    let contains = group(Relation::Contains);
    if !contains.is_empty() {
        println!(
            "contains ({})    {}",
            contains.len(),
            names(&contains, |e| e.dst.as_str())
        );
    }
    let extends = group(Relation::Extends);
    if !extends.is_empty() {
        println!(
            "extends          {}    [{}]",
            names(&extends, |e| e.dst.as_str()),
            evidence_tally(&extends)
        );
    }
    let imports = group(Relation::Imports);
    if !imports.is_empty() {
        println!(
            "imports ({})     {}    [{}]",
            imports.len(),
            names(&imports, |e| e.dst.as_str()),
            evidence_tally(&imports)
        );
    }

    let implements = group(Relation::Implements);
    if !implements.is_empty() {
        println!(
            "implements       {}    [{}]",
            names(&implements, |e| e.dst.as_str()),
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
            names(&implementors, |e| e.src.as_str()),
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
        for (file, (count, site)) in rows.iter().take(EXEMPLARS) {
            let line = site.and_then(|byte| line_of(&repo, file, byte));
            println!("  {}   {count} edges", location(&repo, file, line));
        }
        if rows.len() > EXEMPLARS {
            println!("  … (+{} files)", rows.len() - EXEMPLARS);
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
        let shown: Vec<&str> = dispatches
            .iter()
            .take(EXEMPLARS)
            .map(|e| qualified_of(e.dst.as_str()))
            .collect();
        let mut listed = shown.join(", ");
        if dispatches.len() > EXEMPLARS {
            listed.push_str(&format!(", … (+{})", dispatches.len() - EXEMPLARS));
        }
        println!("dispatches to ({})    {}", dispatches.len(), listed);
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
        let shown: Vec<String> = edges
            .iter()
            .take(EXEMPLARS)
            .map(|e| {
                let q = qualified_of(e.dst.as_str());
                let name = q.rsplit("::").next().unwrap_or(q);
                match site_location(&repo, e) {
                    Some(site) => format!("{name} ({site})"),
                    None => name.to_string(),
                }
            })
            .collect();
        let mut listed = shown.join(", ");
        if edges.len() > EXEMPLARS {
            listed.push_str(&format!(", … (+{})", edges.len() - EXEMPLARS));
        }
        println!(
            "{:<16} {}    [{}]",
            format!("{label} ({})", edges.len()),
            listed,
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
