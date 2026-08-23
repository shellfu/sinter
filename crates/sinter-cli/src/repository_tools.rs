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
        let report = crate::impact::compute_current(repo, &required_string(args, "rev_range")?)?;
        let limit = limit(args, crate::impact::DEFAULT_LIMIT);
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
        let ranges: Vec<String> = args
            .get("ranges")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let (maps, pairs) = crate::overlap::compute_current(repo, &ranges)?;
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
            let filter = traversal_filter(args)?;
            let node = unique_symbol_in(
                store,
                &required_string(args, "symbol")?,
                filter.scopes.as_ref(),
            )?;
            let scopes = store.scope_index()?;
            let limit = limit(args, crate::show::DEFAULT_LIMIT);
            let mut out = crate::show::edges_json(repo, store, &node, &filter, limit)?;
            out["symbol"] = scoped_node_json(&node, &scopes);
            Ok(out)
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
        "context" => crate::context::response(repo, store, &required_string(args, "task")?),
        "affected" => affected(repo, store, args),
        "deps" => dependencies(repo, store, args),
        "path" => path(repo, store, args),
        other => anyhow::bail!("unknown tool {other}"),
    }?;
    if let Some(snapshot) = snapshot {
        // A batched `affected` response is a collection of independently
        // actionable results, so each item carries the snapshot it was
        // computed from. Query results are ordinary members of one semantic
        // response: their shared snapshot belongs only at the response root,
        // matching the CLI JSON contract without redundant per-node metadata.
        if name == "affected"
            && args.get("symbols").is_some()
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

fn affected(repo: &Path, store: &sinter_store::Store, args: &Value) -> Result<Value> {
    let filter = traversal_filter(args)?;
    let depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(10) as usize;
    let (limit, detail) = affected_options(args);
    let one = |symbol: &str| affected_one(store, repo, symbol, &filter, depth, limit, detail);

    if let Some(symbols) = args.get("symbols").and_then(Value::as_array) {
        let results: Vec<Value> = symbols
            .iter()
            .map(|value| {
                let Some(symbol) = value.as_str() else {
                    return json!({"symbol": value, "error": "symbols entries must be strings"});
                };
                one(symbol).unwrap_or_else(
                    |error| json!({"symbol": symbol, "error": format!("{error:#}")}),
                )
            })
            .collect();
        let status = if results.iter().any(|result| result.get("error").is_some()) {
            "partial"
        } else if results.iter().any(|result| result["status"] == "found") {
            "found"
        } else {
            "not_proven"
        };
        return Ok(json!({"status": status, "results": results}));
    }
    one(&required_string(args, "symbol")?)
}

fn affected_one(
    store: &sinter_store::Store,
    repo: &Path,
    symbol: &str,
    filter: &sinter_store::EdgeFilter,
    depth: usize,
    limit: usize,
    detail: bool,
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
    let reached = store.dependents(&node.id, filter, depth)?;
    let scopes = store.scope_index()?;
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
    out["status"] = json!(if reached.is_empty() {
        "not_proven"
    } else {
        "found"
    });
    out["coverage"] =
        crate::coverage::traversal_json(repo, store, filter, evidence, !reached.is_empty())?;
    Ok(out)
}

fn dependencies(repo: &Path, store: &sinter_store::Store, args: &Value) -> Result<Value> {
    let filter = traversal_filter(args)?;
    let depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(10) as usize;
    let limit = limit(args, 50);
    let node = unique_symbol(store, &required_string(args, "symbol")?)?;
    let reached = store.dependencies(&node.id, &filter, depth)?;
    let scopes = store.scope_index()?;
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
    });
    if reached.len() > limit {
        out["truncated"] = json!(reached.len() - limit);
    }
    out["coverage"] =
        crate::coverage::traversal_json(repo, store, &filter, evidence, !reached.is_empty())?;
    Ok(out)
}

fn path(repo: &Path, store: &sinter_store::Store, args: &Value) -> Result<Value> {
    let filter = traversal_filter(args)?;
    let from = unique_symbol(store, &required_string(args, "from")?)?;
    let to = unique_symbol(store, &required_string(args, "to")?)?;
    let path = store.shortest_path(&from.id, &to.id, &filter)?;
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
        .then(|| crate::pathcmd::explain_miss(store, &from, &to, &filter))
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
        crate::coverage::traversal_json(repo, store, &filter, evidence, path.is_some())?;
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
