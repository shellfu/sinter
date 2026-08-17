//! `sinter serve`: MCP server over stdio (newline-delimited JSON-RPC).
//! Hand-rolled: the protocol subset needed (initialize, tools/list,
//! tools/call, ping) is ~all of it; an SDK dependency buys nothing yet.
//! Every edge-walking tool takes evidence/confidence filters.

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::Node;
use sinter_resolve::qualified_of;

use crate::lookup::{Found, edge_filter, find_symbol, open_store, unique_symbol};

pub fn run(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;
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
        let response = match handle(&repo, method, &params) {
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

fn handle(repo: &Path, method: &str, params: &Value) -> Result<Value> {
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
        "tools/list" => Ok(tools_list()),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            // No session-lived handle: redb's lock is exclusive, so holding
            // the store across calls would block `sinter build`/`watch` and
            // pin a stale snapshot. Freshness itself is enforced inside
            // open_store, which every tool path goes through.
            let result = call_tool(repo, name, &args)?;
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
