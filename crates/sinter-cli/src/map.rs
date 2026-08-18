//! `sinter map [repo]`: post-clone orientation in one screen — module
//! shape, most-depended-on symbols, and doc entry points.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use sinter_core::{Node, Relation, SymbolKind};
use sinter_resolve::qualified_of;

use crate::lookup::open_store;
use crate::render::{line_of, location};

const HUBS: usize = 10;
const DOC_SECTIONS: usize = 6;

/// Directory tree (depth 2) with per-directory node counts, keyed by the
/// first path component. Root-level files land under ".".
fn module_tree(nodes: &[Node]) -> BTreeMap<String, (usize, BTreeMap<String, usize>)> {
    let mut tree: BTreeMap<String, (usize, BTreeMap<String, usize>)> = BTreeMap::new();
    for node in nodes {
        let dirs: Vec<&str> = {
            let mut parts: Vec<&str> = node.file.split('/').collect();
            parts.pop(); // file name
            parts
        };
        let top = dirs.first().copied().unwrap_or(".").to_string();
        let entry = tree.entry(top).or_default();
        entry.0 += 1;
        if let Some(second) = dirs.get(1) {
            *entry.1.entry((*second).to_string()).or_default() += 1;
        }
    }
    tree
}

/// Top hub symbols by non-Contains in-degree; count desc, id asc.
fn hubs(nodes: &BTreeMap<&str, &Node>, in_degree: &BTreeMap<&str, usize>) -> Vec<(Node, usize)> {
    let mut ranked: Vec<(&str, usize)> = in_degree
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(id, n)| (*id, *n))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    ranked
        .into_iter()
        .filter_map(|(id, n)| nodes.get(id).map(|node| ((*node).clone(), n)))
        .take(HUBS)
        .collect()
}

/// Level-1 markdown sections of README.md and top-level docs/*.md, in
/// document order.
fn doc_entries(nodes: &[Node]) -> BTreeMap<String, Vec<(u64, String)>> {
    let mut docs: BTreeMap<String, Vec<(u64, String)>> = BTreeMap::new();
    for node in nodes {
        if node.kind != SymbolKind::Section {
            continue;
        }
        let file = node.file.as_str();
        let top_level_doc = file == "README.md"
            || (file.starts_with("docs/")
                && file.ends_with(".md")
                && file.matches('/').count() == 1);
        // A level-1 heading's signature is `# Title` — exactly one `#`.
        let h1 = node.signature.starts_with('#') && !node.signature.starts_with("##");
        if top_level_doc && h1 {
            docs.entry(file.to_string())
                .or_default()
                .push((node.span.start, node.name.clone()));
        }
    }
    for sections in docs.values_mut() {
        sections.sort();
    }
    docs
}

pub fn run(repo: &Path, json: bool) -> Result<()> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
    // One-shot whole-graph read: map runs once per orientation, and a
    // single pass beats a per-node in-edge query storm.
    let graph = store.read_graph()?;
    let nodes: Vec<Node> = graph.nodes().cloned().collect();
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut in_degree: BTreeMap<&str, usize> = BTreeMap::new();
    for edge in graph.edges() {
        if edge.relation != Relation::Contains {
            *in_degree.entry(edge.dst.as_str()).or_default() += 1;
        }
    }
    let tree = module_tree(&nodes);
    let hubs = hubs(&by_id, &in_degree);
    let docs = doc_entries(&nodes);
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".into());

    if json {
        let modules: Vec<serde_json::Value> = tree
            .iter()
            .flat_map(|(top, (count, children))| {
                std::iter::once(serde_json::json!({"path": top, "nodes": count})).chain(
                    children.iter().map(move |(child, n)| {
                        serde_json::json!({"path": format!("{top}/{child}"), "nodes": n})
                    }),
                )
            })
            .collect();
        let hubs: Vec<serde_json::Value> = hubs
            .iter()
            .map(|(node, n)| {
                serde_json::json!({
                    "name": qualified_of(node.id.as_str()),
                    "kind": node.kind.as_str(),
                    "file": node.file,
                    "line": line_of(&repo, &node.file, node.span.start),
                    "in_degree": n,
                })
            })
            .collect();
        let docs: Vec<serde_json::Value> = docs
            .iter()
            .map(|(file, sections)| {
                let names: Vec<&str> = sections.iter().map(|(_, n)| n.as_str()).collect();
                serde_json::json!({"file": file, "sections": names})
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "repo": name,
                "nodes": graph.node_count(),
                "edges": graph.edge_count(),
                "modules": modules,
                "hubs": hubs,
                "docs": docs,
            }))?
        );
        return Ok(());
    }

    println!(
        "{name} — {} nodes, {} edges",
        graph.node_count(),
        graph.edge_count()
    );
    println!();
    println!("Modules");
    for (top, (count, children)) in &tree {
        let label = if *top == "." {
            ".".to_string()
        } else {
            format!("{top}/")
        };
        println!("  {label:<24} {count}");
        for (child, n) in children {
            println!("    {child:<22} {n}");
        }
    }
    if !hubs.is_empty() {
        println!();
        println!("Hubs (most depended-on)");
        for (node, n) in &hubs {
            let line = line_of(&repo, &node.file, node.span.start);
            println!(
                "  {n:>4}  {} {}  {}",
                node.kind.as_str(),
                qualified_of(node.id.as_str()),
                location(&repo, &node.file, line)
            );
        }
    }
    if !docs.is_empty() {
        println!();
        println!("Docs");
        for (file, sections) in &docs {
            let shown: Vec<&str> = sections
                .iter()
                .take(DOC_SECTIONS)
                .map(|(_, n)| n.as_str())
                .collect();
            let more = sections.len().saturating_sub(DOC_SECTIONS);
            let suffix = if more > 0 {
                format!(" (+{more})")
            } else {
                String::new()
            };
            println!("  {file}    {}{suffix}", shown.join(" · "));
        }
    }
    println!();
    println!("Next: sinter ask \"<question>\" · sinter show <symbol> · sinter affected <symbol>");
    Ok(())
}
