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
use sinter_store::Store;

use crate::lookup::{Found, edge_filter, find_symbol, open_store, unique_symbol};

pub fn run(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
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
        let response = match handle(&store, &repo, method, &params) {
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

fn handle(store: &Store, repo: &Path, method: &str, params: &Value) -> Result<Value> {
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
            let result = call_tool(store, repo, name, &args)?;
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

fn call_tool(store: &Store, repo: &Path, name: &str, args: &Value) -> Result<Value> {
    let symbol = |key: &str| -> String {
        args.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    match name {
        "query" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let (exact, nodes) = match find_symbol(store, &symbol("symbol"))? {
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
            let node = unique_symbol(store, &symbol("symbol"))?;
            let reached = store.dependents(&node.id, &filter, depth)?;
            Ok(json!({
                "symbol": node_json(&node),
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
            let from = unique_symbol(store, &symbol("from"))?;
            let to = unique_symbol(store, &symbol("to"))?;
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
        "impact" => {
            let report = crate::impact::compute(repo, &symbol("rev_range"))?;
            Ok(crate::impact::to_json(&report))
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
            "name": "query",
            "description": "Find symbols by exact name, qualified name, or fuzzy match. Results carry signature, doc comment, file, and byte span.",
            "inputSchema": {"type": "object", "properties": {
                "symbol": {"type": "string"},
                "limit": {"type": "integer"},
            }, "required": ["symbol"]},
        },
        {
            "name": "affected",
            "description": "Reverse blast radius: everything transitively depending on a symbol, cross-file. Each edge reports its evidence.",
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
