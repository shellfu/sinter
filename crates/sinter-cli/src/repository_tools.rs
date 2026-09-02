//! Execution of agent graph tools against one repository snapshot.
//!
//! Store lifetime and repository freshness belong outside this module. Each
//! call opens only the handles its operation needs so redb rebuilds are not
//! blocked by a session-lived reader.

use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_resolve::qualified_of;

use crate::graph_tool::{
    affected_json, affected_options, by_file, limit, required_string, scoped_node_json,
    traversal_filter,
};
use crate::lookup::{
    Found, ensure_snapshot, find_symbol, open_current, unique_symbol, unique_symbol_in,
};

pub(crate) fn call(repo: &Path, name: &str, args: &Value) -> Result<Value> {
    if name == "impact" {
        // `impact` pages per collection itself and treats 0 as "all".
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(crate::impact::DEFAULT_LIMIT as u64) as usize;
        let report = crate::impact::compute_current_with_expect(
            repo,
            &required_string(args, "rev_range")?,
            &strings(args, "expect"),
            limit,
        )?;
        return Ok(crate::impact::to_json(&report, limit));
    }
    if name == "ask" {
        let limit = limit(args, 5);
        let scopes = crate::corpus::ScopeSelection::from_json(
            args,
            crate::corpus::ScopeSelection::ask_default(),
        )?;
        let explain = args
            .get("explain")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return crate::ask::ask_response_json_current(
            repo,
            &required_string(args, "question")?,
            limit,
            &scopes,
            explain,
        );
    }
    if name == "overlap" {
        let (maps, pairs) = crate::overlap::compute_current(repo, &strings(args, "ranges"))?;
        return Ok(crate::overlap::to_json(&maps, &pairs));
    }

    let store = &open_current(repo)?;
    let snapshot = matches!(name, "show" | "query" | "affected" | "deps" | "path")
        .then(|| ensure_snapshot(store, args.get("if_snapshot").and_then(Value::as_str)))
        .transpose()?;
    let mut result = match name {
        "map" => map(repo, store, args),
        "unresolved" => {
            let optional = |key: &str| {
                args.get(key)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
            };
            let limit = limit(args, 50);
            let refs = store.unresolved_details(optional("file"), optional("name"))?;
            let root = crate::pipeline::discover_root(repo);
            let classifier = crate::coverage::Classifier::new(&root, store, &refs)?;
            Ok(crate::unresolved::to_json(&root, &classifier, &refs, limit))
        }
        "show" => {
            // `show` reads only `relations` and `scope` (see `show::edges`);
            // evidence and confidence are not part of its contract.
            let mut filter = sinter_store::EdgeFilter {
                relations: crate::lookup::relation_set(&strings(args, "relations"))?,
                ..Default::default()
            };
            let selection = crate::corpus::ScopeSelection::from_json(
                args,
                crate::corpus::ScopeSelection::agent_default(),
            )?;
            if !selection.is_all() {
                filter.scopes = Some(selection.as_set());
            }
            let one = |symbol: &str| show_one(repo, store, args, &filter, symbol);
            batch_or_one(args, "symbols", "symbol", one)
        }
        "query" => {
            let limit = limit(args, 10);
            let selection = crate::corpus::ScopeSelection::from_json(
                args,
                crate::corpus::ScopeSelection::agent_default(),
            )?;
            let (resolution, exact, mut nodes) =
                match find_symbol(store, &required_string(args, "symbol")?)? {
                    Found::Exact(nodes) => ("exact", true, nodes),
                    Found::Relocated(nodes) => ("relocated", false, nodes),
                    Found::Suggestions(nodes) => ("suggestions", false, nodes),
                };
            let scopes = store.scope_index()?;
            selection.narrow(&mut nodes, &scopes);
            Ok(json!({
                "resolution": resolution,
                "exact": exact,
                "scope": selection.json(),
                "results": nodes.iter().take(limit).map(|node| scoped_node_json(node, &scopes)).collect::<Vec<_>>(),
            }))
        }
        "context" => {
            let mut packet =
                crate::context::response(repo, store, &required_string(args, "task")?)?;
            crate::context::tool_calls(&mut packet);
            Ok(packet)
        }
        "grep" => {
            // No `within` = the whole indexed corpus, like the CLI.
            let within = strings(args, "within");
            crate::grep::json_with(
                store,
                repo,
                &required_string(args, "pattern")?,
                &within,
                &traversal_filter(args)?,
                args.get("max_depth").and_then(Value::as_u64).unwrap_or(10) as usize,
                limit(args, 100),
                args.get("no_tests")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )
        }
        "affected" => affected(repo, store, args),
        "deps" => {
            let filter = traversal_filter(args)?;
            let one = |symbol: &str| dependencies(repo, store, args, &filter, symbol);
            batch_or_one(args, "symbols", "symbol", one)
        }
        "path" => {
            let filter = traversal_filter(args)?;
            if let Some(pairs) = args.get("pairs").and_then(Value::as_array) {
                let one = |pair: &Value| {
                    let ends: Vec<&str> = pair
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    let [from, to] = ends[..] else {
                        anyhow::bail!("`pairs` entries must be [from, to] string pairs");
                    };
                    path(repo, store, &filter, from, to)
                };
                Ok(batch(pairs, "pair", one))
            } else {
                path(
                    repo,
                    store,
                    &filter,
                    &required_string(args, "from")?,
                    &required_string(args, "to")?,
                )
            }
        }
        other => anyhow::bail!("unknown tool {other}"),
    }?;
    if let Some(snapshot) = snapshot {
        // A batched response is a collection of independently actionable
        // results, so each item carries the snapshot it was computed from.
        // Query results are ordinary members of one semantic response:
        // their shared snapshot belongs only at the response root, matching
        // the CLI JSON contract without redundant per-node metadata.
        if name != "query"
            && let Some(results) = result.get_mut("results").and_then(Value::as_array_mut)
        {
            for item in results {
                item["snapshot"] = json!(snapshot);
            }
        }
        result["snapshot"] = json!(snapshot);
    }
    Ok(result)
}

/// One `show` card: edges after the filter, plus the optional excerpt.
fn show_one(
    repo: &Path,
    store: &sinter_store::Store,
    args: &Value,
    filter: &sinter_store::EdgeFilter,
    symbol: &str,
) -> Result<Value> {
    let node = unique_symbol_in(store, symbol, filter.scopes.as_ref())?;
    let scopes = store.scope_index()?;
    let limit = limit(args, crate::show::DEFAULT_LIMIT);
    let mut out = crate::show::edges_json(repo, store, &node, filter, limit)?;
    out["symbol"] = scoped_node_json(&node, &scopes);
    if args.get("body").and_then(Value::as_bool).unwrap_or(false) {
        let lines = args
            .get("context_lines")
            .and_then(Value::as_u64)
            .unwrap_or(crate::show::DEFAULT_BODY_LINES as u64) as usize;
        // Same producer as CLI `--body`, so the excerpt is the same
        // bytes; absent (unreadable file) it stays absent, not empty.
        if let Some(body) =
            crate::show::excerpt_lines(repo, &node.file, node.span.start, node.span.end, lines)
        {
            crate::show::excerpt_json(&mut out, &body);
        }
    }
    Ok(out)
}

/// Run `one` per entry of `args[list]` when the batch form was used,
/// otherwise once for the single `args[single]` string.
fn batch_or_one(
    args: &Value,
    list: &str,
    single: &str,
    one: impl Fn(&str) -> Result<Value>,
) -> Result<Value> {
    if let Some(items) = args.get(list).and_then(Value::as_array) {
        return Ok(batch(items, single, |item| {
            let Some(symbol) = item.as_str() else {
                anyhow::bail!("`{list}` entries must be strings");
            };
            one(symbol)
        }));
    }
    one(&required_string(args, single)?)
}

/// `{status, results}` over independently addressed inputs. A failed
/// entry carries the same `{code, message, candidates}` error a single
/// call would, plus the `status` the envelope would have given it, so a
/// caller can act on each result without a second round trip.
fn batch(items: &[Value], key: &str, one: impl Fn(&Value) -> Result<Value>) -> Value {
    let results: Vec<Value> = items
        .iter()
        .map(|item| match one(item) {
            Ok(mut result) => {
                if result.get("status").is_none() {
                    result["status"] = json!("found");
                }
                result
            }
            Err(error) => {
                let document = crate::agent_protocol::mcp_failure_document("batch", &error);
                json!({
                    key: item,
                    "status": document["outcome"]["status"],
                    "error": document["error"],
                })
            }
        })
        .collect();
    let status = if results.iter().any(|result| result.get("error").is_some()) {
        "partial"
    } else if results.iter().any(|result| result["status"] == "found") {
        "found"
    } else {
        "not_proven"
    };
    json!({"status": status, "results": results})
}

/// A string-array argument, absent or malformed entries dropped.
fn strings(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn affected(repo: &Path, store: &sinter_store::Store, args: &Value) -> Result<Value> {
    let filter = traversal_filter(args)?;
    let depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(10) as usize;
    let (limit, detail) = affected_options(args);
    let include_tests = flag(args, "include_tests");
    let through_hubs = flag(args, "through_hubs");
    let one = |symbol: &str| {
        affected_one(
            store,
            repo,
            symbol,
            &filter,
            depth,
            limit,
            detail,
            include_tests,
            through_hubs,
        )
    };
    batch_or_one(args, "symbols", "symbol", one)
}

#[allow(clippy::too_many_arguments)]
fn affected_one(
    store: &sinter_store::Store,
    repo: &Path,
    symbol: &str,
    filter: &sinter_store::EdgeFilter,
    depth: usize,
    limit: usize,
    detail: bool,
    include_tests: bool,
    through_hubs: bool,
) -> Result<Value> {
    let node = match unique_symbol(store, symbol) {
        Ok(node) => node,
        Err(error) if error.is::<crate::lookup::NoMatch>() => {
            let sites = crate::lookup::external_sites(store, symbol)?;
            if sites.is_empty() {
                return Err(error);
            }
            let unresolved = sites.iter().map(|site| site.refs).sum();
            let mut out = json!({
                "status": "found",
                "external": true,
                "note": "symbol is not defined in this repo; sites reference it (dependency blast radius at the repo boundary)",
                "sites": sites.iter().map(|site| json!({
                    "enclosing": site.enclosing,
                    "file": site.file,
                    "refs": site.refs,
                })).collect::<Vec<_>>(),
            });
            out["coverage"] = crate::coverage::traversal_json(
                repo,
                store,
                filter,
                crate::coverage::TraversalEvidence {
                    unresolved,
                    ..Default::default()
                },
                true,
            )?;
            return Ok(out);
        }
        Err(error) => return Err(error),
    };
    let mut reached = store.dependents(&node.id, filter, depth)?;
    let scopes = store.scope_index()?;
    // Same pruning as the CLI: tests are counted, hubs stop the walk, file
    // rows go when a relations filter is set. `include_tests` and
    // `through_hubs` restore the old shape.
    let is_test = |node: &sinter_core::Node| crate::prune::is_test_scope(scopes.scope_of(node));
    let direct = reached.iter().filter(|r| r.depth == 1).count();
    let mut hubs: Vec<(String, usize)> = Vec::new();
    if !through_hubs && direct > 50 && reached.iter().any(|r| r.depth > 1) {
        reached.retain(|r| r.depth == 1);
        hubs.push((qualified_of(node.id.as_str()).to_string(), direct));
    }
    let pruned = crate::prune::prune(
        reached,
        |r| &r.via.dst,
        &crate::prune::Rules {
            keep_tests: include_tests,
            drop_file_rows: filter.relations.is_some(),
            hub_fan_in: (!through_hubs).then_some(crate::prune::HUB_FAN_IN),
            is_test: &is_test,
        },
    );
    hubs.extend(pruned.hubs);
    let tests_hidden = if include_tests { 0 } else { pruned.tests };
    let reached = pruned.rows;
    let unresolved = store.unresolved_named(&node.name)?;
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        reached.iter().map(|item| item.via.confidence),
        unresolved,
    );
    let entries: Vec<Value> = reached
        .iter()
        .take(limit)
        .map(|reached| {
            if detail {
                json!({
                    "node": scoped_node_json(&reached.node, &scopes),
                    "depth": reached.depth,
                    "relation": reached.via.relation.as_str(),
                    "evidence": reached.via.evidence.as_str(),
                    "confidence": match reached.via.confidence {
                        sinter_core::Confidence::Certain => "certain",
                        sinter_core::Confidence::Inferred => "possible",
                    },
                    "site": crate::render::site_json(repo, &reached.via),
                })
            } else {
                let mut entry = json!({
                    "s": qualified_of(reached.node.id.as_str()),
                    "k": reached.node.kind.as_str(),
                    "f": reached.node.file,
                    "scope": scopes.scope_of(&reached.node).as_str(),
                    "e": format!("{}/{}", reached.via.relation.as_str(), reached.via.evidence.as_str()),
                    "c": match reached.via.confidence {
                        sinter_core::Confidence::Certain => "certain",
                        sinter_core::Confidence::Inferred => "possible",
                    },
                    "d": reached.depth,
                });
                let site = crate::render::site_json(repo, &reached.via);
                if !site.is_null() {
                    entry["site"] = site;
                }
                entry
            }
        })
        .collect();
    let mut out = affected_json(
        scoped_node_json(&node, &scopes),
        json!(unresolved),
        Some(crate::pipeline::scip_index_path(repo).is_some()),
        entries,
        by_file(reached.iter().map(|reached| reached.node.file.clone())),
        {
            let (direct, files) = sinter_store::direct_summary(&reached);
            (reached.len(), direct, files)
        },
        limit,
    );
    if tests_hidden > 0 {
        out["tests_hidden"] = json!(tests_hidden);
    }
    if !hubs.is_empty() {
        out["stopped_at_hubs"] = json!(
            hubs.iter()
                .map(|(symbol, fan_in)| json!({"symbol": symbol, "fan_in": fan_in}))
                .collect::<Vec<_>>()
        );
    }
    out["status"] = json!(if reached.is_empty() {
        "not_proven"
    } else {
        "found"
    });
    out["coverage"] =
        crate::coverage::traversal_json(repo, store, filter, evidence, !reached.is_empty())?;
    Ok(out)
}

fn flag(args: &Value, name: &str) -> bool {
    args.get(name).and_then(Value::as_bool).unwrap_or(false)
}

fn dependencies(
    repo: &Path,
    store: &sinter_store::Store,
    args: &Value,
    filter: &sinter_store::EdgeFilter,
    symbol: &str,
) -> Result<Value> {
    // Depth 1 by default, like the CLI: the transitive closure of an entry
    // point is a context bomb.
    let depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(1) as usize;
    let limit = limit(args, 50);
    let node = unique_symbol(store, symbol)?;
    let reached = store.dependencies(&node.id, filter, depth)?;
    let scopes = store.scope_index()?;
    let include_tests = flag(args, "include_tests");
    let is_test = |node: &sinter_core::Node| crate::prune::is_test_scope(scopes.scope_of(node));
    let pruned = crate::prune::prune(
        reached,
        |r| &r.via.dst,
        &crate::prune::Rules {
            keep_tests: include_tests,
            drop_file_rows: filter.relations.is_some(),
            hub_fan_in: None,
            is_test: &is_test,
        },
    );
    let tests_hidden = if include_tests { 0 } else { pruned.tests };
    let reached = pruned.rows;
    let unresolved = store
        .references_in(&node.file)?
        .iter()
        .filter(|reference| reference.enclosing.as_ref() == Some(&node.id))
        .count();
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        reached.iter().map(|item| item.via.confidence),
        unresolved,
    );
    let entries: Vec<Value> = reached
        .iter()
        .take(limit)
        .map(|reached| {
            let mut entry = json!({
                "s": qualified_of(reached.node.id.as_str()),
                "k": reached.node.kind.as_str(),
                "f": reached.node.file,
                "scope": scopes.scope_of(&reached.node).as_str(),
                "e": format!("{}/{}", reached.via.relation.as_str(), reached.via.evidence.as_str()),
                "c": match reached.via.confidence {
                    sinter_core::Confidence::Certain => "certain",
                    sinter_core::Confidence::Inferred => "possible",
                },
                "d": reached.depth,
            });
            let site = crate::render::site_json(repo, &reached.via);
            if !site.is_null() {
                entry["site"] = site;
            }
            entry
        })
        .collect();
    let mut out = json!({
        "status": if reached.is_empty() { "not_proven" } else { "found" },
        "symbol": scoped_node_json(&node, &scopes),
        "total": reached.len(),
        "unresolved_refs_in_symbol": unresolved,
        "by_file": by_file(reached.iter().map(|reached| reached.node.file.clone())),
        "dependencies": entries,
        "max_depth": depth,
    });
    if tests_hidden > 0 {
        out["tests_hidden"] = json!(tests_hidden);
    }
    if reached.len() > limit {
        out["truncated"] = json!(reached.len() - limit);
    }
    out["coverage"] =
        crate::coverage::traversal_json(repo, store, filter, evidence, !reached.is_empty())?;
    Ok(out)
}

fn path(
    repo: &Path,
    store: &sinter_store::Store,
    filter: &sinter_store::EdgeFilter,
    from: &str,
    to: &str,
) -> Result<Value> {
    let from = unique_symbol(store, from)?;
    let to = unique_symbol(store, to)?;
    let path = store.shortest_path(&from.id, &to.id, filter)?;
    let scopes = store.scope_index()?;
    let scope_of_id = |id: &sinter_core::NodeId| {
        let file = id
            .as_str()
            .split_once('#')
            .map_or(id.as_str(), |(file, _)| file);
        scopes.scope_of_id(id.as_str(), file)
    };
    let miss = path
        .is_none()
        .then(|| crate::pathcmd::explain_miss(store, &from, &to, filter))
        .transpose()?;
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        path.iter().flatten().map(|edge| edge.confidence),
        miss.as_ref()
            .map_or(0, |miss| miss.unresolved_matching_target),
    );
    let mut out = json!({
        "status": if path.is_some() { "found" } else { "not_proven" },
        "found": path.is_some(),
        "steps": path.iter().flatten().map(|edge| json!({
            "from": qualified_of(edge.src.as_str()),
            "to": qualified_of(edge.dst.as_str()),
            "from_scope": scope_of_id(&edge.src).as_str(),
            "to_scope": scope_of_id(&edge.dst).as_str(),
            "relation": edge.relation.as_str(),
            "evidence": edge.evidence.as_str(),
            "confidence": match edge.confidence {
                sinter_core::Confidence::Certain => "certain",
                sinter_core::Confidence::Inferred => "possible",
            },
            "site": crate::render::site_json(repo, edge),
        })).collect::<Vec<_>>(),
    });
    if let Some(miss) = &miss {
        out["miss"] = crate::pathcmd::miss_json(repo, miss);
    }
    out["coverage"] =
        crate::coverage::traversal_json(repo, store, filter, evidence, path.is_some())?;
    Ok(out)
}

/// Structural repository inventory, matching `sinter map --json`.
fn map(repo: &Path, store: &sinter_store::Store, args: &Value) -> Result<Value> {
    let selection = crate::corpus::ScopeSelection::from_json(
        args,
        crate::corpus::ScopeSelection::agent_default(),
    )?;
    crate::map::response(repo, store, &selection)
}
