//! `sinter serve`: MCP transport and session orchestration over stdio.
//!
//! The server owns newline-delimited JSON-RPC framing, request routing, and
//! repository freshness. Tool contracts and execution live in their semantic
//! modules; the hand-rolled protocol subset remains intentionally small.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

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
            continue;
        };
        let response = match handle(&scope, method, &params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => error_response(id, method, &params, &error),
        };
        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }
    Ok(())
}

fn error_response(id: Value, method: &str, params: &Value, error: &anyhow::Error) -> Value {
    let mut message = format!("{error:#}");
    if message.contains("no symbol matches") {
        if let Some(position) = message.find(" — try `sinter ask") {
            message.truncate(position);
        }
        message.push_str(" — try the ask tool for concept search");
    }
    let operation = if method == "tools/call" {
        params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    } else {
        method
    };
    let mut data = crate::agent_protocol::failure(operation, error);
    data["error"]["message"] = json!(message);
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32000, "message": message, "data": data}
    })
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
        "tools/list" => Ok(match scope {
            Scope::Repo { .. } => crate::tool_catalog::repository(),
            Scope::Workspace(_) => crate::tool_catalog::workspace(),
        }),
        "tools/call" => call_tool(scope, params),
        other => anyhow::bail!("unknown method {other}"),
    }
}

fn call_tool(scope: &Scope, params: &Value) -> Result<Value> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    crate::agent_protocol::validate_arguments(name, &args, matches!(scope, Scope::Workspace(_)))?;

    // A session-lived store would hold redb's exclusive lock across calls,
    // blocking graph refresh and pinning a stale snapshot.
    let result = match scope {
        Scope::Repo { repo, freshness } => {
            freshness.sync()?;
            crate::repository_tools::call(repo, name, &args)?
        }
        Scope::Workspace(manifest) => crate::workspace_tools::call(manifest, name, &args)?,
    };
    crate::agent_protocol::mcp_success(name, &result)
}
