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

/// Exit when the agent session that started us is gone.
///
/// A stdio server should end at EOF, but the write end of its stdin is
/// inherited by anything the client forked, so a dead client does not
/// always close it. Orphaned servers then live for days, holding graph
/// handles and answering with whatever binary version they were started
/// with. Two cheap belts: the kernel signals us on parent death (Linux),
/// and a 1s poll catches every reparenting the signal misses (parent
/// thread exiting without the process, a subreaper adopting us).
/// Non-unix targets keep the EOF behavior only.
#[cfg(unix)]
fn exit_when_orphaned() {
    #[cfg(target_os = "linux")]
    // SAFETY: prctl with PR_SET_PDEATHSIG takes a signal number by value.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }
    // SAFETY: getppid is always safe.
    let parent = unsafe { libc::getppid() };
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if unsafe { libc::getppid() } != parent {
                std::process::exit(0);
            }
        }
    });
}

#[cfg(not(unix))]
fn exit_when_orphaned() {}

fn serve(scope: Scope) -> Result<()> {
    // Diagnostics ride inside every envelope; stderr stays silent on a
    // stdio transport.
    crate::agent_protocol::set_json_mode();
    exit_when_orphaned();
    sinter_store::quiet_notices();
    // An interactive transport must answer, not hang: if another process
    // holds the graph (a long rebuild), say so and let the agent retry.
    sinter_store::set_open_budget_secs(15);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    // The repository-wide coverage half already sent in this session. One
    // process serves many calls against one graph state; repeating it is
    // the largest fixed cost in a small answer.
    let mut coverage_ref: Option<String> = None;
    for line in stdin.lock().lines() {
        // A transport that cannot be read from is a finished session, not
        // an error to report into a pipe nobody is holding: end the loop.
        let Ok(line) = line else { break };
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
        let response = match handle(&scope, method, &params, &mut coverage_ref) {
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
    let mut data = crate::agent_protocol::mcp_failure_document(operation, error);
    data["error"]["message"] = json!(message);
    // Anything the caller can fix or retry (a miss, an ambiguity, a moved
    // handle, a bad argument, a failed run) is a tool outcome, delivered in
    // `result` with `isError` so clients keep `code` and `candidates`.
    // Only protocol faults (unknown method or tool, arguments that are not
    // an object) stay JSON-RPC errors.
    let protocol_fault = method != "tools/call"
        || data["error"]["code"] == "unknown_operation"
        || !params
            .get("arguments")
            .is_none_or(|arguments| arguments.is_object() || arguments.is_null());
    if protocol_fault {
        return json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32000, "message": message, "data": data}
        });
    }
    let subject = params.pointer("/arguments/symbol").and_then(Value::as_str);
    let result = crate::agent_protocol::mcp_failure(subject, data);
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn handle(
    scope: &Scope,
    method: &str,
    params: &Value,
    coverage_ref: &mut Option<String>,
) -> Result<Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05"),
            "capabilities": {"tools": {}, "resources": {}},
            "serverInfo": {"name": "sinter", "version": env!("CARGO_PKG_VERSION")},
            "instructions": format!(
                "sinter answers code-structure questions from a dependency graph. \
                 Read `structuredContent` (content text is a summary only) and check \
                 `outcome.status` before acting. Key legend, coverage semantics, \
                 batching, and budget paging: see resource {}",
                crate::tool_catalog::GUIDE_URI
            ),
        })),
        "ping" => Ok(json!({})),
        "resources/list" => {
            let mut resources = vec![json!({
                "uri": crate::tool_catalog::GUIDE_URI,
                "name": "guide",
                "description": "How to read sinter results: key legend, coverage, batching, paging",
                "mimeType": "text/markdown",
            })];
            if matches!(scope, Scope::Repo { .. }) {
                resources.push(json!({
                    "uri": crate::coverage::COVERAGE_URI,
                    "name": "coverage",
                    "description":
                        "Repository-wide coverage: what a collapsed `coverage.ref` in a tool result names",
                    "mimeType": "application/json",
                }));
            }
            Ok(json!({"resources": resources}))
        }
        "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
        "resources/read" => {
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
            match (uri, scope) {
                (crate::tool_catalog::GUIDE_URI, _) => Ok(json!({"contents": [{
                    "uri": uri,
                    "mimeType": "text/markdown",
                    "text": crate::tool_catalog::GUIDE,
                }]})),
                (crate::coverage::COVERAGE_URI, Scope::Repo { repo, freshness }) => {
                    freshness.sync()?;
                    let store = crate::lookup::open_current(repo)?;
                    let coverage = crate::coverage::shared_document(repo, &store)?;
                    Ok(json!({"contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": serde_json::to_string(&coverage)?,
                    }]}))
                }
                _ => anyhow::bail!("unknown resource {uri}"),
            }
        }
        "tools/list" => Ok(match scope {
            Scope::Repo { .. } => crate::tool_catalog::repository(),
            Scope::Workspace(_) => crate::tool_catalog::workspace(),
        }),
        "tools/call" => call_tool(scope, params, coverage_ref),
        other => anyhow::bail!("unknown method {other}"),
    }
}

fn call_tool(scope: &Scope, params: &Value, coverage_ref: &mut Option<String>) -> Result<Value> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let mut args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    crate::agent_protocol::validate_arguments(name, &args, matches!(scope, Scope::Workspace(_)))?;
    let budget = crate::agent_protocol::take_budget(&mut args);

    // A session-lived store would hold redb's exclusive lock across calls,
    // blocking graph refresh and pinning a stale snapshot.
    let mut result = match scope {
        Scope::Repo { repo, freshness } => {
            freshness.sync()?;
            let mut result = crate::repository_tools::call(repo, name, &args)?;
            stamp_symbol_lines(&mut result, repo);
            result
        }
        Scope::Workspace(manifest) => crate::workspace_tools::call(manifest, name, &args)?,
    };
    if args.get("include_coverage").and_then(Value::as_bool) == Some(true) {
        let carried = crate::coverage::collapse_repeated(&mut result, coverage_ref.as_deref());
        if carried.is_some() {
            *coverage_ref = carried;
        }
    }
    crate::agent_protocol::mcp_success(name, &result, budget, &args)
}

/// Add `line` to the `symbol` echo (and each batched entry's), read from
/// the file's current content: the MCP trim keeps the echo to what an
/// agent can act on, and a line is what it opens.
fn stamp_symbol_lines(result: &mut Value, repo: &Path) {
    let stamp = |echo: &mut Value| {
        let (Some(file), Some(start)) = (
            echo["file"].as_str().map(str::to_owned),
            echo["span"]["start"].as_u64(),
        ) else {
            return;
        };
        if let Some(line) = crate::render::line_of(repo, &file, start) {
            echo["line"] = json!(line);
        }
    };
    if result.get("symbol").is_some_and(Value::is_object) {
        stamp(&mut result["symbol"]);
    }
    for entry in result
        .get_mut("results")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if entry.get("symbol").is_some_and(Value::is_object) {
            stamp(&mut entry["symbol"]);
        }
    }
}
