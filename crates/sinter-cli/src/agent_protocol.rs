//! Versioned machine contract shared by CLI JSON payloads and MCP.
//!
//! CLI `--json` writes the value stored in an MCP result's `data` field.
//! MCP adds the small envelope because `structuredContent` must be an object
//! and needs to carry outcome/error metadata independently of a tool's data
//! shape (notably `ask`, whose compatibility payload is an array).

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

pub const VERSION: &str = "sinter.agent.v1";

/// Compact CLI JSON. Human-oriented rendering remains the non-JSON path.
pub fn write_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

/// MCP's legacy text body remains the bare data value for older clients;
/// agents with structured-content support receive the versioned contract.
pub fn mcp_success(operation: &str, legacy_payload: &Value) -> Result<Value> {
    let data = protocol_data(operation, legacy_payload);
    Ok(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(legacy_payload)?,
        }],
        "structuredContent": success(operation, data),
        "isError": false,
    }))
}

/// Convert an execution failure into stable machine data. JSON-RPC callers
/// receive this under `error.data`; CLI `--json` writes the same object.
pub fn failure(operation: &str, error: &anyhow::Error) -> Value {
    let message = format!("{error:#}");
    let lookup = error.downcast_ref::<crate::lookup::SymbolLookupError>();
    let code = if let Some(error) = lookup {
        error.code()
    } else if error.is::<crate::lookup::NoMatch>() {
        "no_match"
    } else if message.contains(" is ambiguous") {
        "ambiguous_symbol"
    } else if message.contains("missing required parameter")
        || message.contains("unknown argument")
        || message.contains("must be")
    {
        "invalid_arguments"
    } else if message.contains("unknown tool") {
        "unknown_operation"
    } else {
        "execution_error"
    };
    let candidates = if let Some(error) = lookup {
        error
            .candidates()
            .iter()
            .map(crate::render::node_json)
            .collect::<Vec<_>>()
    } else {
        message
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(Value::from)
            .collect()
    };
    let mut failure = json!({
        "protocol": VERSION,
        "operation": operation,
        "outcome": {
            "status": match code {
                "no_match" => "not_found",
                "relocated_handle" => "relocated",
                "stale_snapshot" => "stale",
                _ => "error",
            },
            "partial": false,
        },
        "error": {
            "code": code,
            "message": message,
            "retryable": code == "stale_snapshot",
            "candidates": candidates,
        },
    });
    if let Some((expected, actual)) = lookup.and_then(|error| error.snapshots()) {
        failure["error"]["expected_snapshot"] = json!(expected);
        failure["error"]["actual_snapshot"] = json!(actual);
    }
    failure
}

/// Reject keys outside the advertised closed input schema. This is kept
/// beside the schema contract so transport declarations and runtime
/// enforcement cannot drift independently.
pub fn validate_arguments(operation: &str, args: &Value, workspace: bool) -> Result<()> {
    let Some(object) = args.as_object() else {
        bail!("arguments for `{operation}` must be a JSON object");
    };
    let allowed: &[&str] = match (workspace, operation) {
        (_, "ask") => &["question", "limit", "scope"],
        (_, "show") => &["symbol", "if_snapshot"],
        (_, "query") => &["symbol", "limit", "scope", "if_snapshot"],
        (false, "affected") => &[
            "symbol",
            "symbols",
            "max_depth",
            "limit",
            "detail",
            "evidence",
            "min_confidence",
            "relations",
            "scope",
            "if_snapshot",
        ],
        (true, "affected") => &[
            "symbol",
            "max_depth",
            "limit",
            "detail",
            "evidence",
            "min_confidence",
            "relations",
            "scope",
            "if_snapshot",
        ],
        (false, "deps") => &[
            "symbol",
            "max_depth",
            "limit",
            "evidence",
            "min_confidence",
            "relations",
            "scope",
            "if_snapshot",
        ],
        (_, "path") => &[
            "from",
            "to",
            "evidence",
            "min_confidence",
            "relations",
            "scope",
            "if_snapshot",
        ],
        (false, "unresolved") => &["file", "name", "limit"],
        (true, "unresolved") => &["member", "file", "name", "limit"],
        (false, "impact") => &["rev_range"],
        (true, "impact") => &["member", "rev_range"],
        (false, "overlap") => &["ranges"],
        (false, "map") => &["scope"],
        _ => bail!("unknown tool `{operation}` for this server scope"),
    };
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        bail!("unknown argument `{key}` for `{operation}`");
    }
    Ok(())
}

/// Close every input schema and attach an operation-specific output schema.
pub fn complete_tool_schemas(list: &mut Value) {
    for tool in list["tools"].as_array_mut().into_iter().flatten() {
        let Some(name) = tool["name"].as_str().map(str::to_owned) else {
            continue;
        };
        if let Some(input) = tool.get_mut("inputSchema").and_then(Value::as_object_mut) {
            input.insert("additionalProperties".to_string(), Value::Bool(false));
        }
        tool["outputSchema"] = output_schema(&name);
        tool["annotations"] = json!({
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        });
    }
}

fn protocol_data(_operation: &str, payload: &Value) -> Value {
    payload.clone()
}

fn success(operation: &str, data: Value) -> Value {
    let partial = is_partial(&data);
    let found = is_found(operation, &data);
    json!({
        "protocol": VERSION,
        "operation": operation,
        "outcome": {
            "status": if !found { "not_found" } else if partial { "partial" } else { "complete" },
            "partial": partial,
        },
        "data": data,
    })
}

fn is_found(operation: &str, data: &Value) -> bool {
    match operation {
        "ask" => data.get("returned").and_then(Value::as_u64).unwrap_or(0) > 0,
        "query" => data
            .get("results")
            .and_then(Value::as_array)
            .is_none_or(|results| !results.is_empty()),
        "affected" => {
            data.get("external").and_then(Value::as_bool) == Some(true)
                || data.get("total").and_then(Value::as_u64).unwrap_or(0) > 0
                || data
                    .get("results")
                    .and_then(Value::as_array)
                    .is_some_and(|results| {
                        results.iter().any(|result| {
                            result.get("total").and_then(Value::as_u64).unwrap_or(0) > 0
                                || result.get("external").and_then(Value::as_bool) == Some(true)
                        })
                    })
        }
        "deps" | "unresolved" => data.get("total").and_then(Value::as_u64).unwrap_or(0) > 0,
        "path" => data.get("found").and_then(Value::as_bool).unwrap_or(false),
        _ => true,
    }
}

fn is_partial(data: &Value) -> bool {
    data.get("analysis_status").and_then(Value::as_str) == Some("partial")
        || data.get("verify_required").and_then(Value::as_bool) == Some(true)
        || data.get("coverage").is_some()
        || data
            .get("unresolved_refs_matching_name")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
        || data
            .get("unresolved_refs_in_symbol")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
        || data
            .get("results")
            .and_then(Value::as_array)
            .is_some_and(|results| results.iter().any(|result| result.get("error").is_some()))
}

fn output_schema(operation: &str) -> Value {
    let data = data_schema(operation);
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("https://sinter.dev/schema/agent/v1/{operation}.json"),
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "protocol": {"const": VERSION},
            "operation": {"const": operation},
            "outcome": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "status": {"enum": ["complete", "partial", "not_found"]},
                    "partial": {"type": "boolean"}
                },
                "required": ["status", "partial"]
            },
            "data": data,
        },
        "required": ["protocol", "operation", "outcome", "data"]
    })
}

fn data_schema(operation: &str) -> Value {
    if operation == "ask" {
        return json!({
            "type": "object",
            "properties": {
                "question": {"type": "string"},
                "limit": {"type": "integer"},
                "returned": {"type": "integer"},
                "truncated": {"type": "integer"},
                "decision": {"enum": ["answer", "verify", "abstain"]},
                "verify_required": {"type": "boolean"},
                "topics": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "topic": {"type": "string"},
                            "status": {"enum": ["ranked", "abstain"]},
                            "verify_required": {"type": "boolean"},
                            "confidence": {
                                "type": "object",
                                "properties": {
                                    "level": {"enum": ["high", "medium", "low", "unrated"]},
                                    "reason": {"type": "string"},
                                    "calibration": {"type": "object"}
                                },
                                "required": ["level", "reason", "calibration"]
                            },
                            "ranking_margin": {"type": "object"},
                            "term_coverage": {"type": "object"},
                            "advice": {"type": ["string", "null"]},
                            "hits": {"type": "array"}
                        },
                        "required": ["topic", "status", "verify_required", "confidence", "ranking_margin", "term_coverage", "advice", "hits"]
                    }
                },
            },
            "required": ["question", "limit", "scope", "returned", "truncated", "decision", "verify_required", "topics"]
        });
    }
    if operation == "affected" {
        return json!({
            "type": "object",
            "anyOf": [
                {"required": ["symbol", "snapshot", "total", "dependents", "coverage"]},
                {"required": ["external", "snapshot", "sites", "coverage"]},
                {"required": ["results", "snapshot"]}
            ]
        });
    }
    if operation == "query" {
        return json!({
            "type": "object",
            "properties": {
                "snapshot": {"type": "string"},
                "scope": {"type": "array", "items": {"type": "string"}},
                "resolution": {"enum": ["exact", "relocated", "suggestions"]},
                "exact": {"type": "boolean"},
                "results": {"type": "array"},
                "result": {"type": "object"}
            },
            "anyOf": [
                {"required": ["snapshot", "scope", "resolution", "exact", "results"]},
                {"required": ["snapshot", "scope", "result"]}
            ]
        });
    }
    let required: &[&str] = match operation {
        "map" => &[
            "scope",
            "nodes",
            "total_nodes",
            "edges",
            "modules",
            "hubs",
            "docs",
        ],
        "show" => &["symbol", "outgoing", "incoming"],
        "deps" => &["symbol", "snapshot", "total", "dependencies", "coverage"],
        "path" => &["snapshot", "found", "steps", "coverage"],
        "unresolved" => &["total", "unresolved"],
        "impact" => &[
            "analysis_status",
            "changed_files",
            "changed_symbols",
            "blast_radius",
        ],
        "overlap" => &["changes", "pairs"],
        _ => &[],
    };
    let properties: Map<String, Value> = required
        .iter()
        .map(|name| ((*name).to_string(), json!({})))
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use serde_json::json;

    use crate::lookup::SymbolLookupError;

    use super::{VERSION, complete_tool_schemas, failure, mcp_success, validate_arguments};

    #[test]
    fn mcp_envelope_data_is_the_cli_payload() {
        let cli = json!({"exact": true, "results": [{"name": "run"}]});
        let result = mcp_success("query", &cli).unwrap();
        assert_eq!(result["structuredContent"]["protocol"], VERSION);
        assert_eq!(result["structuredContent"]["data"], cli);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                result["content"][0]["text"].as_str().unwrap()
            )
            .unwrap(),
            cli
        );
    }

    #[test]
    fn ask_envelope_uses_the_complete_cli_topic_payload() {
        let payload = json!({
            "question": "where is run",
            "limit": 5,
            "returned": 1,
            "truncated": 0,
            "decision": "verify",
            "verify_required": true,
            "topics": [{"topic": "run", "hits": [{"qualified": "run"}]}]
        });
        let result = mcp_success("ask", &payload).unwrap();
        assert_eq!(result["structuredContent"]["data"], payload);
    }

    #[test]
    fn schemas_are_closed_and_have_versioned_outputs() {
        let mut list = json!({"tools": [{
            "name": "query",
            "inputSchema": {"type": "object", "properties": {"symbol": {"type": "string"}}}
        }]});
        complete_tool_schemas(&mut list);
        let tool = &list["tools"][0];
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            tool["outputSchema"]["properties"]["protocol"]["const"],
            VERSION
        );
        assert_eq!(tool["outputSchema"]["additionalProperties"], false);
    }

    #[test]
    fn closed_schema_is_enforced_at_runtime() {
        let error = validate_arguments("show", &json!({"symbol": "run", "guess": true}), false)
            .unwrap_err();
        assert!(error.to_string().contains("unknown argument `guess`"));
    }

    #[test]
    fn ambiguity_is_machine_classifiable() {
        let value = failure("show", &anyhow!("`run` is ambiguous\nrun@a.rs\nrun@b.rs"));
        assert_eq!(value["error"]["code"], "ambiguous_symbol");
        assert_eq!(value["error"]["candidates"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn stale_snapshot_is_typed_and_retryable() {
        let value = failure(
            "show",
            &SymbolLookupError::StaleSnapshot {
                expected: "old".to_string(),
                actual: "new".to_string(),
            }
            .into(),
        );
        assert_eq!(value["error"]["code"], "stale_snapshot");
        assert_eq!(value["error"]["expected_snapshot"], "old");
        assert_eq!(value["error"]["actual_snapshot"], "new");
        assert_eq!(value["error"]["retryable"], true);
    }
}
