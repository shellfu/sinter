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

/// What to run next when the traversal reached nothing: the graph being
/// blind and the symbol having no callers look identical here, and
/// `unresolved` is the verb that tells them apart.
fn verify_command(seeds: &[(String, Node)]) -> String {
    let names = seeds
        .iter()
        .map(|(_, node)| node.name.as_str())
        .collect::<Vec<_>>()
        .join("; sinter unresolved --name ");
    format!("sinter unresolved --name {names}")
}

/// Above this many direct dependents, a truncated text answer rolls up by
/// file instead of printing rows.
const HUB_DIRECT_THRESHOLD: usize = 50;

/// Per-file rollup: direct and transitive counts plus the first dependent
/// names, callers before imports, largest files first.
fn print_file_rollup(rows: &[Row], is_import: impl Fn(&Reached) -> bool) {
    let mut by_file: std::collections::BTreeMap<&str, Vec<&Row>> =
        std::collections::BTreeMap::new();
    for row in rows {
        by_file
            .entry(row.reached.node.file.as_str())
            .or_default()
            .push(row);
    }
    let mut files: Vec<_> = by_file.into_iter().collect();
    files.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
    for (file, mut rows) in files {
        let direct = rows.iter().filter(|r| r.reached.depth == 1).count();
        // Symbols before file nodes: a file name is not a dependent name.
        rows.sort_by_key(|r| {
            (
                is_import(&r.reached) || r.reached.node.kind == sinter_core::SymbolKind::File,
                r.reached.depth,
            )
        });
        let top = rows
            .iter()
            .take(3)
            .map(|r| r.reached.node.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {file}  direct {direct}  transitive {}  {top}",
            rows.len() - direct
        );
    }
}

/// Render as a real tree: each dependent indents under the node it
/// actually reaches (via.dst), not under whatever BFS printed last.
/// Callers (calls/uses/...) print before file-import rows.
fn print_tree(
    root: &Path,
    seeds: &[(String, Node)],
    rows: &[Row],
    multi: bool,
    is_import: impl Fn(&Reached) -> bool,
) {
    let mut children: std::collections::HashMap<&str, Vec<&Row>> = std::collections::HashMap::new();
    for row in rows {
        children
            .entry(row.reached.via.dst.as_str())
            .or_default()
            .push(row);
    }
    for kids in children.values_mut() {
        kids.sort_by_key(|row| is_import(&row.reached));
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
            crate::render::site_location(root, &r.via).unwrap_or_else(|| r.node.file.clone());
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
}

/// `sinter affected`: reverse blast radius — everything transitively
/// depending on the seed symbols, cross-file, unioned and deduplicated.
/// Ok(true) when any dependent (or external reference site) was found
/// (grep-style exit codes).
#[allow(clippy::too_many_arguments)] // mirrors the clap subcommand one-to-one
pub fn run(
    repo: &Path,
    symbols: &[String],
    filter: &EdgeFilter,
    max_depth: usize,
    limit: usize,
    json: bool,
    if_snapshot: Option<&str>,
    full_coverage: bool,
    include_tests: bool,
    through_hubs: bool,
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
            out["coverage"] = crate::coverage::coverage_json(
                &root,
                &store,
                filter,
                crate::coverage::TraversalEvidence {
                    unresolved,
                    ..Default::default()
                },
                true,
                full_coverage,
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
    let scopes = store.scope_index()?;
    let scope_of = |node: &Node| scopes.scope_of(node);
    // File `use` lines are dependents too, but they are not callers: count
    // them separately so "N direct" means N symbols that actually use it.
    let is_import = |r: &Reached| r.via.relation == sinter_core::Relation::Imports;
    let is_test = |node: &Node| crate::prune::is_test_scope(scope_of(node));
    let rules = crate::prune::Rules {
        keep_tests: include_tests,
        drop_file_rows: filter.relations.is_some(),
        hub_fan_in: (!through_hubs).then_some(crate::prune::HUB_FAN_IN),
        is_test: &is_test,
    };
    let mut per_seed = Vec::with_capacity(seeds.len());
    let mut unresolved = 0usize;
    let mut tests = 0usize;
    let mut hubs: Vec<(String, usize)> = Vec::new();
    for (symbol, node) in &seeds {
        unresolved += store.unresolved_named(&node.name)?;
        // ponytail: the store still walks to max_depth before pruning; a
        // depth-aware traversal would stop at the hub instead.
        let mut reached = store.dependents(&node.id, filter, max_depth)?;
        // A seed with more direct callers than fit a screen is itself the
        // hub: its transitive radius is the whole program.
        let direct = reached
            .iter()
            .filter(|r| r.depth == 1 && !is_import(r))
            .count();
        if !through_hubs && direct > HUB_DIRECT_THRESHOLD && reached.iter().any(|r| r.depth > 1) {
            reached.retain(|r| r.depth == 1);
            hubs.push((qualified_of(node.id.as_str()).to_string(), direct));
        }
        let pruned = crate::prune::prune(reached, |r| &r.via.dst, &rules);
        tests += pruned.tests;
        hubs.extend(pruned.hubs);
        per_seed.push((symbol.clone(), pruned.rows));
    }
    let mut rows = union(per_seed);
    let total = rows.len();
    let tests_hidden = if include_tests { 0 } else { tests };
    // Hidden test rows are not the filter's doing: only a truly empty
    // traversal asks whether a flag emptied it.
    let mut filter_excluded = false;
    if total == 0 && tests_hidden == 0 {
        for (_, node) in &seeds {
            filter_excluded |=
                crate::prune::filter_excluded(&store, &node.id, filter, max_depth, false)?;
        }
    }
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
    // Gaps scoped to the files this radius actually touched: unresolved
    // refs there mean dependents may be missing from *this* answer.
    let radius = crate::coverage::radius_unresolved(
        &root,
        &store,
        seeds
            .iter()
            .map(|(_, node)| node.file.as_str())
            .chain(rows.iter().map(|row| row.reached.node.file.as_str())),
    )?;
    // An interface/trait method seed with no implementation bound to it:
    // its implementors, the dependents that break first, are not in the
    // radius at all.
    let mut implementation_gap = false;
    for (_, node) in &seeds {
        if node.kind != sinter_core::SymbolKind::Method {
            continue;
        }
        let in_edges = store.in_edges(&node.id)?;
        let owner = in_edges
            .iter()
            .find(|e| e.relation == sinter_core::Relation::Contains)
            .and_then(|e| store.node(&e.src).transpose())
            .transpose()?;
        let abstract_owner = owner.is_some_and(|o| {
            matches!(
                o.kind,
                sinter_core::SymbolKind::Interface | sinter_core::SymbolKind::Trait
            )
        });
        if abstract_owner
            && !in_edges
                .iter()
                .any(|e| e.relation == sinter_core::Relation::Implements)
        {
            implementation_gap = true;
        }
    }
    const IMPLEMENTATION_GAP: &str = "implementations not traversed";
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
        if total == 0 {
            out["verify_with"] = serde_json::json!(verify_command(&seeds));
        }
        if filter_excluded {
            out["reason"] = serde_json::json!("filter_excluded");
        }
        if tests_hidden > 0 {
            out["tests_hidden"] = serde_json::json!(tests_hidden);
        }
        if !hubs.is_empty() {
            out["stopped_at_hubs"] = serde_json::json!(
                hubs.iter()
                    .map(|(symbol, fan_in)| serde_json::json!({"symbol": symbol, "fan_in": fan_in}))
                    .collect::<Vec<_>>()
            );
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
        if implementation_gap
            && let Some(limitations) = out["coverage"]["limitations"].as_array_mut()
        {
            limitations.push(serde_json::json!(IMPLEMENTATION_GAP));
        }
        crate::agent_protocol::write_json(&out)?;
        return Ok(total > 0);
    }
    let label = seed_label(&seeds);
    let hidden = if tests_hidden > 0 {
        format!(", tests: {tests_hidden} (--include-tests)")
    } else {
        String::new()
    };
    if total == 0 {
        println!("not proven: 0 dependents observed for {label}{hidden}");
        if filter_excluded {
            println!(
                "  reason: filter excluded them (--max-depth 0 / --certain / --evidence / --relations); drop the flag to see them"
            );
        }
        println!("  verify: {}", verify_command(&seeds));
    } else {
        let imports = if importing_files > 0 {
            format!("; {importing_files} file(s) import it")
        } else {
            String::new()
        };
        println!(
            "{} dependents of {label}: {direct} direct in {direct_files} file(s){imports}, {} transitive{hidden}",
            total,
            total - direct - importing_files,
        );
    }
    // A hub that does not fit the limit is read by file, not by row:
    // `--limit <total>` (or `--json`) still yields every row.
    if direct > HUB_DIRECT_THRESHOLD && total > limit {
        print_file_rollup(&rows, is_import);
        println!("{total} rows; --limit {total} / --json for rows");
    } else {
        rows.truncate(limit);
        print_tree(&root, &seeds, &rows, multi, is_import);
        if total > limit {
            println!(
                "{} more dependents below cutoff · `sinter affected --limit {}` to widen",
                total - limit,
                total,
            );
        }
    }
    for (symbol, fan_in) in &hubs {
        println!("  stopped at hub {symbol} (fan-in {fan_in}); --through-hubs to continue");
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
    if let Some(note) = crate::coverage::radius_note(radius) {
        println!("{note}");
    }
    crate::coverage::print_footer(&root, &store, filter, evidence, total > 0, Some(&snapshot))?;
    if implementation_gap {
        println!("  gap: {IMPLEMENTATION_GAP}");
    }
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
