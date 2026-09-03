//! Versioned machine contract shared by CLI JSON payloads and MCP.
//!
//! CLI `--json` writes the value stored in an MCP result's `data` field.
//! MCP adds the small envelope because `structuredContent` must be an object
//! and needs to carry outcome/error metadata independently of a tool's data
//! shape (notably `ask`, whose compatibility payload is an array).

use std::cell::RefCell;
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
const TEXT_FIELDS: [&str; 6] = ["doc", "signature", "excerpt", "snippet", "text", "t"];
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

// Diagnostic sink for the command in flight. A CLI process serves one
// command on one thread, and the MCP loop answers one request on one
// thread, so the buffer lives on that thread: no lock, no poisoning, no
// context threaded through call sites, no leakage between requests.
//
// ponytail: a warning raised on a rayon worker (index build) would never
// reach the envelope written by the main thread; make this a
// `Mutex<Vec<String>>` static if a parallel path ever needs to warn.
thread_local! {
    static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Buffer a diagnostic for the machine envelope. Ambiguity notes, ignored
/// candidates and degraded-resolution notices are part of the answer, so an
/// agent must find them in the document it parses rather than beside it.
///
/// Outside `--json` no envelope is ever written, so the note goes straight
/// to stderr with the wording terminals already show.
///
/// ponytail: JSON mode is read once off argv unless `set_json_mode` ran
/// first; set it explicitly from `main` if a verb ever gains a JSON switch
/// other than `--json`.
pub fn warn(message: impl Into<String>) {
    let message = message.into();
    if !json_mode() {
        eprintln!("note: {message}");
    }
    WARNINGS.with_borrow_mut(|buffer| buffer.push(message));
}

static JSON_MODE: OnceLock<bool> = OnceLock::new();

/// Declare that every diagnostic reaches the caller inside an envelope, so
/// nothing is echoed to stderr. `serve` calls this: an MCP stdio transport
/// has no terminal, and a stray line on stderr is noise a client may log
/// as a failure.
pub fn set_json_mode() {
    let _ = JSON_MODE.set(true);
}

fn json_mode() -> bool {
    *JSON_MODE.get_or_init(|| std::env::args().any(|arg| arg == "--json"))
}

/// Drain the sink. Called once per emitted envelope, so a warning is
/// reported exactly once and never leaks into a later MCP response.
fn take_warnings() -> Vec<String> {
    WARNINGS.with_borrow_mut(std::mem::take)
}

/// Attach buffered warnings to a JSON object, omitting the key when there
/// are none: an agent pays for every envelope byte on every later turn.
fn insert_warnings(value: &mut Value, warnings: Vec<String>) {
    if warnings.is_empty() {
        return;
    }
    if let Some(map) = value.as_object_mut() {
        map.insert("warnings".to_string(), json!(warnings));
    }
}

/// Pull `budget_bytes` out of validated MCP arguments and apply the MCP
/// default. `cursor` stays in the arguments: tools page from it (see
/// `graph_tool::limit`) so a page is cut from the whole result, not from
/// the first `limit` rows.
pub fn take_budget(args: &mut Value) -> Budget {
    let bytes = args
        .as_object_mut()
        .and_then(|object| object.remove("budget_bytes"))
        .and_then(|value| value.as_u64())
        .unwrap_or(MCP_DEFAULT_BUDGET_BYTES as u64);
    Budget {
        bytes: (bytes > 0).then_some(bytes as usize),
        cursor: args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize,
    }
}

/// Compact CLI JSON. Human-oriented rendering remains the non-JSON path.
pub fn write_json(value: &Value) -> Result<()> {
    let budget = CLI_BUDGET.get().copied().unwrap_or_default();
    let mut value = value.clone();
    insert_warnings(&mut value, take_warnings());
    fit(&mut value, budget, |data| {
        Ok(serde_json::to_string(data)?.len() + 1)
    })?;
    println!("{}", serde_json::to_string(&value)?);
    Ok(())
}

/// Key legend for terse dependent rows, emitted only for keys actually
/// present so repository (`d`) and workspace (`p`) rows each get theirs.
const LEGEND: [(&str, &str); 13] = [
    ("s", "symbol"),
    ("k", "kind"),
    ("f", "file"),
    ("e", "evidence"),
    ("c", "certainty"),
    ("d", "depth"),
    ("p", "parent"),
    ("site", "file:line"),
    ("sites", "all kept call sites"),
    ("sites_total", "call sites in all"),
    ("seeds", "reached-from"),
    ("l", "line"),
    ("t", "text"),
];

/// MCP tool result: `structuredContent` carries the versioned contract and
/// `content[0].text` is a one-line summary (the MCP spec allows text to
/// summarize when structured content is present). Bounded to `budget`,
/// measured on the whole tool result. `coverage.compiler_index` is slimmed
/// here: per-project indexer detail stays on CLI `--json` and `doctor`.
/// `args` are the validated tool arguments: `include_coverage` and the
/// traversal filters shape the envelope (`outcome.reason`).
pub fn mcp_success(
    operation: &str,
    payload: &Value,
    budget: Budget,
    args: &Value,
) -> Result<Value> {
    // Drained once here, not inside `envelope`: `fit` rebuilds the envelope
    // on every sizing pass.
    let warnings = take_warnings();
    let mut data = payload.clone();
    slim_compiler_index(&mut data);
    // The verdict reads the full payload (coverage included) before the
    // MCP trim removes what an agent did not ask for.
    let verdict = outcome(operation, &data, args, &warnings);
    slim_for_mcp(
        &mut data,
        args.get("include_coverage")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    let envelope = |data: &Value| -> Result<Value> {
        let mut data = data.clone();
        if (budget.cursor == 0 || data.get("truncated").is_some_and(|t| t != false))
            && let Some(legend) = legend(&data)
        {
            data["legend"] = json!(legend);
        }
        let summary = summary(operation, &data, &warnings);
        let mut structured = success(operation, data, verdict.clone());
        insert_warnings(&mut structured, warnings.clone());
        // Mirrored under `outcome`: a client that reads only the verdict
        // must still see that the answer rests on a tie-break or a
        // degraded resolution.
        insert_warnings(&mut structured["outcome"], warnings.clone());
        Ok(json!({
            "content": [{"type": "text", "text": summary}],
            "structuredContent": structured,
            "isError": false,
        }))
    };
    fit(&mut data, budget, |data| {
        Ok(serde_json::to_string(&envelope(data)?)?.len())
    })?;
    envelope(&data)
}

/// Fields of the `symbol` echo an agent can act on. Doc, signature, span
/// and snapshot id are what `show` is for; the echo only confirms which
/// node answered and how to address it again.
const SYMBOL_ECHO: [&str; 6] = ["symbol_key", "qualified", "kind", "file", "line", "member"];

/// MCP-only byte trim applied to the CLI payload: the `symbol` echo keeps
/// its addressing fields, a `site` that repeats the row's own file becomes
/// `l`, and the coverage block is dropped unless asked for (or its
/// `filters` when they are the defaults). Batched `results` get the same
/// treatment per entry.
fn slim_for_mcp(data: &mut Value, include_coverage: bool) {
    let Some(map) = data.as_object_mut() else {
        return;
    };
    if let Some(echo) = map.get_mut("symbol").and_then(Value::as_object_mut) {
        echo.retain(|key, _| SYMBOL_ECHO.contains(&key.as_str()));
    }
    if !include_coverage {
        map.remove("coverage");
    } else if let Some(coverage) = map.get_mut("coverage").and_then(Value::as_object_mut)
        && coverage.get("filters").is_some_and(default_filters)
    {
        coverage.remove("filters");
    }
    for value in map.values_mut() {
        let Some(rows) = value.as_array_mut() else {
            continue;
        };
        for row in rows.iter_mut().filter_map(Value::as_object_mut) {
            if row.get("symbol").is_some_and(Value::is_object) || row.contains_key("error") {
                // A batched entry is one answer: trim it like the root.
                let mut entry = Value::Object(std::mem::take(row));
                slim_for_mcp(&mut entry, include_coverage);
                if let Value::Object(entry) = entry {
                    *row = entry;
                }
                continue;
            }
            let (Some(file), Some(site)) = (
                row.get("f").and_then(Value::as_str),
                row.get("site").and_then(Value::as_str),
            ) else {
                continue;
            };
            if site == file {
                row.remove("site");
            } else if let Some(line) = site
                .strip_prefix(file)
                .and_then(|rest| rest.strip_prefix(':'))
                .and_then(|line| line.parse::<u64>().ok())
            {
                row.remove("site");
                row.insert("l".to_string(), json!(line));
            }
        }
    }
}

/// True when a `coverage.filters` block describes an unfiltered traversal
/// at the MCP default scope: nothing an agent did not already know.
fn default_filters(filters: &Value) -> bool {
    filters["relations"]["mode"] == "all_dependencies"
        && filters["evidence"]["mode"] == "all_available"
        && filters["min_confidence"] == "any"
        && filters["scope"]["values"]
            == json!(crate::corpus::ScopeSelection::agent_default().labels())
}

/// Legend for the first terse row reachable from `data`, `None` when the
/// response has no terse rows.
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
            // Graph rows key the symbol `s`; `grep` hits are text locations
            // with no symbol, and their `l`/`t` pair belongs to no other
            // verb, so neither shape can mislabel the other.
            if (map.contains_key("s") || map.contains_key("l")) && map.contains_key("f") {
                return Some(map);
            }
            map.values().find_map(|v| first_terse_row(v, depth - 1))
        }
        Value::Array(items) => items.iter().find_map(|v| first_terse_row(v, depth - 1)),
        _ => None,
    }
}

/// MCP tool result for a lookup that ran and found nothing. Per the MCP
/// spec this is an execution outcome (`isError: true` in `result`), not a
/// JSON-RPC error: most clients surface only `error.message` and drop
/// `error.data`, which is where the close-name candidates would have gone.
/// `structured` is the `failure` document; `subject` is the symbol asked for.
pub fn mcp_failure(subject: Option<&str>, structured: Value) -> Value {
    let operation = structured["operation"].as_str().unwrap_or("unknown");
    let status = structured["outcome"]["status"]
        .as_str()
        .unwrap_or("error")
        .replace('_', " ");
    let mut text = operation.to_string();
    if let Some(subject) = subject {
        text.push_str(&format!(" {subject}"));
    }
    text.push_str(&format!(": {status}"));
    let names: Vec<&str> = structured["error"]["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.as_str().or_else(|| c["name"].as_str()))
        .collect();
    if names.is_empty() {
        if let Some(line) = structured["error"]["message"]
            .as_str()
            .and_then(|m| m.lines().next())
        {
            text.push_str(&format!("; {line}"));
        }
    } else {
        let shown = names.len().min(5);
        let label = if structured["error"]["code"] == "no_match" {
            "close names"
        } else {
            "candidates"
        };
        text.push_str(&format!("; {label}: {}", names[..shown].join(", ")));
        if names.len() > shown {
            text.push_str(&format!(" (+{} more)", names.len() - shown));
        }
    }
    json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": structured,
        "isError": true,
    })
}

/// One plain line (<= 200 bytes): operation, subject, warnings, shown/total
/// per list. Warnings lead: a tie-break or degraded resolution changes what
/// the counts mean, and many clients show only this line.
fn summary(operation: &str, data: &Value, warnings: &[String]) -> String {
    let subject = data.get("symbol").map_or_else(String::new, |s| match s {
        Value::String(s) => format!(" {s}"),
        Value::Object(o) => o
            .get("qualified")
            .or_else(|| o.get("name"))
            .and_then(Value::as_str)
            .map_or_else(String::new, |s| format!(" {s}")),
        _ => String::new(),
    });
    // The verdict is already in the payload; many clients surface only
    // `content[]`, so a negative must say so in the line an agent reads
    // first. Held out of `line` until the end so truncation cannot eat it.
    let status_note = match data.get("status").and_then(Value::as_str) {
        None | Some("found") => String::new(),
        Some("not_proven") => " NOT PROVEN (absence unproven);".to_string(),
        Some(other) => format!(" status {other};"),
    };
    let mut line = format!("{operation}{subject}:");
    for warning in warnings {
        line.push_str(&format!(" {warning};"));
    }
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
    line.push_str(&status_note);
    line.push_str(" see structuredContent");
    if line.len() > 200 {
        let cap = 176usize.saturating_sub(status_note.len());
        let cut = line
            .char_indices()
            .take_while(|(i, _)| *i < cap)
            .last()
            .map_or(0, |(i, _)| i);
        line = format!("{}…{status_note} see structuredContent", &line[..cut]);
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
        trim(data, budget.cursor, usize::MAX, usize::MAX, payload_len)?;
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
        let changed = trim(data, budget.cursor, ceiling, target, payload_len)?;
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
            // Nothing left to cut: the minimal response is the answer,
            // flagged rather than refused, so a caller with a tight budget
            // still learns the verdict and how to page for the rest.
            data["budget_truncated"] = json!(true);
            return Ok(());
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
/// of objects, `ask`'s per-topic hits, and the lists inside each batched
/// `results` entry (a batch pages its rows, never its answers; `query`'s
/// flat `results` pages as one list).
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
        } else if key == "results" {
            let nested: Vec<String> = v
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .flat_map(|(i, entry)| {
                    list_pointers(entry)
                        .into_iter()
                        .map(move |inner| format!("/results/{i}{inner}"))
                })
                .collect();
            if nested.is_empty() {
                if v.as_array().is_some_and(|a| a.iter().any(Value::is_object)) {
                    out.push("/results".to_string());
                }
            } else {
                out.extend(nested);
            }
        } else if v.as_array().is_some_and(|a| a.iter().any(Value::is_object)) {
            out.push(format!("/{key}"));
        }
    }
    out
}

/// Rows the tool itself left out: an integer `truncated` (affected, deps,
/// grep, ask) or a non-empty per-group object (show, impact).
fn tool_truncated(data: &Value) -> bool {
    match data.get("truncated") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) > 0,
        Some(Value::Object(groups)) => !groups.is_empty(),
        _ => false,
    }
}

/// Apply the cursor, cap text, collapse diagnostics if still over, then drop
/// trailing entries (largest lists first) until `len(data) <= target`.
/// Records `truncated`, `totals`, `next_cursor` when anything was omitted;
/// `next_cursor` is present whenever rows remain beyond this page, whether
/// the byte budget or the tool's own `limit` cut them.
fn trim(
    data: &mut Value,
    cursor: usize,
    ceiling: usize,
    target: usize,
    len: fn(&Value) -> usize,
) -> Result<bool> {
    let pointers = list_pointers(data);
    let list_len = |data: &Value, p: &str| {
        data.pointer(p)
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    };
    // A verb with one list and a `total` reports the whole result's size;
    // otherwise the list as delivered is the best count available.
    let known_total = (pointers.len() == 1)
        .then(|| data.get("total").and_then(Value::as_u64))
        .flatten()
        .map(|total| total as usize);
    let totals: Map<String, Value> = pointers
        .iter()
        .map(|p| {
            (
                p.trim_start_matches('/').to_string(),
                json!(known_total.unwrap_or_else(|| list_len(data, p))),
            )
        })
        .collect();
    if cursor > 0 {
        let longest = pointers
            .iter()
            .map(|p| list_len(data, p))
            .max()
            .unwrap_or(0);
        let end = known_total.unwrap_or(longest);
        if cursor >= end {
            bail!("`cursor` must be below {end} (rows available); got {cursor}");
        }
    }
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
        let longest = pointers.iter().max_by_key(|p| list_len(data, p));
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
    let kept = pointers
        .iter()
        .map(|p| list_len(data, p))
        .max()
        .unwrap_or(0);
    let remaining = dropped > 0
        || tool_truncated(data)
        || totals
            .values()
            .filter_map(Value::as_u64)
            .any(|total| total as usize > cursor + kept);
    // ponytail: one offset shared by every list in the payload; per-list
    // cursors if a multi-list verb (show) ever needs independent paging.
    let Some(map) = data.as_object_mut() else {
        return Ok(changed);
    };
    if dropped > 0 {
        // `ask`/`impact`/`show` already carry their own `truncated`; leave
        // it alone and flag the budget cut beside it.
        if map.contains_key("truncated") {
            map.insert("budget_truncated".into(), json!(true));
        } else {
            map.insert("truncated".into(), json!(true));
        }
    }
    if remaining {
        map.insert("next_cursor".into(), json!(cursor + kept));
    }
    if dropped == 0 && cursor == 0 {
        // Untouched page: only the resume point is added, so a payload the
        // budget never cut stays the CLI document plus `next_cursor`.
        return Ok(changed);
    }
    // A tool that reports its own `totals` (show, impact) is authoritative.
    map.entry("totals").or_insert(Value::Object(totals));
    Ok(true)
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
            // The searched universe and negative-proof qualifiers are part
            // of the answer, not expendable diagnostics. Keep them even
            // when the detailed compiler/index health must be collapsed.
            let essentials: Vec<(&str, Value)> =
                ["status", "completeness", "conclusive", "universe"]
                    .into_iter()
                    .filter_map(|field| inner.get(field).cloned().map(|value| (field, value)))
                    .collect();
            inner.clear();
            inner.insert("omitted".into(), json!("budget"));
            for (field, value) in essentials {
                inner.insert(field.to_string(), value);
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
    } else if message.contains("another sinter process is building this graph") {
        // Not the caller's mistake and not permanent: name it so an agent
        // retries instead of reading a generic failure as a dead end.
        "busy"
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
                "busy" => "busy",
                _ => "error",
            },
            "partial": false,
        },
        "error": {
            "code": code,
            "message": message,
            "retryable": matches!(code, "stale_snapshot" | "busy"),
            "candidates": candidates,
        },
    });
    insert_warnings(&mut failure, take_warnings());
    if let Some((expected, actual)) = lookup.and_then(|error| error.snapshots()) {
        failure["error"]["expected_snapshot"] = json!(expected);
        failure["error"]["actual_snapshot"] = json!(actual);
    }
    failure
}

/// [`failure`] for MCP: lookup candidates become the `Name@file[:line]`
/// selectors an agent pastes straight back as `symbol`. CLI `--json`
/// keeps the full node objects (`snapshot_id` is a CLI relocation flow).
pub fn mcp_failure_document(operation: &str, error: &anyhow::Error) -> Value {
    let mut document = failure(operation, error);
    if let Some(lookup) = error.downcast_ref::<crate::lookup::SymbolLookupError>() {
        document["error"]["candidates"] =
            json!(crate::lookup::candidate_selectors(lookup.candidates()));
    }
    document
}

/// Enforce the advertised `inputSchema` at runtime: closed key set,
/// required keys, and the `type`/`minimum`/`enum`/`items` of every
/// property. The schema in `tool_catalog` is the one contract; an argument
/// that is advertised but silently ignored (or accepted with the wrong
/// type) leaves an agent no way to learn the truth except by failing.
/// `null` counts as absent, matching how tools read optional arguments.
pub fn validate_arguments(operation: &str, args: &Value, workspace: bool) -> Result<()> {
    let Some(object) = args.as_object() else {
        bail!("arguments for `{operation}` must be a JSON object");
    };
    let Some(schema) = crate::tool_catalog::input_schema(operation, workspace) else {
        bail!("unknown tool `{operation}` for this server scope");
    };
    let properties = schema["properties"].as_object();
    for (key, value) in object {
        let Some(property) = properties.and_then(|p| p.get(key)) else {
            bail!("unknown argument `{key}` for `{operation}`");
        };
        if !value.is_null() {
            check_type(key, value, property)?;
        }
    }
    for required in schema["required"].as_array().into_iter().flatten() {
        let key = required.as_str().unwrap_or("");
        if object.get(key).is_none_or(Value::is_null) {
            bail!("missing required parameter `{key}` for `{operation}`");
        }
    }
    Ok(())
}

fn check_type(key: &str, value: &Value, schema: &Value) -> Result<()> {
    let expected = schema["type"].as_str().unwrap_or("any");
    let actual = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    if expected != "any" && expected != actual {
        bail!(
            "`{key}` must be {} {expected} (got {actual}: {value})",
            article(expected)
        );
    }
    if let Some(minimum) = schema["minimum"].as_i64()
        && value.as_i64().is_some_and(|n| n < minimum)
    {
        bail!("`{key}` must be an integer >= {minimum} (got {value})");
    }
    if let Some(options) = schema["enum"].as_array()
        && !options.contains(value)
    {
        let names: Vec<&str> = options.iter().filter_map(Value::as_str).collect();
        bail!("`{key}` must be one of {} (got {value})", names.join(", "));
    }
    if let Some(items) = value.as_array()
        && schema.get("items").is_some()
    {
        for (index, item) in items.iter().enumerate() {
            check_type(&format!("{key}[{index}]"), item, &schema["items"])?;
        }
    }
    Ok(())
}

fn article(noun: &str) -> &'static str {
    if noun.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    }
}

/// Inject the three envelope-level arguments every tool honors. Defaults
/// are stated once in the guide resource rather than on twelve schemas,
/// and unknown arguments are rejected by `validate_arguments`, so no
/// `additionalProperties` line is repeated here either. No `outputSchema`:
/// clients validate against it, agents never read it, and
/// `structuredContent` is documented by the guide resource.
pub fn complete_tool_schemas(list: &mut Value) {
    for tool in list["tools"].as_array_mut().into_iter().flatten() {
        if let Some(input) = tool.get_mut("inputSchema").and_then(Value::as_object_mut)
            && let Some(props) = input.get_mut("properties").and_then(Value::as_object_mut)
        {
            props.insert(
                "budget_bytes".to_string(),
                json!({
                    "type": "integer",
                    "description": "max bytes; 0 = all (8000)",
                }),
            );
            props.insert(
                "cursor".to_string(),
                json!({
                    "type": "integer", "minimum": 0,
                    "description": "resume at next_cursor",
                }),
            );
            props.insert(
                "include_coverage".to_string(),
                json!({
                    "type": "boolean",
                    "description": "keep the coverage block",
                }),
            );
        }
        tool["annotations"] = json!({"readOnlyHint": true});
    }
}

fn success(operation: &str, data: Value, outcome: Value) -> Value {
    json!({
        "protocol": VERSION,
        "operation": operation,
        "outcome": outcome,
        "data": data,
    })
}

/// The one verdict an agent reads: `status` folds the payload's own
/// `status`/`outcome`/`decision` fields plus coverage into
/// `complete|partial|not_found|not_proven`, and `reason` names why an
/// answer is less than complete when the payload can tell.
fn outcome(operation: &str, data: &Value, args: &Value, warnings: &[String]) -> Value {
    let partial = is_partial(data);
    let found = is_found(operation, data);
    let not_proven = is_not_proven(data);
    let abstain = data.get("outcome") == Some(&json!("abstain"))
        || data.get("decision") == Some(&json!("abstain"));
    let truncated = tool_truncated(data);
    let status = if not_proven {
        "not_proven"
    } else if !found {
        "not_found"
    } else if partial || abstain || truncated {
        "partial"
    } else {
        "complete"
    };
    let filtered = args.get("max_depth").and_then(Value::as_u64) == Some(0)
        || ["min_confidence", "evidence", "relations", "scope"]
            .iter()
            .any(|key| args.get(*key).is_some_and(|v| !v.is_null()))
        || data
            .pointer("/miss/excluded_by_filter")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0;
    let no_scip = data.get("scip_evidence_available") == Some(&json!(false))
        || data
            .pointer("/coverage/compiler_index/state")
            .and_then(Value::as_str)
            == Some("missing");
    let reason = if not_proven && filtered {
        Some("filter_excluded")
    } else if not_proven && no_scip {
        Some("no_scip")
    } else if abstain {
        Some("abstain")
    } else if warnings.iter().any(|w| w.contains(" ignored (")) {
        Some("tie_break")
    } else if truncated {
        Some("limit_reached")
    } else {
        None
    };
    let mut outcome = json!({"status": status, "partial": status != "complete"});
    if let Some(reason) = reason {
        outcome["reason"] = json!(reason);
    }
    outcome
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
    // A batch is found when any entry is: each entry carries its own
    // `status`, so the caller can still tell the misses apart.
    if let Some(results) = data.get("results").and_then(Value::as_array)
        && results.iter().all(|r| r.get("status").is_some())
    {
        return results.iter().any(|r| r["status"] == "found");
    }
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

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use serde_json::json;

    use crate::lookup::SymbolLookupError;

    use serde_json::Value;

    use super::{Budget, VERSION, complete_tool_schemas, failure, validate_arguments};

    fn all_args() -> serde_json::Value {
        json!({"include_coverage": true})
    }

    fn mcp_success(
        operation: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        super::mcp_success(operation, payload, Budget::default(), &json!({}))
    }

    /// Every advertised argument validates in the type the schema names,
    /// and a wrong type is a named rejection rather than a silent default.
    #[test]
    fn arguments_are_checked_against_the_advertised_schema() {
        let injected = ["budget_bytes", "cursor"];
        for (workspace, catalog) in [
            (false, crate::tool_catalog::repository()),
            (true, crate::tool_catalog::workspace()),
        ] {
            let scope = if workspace { "workspace" } else { "repository" };
            for tool in catalog["tools"].as_array().unwrap() {
                let name = tool["name"].as_str().unwrap();
                let properties = tool["inputSchema"]["properties"].as_object().unwrap();
                for key in injected {
                    assert!(
                        properties.contains_key(key),
                        "{scope} `{name}` lacks `{key}`"
                    );
                }
                let sample = |key: &str| match properties[key]["type"].as_str().unwrap() {
                    "string" => properties[key]["enum"][0]
                        .as_str()
                        .map_or(json!("x"), Value::from),
                    "integer" => json!(1),
                    "boolean" => json!(true),
                    "array" => json!([]),
                    other => panic!("{scope} `{name}.{key}`: unexpected type {other}"),
                };
                let mut required = json!({});
                for key in tool["inputSchema"]["required"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let key = key.as_str().unwrap();
                    required[key] = sample(key);
                }
                for key in properties.keys() {
                    let mut args = required.clone();
                    args[key] = sample(key);
                    validate_arguments(name, &args, workspace).unwrap_or_else(|error| {
                        panic!("{scope} `{name}` advertises `{key}` but rejects it: {error}")
                    });
                    let mut wrong = required.clone();
                    wrong[key] = json!({"not": "expected"});
                    let error = validate_arguments(name, &wrong, workspace).unwrap_err();
                    assert!(
                        error.to_string().contains(&format!("`{key}` must be")),
                        "{scope} `{name}.{key}`: {error}"
                    );
                }
                assert!(
                    validate_arguments(name, &json!({"not_a_real_argument": 1}), workspace)
                        .is_err(),
                    "{scope} `{name}` accepts an argument it never advertised"
                );
            }
        }
        let reject = |args: serde_json::Value| {
            validate_arguments("affected", &args, false)
                .unwrap_err()
                .to_string()
        };
        assert!(
            reject(json!({"symbol": "x", "max_depth": "two"}))
                .contains("`max_depth` must be an integer")
        );
        assert!(
            reject(json!({"symbol": "x", "max_depth": -1}))
                .contains("`max_depth` must be an integer >= 0")
        );
        assert!(
            reject(json!({"symbol": "x", "relations": "calls"}))
                .contains("`relations` must be an array")
        );
        assert!(
            reject(json!({"symbol": "x", "relations": ["phones"]}))
                .contains("`relations[0]` must be one of")
        );
        assert!(
            reject(json!({"symbol": "x", "scope": ["everything"]}))
                .contains("`scope[0]` must be one of")
        );
        assert!(
            reject(json!({"symbol": "x", "min_confidence": "sometimes"}))
                .contains("`min_confidence` must be one of")
        );
        // Evidence tiers stay on the MCP surface: an agent that cannot ask
        // for compiler-grade-only edges cannot make a negative claim.
        validate_arguments(
            "affected",
            &json!({"symbol": "x", "min_confidence": "certain", "evidence": ["scip"]}),
            false,
        )
        .unwrap();
        assert!(
            reject(json!({"symbol": "x", "cursor": -5}))
                .contains("`cursor` must be an integer >= 0")
        );
        validate_arguments(
            "affected",
            &json!({"symbol": "x", "max_depth": 0, "relations": ["calls"]}),
            false,
        )
        .unwrap();
        // Required keys are checked here, not by the tool.
        assert!(
            validate_arguments("query", &json!({}), false)
                .unwrap_err()
                .to_string()
                .contains("missing required parameter `symbol`")
        );
    }

    #[test]
    fn warnings_ride_inside_the_envelope() {
        super::warn("2 other `Foo` ignored");
        super::warn(String::from("resolution degraded"));
        let value = failure("show", &anyhow!("boom"));
        assert_eq!(
            value["warnings"],
            json!(["2 other `Foo` ignored", "resolution degraded"])
        );
        super::warn("only once");
        let first = super::mcp_success(
            "query",
            &json!({"results": []}),
            Budget::default(),
            &json!({}),
        )
        .unwrap();
        assert_eq!(first["structuredContent"]["warnings"], json!(["only once"]));
        // Drained, so the next envelope of the same process is clean.
        let second = super::mcp_success(
            "query",
            &json!({"results": []}),
            Budget::default(),
            &json!({}),
        )
        .unwrap();
        assert!(second["structuredContent"].get("warnings").is_none());
    }

    #[test]
    fn warnings_lead_the_summary_line_and_mirror_under_outcome() {
        super::take_warnings();
        super::warn("2 other `to_json` ignored (in-degree): to_json@a.rs, to_json@b.rs");
        let result = super::mcp_success(
            "show",
            &json!({"symbol": "to_json", "incoming": [{"s": "x", "f": "a.rs"}]}),
            Budget::default(),
            &json!({}),
        )
        .unwrap();
        assert_eq!(
            result["content"][0]["text"],
            "show to_json: 2 other `to_json` ignored (in-degree): to_json@a.rs, to_json@b.rs; \
             incoming 1/1; see structuredContent"
        );
        let outcome = &result["structuredContent"]["outcome"];
        assert_eq!(outcome["status"], "complete");
        assert_eq!(outcome["warnings"].as_array().unwrap().len(), 1);
        assert_eq!(result["structuredContent"]["warnings"], outcome["warnings"]);
        let clean = super::mcp_success(
            "show",
            &json!({"symbol": "x"}),
            Budget::default(),
            &json!({}),
        )
        .unwrap();
        assert!(
            clean["structuredContent"]["outcome"]
                .get("warnings")
                .is_none()
        );
    }

    #[test]
    fn a_lookup_miss_is_a_tool_result_with_candidates_in_the_text() {
        let miss = failure(
            "show",
            &crate::lookup::NoMatch(
                "no exact match for `take_budgt`; close names:\n  take_budget\n  take_budgets"
                    .to_string(),
            )
            .into(),
        );
        let result = super::mcp_failure(Some("take_budgt"), miss);
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["content"][0]["text"],
            "show take_budgt: not found; close names: take_budget, take_budgets"
        );
        assert_eq!(
            result["structuredContent"]["outcome"]["status"],
            "not_found"
        );
        assert_eq!(result["structuredContent"]["error"]["code"], "no_match");
        assert_eq!(
            result["structuredContent"]["error"]["candidates"],
            json!(["take_budget", "take_budgets"])
        );

        let bare = failure(
            "show",
            &crate::lookup::NoMatch("no symbol matches `Zz` — try the ask tool".to_string()).into(),
        );
        assert_eq!(
            super::mcp_failure(Some("Zz"), bare)["content"][0]["text"],
            "show Zz: not found; no symbol matches `Zz` — try the ask tool"
        );
    }

    #[test]
    fn envelope_omits_the_warnings_key_when_there_are_none() {
        super::take_warnings();
        let mut value = json!({"results": []});
        super::insert_warnings(&mut value, Vec::new());
        assert!(value.get("warnings").is_none());
        super::insert_warnings(&mut value, vec!["note".to_string()]);
        assert_eq!(value["warnings"], json!(["note"]));
    }

    #[test]
    fn plain_text_mode_still_writes_the_note_to_stderr() {
        // `cargo test` argv carries no `--json`, so this is the plain-text
        // path: the note is written to stderr and still buffered.
        assert!(!super::json_mode());
        super::take_warnings();
        super::warn("ambiguous");
        assert_eq!(super::take_warnings(), vec!["ambiguous".to_string()]);
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
            "status": "found",
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
        let first =
            super::mcp_success("affected", &payload, Budget::default(), &all_args()).unwrap();
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
            &json!({}),
        )
        .unwrap();
        assert!(paged["structuredContent"]["data"].get("legend").is_none());
    }

    #[test]
    fn a_negative_summary_says_not_proven_in_the_text_line() {
        // The one line many MCP clients surface. Without the verdict it
        // reads as "no callers"; the payload already knows better.
        for (operation, payload) in [
            (
                "affected",
                json!({"status": "not_proven", "symbol": "orphan_wipe", "total": 0, "dependents": []}),
            ),
            (
                "grep",
                json!({"status": "not_proven", "pattern": "x", "total": 0, "matches": []}),
            ),
        ] {
            let text = mcp_success(operation, &payload).unwrap()["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(text.contains("NOT PROVEN (absence unproven)"), "{text}");
            assert!(text.len() <= 200, "{text}");
        }
        assert_eq!(
            mcp_success(
                "affected",
                &json!({"status": "not_proven", "symbol": "orphan_wipe", "total": 0, "dependents": []})
            )
            .unwrap()["content"][0]["text"],
            "affected orphan_wipe: total 0; NOT PROVEN (absence unproven); \
             see structuredContent"
        );
        // A non-`found`, non-`not_proven` status is named, never dropped.
        let partial = mcp_success(
            "affected",
            &json!({"status": "partial", "symbol": "x", "total": 0, "dependents": []}),
        )
        .unwrap();
        assert_eq!(
            partial["content"][0]["text"],
            "affected x: total 0; status partial; see structuredContent"
        );
    }

    #[test]
    fn the_status_note_survives_summary_truncation() {
        let mut payload = json!({"status": "not_proven", "total": 0});
        for i in 0..40 {
            payload[format!("list_with_a_long_name_{i}")] = json!([{"s": "a", "f": "b"}]);
        }
        let text = mcp_success("affected", &payload).unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.len() <= 200, "{} bytes: {text}", text.len());
        assert!(text.contains("NOT PROVEN (absence unproven)"), "{text}");
        assert!(text.ends_with("see structuredContent"), "{text}");
    }

    #[test]
    fn grep_hits_are_shortened_not_dropped_under_a_tight_budget() {
        let long = "x".repeat(500);
        let payload = json!({
            "status": "found",
            "pattern": "x+",
            "total": 2,
            "matches": [
                {"f": "a.rs", "l": 12, "t": long},
                {"f": "b.rs", "l": 30, "t": "short hit"},
            ],
        });
        let result = super::mcp_success(
            "grep",
            &payload,
            Budget {
                bytes: Some(600),
                cursor: 0,
            },
            &json!({}),
        )
        .unwrap();
        let data = &result["structuredContent"]["data"];
        let matches = data["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2, "{data}");
        let text = matches[0]["t"].as_str().unwrap();
        assert!(text.len() < 500 && text.ends_with('…'), "{text}");
        assert_eq!(matches[0]["l"], 12);
        assert_eq!(data["legend"], "f=file l=line t=text");
    }

    #[test]
    fn multi_seed_rows_decode_their_provenance_key() {
        let payload = json!({
            "dependents": [
                {"s": "a", "k": "fn", "f": "a.rs", "e": "calls", "c": "certain",
                 "d": 1, "seeds": ["Foo", "Bar"]}
            ]
        });
        let result = mcp_success("affected", &payload).unwrap();
        assert_eq!(
            result["structuredContent"]["data"]["legend"],
            "s=symbol k=kind f=file e=evidence c=certainty d=depth seeds=reached-from"
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

    fn rows(n: usize) -> Vec<serde_json::Value> {
        (0..n)
            .map(|i| json!({"s": format!("dep{i}"), "k": "fn", "f": "a.rs", "site": "a.rs:7"}))
            .collect()
    }

    #[test]
    fn next_cursor_pages_over_the_whole_result_and_rejects_a_cursor_past_it() {
        // The tool cut at its own limit: no budget cut, still a resume point.
        let cut_by_tool = json!({"total": 60, "truncated": 10, "dependents": rows(50)});
        let first = mcp_success("affected", &cut_by_tool).unwrap();
        let data = &first["structuredContent"]["data"];
        assert_eq!(data["next_cursor"], 50, "{data}");
        assert!(
            data.get("totals").is_none(),
            "untouched page keeps the CLI shape: {data}"
        );
        assert_eq!(
            first["structuredContent"]["outcome"]["reason"],
            "limit_reached"
        );
        // Sites in the row's own file collapse to a line.
        assert_eq!(data["dependents"][0]["l"], 7);
        assert!(data["dependents"][0].get("site").is_none());

        // Last page: the tool delivered rows 50..60, nothing remains.
        let last = super::mcp_success(
            "affected",
            &json!({"total": 60, "dependents": rows(60)}),
            Budget {
                bytes: None,
                cursor: 50,
            },
            &json!({}),
        )
        .unwrap();
        let data = &last["structuredContent"]["data"];
        assert_eq!(data["dependents"].as_array().unwrap().len(), 10);
        assert!(data.get("next_cursor").is_none(), "{data}");
        assert_eq!(data["totals"]["dependents"], 60);

        let past = super::mcp_success(
            "affected",
            &json!({"total": 60, "dependents": rows(60)}),
            Budget {
                bytes: None,
                cursor: 60,
            },
            &json!({}),
        )
        .unwrap_err();
        assert!(
            past.to_string().contains("`cursor` must be below 60"),
            "{past}"
        );
    }

    #[test]
    fn a_budget_below_the_minimal_answer_flags_instead_of_failing() {
        let result = super::mcp_success(
            "affected",
            &json!({"symbol": "x", "total": 60, "dependents": rows(60)}),
            Budget {
                bytes: Some(100),
                cursor: 0,
            },
            &json!({}),
        )
        .unwrap();
        let data = &result["structuredContent"]["data"];
        assert_eq!(data["budget_truncated"], true, "{data}");
        assert_eq!(data["total"], 60);
        assert_eq!(data["next_cursor"], 0, "{data}");
    }

    #[test]
    fn a_batch_pages_its_rows_and_never_drops_an_answer() {
        let payload = json!({"status": "partial", "results": [
            {"symbol": {"symbol_key": "k", "doc": "long"}, "status": "found", "total": 40,
             "dependents": rows(40)},
            {"symbol": "Zz", "status": "not_found", "error": {"code": "no_match", "candidates": []}},
        ]});
        let result = super::mcp_success(
            "affected",
            &payload,
            Budget {
                bytes: Some(1200),
                cursor: 0,
            },
            &json!({}),
        )
        .unwrap();
        let data = &result["structuredContent"]["data"];
        let results = data["results"].as_array().unwrap();
        assert_eq!(results.len(), 2, "{data}");
        assert_eq!(results[1]["error"]["code"], "no_match");
        assert!(
            results[0]["dependents"].as_array().unwrap().len() < 40,
            "{data}"
        );
        assert!(results[0]["symbol"].get("doc").is_none(), "{data}");
        assert_eq!(data["totals"]["results/0/dependents"], 40, "{data}");
        assert!(data["next_cursor"].is_u64(), "{data}");
    }

    #[test]
    fn budget_collapse_keeps_the_searched_universe_and_claim_qualifiers() {
        let mut data = json!({
            "coverage": {
                "status": "not_proven",
                "completeness": "partial",
                "conclusive": false,
                "universe": {
                    "mode": "workspace",
                    "name": "shop",
                    "members": {"auth": {"root": "/repos/auth"}}
                },
                "compiler_index": {"projects": [{"large": "x".repeat(1000)}]},
            }
        });

        assert!(super::collapse(&mut data));
        assert_eq!(data["coverage"]["omitted"], "budget");
        assert_eq!(data["coverage"]["status"], "not_proven");
        assert_eq!(data["coverage"]["completeness"], "partial");
        assert_eq!(data["coverage"]["conclusive"], false);
        assert_eq!(data["coverage"]["universe"]["name"], "shop");
        assert!(data["coverage"].get("compiler_index").is_none());
    }

    /// Unknown arguments are rejected by `validate_arguments`, not by an
    /// `additionalProperties: false` line repeated on every schema.
    #[test]
    fn envelope_arguments_are_injected_and_unknown_ones_rejected() {
        let mut list = json!({"tools": [{
            "name": "query",
            "inputSchema": {"type": "object", "properties": {"symbol": {"type": "string"}}}
        }]});
        complete_tool_schemas(&mut list);
        let tool = &list["tools"][0];
        assert!(tool.get("outputSchema").is_none());
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        for key in ["budget_bytes", "cursor", "include_coverage"] {
            assert!(
                tool["inputSchema"]["properties"].get(key).is_some(),
                "{key}"
            );
        }
        assert!(
            validate_arguments("query", &json!({"symbol": "x", "nope": 1}), false)
                .unwrap_err()
                .to_string()
                .contains("unknown argument `nope`")
        );
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
