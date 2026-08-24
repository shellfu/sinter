use std::path::Path;

use anyhow::Result;
use serde_json::Value;
use sinter_core::Node;
use sinter_resolve::qualified_of;

use sinter_store::{EdgeFilter, Reached};

use crate::lookup::{ensure_snapshot, ensure_snapshot_token, open_store, unique_symbol_in};
use crate::render::node_json;

/// One unioned dependent plus the seeds that reached it. A node reached
/// from several seeds appears once; `seeds` is its provenance, in the
/// order the seeds were given.
struct Row {
    seeds: Vec<String>,
    reached: Reached,
}

/// A workspace dependent plus its seed provenance. Union key is
/// (member, node id): the same name in two members is two dependents.
struct WsRow {
    seeds: Vec<String>,
    reached: crate::workspace::WsReached,
}

/// Union the per-seed traversals, deduplicated by node id. The first seed
/// that reaches a node owns its edge, depth, and tree position; later
/// seeds only add provenance.
fn union(per_seed: Vec<(String, Vec<Reached>)>) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (symbol, reached) in per_seed {
        for item in reached {
            match index.get(item.node.id.as_str()) {
                Some(&at) => {
                    if rows[at].seeds.last() != Some(&symbol) {
                        rows[at].seeds.push(symbol.clone());
                    }
                }
                None => {
                    index.insert(item.node.id.as_str().to_string(), rows.len());
                    rows.push(Row {
                        seeds: vec![symbol.clone()],
                        reached: item,
                    });
                }
            }
        }
    }
    rows
}

/// What the answer is about: one seed reads as today (`qualified (file)`),
/// several read as the seed list. Single-seed output must not change.
fn seed_label(seeds: &[(String, Node)]) -> String {
    match seeds {
        [(_, node)] => format!("{} ({})", qualified_of(node.id.as_str()), node.file),
        _ => format!(
            "{} ({} seeds)",
            seeds
                .iter()
                .map(|(symbol, _)| symbol.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            seeds.len()
        ),
    }
}

/// One terse dependent row, same shape as the MCP `affected` tool.
/// `seeds` is `None` for single-seed calls, which stay byte-identical.
fn entry_json(root: &Path, r: &Reached, scope: &str, seeds: Option<&[String]>) -> Value {
    let mut entry = serde_json::json!({
        "s": qualified_of(r.node.id.as_str()),
        "k": r.node.kind.as_str(),
        "f": r.node.file,
        "scope": scope,
        "e": format!("{}/{}", r.via.relation.as_str(), r.via.evidence.as_str()),
        "c": match r.via.confidence {
            sinter_core::Confidence::Certain => "certain",
            sinter_core::Confidence::Inferred => "possible",
        },
        "d": r.depth,
    });
    let site = crate::render::site_json(root, &r.via);
    if !site.is_null() {
        entry["site"] = site;
    }
    if let Some(seeds) = seeds {
        entry["seeds"] = serde_json::json!(seeds);
    }
    entry
}

/// `sinter affected`: reverse blast radius — everything transitively
/// depending on the seed symbols, cross-file, unioned and deduplicated.
/// Ok(true) when any dependent (or external reference site) was found
/// (grep-style exit codes).
pub fn run(
    repo: &Path,
    symbols: &[String],
    filter: &EdgeFilter,
    max_depth: usize,
    limit: usize,
    json: bool,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let Some(first) = symbols.first() else {
        anyhow::bail!("affected: no symbol given");
    };
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let root = crate::pipeline::discover_root(repo);
    // Resolve every seed independently: one bad seed must not cost the
    // answer for the others.
    let mut seeds: Vec<(String, Node)> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    let mut first_error = None;
    for symbol in symbols {
        match unique_symbol_in(&store, symbol, filter.scopes.as_ref()) {
            Ok(node) => seeds.push((symbol.clone(), node)),
            Err(e) => {
                failed.push((symbol.clone(), format!("{e:#}")));
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }
    // Provenance is a property of the invocation, not of how many seeds
    // survived: an agent that passed one symbol always parses one shape.
    let multi = symbols.len() > 1;
    if seeds.is_empty() {
        let e = first_error.expect("a failed seed carries its error");
        // Not defined here — dependency blast radius at the repo boundary
        // is still an answer: every site referencing the external symbol.
        if !e.is::<crate::lookup::NoMatch>() {
            return Err(e);
        }
        let sites = crate::lookup::external_sites(&store, first)?;
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
            "`{first}` is not defined in this repo; {total} reference(s) at {} site(s):",
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
    if !json {
        for (symbol, error) in &failed {
            eprintln!("warning: seed `{symbol}` not resolved: {error}");
        }
    }
    let mut per_seed = Vec::with_capacity(seeds.len());
    let mut unresolved = 0usize;
    for (symbol, node) in &seeds {
        unresolved += store.unresolved_named(&node.name)?;
        per_seed.push((
            symbol.clone(),
            store.dependents(&node.id, filter, max_depth)?,
        ));
    }
    let mut rows = union(per_seed);
    let scopes = store.scope_index()?;
    let scope_of = |node: &Node| scopes.scope_of(node);
    let total = rows.len();
    // File `use` lines are dependents too, but they are not callers: count
    // them separately so "N direct" means N symbols that actually use it.
    let is_import = |r: &Reached| r.via.relation == sinter_core::Relation::Imports;
    let callers: Vec<&Row> = rows
        .iter()
        .filter(|row| row.reached.depth == 1 && !is_import(&row.reached))
        .collect();
    let direct = callers.len();
    let direct_files = callers
        .iter()
        .map(|row| row.reached.node.file.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let importing_files = rows
        .iter()
        .filter(|row| row.reached.depth == 1 && is_import(&row.reached))
        .map(|row| row.reached.node.file.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        rows.iter().map(|row| row.reached.via.confidence),
        unresolved,
    );
    if json {
        // Same shape as the MCP `affected` tool (terse entries).
        let entries: Vec<Value> = rows
            .iter()
            .take(limit)
            .map(|row| {
                entry_json(
                    &root,
                    &row.reached,
                    scope_of(&row.reached.node).as_str(),
                    multi.then_some(row.seeds.as_slice()),
                )
            })
            .collect();
        let mut counts = std::collections::HashMap::<String, u64>::new();
        for row in &rows {
            *counts.entry(row.reached.node.file.clone()).or_default() += 1;
        }
        let mut pairs: Vec<_> = counts.into_iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        pairs.truncate(10);
        let symbols_json: Vec<Value> = seeds
            .iter()
            .map(|(_, node)| {
                let mut symbol_json = node_json(node);
                symbol_json["scope"] = serde_json::json!(scope_of(node).as_str());
                symbol_json
            })
            .collect();
        let mut out = serde_json::json!({
            "status": if !failed.is_empty() {
                "partial"
            } else if total > 0 {
                "found"
            } else {
                "not_proven"
            },
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
        if multi {
            out["symbols"] = serde_json::json!(symbols_json);
        } else {
            out["symbol"] = symbols_json[0].clone();
        }
        if !failed.is_empty() {
            // Same per-symbol error shape the MCP batch reports.
            out["failed"] = serde_json::json!(
                failed
                    .iter()
                    .map(|(symbol, error)| serde_json::json!({"symbol": symbol, "error": error}))
                    .collect::<Vec<_>>()
            );
        }
        if total > limit {
            out["truncated"] = serde_json::json!(total - limit);
        }
        out["coverage"] =
            crate::coverage::traversal_json(&root, &store, filter, evidence, total > 0)?;
        crate::agent_protocol::write_json(&out)?;
        return Ok(total > 0);
    }
    rows.truncate(limit);
    let label = seed_label(&seeds);
    if total == 0 {
        println!("not proven: 0 dependents observed for {label}");
    } else {
        let imports = if importing_files > 0 {
            format!("; {importing_files} file(s) import it")
        } else {
            String::new()
        };
        println!(
            "{} dependents of {label}: {direct} direct in {direct_files} file(s){imports}, {} transitive",
            total,
            total - direct - importing_files,
        );
    }
    // Render as a real tree: each dependent indents under the node it
    // actually reaches (via.dst), not under whatever BFS printed last.
    let mut children: std::collections::HashMap<&str, Vec<&Row>> = std::collections::HashMap::new();
    for row in &rows {
        children
            .entry(row.reached.via.dst.as_str())
            .or_default()
            .push(row);
    }
    let mut stack: Vec<(&Row, usize)> = Vec::new();
    for (_, node) in seeds.iter().rev() {
        if let Some(roots) = children.get(node.id.as_str()) {
            for row in roots.iter().rev() {
                stack.push((row, 1));
            }
        }
    }
    while let Some((row, depth)) = stack.pop() {
        let r = &row.reached;
        // The call site (in the dependent's own file) replaces the bare
        // file — "depends on it at file:line", not just "depends on it".
        let place =
            crate::render::site_location(&root, &r.via).unwrap_or_else(|| r.node.file.clone());
        // Provenance: which seed(s) reached this row. Omitted for
        // single-seed calls, whose output must not change.
        let from = if multi {
            format!("  <- {}", row.seeds.join(", "))
        } else {
            String::new()
        };
        println!(
            "  {}{} {}  {}  [{}/{}]{from}",
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
        let names = seeds
            .iter()
            .map(|(_, node)| node.name.as_str())
            .collect::<Vec<_>>()
            .join("`, `");
        println!(
            "  note: {unresolved} unresolved ref(s) also name `{names}` — dependents may be missing; {}",
            crate::coverage::unresolved_hint(&root)
        );
    }
    crate::coverage::print_footer(&root, &store, filter, evidence, total > 0, Some(&snapshot))?;
    Ok(total > 0)
}

/// `sinter affected --workspace`: cross-repo blast radius over member
/// stores plus boundary links, unioned across seeds.
pub fn run_workspace(
    manifest: &std::path::Path,
    symbols: &[String],
    filter: &EdgeFilter,
    max_depth: usize,
    limit: usize,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    if symbols.is_empty() {
        anyhow::bail!("affected: no symbol given");
    }
    let ws = crate::workspace::load(manifest)?;
    let snapshot = crate::workspace::snapshot_token(&ws)?;
    ensure_snapshot_token(if_snapshot, &snapshot)?;
    let mut seeds: Vec<(String, String, Node)> = Vec::new();
    let mut first_error = None;
    for symbol in symbols {
        match crate::workspace::find_symbol(&ws, symbol) {
            Ok((member, node)) => seeds.push((symbol.clone(), member, node)),
            Err(e) => {
                eprintln!("warning: seed `{symbol}` not resolved: {e:#}");
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }
    let Some((_, first_member, first_node)) = seeds.first() else {
        return Err(first_error.expect("a failed seed carries its error"));
    };
    let multi = symbols.len() > 1;
    // Union by (member, node id): the same name in two members is two
    // distinct dependents.
    let mut rows: Vec<WsRow> = Vec::new();
    let mut index: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for (symbol, member, node) in &seeds {
        for item in crate::workspace::dependents(&ws, member, &node.id, filter, max_depth)? {
            let key = (item.member.clone(), item.node.id.as_str().to_string());
            match index.get(&key) {
                Some(&at) => {
                    if rows[at].seeds.last() != Some(symbol) {
                        rows[at].seeds.push(symbol.clone());
                    }
                }
                None => {
                    index.insert(key, rows.len());
                    rows.push(WsRow {
                        seeds: vec![symbol.clone()],
                        reached: item,
                    });
                }
            }
        }
    }
    let total = rows.len();
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        rows.iter().map(|row| row.reached.evidence.confidence()),
        0,
    );
    rows.truncate(limit);
    let label = if multi {
        format!(
            "{} ({} seeds)",
            symbols
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            symbols.len()
        )
    } else {
        format!(
            "{first_member}:{} ({})",
            qualified_of(first_node.id.as_str()),
            first_node.file
        )
    };
    if total == 0 {
        println!("not proven: 0 dependents observed for {label}");
    } else {
        println!("{total} dependents of {label}");
    }
    let mut children: std::collections::HashMap<(&str, &str), Vec<&WsRow>> =
        std::collections::HashMap::new();
    for row in &rows {
        children
            .entry((row.reached.parent.0.as_str(), row.reached.parent.1.as_str()))
            .or_default()
            .push(row);
    }
    let mut stack: Vec<(&WsRow, usize)> = Vec::new();
    for (_, member, node) in seeds.iter().rev() {
        if let Some(roots) = children.get(&(member.as_str(), node.id.as_str())) {
            for row in roots.iter().rev() {
                stack.push((row, 1));
            }
        }
    }
    while let Some((row, depth)) = stack.pop() {
        let r = &row.reached;
        let from = if multi {
            format!("  <- {}", row.seeds.join(", "))
        } else {
            String::new()
        };
        println!(
            "  {}{}:{} {}  {}  [{}/{}]{from}",
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

#[cfg(test)]
mod tests {
    use super::{Row, entry_json, seed_label, union};
    use sinter_core::{Confidence, Edge, Evidence, Node, NodeId, Relation, Span, SymbolKind};
    use sinter_store::Reached;

    fn node(id: &str) -> Node {
        Node {
            id: NodeId::new(id),
            kind: SymbolKind::Function,
            name: id.rsplit('#').next().unwrap_or(id).to_string(),
            file: id.split('#').next().unwrap_or(id).to_string(),
            span: Span { start: 0, end: 1 },
            signature: String::new(),
            doc: None,
        }
    }

    fn reached(id: &str, from: &str, depth: usize) -> Reached {
        Reached {
            node: node(id),
            depth,
            via: Edge {
                src: NodeId::new(id),
                dst: NodeId::new(from),
                relation: Relation::Calls,
                evidence: Evidence::Scope,
                confidence: Confidence::Certain,
                site: None,
            },
        }
    }

    fn seeds_of(rows: &[Row]) -> Vec<(String, Vec<String>)> {
        rows.iter()
            .map(|row| (row.reached.node.id.as_str().to_string(), row.seeds.clone()))
            .collect()
    }

    #[test]
    fn union_deduplicates_by_node_and_records_every_seed() {
        let rows = union(vec![
            (
                "a".to_string(),
                vec![
                    reached("f.rs#x", "f.rs#a", 1),
                    reached("f.rs#y", "f.rs#x", 2),
                ],
            ),
            (
                "b".to_string(),
                vec![
                    reached("f.rs#x", "f.rs#b", 1),
                    reached("f.rs#z", "f.rs#b", 1),
                ],
            ),
        ]);
        assert_eq!(
            seeds_of(&rows),
            vec![
                ("f.rs#x".to_string(), vec!["a".to_string(), "b".to_string()]),
                ("f.rs#y".to_string(), vec!["a".to_string()]),
                ("f.rs#z".to_string(), vec!["b".to_string()]),
            ]
        );
        // First seed to reach a node owns its edge and depth.
        assert_eq!(rows[0].reached.via.dst.as_str(), "f.rs#a");
        assert_eq!(rows[0].reached.depth, 1);
    }

    #[test]
    fn union_does_not_repeat_a_seed_for_the_same_node() {
        let rows = union(vec![(
            "a".to_string(),
            vec![
                reached("f.rs#x", "f.rs#a", 1),
                reached("f.rs#x", "f.rs#a", 2),
            ],
        )]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seeds, vec!["a".to_string()]);
    }

    #[test]
    fn single_seed_json_row_carries_no_provenance() {
        let root = std::path::Path::new(".");
        let one = entry_json(root, &reached("f.rs#x", "f.rs#a", 1), "repo", None);
        assert!(one.get("seeds").is_none());
        assert_eq!(
            one.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["c", "d", "e", "f", "k", "s", "scope"]
        );
        let many = entry_json(
            root,
            &reached("f.rs#x", "f.rs#a", 1),
            "repo",
            Some(&["a".to_string(), "b".to_string()]),
        );
        assert_eq!(many["seeds"], serde_json::json!(["a", "b"]));
        // Provenance is additive: nothing else about the row changes.
        for key in ["s", "k", "f", "scope", "e", "c", "d"] {
            assert_eq!(many[key], one[key]);
        }
    }

    #[test]
    fn single_seed_label_is_unchanged_by_multi_seed_support() {
        let seeds = vec![("x".to_string(), node("f.rs#x"))];
        assert_eq!(seed_label(&seeds), "x (f.rs)");
        let two = vec![
            ("x".to_string(), node("f.rs#x")),
            ("y".to_string(), node("g.rs#y")),
        ];
        assert_eq!(seed_label(&two), "x, y (2 seeds)");
    }
}
