//! Versioned machine contract shared by CLI JSON payloads and MCP.
//!
//! CLI `--json` writes the value stored in an MCP result's `data` field.
//! MCP adds the small envelope because `structuredContent` must be an object
//! and needs to carry outcome/error metadata independently of a tool's data
//! shape (notably `ask`, whose compatibility payload is an array).

use std::sync::OnceLock;

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

pub const VERSION: &str = "sinter.agent.v1";

/// Default MCP byte budget. Tool results land directly in an agent's context
/// window, where every byte is paid for on every later turn, so MCP is
/// bounded unless the caller asks otherwise. CLI JSON is unbounded by
/// default: it goes to a terminal or a pipe (`| jq`, `| head`) where the
/// reader controls consumption and silent truncation would surprise.
pub const MCP_DEFAULT_BUDGET_BYTES: usize = 8000;

/// Per-field ceilings tried in order for free-text fields (doc, signature,
/// excerpt); entries are only dropped once the smallest ceiling still
/// overflows the budget.
const TEXT_CEILINGS: [usize; 3] = [400, 160, 60];
const TEXT_FIELDS: [&str; 5] = ["doc", "signature", "excerpt", "snippet", "text"];
/// Coverage/diagnostic envelopes are lowest priority: collapsed before
/// result entries are dropped.
const DIAGNOSTIC_FIELDS: [&str; 3] = ["coverage", "health", "compiler_index"];

/// Output size bound plus the offset at which trimmable lists resume.
#[derive(Clone, Copy, Debug, Default)]
pub struct Budget {
    pub bytes: Option<usize>,
    pub cursor: usize,
}

static CLI_BUDGET: OnceLock<Budget> = OnceLock::new();

/// Record the process-wide CLI budget (`--budget-bytes`, `--offset`) so every
/// `--json` writer honors it without per-command plumbing.
pub fn set_cli_budget(budget: Budget) {
    let _ = CLI_BUDGET.set(budget);
}

/// Pull `budget_bytes`/`cursor` out of MCP arguments (so tool dispatch never
/// sees them) and apply the MCP default.
pub fn take_budget(args: &mut Value) -> Result<Budget> {
    let object = args.as_object_mut();
    let int = |object: &mut Option<&mut Map<String, Value>>, key: &str| -> Result<Option<u64>> {
        match object.as_mut().and_then(|o| o.remove(key)) {
            None => Ok(None),
            Some(v) => v
                .as_u64()
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("`{key}` must be a non-negative integer")),
        }
    };
    let mut object = object;
    let bytes = int(&mut object, "budget_bytes")?.unwrap_or(MCP_DEFAULT_BUDGET_BYTES as u64);
    let cursor = int(&mut object, "cursor")?.unwrap_or(0);
    Ok(Budget {
        bytes: (bytes > 0).then_some(bytes as usize),
        cursor: cursor as usize,
    })
}

/// Compact CLI JSON. Human-oriented rendering remains the non-JSON path.
pub fn write_json(value: &Value) -> Result<()> {
    let budget = CLI_BUDGET.get().copied().unwrap_or_default();
    let mut value = value.clone();
    fit(&mut value, budget, |data| {
        Ok(serde_json::to_string(data)?.len() + 1)
    })?;
    println!("{}", serde_json::to_string(&value)?);
    Ok(())
}

/// Key legend for terse dependent rows, emitted only for keys actually
/// present so repository (`d`) and workspace (`p`) rows each get theirs.
const LEGEND: [(&str, &str); 8] = [
    ("s", "symbol"),
    ("k", "kind"),
    ("f", "file"),
    ("e", "evidence"),
    ("c", "certainty"),
    ("d", "depth"),
    ("p", "parent"),
    ("site", "file:line"),
];

/// MCP tool result: `structuredContent` carries the versioned contract and
/// `content[0].text` is a one-line summary (the MCP spec allows text to
/// summarize when structured content is present). Bounded to `budget`,
/// measured on the whole tool result. `coverage.compiler_index` is slimmed
/// here: per-project indexer detail stays on CLI `--json` and `doctor`.
pub fn mcp_success(operation: &str, payload: &Value, budget: Budget) -> Result<Value> {
    let envelope = |data: &Value| -> Result<Value> {
        let mut data = data.clone();
        if (budget.cursor == 0 || data.get("truncated").is_some_and(|t| t != false))
            && let Some(legend) = legend(&data)
        {
            data["legend"] = json!(legend);
        }
        Ok(json!({
            "content": [{"type": "text", "text": summary(operation, &data)}],
            "structuredContent": success(operation, data),
            "isError": false,
        }))
    };
    let mut data = payload.clone();
    slim_compiler_index(&mut data);
    fit(&mut data, budget, |data| {
        Ok(serde_json::to_string(&envelope(data)?)?.len())
    })?;
    envelope(&data)
}

/// Legend for the first terse row (an object carrying `s`) reachable from
/// `data`, `None` when the response has no terse rows.
fn legend(data: &Value) -> Option<String> {
    let keys = first_terse_row(data, 8)?;
    let parts: Vec<String> = LEGEND
        .iter()
        .filter(|(k, _)| keys.contains_key(*k))
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    Some(parts.join(" "))
}

fn first_terse_row(value: &Value, depth: usize) -> Option<&Map<String, Value>> {
    if depth == 0 {
        return None;
    }
    match value {
        Value::Object(map) => {
            if map.contains_key("s") && map.contains_key("f") {
                return Some(map);
            }
            map.values().find_map(|v| first_terse_row(v, depth - 1))
        }
        Value::Array(items) => items.iter().find_map(|v| first_terse_row(v, depth - 1)),
        _ => None,
    }
}

/// One plain line (<= 200 bytes): operation, subject, shown/total per list.
fn summary(operation: &str, data: &Value) -> String {
    let subject = data.get("symbol").map_or_else(String::new, |s| match s {
        Value::String(s) => format!(" {s}"),
        Value::Object(o) => o
            .get("qualified")
            .or_else(|| o.get("name"))
            .and_then(Value::as_str)
            .map_or_else(String::new, |s| format!(" {s}")),
        _ => String::new(),
    });
    let mut line = format!("{operation}{subject}:");
    if let Some(total) = data.get("total").and_then(Value::as_u64) {
        line.push_str(&format!(" total {total};"));
    }
    for (key, value) in data.as_object().into_iter().flatten() {
        let Some(list) = value.as_array().filter(|l| l.iter().any(Value::is_object)) else {
            continue;
        };
        let total = data
            .pointer(&format!("/totals/{key}"))
            .and_then(Value::as_u64)
            .unwrap_or(list.len() as u64);
        line.push_str(&format!(" {key} {}/{total};", list.len()));
    }
    match data.get("truncated") {
        Some(Value::Number(n)) if n.as_u64() != Some(0) => {
            line.push_str(&format!(" truncated {n};"))
        }
        Some(Value::Bool(true)) => line.push_str(" truncated;"),
        _ => {}
    }
    line.push_str(" see structuredContent");
    if line.len() > 200 {
        let cut = line
            .char_indices()
            .take_while(|(i, _)| *i < 176)
            .last()
            .map_or(0, |(i, _)| i);
        line = format!("{}… see structuredContent", &line[..cut]);
    }
    line
}

/// Replace every `compiler_index` object with its MCP summary:
/// `{state, stale_inputs, missing_index_for}`.
pub(crate) fn slim_compiler_index(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if key == "compiler_index"
                    && let Value::Object(index) = v
                    && index.contains_key("projects")
                {
                    let missing: Vec<Value> = index
                        .get("projects")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|p| p.get("freshness").and_then(Value::as_str) != Some("fresh"))
                        .flat_map(|p| p.get("languages").and_then(Value::as_array).cloned())
                        .flatten()
                        .collect();
                    let mut missing = missing;
                    missing.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                    missing.dedup();
                    *v = json!({
                        "state": index.get("state").cloned().unwrap_or(Value::Null),
                        "stale_inputs": index.get("stale_inputs").cloned().unwrap_or(json!(0)),
                        "missing_index_for": missing,
                    });
                } else {
                    slim_compiler_index(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(slim_compiler_index),
        _ => {}
    }
}

/// Shrink `data` until `measure` reports at most `budget.bytes`, applying the
/// cursor offset regardless. The measured size is the final wire size, so
/// envelope overhead is accounted for by iterating on the inner target.
fn fit(data: &mut Value, budget: Budget, measure: impl Fn(&Value) -> Result<usize>) -> Result<()> {
    let Some(limit) = budget.bytes else {
        if budget.cursor > 0 {
            trim(data, budget.cursor, usize::MAX, usize::MAX, payload_len);
        }
        return Ok(());
    };
    let original = data.clone();
    let mut target = limit;
    loop {
        *data = original.clone();
        let over = |v: &Value| payload_len(v) > target;
        let ceiling = TEXT_CEILINGS
            .iter()
            .copied()
            .find(|&c| !over(&text_capped(data, c)))
            .unwrap_or(TEXT_CEILINGS[2]);
        let changed = trim(data, budget.cursor, ceiling, target, payload_len);
        // Stamped only when the budget changed something, so an untouched
        // payload stays byte-identical between CLI and MCP.
        if changed {
            data["budget_bytes"] = json!(limit);
        }
        let actual = measure(data)?;
        if actual <= limit {
            return Ok(());
        }
        let overshoot = actual - limit;
        target = if overshoot < target {
            target - overshoot
        } else {
            target / 2
        };
        if target < 32 {
            bail!("budget of {limit} bytes is too small for a {actual}-byte minimal response");
        }
    }
}

fn payload_len(v: &Value) -> usize {
    serde_json::to_string(v).map_or(usize::MAX, |s| s.len())
}

fn text_capped(data: &Value, ceiling: usize) -> Value {
    let mut copy = data.clone();
    cap_text(&mut copy, ceiling);
    copy
}

fn cap_text(value: &mut Value, ceiling: usize) -> bool {
    let mut changed = false;
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if TEXT_FIELDS.contains(&key.as_str())
                    && let Some(s) = v.as_str()
                    && s.len() > ceiling
                {
                    let cut = s
                        .char_indices()
                        .take_while(|(i, _)| *i < ceiling)
                        .last()
                        .map_or(0, |(i, _)| i);
                    *v = Value::String(format!("{}…", &s[..cut]));
                    changed = true;
                } else {
                    changed |= cap_text(v, ceiling);
                }
            }
        }
        Value::Array(items) => {
            for v in items {
                changed |= cap_text(v, ceiling);
            }
        }
        _ => {}
    }
    changed
}

/// JSON pointers of the lists a cursor pages through: every top-level array
/// of objects, plus `ask`'s per-topic hits.
fn list_pointers(data: &Value) -> Vec<String> {
    let Some(map) = data.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, v) in map {
        if key == "topics" {
            for (i, topic) in v.as_array().into_iter().flatten().enumerate() {
                if topic.get("hits").and_then(Value::as_array).is_some() {
                    out.push(format!("/topics/{i}/hits"));
                }
            }
        } else if v.as_array().is_some_and(|a| a.iter().any(Value::is_object)) {
            out.push(format!("/{key}"));
        }
    }
    out
}

/// Apply the cursor, cap text, collapse diagnostics if still over, then drop
/// trailing entries (largest lists first) until `len(data) <= target`.
/// Records `truncated`, `totals`, `next_cursor` when anything was omitted.
fn trim(
    data: &mut Value,
    cursor: usize,
    ceiling: usize,
    target: usize,
    len: fn(&Value) -> usize,
) -> bool {
    let pointers = list_pointers(data);
    let totals: Map<String, Value> = pointers
        .iter()
        .map(|p| {
            (
                p.trim_start_matches('/').to_string(),
                json!(
                    data.pointer(p)
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len)
                ),
            )
        })
        .collect();
    for p in &pointers {
        if let Some(list) = data.pointer_mut(p).and_then(Value::as_array_mut) {
            list.drain(..cursor.min(list.len()));
        }
    }
    let mut changed = cap_text(data, ceiling);
    if len(data) > target {
        changed |= collapse(data);
    }
    let mut dropped = 0usize;
    while len(data) > target {
        let longest = pointers.iter().max_by_key(|p| {
            data.pointer(p)
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        });
        let Some(list) = longest
            .and_then(|p| data.pointer_mut(p))
            .and_then(Value::as_array_mut)
        else {
            break;
        };
        if list.pop().is_none() {
            break;
        }
        dropped += 1;
    }
    if dropped == 0 && cursor == 0 {
        return changed;
    }
    let kept = pointers
        .iter()
        .map(|p| {
            data.pointer(p)
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        })
        .max()
        .unwrap_or(0);
    // ponytail: one offset shared by every list in the payload; per-list
    // cursors if a multi-list verb (show) ever needs independent paging.
    let Some(map) = data.as_object_mut() else {
        return changed;
    };
    if dropped > 0 {
        // `ask`/`impact` already carry an integer `truncated`; leave its
        // type alone and flag the budget cut beside it.
        match map.get("truncated") {
            Some(Value::Number(_)) => map.insert("budget_truncated".into(), json!(true)),
            _ => map.insert("truncated".into(), json!(true)),
        };
        map.insert("next_cursor".into(), json!(cursor + kept));
    }
    map.insert("totals".into(), Value::Object(totals));
    true
}

/// Reduce coverage/diagnostic envelopes to their status, here and inside
/// every top-level list entry (batched `affected` carries one per result).
fn collapse(data: &mut Value) -> bool {
    let mut changed = false;
    let Some(map) = data.as_object_mut() else {
        return false;
    };
    for (key, value) in map.iter_mut() {
        if DIAGNOSTIC_FIELDS.contains(&key.as_str())
            && let Value::Object(inner) = value
            && !inner.contains_key("omitted")
        {
            let status = inner.get("status").cloned();
            inner.clear();
            inner.insert("omitted".into(), json!("budget"));
            if let Some(status) = status {
                inner.insert("status".into(), status);
            }
            changed = true;
        } else if let Value::Array(items) = value {
            for item in items {
                changed |= collapse(item);
            }
        }
    }
    changed
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
        (_, "ask") => &["question", "limit", "scope", "explain"],
        (false, "context") => &["task"],
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
        (false, "impact") => &["rev_range", "limit"],
        (true, "impact") => &["member", "rev_range", "limit"],
        (false, "overlap") => &["ranges"],
        (false, "map") => &["scope"],
        _ => bail!("unknown tool `{operation}` for this server scope"),
    };
    if let Some(key) = object.keys().find(|key| {
        !allowed.contains(&key.as_str()) && !matches!(key.as_str(), "budget_bytes" | "cursor")
    }) {
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
            if let Some(props) = input.get_mut("properties").and_then(Value::as_object_mut) {
                props.insert(
                    "budget_bytes".to_string(),
                    json!({
                        "type": "integer", "minimum": 0, "default": MCP_DEFAULT_BUDGET_BYTES,
                        "description": "max result bytes; 0 = unlimited",
                    }),
                );
                props.insert(
                    "cursor".to_string(),
                    json!({
                        "type": "integer", "minimum": 0, "default": 0,
                        "description": "resume lists at prior next_cursor",
                    }),
                );
            }
        }
        tool["outputSchema"] = output_schema(&name);
        tool["annotations"] = json!({"readOnlyHint": true});
    }
}

fn success(operation: &str, data: Value) -> Value {
    let partial = is_partial(&data);
    let found = is_found(operation, &data);
    let not_proven = is_not_proven(&data);
    json!({
        "protocol": VERSION,
        "operation": operation,
        "outcome": {
            "status": if not_proven { "not_proven" } else if !found { "not_found" } else if partial { "partial" } else { "complete" },
            "partial": partial,
        },
        "data": data,
    })
}

fn is_not_proven(data: &Value) -> bool {
    data.get("status").and_then(Value::as_str) == Some("not_proven")
        || data
            .get("coverage")
            .and_then(|coverage| coverage.get("status"))
            .and_then(Value::as_str)
            == Some("not_proven")
        || data
            .get("results")
            .and_then(Value::as_array)
            .is_some_and(|results| {
                !results.is_empty()
                    && results.iter().all(|result| {
                        result.get("status").and_then(Value::as_str) == Some("not_proven")
                    })
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
        || data.pointer("/health/status").and_then(Value::as_str) == Some("partial")
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

/// Envelope-only output schema: clients validate `structuredContent`
/// against it, agents never read it, and per-operation data shapes cost
/// ~9 KB of every tools/list. `data` is documented by the CLI `--json`
/// contract and the guide resource.
fn output_schema(_operation: &str) -> Value {
    json!({"type": "object", "required": ["protocol", "operation", "outcome", "data"]})
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use serde_json::json;

    use crate::lookup::SymbolLookupError;

    use super::{Budget, VERSION, complete_tool_schemas, failure, validate_arguments};

    fn mcp_success(
        operation: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        super::mcp_success(operation, payload, Budget::default())
    }

    #[test]
    fn mcp_envelope_data_is_the_cli_payload() {
        let cli = json!({"exact": true, "results": [{"name": "run"}]});
        let result = mcp_success("query", &cli).unwrap();
        assert_eq!(result["structuredContent"]["protocol"], VERSION);
        assert_eq!(result["structuredContent"]["data"], cli);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.len() <= 200, "{text}");
        assert_eq!(text, "query: results 1/1; see structuredContent");
        assert!(result["structuredContent"]["data"].get("legend").is_none());
    }

    #[test]
    fn terse_rows_get_a_legend_and_a_summary_line() {
        let payload = json!({
            "symbol": "Store::in_edges",
            "total": 71,
            "dependents": [{"s": "a", "k": "fn", "f": "a.rs", "e": "calls", "c": "certain", "d": 1}],
            "coverage": {"compiler_index": {
                "state": "stale", "stale_inputs": 9, "indexable_languages": ["go", "rust"],
                "projects": [
                    {"freshness": "stale", "languages": ["rust"], "indexer": "rust-analyzer"},
                    {"freshness": "fresh", "languages": ["go"]}
                ]
            }}
        });
        let first = mcp_success("affected", &payload).unwrap();
        let data = &first["structuredContent"]["data"];
        assert_eq!(
            data["legend"],
            "s=symbol k=kind f=file e=evidence c=certainty d=depth"
        );
        assert_eq!(
            data["coverage"]["compiler_index"],
            json!({"state": "stale", "stale_inputs": 9, "missing_index_for": ["rust"]})
        );
        assert_eq!(
            first["content"][0]["text"],
            "affected Store::in_edges: total 71; dependents 1/1; see structuredContent"
        );
        let paged = super::mcp_success(
            "affected",
            &payload,
            Budget {
                bytes: None,
                cursor: 1,
            },
        )
        .unwrap();
        assert!(paged["structuredContent"]["data"].get("legend").is_none());
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
    fn traversal_miss_is_not_proven_in_the_agent_outcome() {
        let payload = json!({
            "status": "not_proven",
            "total": 0,
            "dependencies": [],
            "coverage": {"status": "not_proven"},
        });
        let result = mcp_success("deps", &payload).unwrap();
        assert_eq!(
            result["structuredContent"]["outcome"]["status"],
            "not_proven"
        );
        assert_eq!(result["structuredContent"]["data"]["total"], 0);
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
        assert_eq!(tool["outputSchema"]["required"][0], "protocol");
        assert_eq!(tool["outputSchema"]["required"][3], "data");
    }

    #[test]
    fn partial_health_is_a_partial_outcome() {
        let result = mcp_success(
            "map",
            &json!({
                "health": {"status": "partial"},
                "nodes": 1,
                "modules": [],
                "hubs": [],
                "docs": [],
            }),
        )
        .unwrap();
        assert_eq!(result["structuredContent"]["outcome"]["status"], "partial");
    }

    #[test]
    fn closed_schema_is_enforced_at_runtime() {
        validate_arguments("ask", &json!({"question": "run", "explain": true}), false).unwrap();
        validate_arguments("ask", &json!({"question": "run", "explain": true}), true).unwrap();
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
