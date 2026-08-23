//! `sinter context "<task>"`: the smallest evidence packet an agent needs
//! before editing. Pure composition over `ask`, `show`-style cards, depth-1
//! `deps`/`affected`, `impact`'s affected-test selection, and one coverage
//! envelope. No new graph machinery, no scoring of its own.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::{Confidence, Node, NodeId, Relation};
use sinter_resolve::qualified_of;
use sinter_store::{EdgeFilter, Reached, Store};

use crate::ask::confidence::HIGH_MARGIN_PERMILLE;
use crate::corpus::ScopeSelection;
use crate::lookup::{ensure_snapshot, open_store};
use crate::render::{ellipsize, line_of};

/// Hits `ask` is asked for; the packet keeps every one as a candidate but
/// only expands the contenders (see `is_contender`).
const ASK_LIMIT: usize = 5;
const MAX_FOCUS: usize = 3;
/// Direct dependency/dependent rows kept per focus candidate.
const EDGE_ROWS: usize = 8;
/// Test rows kept; `impact` uses the same per-collection budget.
const TEST_ROWS: usize = crate::impact::DEFAULT_LIMIT;
const EXCERPT_LINES: usize = 12;

/// Runner-up within the `ask` high-margin band of the top score is still a
/// plausible edit target and gets expanded too.
fn is_contender(score: i64, top: i64) -> bool {
    top > 0 && score * 1000 >= top * (1000 - HIGH_MARGIN_PERMILLE)
}

/// Every `ask` hit across topics, best first, deduplicated by handle.
fn ranked_hits(ask: &Value) -> Vec<Value> {
    let mut hits: Vec<Value> = ask["topics"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|topic| topic["hits"].as_array().into_iter().flatten().cloned())
        .collect();
    hits.sort_by(|a, b| b["score"].as_i64().cmp(&a["score"].as_i64()));
    let mut seen = BTreeSet::new();
    hits.retain(|hit| seen.insert(hit["id"].as_str().unwrap_or("").to_owned()));
    hits
}

fn excerpt(repo: &Path, node: &Node) -> Option<String> {
    let source = std::fs::read_to_string(repo.join(&node.file)).ok()?;
    let start = (node.span.start as usize).min(source.len());
    let end = (node.span.end as usize).min(source.len());
    let body = source.get(start..end)?;
    Some(
        body.lines()
            .take(EXCERPT_LINES)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn edge_row(r: &Reached) -> Value {
    json!({
        "s": qualified_of(r.node.id.as_str()),
        "k": r.node.kind.as_str(),
        "f": r.node.file,
        "e": format!("{}/{}", r.via.relation.as_str(), r.via.evidence.as_str()),
    })
}

fn rows(reached: &[&Reached]) -> Vec<Value> {
    reached
        .iter()
        .take(EDGE_ROWS)
        .map(|r| edge_row(r))
        .collect()
}

/// `show`-style card plus direct deps/affected for one focus candidate.
fn card(
    repo: &Path,
    store: &Store,
    hit: &Value,
    node: &Node,
    filter: &EdgeFilter,
    confidences: &mut Vec<Confidence>,
) -> Result<Value> {
    let deps = store.dependencies(&node.id, filter, 1)?;
    let dependents = store.dependents(&node.id, filter, 1)?;
    confidences.extend(
        deps.iter()
            .chain(dependents.iter())
            .map(|r| r.via.confidence),
    );
    let (callers, importers): (Vec<&Reached>, Vec<&Reached>) = dependents
        .iter()
        .partition(|r| r.via.relation != Relation::Imports);
    let (direct, direct_files) = sinter_store::direct_summary(&dependents);
    let dep_refs: Vec<&Reached> = deps.iter().collect();
    Ok(json!({
        "id": hit["id"],
        "handle": format!("{}@{}", node.name, node.file),
        "qualified": qualified_of(node.id.as_str()),
        "name": node.name,
        "kind": node.kind.as_str(),
        "file": node.file,
        "line": line_of(repo, &node.file, node.span.start),
        "end_line": line_of(repo, &node.file, node.span.end),
        "signature": node.signature,
        "doc": hit["doc"],
        "excerpt": excerpt(repo, node),
        "why": {"matched": hit["matched"], "roles": hit["roles"], "channels": hit["channels"]},
        "deps": {"total": deps.len(), "direct": rows(&dep_refs)},
        "affected": {
            "direct": direct,
            "direct_files": direct_files,
            "callers": rows(&callers),
            "importing_files": importers.len(),
            "importers": rows(&importers),
        },
    }))
}

/// The packet. Shared by CLI `--json` and the MCP `context` tool.
pub(crate) fn response(repo: &Path, store: &Store, task: &str) -> Result<Value> {
    let root = crate::pipeline::discover_root(repo).canonicalize()?;
    let snapshot = ensure_snapshot(store, None)?;
    let ask = crate::ask::ask_response_with_store(
        &root,
        store,
        task,
        ASK_LIMIT,
        &ScopeSelection::agent_default(),
        false,
    )?;
    let abstain = ask["decision"] == "abstain";
    let abstain_reason = ask["topics"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|topic| topic["status"] == "abstain")
        .map(|topic| topic["confidence"]["reason"].clone())
        .unwrap_or(Value::Null);
    let hits = ranked_hits(&ask);
    let top = hits.first().and_then(|h| h["score"].as_i64()).unwrap_or(0);

    let filter = EdgeFilter::default();
    let mut candidates = Vec::with_capacity(hits.len());
    let mut focus: Vec<Node> = Vec::new();
    let mut confidences = Vec::new();
    let mut unresolved = 0usize;
    for (rank, hit) in hits.iter().enumerate() {
        let id = NodeId::new(hit["snapshot_id"].as_str().unwrap_or(""));
        let Some(node) = store.node(&id)? else {
            continue;
        };
        let score = hit["score"].as_i64().unwrap_or(0);
        let expand =
            focus.len() < MAX_FOCUS && (rank == 0 || (!abstain && is_contender(score, top)));
        let mut entry = if expand {
            card(&root, store, hit, &node, &filter, &mut confidences)?
        } else {
            json!({
                "id": hit["id"],
                "handle": format!("{}@{}", node.name, node.file),
                "qualified": qualified_of(node.id.as_str()),
                "kind": node.kind.as_str(),
                "file": node.file,
                "line": hit["line"],
                "why": {"matched": hit["matched"], "roles": hit["roles"], "channels": hit["channels"]},
            })
        };
        entry["rank"] = json!(rank + 1);
        entry["score"] = json!(score);
        entry["focus"] = json!(expand);
        if expand {
            unresolved += store.unresolved_named(&node.name)?;
            focus.push(node);
        }
        candidates.push(entry);
    }

    let radius = crate::impact::blast_radius(store, &filter, &focus)?;
    let tests = crate::impact::affected_tests(store, &radius, &focus)?;
    let tests_total = tests.len();
    let test_rows: Vec<Value> = tests
        .iter()
        .take(TEST_ROWS)
        .map(|t| json!({"qualified": t.qualified, "kind": t.kind, "file": t.file}))
        .collect();

    let evidence = crate::coverage::TraversalEvidence::from_confidences(confidences, unresolved);
    let coverage =
        crate::coverage::traversal_json(&root, store, &filter, evidence, !focus.is_empty())?;

    let mut next_actions: Vec<String> = Vec::new();
    if abstain || focus.is_empty() {
        let terms = ask["topics"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|t| t["query_terms"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .filter(|term| term.len() > 2)
            .collect::<Vec<_>>()
            .join("|");
        next_actions.push(format!("rg -n \"{terms}\""));
        next_actions.push("sinter map".to_string());
        next_actions.push("sinter ask \"<one concrete term from the task>\"".to_string());
    }
    for node in &focus {
        let handle = format!("{}@{}", node.name, node.file);
        next_actions.push(format!("sinter show {handle}"));
        next_actions.push(format!("sinter affected {handle} --max-depth 3"));
    }
    next_actions
        .push("sinter impact  # after editing: changed symbols, blast radius, tests".to_string());

    Ok(json!({
        "task": task,
        "snapshot": snapshot,
        "outcome": if abstain || focus.is_empty() { "abstain" } else { "ranked" },
        "candidates": candidates,
        "tests": test_rows,
        "tests_total": tests_total,
        "gaps": {
            "abstain_reason": abstain_reason,
            "unresolved_refs_matching_candidates": unresolved,
            "ask_advice": ask["topics"][0]["advice"],
        },
        "coverage": coverage,
        "next_actions": next_actions,
    }))
}

/// Ok(true) when the packet has a ranked edit target (grep-style exit codes).
pub fn run(repo: &Path, task: &str, json: bool) -> Result<bool> {
    let store = open_store(repo)?;
    let packet = response(repo, &store, task)?;
    let ranked = packet["outcome"] == "ranked";
    if json {
        crate::agent_protocol::write_json(&packet)?;
        return Ok(ranked);
    }
    print_packet(&packet);
    Ok(ranked)
}

/// Compact human rendering, bounded to roughly forty lines.
fn print_packet(p: &Value) {
    let list = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .map(|r| {
                format!(
                    "{} ({})",
                    r["s"].as_str().unwrap_or(""),
                    r["f"].as_str().unwrap_or("")
                )
            })
            .collect()
    };
    println!(
        "context: {}  [{}]",
        p["task"].as_str().unwrap_or(""),
        p["outcome"].as_str().unwrap_or("")
    );
    for c in p["candidates"].as_array().into_iter().flatten() {
        let marker = if c["focus"] == true { "*" } else { " " };
        println!(
            "{marker}{}. {} {}  {}:{}  [{}]",
            c["rank"],
            c["kind"].as_str().unwrap_or(""),
            c["qualified"].as_str().unwrap_or(""),
            c["file"].as_str().unwrap_or(""),
            c["line"],
            c["why"]["matched"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        );
        if c["focus"] != true {
            continue;
        }
        if let Some(sig) = c["signature"].as_str().filter(|s| !s.is_empty()) {
            println!("     {}", ellipsize(sig, 100));
        }
        if let Some(doc) = c["doc"].as_str() {
            println!("     /// {}", ellipsize(doc, 100));
        }
        let deps = list(&c["deps"]["direct"]);
        println!(
            "     deps ({}): {}",
            c["deps"]["total"],
            ellipsize(&deps.join(", "), 110)
        );
        let callers = list(&c["affected"]["callers"]);
        println!(
            "     affected: {} direct in {} file(s); {} importing file(s): {}",
            c["affected"]["direct"],
            c["affected"]["direct_files"],
            c["affected"]["importing_files"],
            ellipsize(&callers.join(", "), 90)
        );
    }
    let tests: Vec<String> = p["tests"]
        .as_array()
        .into_iter()
        .flatten()
        .take(6)
        .map(|t| t["qualified"].as_str().unwrap_or("").to_string())
        .collect();
    println!(
        "tests ({}): {}",
        p["tests_total"],
        ellipsize(&tests.join(", "), 110)
    );
    println!(
        "gaps: coverage {}; unresolved refs naming candidates {}; abstain {}",
        p["coverage"]["status"].as_str().unwrap_or("?"),
        p["gaps"]["unresolved_refs_matching_candidates"],
        p["gaps"]["abstain_reason"].as_str().unwrap_or("none")
    );
    println!("next:");
    for a in p["next_actions"].as_array().into_iter().flatten() {
        println!("  {}", a.as_str().unwrap_or(""));
    }
    println!("  snapshot: {}", p["snapshot"].as_str().unwrap_or(""));
}
