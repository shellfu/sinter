//! `sinter serve`: MCP server over stdio (newline-delimited JSON-RPC).
//! Hand-rolled: the protocol subset needed (initialize, tools/list,
//! tools/call, ping) is ~all of it; an SDK dependency buys nothing yet.
//! Every edge-walking tool takes evidence/confidence filters.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::Node;
use sinter_resolve::qualified_of;

use crate::lookup::{Found, edge_filter, find_symbol, open_current, open_store, unique_symbol};

/// One server owns one scope (D28): a repository, or a whole workspace.
enum Scope {
    Repo {
        repo: PathBuf,
        freshness: crate::freshness::RepoFreshness,
    },
    Workspace(PathBuf),
}

pub fn run(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;
    let freshness = crate::freshness::RepoFreshness::new(&repo)?;
    serve(Scope::Repo { repo, freshness })
}

pub fn run_workspace(manifest: &Path) -> Result<()> {
    serve(Scope::Workspace(manifest.canonicalize()?))
}

fn serve(scope: Scope) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request): Result<Value, _> = serde_json::from_str(&line) else {
            writeln!(
                stdout,
                "{}",
                json!({"jsonrpc": "2.0", "id": Value::Null,
                       "error": {"code": -32700, "message": "parse error"}})
            )?;
            stdout.flush()?;
            continue;
        };
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            continue; // notification — nothing to answer
        };
        let response = match handle(&scope, method, &params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => {
                // Recovery hint: lookup already appends close-name
                // suggestions when it has them; a bare miss would
                // otherwise dead-end the calling agent. Its hint names
                // the CLI verb — swap it for the MCP tool, exactly one
                // hint either way.
                let mut message = format!("{e:#}");
                if message.contains("no symbol matches") {
                    if let Some(pos) = message.find(" — try `sinter ask") {
                        message.truncate(pos);
                    }
                    message.push_str(" — try the ask tool for concept search");
                }
                json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": -32000, "message": message}
                })
            }
        };
        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }
    Ok(())
}

fn handle(scope: &Scope, method: &str, params: &Value) -> Result<Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "sinter", "version": env!("CARGO_PKG_VERSION")},
        })),
        "ping" => Ok(json!({})),
        // The list is the scope's honest surface: workspace mode serves
        // only the tools that genuinely cross repositories.
        "tools/list" => Ok(match scope {
            Scope::Repo { .. } => tools_list(),
            Scope::Workspace(_) => ws_tools_list(),
        }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            // No session-lived handle: redb's lock is exclusive, so holding
            // the store across calls would block `sinter build`/`watch` and
            // pin a stale snapshot. Freshness itself is enforced inside
            // open_store (repo scope) or the per-member sync (workspace).
            let result = match scope {
                Scope::Repo { repo, freshness } => {
                    freshness.sync()?;
                    call_tool(repo, name, &args)?
                }
                Scope::Workspace(manifest) => ws_call_tool(manifest, name, &args)?,
            };
            // Compact encoding: the consumer is an agent, not a human;
            // pretty-printing is pure token waste.
            Ok(json!({
                "content": [{"type": "text", "text": serde_json::to_string(&result)?}]
            }))
        }
        other => anyhow::bail!("unknown method {other}"),
    }
}

fn filter_args(args: &Value) -> (Vec<String>, bool) {
    let evidence = args
        .get("evidence")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let certain = args
        .get("min_confidence")
        .and_then(Value::as_str)
        .is_some_and(|c| c == "certain");
    (evidence, certain)
}

/// `relations` array to the --relations-style name list.
fn relations_args(args: &Value) -> Vec<String> {
    args.get("relations")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn node_json(node: &Node) -> Value {
    json!({
        "id": node.id.as_str(),
        "qualified": qualified_of(node.id.as_str()),
        "name": node.name,
        "kind": node.kind.as_str(),
        "file": node.file,
        "span": {"start": node.span.start, "end": node.span.end},
        "signature": node.signature,
        "doc": node.doc,
    })
}

/// limit/detail knobs shared by both scopes' `affected`.
fn affected_args(args: &Value) -> (usize, bool) {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
    let detail = args.get("detail").and_then(Value::as_bool).unwrap_or(false);
    (limit, detail)
}

/// Descending per-file dependent counts, top 10 — the summary an agent
/// reads instead of scrolling N entries.
fn by_file(files: impl Iterator<Item = String>) -> Value {
    let mut counts = std::collections::HashMap::<String, u64>::new();
    for f in files {
        *counts.entry(f).or_default() += 1;
    }
    let mut pairs: Vec<_> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs.truncate(10);
    json!(pairs)
}

/// Terse dependents keep responses bounded: no doc, signature, span, or id
/// per entry — `show` exists for that. Adds "truncated" only when nonzero.
fn affected_json(
    symbol: Value,
    unresolved: Value,
    scip: Option<bool>,
    entries: Vec<Value>,
    files: Value,
    // (total, direct, files containing a direct dependent)
    counts: (usize, usize, usize),
    limit: usize,
) -> Value {
    let (total, direct, direct_files) = counts;
    let mut out = json!({
        "symbol": symbol,
        "total": total,
        "direct": direct,
        "direct_files": direct_files,
        "unresolved_refs_matching_name": unresolved,
        "by_file": files,
        "dependents": entries,
    });
    if let Some(scip) = scip {
        out["scip_evidence_available"] = json!(scip);
    }
    if total > limit {
        out["truncated"] = json!(total - limit);
    }
    out
}

/// One symbol's repo-scope blast radius, summary-first. The root keeps the
/// full node (one doc is cheap and useful); dependents are terse.
fn affected_one(
    store: &sinter_store::Store,
    repo: &Path,
    sym: &str,
    filter: &sinter_store::EdgeFilter,
    depth: usize,
    limit: usize,
    detail: bool,
) -> Result<Value> {
    let node = match unique_symbol(store, sym) {
        Ok(node) => node,
        Err(e) => {
            let sites = crate::lookup::external_sites(store, sym)?;
            if sites.is_empty() {
                return Err(e);
            }
            return Ok(json!({
                "external": true,
                "note": "symbol is not defined in this repo; sites reference it (dependency blast radius at the repo boundary)",
                "sites": sites.iter().map(|s| json!({
                    "enclosing": s.enclosing,
                    "file": s.file,
                    "refs": s.refs,
                })).collect::<Vec<_>>(),
            }));
        }
    };
    let reached = store.dependents(&node.id, filter, depth)?;
    // Honest-empty signal: unresolved refs sharing the name mean
    // the dependents list may be incomplete, never authoritative.
    let unresolved = store.unresolved_named(&node.name)?;
    let entries: Vec<Value> = reached
        .iter()
        .take(limit)
        .map(|r| {
            if detail {
                json!({
                    "node": node_json(&r.node),
                    "depth": r.depth,
                    "relation": r.via.relation.as_str(),
                    "evidence": r.via.evidence.as_str(),
                    "site": crate::render::site_json(repo, &r.via),
                })
            } else {
                let mut entry = json!({
                    "s": qualified_of(r.node.id.as_str()),
                    "k": r.node.kind.as_str(),
                    "f": r.node.file,
                    "e": format!("{}/{}", r.via.relation.as_str(), r.via.evidence.as_str()),
                    "d": r.depth,
                });
                let site = crate::render::site_json(repo, &r.via);
                if !site.is_null() {
                    entry["site"] = site;
                }
                entry
            }
        })
        .collect();
    let mut out = affected_json(
        node_json(&node),
        json!(unresolved),
        Some(crate::pipeline::scip_index_path(repo).is_some()),
        entries,
        by_file(reached.iter().map(|r| r.node.file.clone())),
        {
            let (d, f) = sinter_store::direct_summary(&reached);
            (reached.len(), d, f)
        },
        limit,
    );
    if reached.is_empty() {
        out["coverage"] = crate::coverage::negative_json(repo, store)?;
    }
    Ok(out)
}

fn call_tool(repo: &Path, name: &str, args: &Value) -> Result<Value> {
    // A missing/misnamed parameter must say so — an empty-string default
    // falls through to misleading downstream errors ("no searchable
    // terms") that hide the actual mistake from the calling agent.
    let symbol = |key: &str| -> Result<String> {
        args.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "missing required parameter `{key}` (got: {})",
                    args.as_object()
                        .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
                        .filter(|k| !k.is_empty())
                        .unwrap_or_else(|| "no arguments".to_string())
                )
            })
    };
    if name == "impact" {
        // compute opens the store itself; redb forbids a second in-process
        // open, so no shared handle may be alive here.
        let report = crate::impact::compute_current(repo, &symbol("rev_range")?)?;
        return Ok(crate::impact::to_json(&report));
    }
    if name == "ask" {
        // ask_json opens the store itself — same constraint as impact.
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5) as usize;
        let hits = crate::ask::ask_json_current(repo, &symbol("question")?, limit)?;
        return Ok(json!({ "hits": hits }));
    }
    if name == "overlap" {
        // impact::compute opens the store per range — same constraint.
        let ranges: Vec<String> = args
            .get("ranges")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        return overlap_json_current(repo, &ranges);
    }
    let store = &open_current(repo)?;
    match name {
        "map" => map_json(repo, store),
        "show" => {
            let node = unique_symbol(store, &symbol("symbol")?)?;
            let edge_json = |e: &sinter_core::Edge, other: &sinter_core::NodeId| {
                json!({
                    "symbol": qualified_of(other.as_str()),
                    "relation": e.relation.as_str(),
                    "evidence": e.evidence.as_str(),
                    "site": crate::render::site_json(repo, e),
                })
            };
            let out: Vec<Value> = store
                .out_edges(&node.id)?
                .iter()
                .map(|e| edge_json(e, &e.dst))
                .collect();
            let inn: Vec<Value> = store
                .in_edges(&node.id)?
                .iter()
                .map(|e| edge_json(e, &e.src))
                .collect();
            Ok(json!({
                "symbol": node_json(&node),
                "outgoing": out,
                "incoming": inn,
            }))
        }
        "query" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let (exact, nodes) = match find_symbol(store, &symbol("symbol")?)? {
                Found::Exact(nodes) => (true, nodes),
                Found::Suggestions(nodes) => (false, nodes),
            };
            Ok(json!({
                "exact": exact,
                "results": nodes.iter().take(limit).map(node_json).collect::<Vec<_>>(),
            }))
        }
        "affected" => {
            let (evidence, certain) = filter_args(args);
            let mut filter = edge_filter(&evidence, certain)?;
            filter.relations = crate::lookup::relation_set(&relations_args(args))?;
            let depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(10) as usize;
            let (limit, detail) = affected_args(args);
            let one = |sym: &str| -> Result<Value> {
                affected_one(store, repo, sym, &filter, depth, limit, detail)
            };
            // Batch: one call answers many symbols; a bad symbol becomes an
            // error entry instead of failing the whole call.
            if let Some(list) = args.get("symbols").and_then(Value::as_array) {
                let results: Vec<Value> = list
                    .iter()
                    .map(|v| {
                        let Some(sym) = v.as_str() else {
                            return json!({"symbol": v, "error": "symbols entries must be strings"});
                        };
                        one(sym)
                            .unwrap_or_else(|e| json!({"symbol": sym, "error": format!("{e:#}")}))
                    })
                    .collect();
                return Ok(json!({"results": results}));
            }
            one(&symbol("symbol")?)
        }
        "deps" => {
            let (evidence, certain) = filter_args(args);
            let mut filter = edge_filter(&evidence, certain)?;
            filter.relations = crate::lookup::relation_set(&relations_args(args))?;
            let depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(10) as usize;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            let node = unique_symbol(store, &symbol("symbol")?)?;
            let reached = store.dependencies(&node.id, &filter, depth)?;
            // Honest-empty signal: unresolved refs inside this definition
            // mean the dependency list may be incomplete.
            let unresolved = store
                .references_in(&node.file)?
                .iter()
                .filter(|r| r.enclosing.as_ref() == Some(&node.id))
                .count();
            let entries: Vec<Value> = reached
                .iter()
                .take(limit)
                .map(|r| {
                    let mut entry = json!({
                        "s": qualified_of(r.node.id.as_str()),
                        "k": r.node.kind.as_str(),
                        "f": r.node.file,
                        "e": format!("{}/{}", r.via.relation.as_str(), r.via.evidence.as_str()),
                        "d": r.depth,
                    });
                    let site = crate::render::site_json(repo, &r.via);
                    if !site.is_null() {
                        entry["site"] = site;
                    }
                    entry
                })
                .collect();
            let mut out = json!({
                "symbol": node_json(&node),
                "total": reached.len(),
                "unresolved_refs_in_symbol": unresolved,
                "by_file": by_file(reached.iter().map(|r| r.node.file.clone())),
                "dependencies": entries,
            });
            if reached.len() > limit {
                out["truncated"] = json!(reached.len() - limit);
            }
            if reached.is_empty() {
                out["coverage"] = crate::coverage::negative_json(repo, store)?;
            }
            Ok(out)
        }
        "path" => {
            let (evidence, certain) = filter_args(args);
            let mut filter = edge_filter(&evidence, certain)?;
            filter.relations = crate::lookup::relation_set(&relations_args(args))?;
            let from = unique_symbol(store, &symbol("from")?)?;
            let to = unique_symbol(store, &symbol("to")?)?;
            let path = store.shortest_path(&from.id, &to.id, &filter)?;
            let mut out = json!({
                "found": path.is_some(),
                "steps": path.iter().flatten().map(|e| json!({
                    "from": qualified_of(e.src.as_str()),
                    "to": qualified_of(e.dst.as_str()),
                    "relation": e.relation.as_str(),
                    "evidence": e.evidence.as_str(),
                    "site": crate::render::site_json(repo, e),
                })).collect::<Vec<_>>(),
            });
            if path.is_none() {
                let miss = crate::pathcmd::explain_miss(store, &from, &to, &filter)?;
                out["miss"] = crate::pathcmd::miss_json(repo, &miss);
                out["coverage"] = crate::coverage::negative_json(repo, store)?;
            }
            Ok(out)
        }
        other => anyhow::bail!("unknown tool {other}"),
    }
}

/// One-screen orientation card, mirroring `sinter map --json` (map.rs
/// keeps its helpers private to that verb): module tree with per-directory
/// symbol counts, most depended-on hubs, doc entry points.
fn map_json(repo: &Path, store: &sinter_store::Store) -> Result<Value> {
    use std::collections::BTreeMap;
    // Streamed reads, matching map.rs: materializing (and re-validating)
    // the whole graph via read_graph took seconds and gigabytes on big
    // corpora just to count in-degrees.
    let node_count = store.node_count()?;
    let edge_count = store.edge_count()?;
    let nodes: Vec<Node> = store.all_nodes()?;
    // Depth-2 directory counts; lexicographic order keeps each top-level
    // directory adjacent to its children.
    let mut tree: BTreeMap<String, usize> = BTreeMap::new();
    for node in &nodes {
        let mut parts: Vec<&str> = node.file.split('/').collect();
        parts.pop(); // file name
        let top = parts.first().copied().unwrap_or(".");
        *tree.entry(top.to_string()).or_default() += 1;
        if let Some(second) = parts.get(1) {
            *tree.entry(format!("{top}/{second}")).or_default() += 1;
        }
    }
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let degrees = store.in_degrees()?;
    let mut ranked: Vec<(&str, usize)> = degrees.iter().map(|(id, n)| (id.as_str(), *n)).collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let hubs: Vec<Value> = ranked
        .into_iter()
        .filter_map(|(id, n)| {
            by_id.get(id).map(|node| {
                json!({
                    "name": qualified_of(id),
                    "kind": node.kind.as_str(),
                    "file": node.file,
                    "line": crate::render::line_of(repo, &node.file, node.span.start),
                    "in_degree": n,
                })
            })
        })
        .take(10)
        .collect();
    // Level-1 sections of README.md and top-level docs/*.md, in doc order.
    let mut docs: BTreeMap<String, Vec<(u64, String)>> = BTreeMap::new();
    for node in &nodes {
        if node.kind != sinter_core::SymbolKind::Section {
            continue;
        }
        let file = node.file.as_str();
        let top_level_doc = file == "README.md"
            || (file.starts_with("docs/")
                && file.ends_with(".md")
                && file.matches('/').count() == 1);
        let h1 = node.signature.starts_with('#') && !node.signature.starts_with("##");
        if top_level_doc && h1 {
            docs.entry(file.to_string())
                .or_default()
                .push((node.span.start, node.name.clone()));
        }
    }
    Ok(json!({
        "repo": repo.file_name().map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".into()),
        "nodes": node_count,
        "edges": edge_count,
        "modules": tree.iter()
            .map(|(path, n)| json!({"path": path, "nodes": n}))
            .collect::<Vec<_>>(),
        "hubs": hubs,
        "docs": docs.iter().map(|(file, sections)| {
            let mut sections = sections.clone();
            sections.sort();
            json!({"file": file,
                   "sections": sections.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>()})
        }).collect::<Vec<_>>(),
    }))
}

/// Pairwise merge risk between rev ranges — `sinter overlap`'s compute,
/// with per-range summaries as counts, not full sets (agent context
/// budget).
fn overlap_json_current(repo: &Path, ranges: &[String]) -> Result<Value> {
    let (maps, pairs) = crate::overlap::compute_current(repo, ranges)?;
    Ok(json!({
        "changes": maps.iter().map(|p| json!({
            "label": p.label,
            "touched": p.touched.len(),
            "radius": p.radius.len(),
            "files": p.files.len(),
        })).collect::<Vec<_>>(),
        "pairs": serde_json::to_value(pairs)?,
    }))
}

fn tools_list() -> Value {
    let filters = json!({
        "evidence": {"type": "array", "items": {"type": "string",
            "enum": ["structural", "scope", "import", "scip", "dynamic"]},
            "description": "restrict to these evidence kinds"},
        "min_confidence": {"type": "string", "enum": ["certain", "inferred"],
            "description": "certain = compiler-grade edges only"},
        "relations": {"type": "array", "items": {"type": "string",
            "enum": ["calls", "uses", "imports", "implements", "extends"]},
            "description": "follow only these relations (e.g. drop file-level imports)"},
    });
    json!({"tools": [
        {
            "name": "map",
            "description": "One-screen orientation card for the repository: node/edge totals, the module tree with per-directory symbol counts, the most depended-on hub symbols, and doc entry points. Call this first in an unfamiliar repo.",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "ask",
            "description": "Answer a vague or conceptual codebase question (\"where is X handled\", \"how does Y work\") with ranked, content-bearing hits: signature, doc, file:line, and match provenance. Use this first when no exact symbol is known.",
            "inputSchema": {"type": "object", "properties": {
                "question": {"type": "string"},
                "limit": {"type": "integer"},
            }, "required": ["question"]},
        },
        {
            "name": "show",
            "description": "Orient on one symbol: signature, doc, file, plus every incoming and outgoing edge with relation, evidence, and call site (`site`: file:line of the reference).",
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string"},
            }, "required": ["symbol"]},
        },
        {
            "name": "query",
            "description": "Find symbols by exact name, qualified name, or fuzzy match. Results carry signature, doc comment, file, and byte span.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string"},
                "limit": {"type": "integer"},
            }, "required": ["symbol"]},
        },
        {
            "name": "affected",
            "description": "Reverse blast radius: everything transitively depending on a symbol, cross-file. Summary-first: total, by_file (top files by dependent count), then dependents capped at `limit` (default 50; `truncated` reports how many were omitted). Terse dependent keys: s=qualified symbol, k=kind, f=file, e=relation/evidence, d=depth, site=file:line of the referencing site when known. Pass detail:true for full nodes within the limit. Pass `symbols` (array) to batch many symbols in one call — response is {results:[...]}, per-symbol errors inline. A zero result carries `coverage.status=not_proven` with snapshot and graph gaps; it is never absence proof.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string"},
                "symbols": {"type": "array", "items": {"type": "string"},
                    "description": "batch: blast radius for each; overrides `symbol`"},
                "max_depth": {"type": "integer"},
                "limit": {"type": "integer",
                    "description": "max dependents returned (default 50)"},
                "detail": {"type": "boolean",
                    "description": "full node objects instead of terse entries"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
            }},
        },
        {
            "name": "deps",
            "description": "Forward blast radius: everything a symbol transitively depends on (calls, uses, imports), cross-file. Summary-first: total, by_file, then dependencies capped at `limit` (default 50; `truncated` reports how many were omitted). Terse keys: s=qualified symbol, k=kind, f=file, e=relation/evidence, d=depth, site=file:line of the referencing site when known. A zero result carries `coverage.status=not_proven` with snapshot and graph gaps; it is never absence proof.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string"},
                "max_depth": {"type": "integer"},
                "limit": {"type": "integer",
                    "description": "max dependencies returned (default 50)"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
            }, "required": ["symbol"]},
        },
        {
            "name": "path",
            "description": "Shortest dependency path from one symbol to another, with the relation, evidence, and call site (`site`: file:line) of every step. A miss carries diagnostics plus `coverage.status=not_proven`; `found:false` is never absence proof.",
            "inputSchema": {"type": "object", "properties": {
                "from": {"type": "string"},
                "to": {"type": "string"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
            }, "required": ["from", "to"]},
        },
        {
            "name": "impact",
            "description": "Changed symbols, blast radius, and affected tests for a git rev range (e.g. HEAD~1..HEAD).",
            "inputSchema": {"type": "object", "properties": {
                "rev_range": {"type": "string"},
            }, "required": ["rev_range"]},
        },
        {
            "name": "overlap",
            "description": "Rank pairwise merge risk between several in-flight changes (git rev ranges, e.g. open PRs). Tiers: direct = both touch the same symbol (textual or semantic collision); radius = one touches a symbol the other's touched code depends on (merges clean, breaks semantically); file = same file, disjoint symbols. Ranges accept `label=range` (e.g. pr-12=main...branch).",
            "inputSchema": {"type": "object", "properties": {
                "ranges": {"type": "array", "items": {"type": "string"}, "minItems": 2,
                    "description": "two or more rev-ranges, optionally labeled `label=range`"},
            }, "required": ["ranges"]},
        },
    ]})
}

/// Workspace-scope dispatch. Freshness first: every member syncs (scan-
/// floor no-op when clean) and boundary links refresh when any member
/// changed, so cross-repo answers are as current as repo-scope ones.
fn ws_call_tool(manifest: &Path, name: &str, args: &Value) -> Result<Value> {
    let symbol = |key: &str| -> Result<String> {
        args.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("missing required parameter `{key}`"))
    };
    let ws = crate::workspace::load(manifest)?;
    for repo in ws.members.values() {
        crate::pipeline::build(repo, None)?;
    }
    if !crate::workspace::stale_members(&ws)?.is_empty() {
        crate::workspace::refresh(&ws)?;
    }

    let member_node = |node: &Node, member: &str| {
        json!({
            "member": member,
            "qualified": format!("{member}:{}", qualified_of(node.id.as_str())),
            "name": node.name,
            "kind": node.kind.as_str(),
            "file": node.file,
            "signature": node.signature,
            "doc": node.doc,
        })
    };

    match name {
        "ask" => {
            // Fan candidate gathering out across members and merge-rank,
            // the same shape ask::run_workspace prints for the CLI.
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5) as usize;
            let question = symbol("question")?;
            let mut hits: Vec<Value> = Vec::new();
            for (member, repo) in &ws.members {
                for mut hit in crate::ask::ask_json(repo, &question, limit)? {
                    hit["member"] = json!(member);
                    hits.push(hit);
                }
            }
            // Deterministic: score desc, then member, file, span start.
            hits.sort_by(|a, b| {
                b["score"]
                    .as_i64()
                    .cmp(&a["score"].as_i64())
                    .then_with(|| a["member"].as_str().cmp(&b["member"].as_str()))
                    .then_with(|| a["file"].as_str().cmp(&b["file"].as_str()))
                    .then_with(|| {
                        a["span"]["start"]
                            .as_u64()
                            .cmp(&b["span"]["start"].as_u64())
                    })
            });
            hits.truncate(limit);
            Ok(json!({"hits": hits}))
        }
        "show" => {
            let (member, node) = crate::workspace::find_symbol(&ws, &symbol("symbol")?)?;
            let links = crate::workspace::LinkStore::open(&ws)?;
            let link_json = |m: &str, id: &str, l: &crate::workspace::Link| {
                json!({
                    "member": m,
                    "symbol": qualified_of(id),
                    "relation": l.relation.as_str(),
                    "evidence": l.evidence.as_str(),
                    "via": l.via,
                })
            };
            let boundary_out: Vec<Value> = links
                .out_links(&member, node.id.as_str())?
                .iter()
                .map(|l| link_json(&l.dst_member, &l.dst_id, l))
                .collect();
            let boundary_in: Vec<Value> = links
                .in_links(&member, node.id.as_str())?
                .iter()
                .map(|l| link_json(&l.src_member, &l.src_id, l))
                .collect();
            let store = sinter_store::Store::open(crate::pipeline::db_path(&ws.members[&member]))?;
            let member_root = ws.members[&member].clone();
            let edge_json = |e: &sinter_core::Edge, other: &sinter_core::NodeId| {
                json!({
                    "symbol": qualified_of(other.as_str()),
                    "relation": e.relation.as_str(),
                    "evidence": e.evidence.as_str(),
                    "site": crate::render::site_json(&member_root, e),
                })
            };
            let out: Vec<Value> = store
                .out_edges(&node.id)?
                .iter()
                .map(|e| edge_json(e, &e.dst))
                .collect();
            let inn: Vec<Value> = store
                .in_edges(&node.id)?
                .iter()
                .map(|e| edge_json(e, &e.src))
                .collect();
            Ok(json!({
                "symbol": member_node(&node, &member),
                "outgoing": out,
                "incoming": inn,
                "boundary_outgoing": boundary_out,
                "boundary_incoming": boundary_in,
            }))
        }
        "impact" => {
            // Mirrors `sinter impact --workspace` (impact::run renders for
            // humans; MCP needs the JSON): compute inside the changed
            // member, then continue the radius across boundary links.
            let member = symbol("member")?;
            let repo = ws
                .members
                .get(&member)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown member `{member}` (members: {})",
                        ws.members.keys().cloned().collect::<Vec<_>>().join(", ")
                    )
                })?
                .clone();
            let mut report = crate::impact::compute(&repo, &symbol("rev_range")?)?;
            // Resolve changed symbols to node ids first, then drop the
            // handle: workspace traversal opens every member store itself,
            // and redb forbids a second open of the same file in-process.
            let changed_ids: Vec<sinter_core::NodeId> = {
                let store = open_store(&repo)?;
                report
                    .changed_symbols
                    .iter()
                    .filter_map(|c| unique_symbol(&store, &c.qualified).ok().map(|n| n.id))
                    .collect()
            };
            let filter = sinter_store::EdgeFilter::default();
            let mut cross: std::collections::BTreeMap<String, crate::impact::SymbolRef> =
                std::collections::BTreeMap::new();
            for node_id in &changed_ids {
                for reached in crate::workspace::dependents(&ws, &member, node_id, &filter, 25)? {
                    if reached.member == member {
                        continue; // local radius already counted
                    }
                    let key = format!("{}:{}", reached.member, reached.node.id.as_str());
                    let sym = crate::impact::SymbolRef {
                        qualified: qualified_of(reached.node.id.as_str()).to_string(),
                        kind: reached.node.kind.as_str(),
                        file: format!("{}:{}", reached.member, reached.node.file),
                    };
                    if crate::impact::is_test(&reached.node) {
                        report.affected_tests.push(sym.clone());
                    }
                    cross.insert(key, sym);
                }
            }
            report.blast_radius.extend(cross.into_values());
            Ok(crate::impact::to_json(&report))
        }
        "query" => {
            let (member, node) = crate::workspace::find_symbol(&ws, &symbol("symbol")?)?;
            Ok(json!({"result": member_node(&node, &member)}))
        }
        "affected" => {
            let (evidence, certain) = filter_args(args);
            let mut filter = edge_filter(&evidence, certain)?;
            filter.relations = crate::lookup::relation_set(&relations_args(args))?;
            let depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(10) as usize;
            let (limit, detail) = affected_args(args);
            let (member, node) = crate::workspace::find_symbol(&ws, &symbol("symbol")?)?;
            let reached = crate::workspace::dependents(&ws, &member, &node.id, &filter, depth)?;
            // Honest-empty signal from the origin member's store.
            let store = sinter_store::Store::open(crate::pipeline::db_path(&ws.members[&member]))?;
            let unresolved = store.unresolved_named(&node.name)?;
            let entries: Vec<Value> = reached
                .iter()
                .take(limit)
                .map(|r| {
                    if detail {
                        json!({
                            "node": member_node(&r.node, &r.member),
                            "relation": r.relation.as_str(),
                            "evidence": r.evidence.as_str(),
                            "parent": format!("{}:{}", r.parent.0, qualified_of(&r.parent.1)),
                        })
                    } else {
                        json!({
                            "s": format!("{}:{}", r.member, qualified_of(r.node.id.as_str())),
                            "k": r.node.kind.as_str(),
                            "f": r.node.file,
                            "e": format!("{}/{}", r.relation.as_str(), r.evidence.as_str()),
                            "p": format!("{}:{}", r.parent.0, qualified_of(&r.parent.1)),
                        })
                    }
                })
                .collect();
            let direct: Vec<_> = reached
                .iter()
                .filter(|r| r.parent.0 == member && r.parent.1 == node.id.as_str())
                .collect();
            let direct_files = direct
                .iter()
                .map(|r| (r.member.as_str(), r.node.file.as_str()))
                .collect::<std::collections::HashSet<_>>()
                .len();
            Ok(affected_json(
                member_node(&node, &member),
                json!(unresolved),
                None,
                entries,
                by_file(reached.iter().map(|r| r.node.file.clone())),
                (reached.len(), direct.len(), direct_files),
                limit,
            ))
        }
        "path" => {
            let (from_member, from) = crate::workspace::find_symbol(&ws, &symbol("from")?)?;
            let (to_member, to) = crate::workspace::find_symbol(&ws, &symbol("to")?)?;
            let (evidence, certain) = filter_args(args);
            let mut filter = edge_filter(&evidence, certain)?;
            filter.relations = crate::lookup::relation_set(&relations_args(args))?;
            let steps = crate::workspace::shortest_path(
                &ws,
                (&from_member, &from.id),
                (&to_member, &to.id),
                &filter,
            )?;
            Ok(json!({
                "found": steps.is_some(),
                "steps": steps.unwrap_or_default().iter().map(
                    |(fm, fid, rel, evid, tm, tid)| json!({
                        "from": format!("{fm}:{}", qualified_of(fid)),
                        "to": format!("{tm}:{}", qualified_of(tid)),
                        "relation": rel.as_str(),
                        "evidence": evid.as_str(),
                    })
                ).collect::<Vec<_>>(),
            }))
        }
        other => {
            anyhow::bail!(
                "unknown tool {other} (workspace scope serves: ask, show, query, affected, path, impact)"
            )
        }
    }
}

fn ws_tools_list() -> Value {
    let filters = json!({
        "evidence": {"type": "array", "items": {"type": "string",
            "enum": ["structural", "scope", "import", "scip", "dynamic"]},
            "description": "restrict to these evidence kinds"},
        "min_confidence": {"type": "string", "enum": ["certain", "inferred"],
            "description": "certain = compiler-grade edges only"},
        "relations": {"type": "array", "items": {"type": "string",
            "enum": ["calls", "uses", "imports", "implements", "extends"]},
            "description": "follow only these relations (e.g. drop file-level imports)"},
    });
    let addressing = "Symbols accept `member:Symbol` (member from the workspace manifest) or any bare name that resolves uniquely across members.";
    json!({"tools": [
        {
            "name": "ask",
            "description": "Answer a vague or conceptual question (\"where is X handled\", \"how does Y work\") across every workspace member: candidates gathered per member, merge-ranked, each hit tagged with its member. Use this first when no exact symbol is known.",
            "inputSchema": {"type": "object", "properties": {
                "question": {"type": "string"},
                "limit": {"type": "integer"},
            }, "required": ["question"]},
        },
        {
            "name": "show",
            "description": format!("Orient on one symbol: signature, doc, file, every incoming and outgoing edge inside its member (with relation, evidence, and call site), plus boundary links into and out of the other members. {addressing}"),
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string"},
            }, "required": ["symbol"]},
        },
        {
            "name": "query",
            "description": format!("Resolve a symbol across every workspace member. {addressing}"),
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string"},
            }, "required": ["symbol"]},
        },
        {
            "name": "affected",
            "description": format!("Cross-repository blast radius: everything transitively depending on a symbol across all workspace members, boundary links included. Summary-first: total, by_file, then dependents capped at `limit` (default 50; `truncated` reports omissions). Terse dependent keys: s=member:qualified symbol, k=kind, f=file, e=relation/evidence, p=parent. Pass detail:true for full nodes within the limit. unresolved_refs_matching_name > 0 means the list may be incomplete. {addressing}"),
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string"},
                "max_depth": {"type": "integer"},
                "limit": {"type": "integer",
                    "description": "max dependents returned (default 50)"},
                "detail": {"type": "boolean",
                    "description": "full node objects instead of terse entries"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
            }, "required": ["symbol"]},
        },
        {
            "name": "path",
            "description": format!("Shortest dependency path between two symbols, crossing repository boundaries through import and declared links. {addressing}"),
            "inputSchema": {"type": "object", "properties": {
                "from": {"type": "string"},
                "to": {"type": "string"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
                "relations": filters["relations"],
            }, "required": ["from", "to"]},
        },
        {
            "name": "impact",
            "description": "Changed symbols, blast radius, and affected tests for a git rev range (e.g. HEAD~1..HEAD) in one member, with the radius continued across boundary links into the other members (cross-member entries carry a `member:` file prefix).",
            "inputSchema": {"type": "object", "properties": {
                "member": {"type": "string",
                    "description": "workspace member the rev range applies to"},
                "rev_range": {"type": "string"},
            }, "required": ["member", "rev_range"]},
        },
    ]})
}
