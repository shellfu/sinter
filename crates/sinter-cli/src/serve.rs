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

use crate::lookup::{Found, edge_filter, find_symbol, open_store, unique_symbol};

/// One server owns one scope (D28): a repository, or a whole workspace.
enum Scope {
    Repo(PathBuf),
    Workspace(PathBuf),
}

pub fn run(repo: &Path) -> Result<()> {
    serve(Scope::Repo(repo.canonicalize()?))
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
            Err(e) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32000, "message": format!("{e:#}")}
            }),
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
            Scope::Repo(_) => tools_list(),
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
                Scope::Repo(repo) => call_tool(repo, name, &args)?,
                Scope::Workspace(manifest) => ws_call_tool(manifest, name, &args)?,
            };
            Ok(json!({
                "content": [{"type": "text", "text": serde_json::to_string_pretty(&result)?}]
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
        let report = crate::impact::compute(repo, &symbol("rev_range")?)?;
        return Ok(crate::impact::to_json(&report));
    }
    if name == "ask" {
        // ask_json opens the store itself — same constraint as impact.
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5) as usize;
        let hits = crate::ask::ask_json(repo, &symbol("question")?, limit)?;
        return Ok(json!({ "hits": hits }));
    }
    let store = &open_store(repo)?;
    match name {
        "show" => {
            let node = unique_symbol(store, &symbol("symbol")?)?;
            let edge_json = |e: &sinter_core::Edge, other: &sinter_core::NodeId| {
                json!({
                    "symbol": qualified_of(other.as_str()),
                    "relation": e.relation.as_str(),
                    "evidence": e.evidence.as_str(),
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
            let filter = edge_filter(&evidence, certain)?;
            let depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(10) as usize;
            let node = match unique_symbol(store, &symbol("symbol")?) {
                Ok(node) => node,
                Err(e) => {
                    let sites = crate::lookup::external_sites(store, &symbol("symbol")?)?;
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
            let reached = store.dependents(&node.id, &filter, depth)?;
            // Honest-empty signal: unresolved refs sharing the name mean
            // the dependents list may be incomplete, never authoritative.
            let unresolved = store.unresolved_named(&node.name)?;
            Ok(json!({
                "symbol": node_json(&node),
                "unresolved_refs_matching_name": unresolved,
                "scip_evidence_available": crate::pipeline::scip_index_path(repo).is_some(),
                "dependents": reached.iter().map(|r| json!({
                    "node": node_json(&r.node),
                    "depth": r.depth,
                    "relation": r.via.relation.as_str(),
                    "evidence": r.via.evidence.as_str(),
                })).collect::<Vec<_>>(),
            }))
        }
        "path" => {
            let (evidence, certain) = filter_args(args);
            let filter = edge_filter(&evidence, certain)?;
            let from = unique_symbol(store, &symbol("from")?)?;
            let to = unique_symbol(store, &symbol("to")?)?;
            let path = store.shortest_path(&from.id, &to.id, &filter)?;
            Ok(json!({
                "found": path.is_some(),
                "steps": path.unwrap_or_default().iter().map(|e| json!({
                    "from": qualified_of(e.src.as_str()),
                    "to": qualified_of(e.dst.as_str()),
                    "relation": e.relation.as_str(),
                    "evidence": e.evidence.as_str(),
                })).collect::<Vec<_>>(),
            }))
        }
        other => anyhow::bail!("unknown tool {other}"),
    }
}

fn tools_list() -> Value {
    let filters = json!({
        "evidence": {"type": "array", "items": {"type": "string",
            "enum": ["structural", "scope", "import", "scip"]},
            "description": "restrict to these evidence kinds"},
        "min_confidence": {"type": "string", "enum": ["certain", "inferred"],
            "description": "certain = compiler-grade edges only"},
    });
    json!({"tools": [
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
            "description": "Orient on one symbol: signature, doc, file, plus every incoming and outgoing edge with relation and evidence.",
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
            "description": "Reverse blast radius: everything transitively depending on a symbol, cross-file. Each edge reports its evidence. unresolved_refs_matching_name > 0 means the list may be incomplete — refine, or run `sinter scip`.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string"},
                "max_depth": {"type": "integer"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
            }, "required": ["symbol"]},
        },
        {
            "name": "path",
            "description": "Shortest dependency path from one symbol to another, with the relation and evidence of every step.",
            "inputSchema": {"type": "object", "properties": {
                "from": {"type": "string"},
                "to": {"type": "string"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
            }, "required": ["from", "to"]},
        },
        {
            "name": "impact",
            "description": "Changed symbols, blast radius, and affected tests for a git rev range (e.g. HEAD~1..HEAD).",
            "inputSchema": {"type": "object", "properties": {
                "rev_range": {"type": "string"},
            }, "required": ["rev_range"]},
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
        "query" => {
            let (member, node) = crate::workspace::find_symbol(&ws, &symbol("symbol")?)?;
            Ok(json!({"result": member_node(&node, &member)}))
        }
        "affected" => {
            let (evidence, certain) = filter_args(args);
            let filter = edge_filter(&evidence, certain)?;
            let depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(10) as usize;
            let (member, node) = crate::workspace::find_symbol(&ws, &symbol("symbol")?)?;
            let reached = crate::workspace::dependents(&ws, &member, &node.id, &filter, depth)?;
            // Honest-empty signal from the origin member's store.
            let store = sinter_store::Store::open(crate::pipeline::db_path(&ws.members[&member]))?;
            let unresolved = store.unresolved_named(&node.name)?;
            Ok(json!({
                "symbol": member_node(&node, &member),
                "unresolved_refs_matching_name": unresolved,
                "dependents": reached.iter().map(|r| json!({
                    "node": member_node(&r.node, &r.member),
                    "relation": r.relation.as_str(),
                    "evidence": r.evidence.as_str(),
                    "parent": format!("{}:{}", r.parent.0, qualified_of(&r.parent.1)),
                })).collect::<Vec<_>>(),
            }))
        }
        "path" => {
            let (from_member, from) = crate::workspace::find_symbol(&ws, &symbol("from")?)?;
            let (to_member, to) = crate::workspace::find_symbol(&ws, &symbol("to")?)?;
            let (evidence, certain) = filter_args(args);
            let filter = edge_filter(&evidence, certain)?;
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
            anyhow::bail!("unknown tool {other} (workspace scope serves: query, affected, path)")
        }
    }
}

fn ws_tools_list() -> Value {
    let filters = json!({
        "evidence": {"type": "array", "items": {"type": "string",
            "enum": ["structural", "scope", "import", "scip"]},
            "description": "restrict to these evidence kinds"},
        "min_confidence": {"type": "string", "enum": ["certain", "inferred"],
            "description": "certain = compiler-grade edges only"},
    });
    let addressing = "Symbols accept `member:Symbol` (member from the workspace manifest) or any bare name that resolves uniquely across members.";
    json!({"tools": [
        {
            "name": "query",
            "description": format!("Resolve a symbol across every workspace member. {addressing}"),
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string"},
            }, "required": ["symbol"]},
        },
        {
            "name": "affected",
            "description": format!("Cross-repository blast radius: everything transitively depending on a symbol across all workspace members, boundary links included. Each edge reports its evidence; unresolved_refs_matching_name > 0 means the list may be incomplete. {addressing}"),
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string"},
                "max_depth": {"type": "integer"},
                "evidence": filters["evidence"],
                "min_confidence": filters["min_confidence"],
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
            }, "required": ["from", "to"]},
        },
    ]})
}
