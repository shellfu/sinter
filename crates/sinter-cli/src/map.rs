//! `sinter map [repo]`: bounded post-clone structural inventory — module
//! shape, explicitly measured dependency hubs, docs, and graph health.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use sinter_core::{CorpusScope, Node, NodeId, Relation, SymbolKind};
use sinter_resolve::qualified_of;

use crate::corpus::ScopeSelection;
use crate::lookup::open_store;
use crate::render::{line_of, location};

const HUBS: usize = 10;
/// Nodes (by total in-degree) whose cross-module in-degree is measured.
const CROSS_POOL: usize = 200;
const DOC_SECTIONS: usize = 6;

#[derive(Default)]
struct ModuleStats {
    nodes: usize,
    files: BTreeSet<String>,
}

impl ModuleStats {
    fn record(&mut self, node: &Node) {
        self.nodes += 1;
        self.files.insert(node.file.clone());
    }
}

#[derive(Default)]
struct ModuleBranch {
    total: ModuleStats,
    children: BTreeMap<String, ModuleStats>,
}

/// Directory tree (depth 2) with node and distinct-file counts. Both are
/// reported because symbol density alone exaggerates large source files.
fn module_tree(nodes: &[Node]) -> BTreeMap<String, ModuleBranch> {
    let mut tree: BTreeMap<String, ModuleBranch> = BTreeMap::new();
    for node in nodes {
        let dirs: Vec<&str> = {
            let mut parts: Vec<&str> = node.file.split('/').collect();
            parts.pop(); // file name
            parts
        };
        let top = dirs.first().copied().unwrap_or(".").to_string();
        let entry = tree.entry(top).or_default();
        entry.total.record(node);
        if let Some(second) = dirs.get(1) {
            entry
                .children
                .entry((*second).to_string())
                .or_default()
                .record(node);
        }
    }
    tree
}

/// A symbol ranked as a dependency hub.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hub {
    node: Node,
    /// Non-Contains in-degree.
    total: usize,
    /// In-edges whose source lives in a different depth-2 module.
    cross: usize,
}

/// Module key of a repo-relative file: the first two directories (the
/// same depth-2 grain as the modules table, so `crates/a` and `crates/b`
/// are distinct modules); `.` for root files.
fn top_of(file: &str) -> &str {
    match file.rsplit_once('/') {
        Some((dirs, _)) => {
            let cut = dirs
                .match_indices('/')
                .nth(1)
                .map_or(dirs.len(), |(i, _)| i);
            &dirs[..cut]
        }
        None => ".",
    }
}

/// Nodes that may appear as hubs: real symbols, not aliases, containers,
/// or generated/vendored code whose fan-in says nothing about design.
fn hub_candidate(node: &Node, scope: CorpusScope) -> bool {
    !matches!(
        node.kind,
        SymbolKind::TypeAlias | SymbolKind::Module | SymbolKind::File
    ) && !matches!(scope, CorpusScope::Generated | CorpusScope::Vendor)
}

/// Rank hubs: cross-module in-degree desc, total desc, id asc; top HUBS.
fn rank_hubs(mut hubs: Vec<Hub>) -> Vec<Hub> {
    hubs.sort_by(|a, b| {
        b.cross
            .cmp(&a.cross)
            .then_with(|| b.total.cmp(&a.total))
            .then_with(|| a.node.id.cmp(&b.node.id))
    });
    hubs.truncate(HUBS);
    hubs
}

/// Hubs from the store: filter candidates, measure cross-module in-degree,
/// rank.
fn hubs(
    store: &sinter_store::Store,
    nodes: &BTreeMap<&str, &Node>,
    scopes: &sinter_store::ScopeIndex,
) -> Result<Vec<Hub>> {
    let mut ranked: Vec<(&Node, usize)> = store
        .in_degrees()?
        .into_iter()
        .filter_map(|(id, n)| nodes.get(id.as_str()).map(|node| (*node, n)))
        .filter(|(node, _)| hub_candidate(node, scopes.scope_of(node)))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
    // ponytail: cross-degree is measured only for the top CROSS_POOL by
    // total; a node below that cut with higher cross-degree is missed.
    // Widen the pool or stream IN_EDGES if that shows up in practice.
    ranked.truncate(CROSS_POOL);
    let ids: Vec<NodeId> = ranked.iter().map(|(node, _)| node.id.clone()).collect();
    let in_edges = store.in_edges_many(&ids)?;
    let hubs = ranked
        .into_iter()
        .map(|(node, total)| {
            let top = top_of(&node.file);
            let cross = in_edges
                .get(&node.id)
                .map(|edges| {
                    edges
                        .iter()
                        .filter(|e| e.relation != Relation::Contains)
                        .map(|e| e.src.as_str().split('#').next().unwrap_or(""))
                        .filter(|src_file| top_of(src_file) != top)
                        .count()
                })
                .unwrap_or(0);
            Hub {
                node: node.clone(),
                total,
                cross,
            }
        })
        .collect();
    Ok(rank_hubs(hubs))
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

pub fn run(repo: &Path, json: bool, scopes: &ScopeSelection) -> Result<()> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
    let view = response(&repo, &store, scopes)?;
    if json {
        crate::agent_protocol::write_json(&view)?;
        return Ok(());
    }

    let name = view["repo"].as_str().unwrap_or(".");
    let node_count = view["nodes"].as_u64().unwrap_or(0);
    let total_nodes = view["total_nodes"].as_u64().unwrap_or(0);
    let edge_count = view["edges"].as_u64().unwrap_or(0);
    println!(
        "{name} — repository inventory · {node_count} scoped nodes ({total_nodes} total), {edge_count} edges · scope {}",
        scopes.labels().join(",")
    );
    println!();
    println!("Modules (depth 2; node/file counts)");
    for module in view["modules"].as_array().into_iter().flatten() {
        let path = module["path"].as_str().unwrap_or(".");
        let count = module["nodes"].as_u64().unwrap_or(0);
        let files = module["files"].as_u64().unwrap_or(0);
        if let Some((_, child)) = path.split_once('/') {
            println!("    {child:<22} {count:>5} / {files:<5}");
        } else {
            let label = if path == "." {
                ".".to_string()
            } else {
                format!("{path}/")
            };
            println!("  {label:<24} {count:>5} / {files:<5}");
        }
    }
    let hubs = view["hubs"].as_array().cloned().unwrap_or_default();
    if !hubs.is_empty() {
        println!();
        println!("Dependency hubs (non-containment in-degree; cross = from other modules)");
        for hub in hubs {
            let file = hub["file"].as_str().unwrap_or("");
            let line = hub["line"].as_u64().map(|line| line as usize);
            println!(
                "  {:>4} (cross {:>3})  {} {}  {}",
                hub["in_degree"].as_u64().unwrap_or(0),
                hub["cross_in_degree"].as_u64().unwrap_or(0),
                hub["kind"].as_str().unwrap_or("symbol"),
                hub["name"].as_str().unwrap_or(""),
                location(&repo, file, line),
            );
        }
    }
    let docs = view["docs"].as_array().cloned().unwrap_or_default();
    if !docs.is_empty() {
        println!();
        println!("Docs");
        for doc in docs {
            let sections = doc["sections"].as_array().cloned().unwrap_or_default();
            let shown = sections
                .iter()
                .take(DOC_SECTIONS)
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>();
            let more = sections.len().saturating_sub(DOC_SECTIONS);
            let suffix = if more > 0 {
                format!(" (+{more})")
            } else {
                String::new()
            };
            println!(
                "  {}    {}{suffix}",
                doc["file"].as_str().unwrap_or(""),
                shown.join(" · ")
            );
        }
    }
    let health = &view["health"];
    println!();
    println!("Graph health");
    let waiting = health["graph"]["missing_compiler_index"]
        .as_u64()
        .unwrap_or(0);
    println!(
        "  {} · compiler index {}{} · actionable unresolved {} · partial-syntax files {} · unindexed files {}",
        health["status"].as_str().unwrap_or("partial"),
        health["compiler_index"]["state"]
            .as_str()
            .unwrap_or("unknown"),
        if waiting > 0 {
            format!(" · {waiting} refs waiting on `sinter scip`")
        } else {
            String::new()
        },
        health["graph"]["actionable_unresolved"]
            .as_u64()
            .unwrap_or(0),
        health["graph"]["syntax_error_files"].as_u64().unwrap_or(0),
        health["graph"]["unindexed_files"].as_u64().unwrap_or(0),
    );
    println!("  interpretation: structural inventory, not runtime entry-point or ownership proof");
    println!();
    println!(
        "Next: sinter ask \"<question>\" · sinter show <symbol> · sinter affected <symbol> · sinter doctor"
    );
    Ok(())
}

/// Canonical repository-orientation payload shared by CLI JSON and MCP.
pub(crate) fn response(
    repo: &Path,
    store: &sinter_store::Store,
    scopes: &ScopeSelection,
) -> Result<serde_json::Value> {
    // Streamed reads: materializing (and re-validating) the whole graph
    // via read_graph took seconds and gigabytes on big corpora just to
    // count in-degrees.
    let total_nodes = store.node_count()?;
    let edge_count = store.edge_count()?;
    let persisted_scopes = store.scope_index()?;
    let nodes: Vec<Node> = store
        .all_nodes()?
        .into_iter()
        .filter(|node| scopes.contains(persisted_scopes.scope_of(node)))
        .collect();
    let node_count = nodes.len();
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let tree = module_tree(&nodes);
    let hubs = hubs(store, &by_id, &persisted_scopes)?;
    let docs = doc_entries(&nodes);
    let health = crate::coverage::orientation_health_json(repo, store)?;
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".into());

    let modules: Vec<serde_json::Value> = tree
        .iter()
        .flat_map(|(top, branch)| {
            std::iter::once(serde_json::json!({
                "path": top,
                "nodes": branch.total.nodes,
                "files": branch.total.files.len(),
            }))
            .chain(branch.children.iter().map(move |(child, stats)| {
                serde_json::json!({
                    "path": format!("{top}/{child}"),
                    "nodes": stats.nodes,
                    "files": stats.files.len(),
                })
            }))
        })
        .collect();
    let hubs: Vec<serde_json::Value> = hubs
        .iter()
        .map(|Hub { node, total, cross }| {
            serde_json::json!({
                "name": qualified_of(node.id.as_str()),
                "kind": node.kind.as_str(),
                "scope": persisted_scopes.scope_of(node).as_str(),
                "file": node.file,
                "line": line_of(repo, &node.file, node.span.start),
                "in_degree": total,
                "cross_in_degree": cross,
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
    Ok(serde_json::json!({
        "repo": name,
        "scope": scopes.json(),
        "orientation": {
            "kind": "repository_inventory",
            "module_depth": 2,
            "hub_metric": "cross_module_in_degree_then_non_contains_in_degree",
            "hub_excludes": ["typealias", "module", "file", "generated", "vendor"],
            "hub_limit": HUBS,
            "doc_entry_rule": "level_1_readme_and_top_level_docs",
            "claim_boundary": "structural_evidence_not_runtime_architecture",
        },
        "health": health,
        "nodes": node_count,
        "total_nodes": total_nodes,
        "edges": edge_count,
        "modules": modules,
        "hubs": hubs,
        "docs": docs,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sinter_core::Span;

    fn node(id: &str, kind: SymbolKind, file: &str) -> Node {
        Node {
            id: NodeId::new(id),
            kind,
            name: id.to_string(),
            file: file.to_string(),
            span: Span { start: 0, end: 0 },
            signature: String::new(),
            doc: None,
        }
    }

    #[test]
    fn top_of_uses_first_directory() {
        assert_eq!(top_of("crates/a/src/x.rs"), "crates/a");
        assert_eq!(top_of("src/x.rs"), "src");
        assert_eq!(top_of("README.md"), ".");
    }

    #[test]
    fn aliases_containers_and_generated_code_are_not_hubs() {
        let f = node("f", SymbolKind::Function, "src/a.rs");
        assert!(hub_candidate(&f, CorpusScope::Production));
        assert!(hub_candidate(&f, CorpusScope::Test));
        assert!(!hub_candidate(&f, CorpusScope::Generated));
        assert!(!hub_candidate(&f, CorpusScope::Vendor));
        for kind in [SymbolKind::TypeAlias, SymbolKind::Module, SymbolKind::File] {
            assert!(!hub_candidate(
                &node("x", kind, "src/a.rs"),
                CorpusScope::Production
            ));
        }
    }

    #[test]
    fn hubs_rank_cross_module_before_total() {
        let hub = |id: &str, total, cross| Hub {
            node: node(id, SymbolKind::Struct, "src/a.rs"),
            total,
            cross,
        };
        let ranked = rank_hubs(vec![
            hub("local", 500, 1),
            hub("shared", 129, 87),
            hub("tie_b", 10, 5),
            hub("tie_a", 10, 5),
        ]);
        let ids: Vec<&str> = ranked.iter().map(|h| h.node.id.as_str()).collect();
        assert_eq!(ids, ["shared", "tie_a", "tie_b", "local"]);
    }

    #[test]
    fn hubs_are_capped() {
        let many = (0..HUBS + 5)
            .map(|i| Hub {
                node: node(&format!("n{i}"), SymbolKind::Struct, "src/a.rs"),
                total: i,
                cross: i,
            })
            .collect();
        assert_eq!(rank_hubs(many).len(), HUBS);
    }
}
