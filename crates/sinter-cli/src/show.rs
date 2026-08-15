//! `sinter show <symbol>`: the "I found it, now orient me" card — grouped,
//! capped, evidence-tagged. One bounded screen, never a BFS dump.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use sinter_core::{Edge, Relation, SymbolKind};
use sinter_resolve::qualified_of;

use crate::lookup::{open_store, unique_symbol};
use crate::render::{ellipsize, line_of, location};

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

pub fn run(repo: &Path, symbol: &str) -> Result<()> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
    let node = unique_symbol(&store, symbol)?;
    let line = line_of(&repo, &node.file, node.span.start);

    println!(
        "{} {}    {} ({}..{})",
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
    if node.kind == SymbolKind::File && !imports.is_empty() {
        println!(
            "imports ({})     {}    [{}]",
            imports.len(),
            names(&imports, |e| e.dst.as_str()),
            evidence_tally(&imports)
        );
    }

    // used by: incoming non-contains edges grouped by source file.
    let dependents: Vec<&Edge> = inn
        .iter()
        .filter(|e| e.relation != Relation::Contains)
        .collect();
    if !dependents.is_empty() {
        let mut per_file: BTreeMap<&str, usize> = BTreeMap::new();
        for e in &dependents {
            let file = e
                .src
                .as_str()
                .split_once('#')
                .map_or(e.src.as_str(), |(f, _)| f);
            *per_file.entry(file).or_default() += 1;
        }
        println!(
            "used by ({} files, {} edges)",
            per_file.len(),
            dependents.len()
        );
        let mut rows: Vec<(&str, usize)> = per_file.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        for (file, count) in rows.iter().take(EXEMPLARS) {
            println!("  {file}   {count} edges");
        }
        if rows.len() > EXEMPLARS {
            println!("  … (+{} files)", rows.len() - EXEMPLARS);
        }
    }

    let calls: Vec<&Edge> = out
        .iter()
        .filter(|e| matches!(e.relation, Relation::Calls | Relation::Uses))
        .collect();
    if !calls.is_empty() {
        println!(
            "calls ({})       {}    [{}]",
            calls.len(),
            names(&calls, |e| e.dst.as_str()),
            evidence_tally(&calls)
        );
    }

    let unresolved = store.references_in(&node.file)?.len();
    println!("unresolved refs in this file: {unresolved}");
    println!();
    println!(
        "Next: sinter affected {} --max-depth 3",
        qualified_of(node.id.as_str())
    );
    Ok(())
}
