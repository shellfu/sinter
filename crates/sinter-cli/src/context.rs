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
use crate::lookup::{Found, ensure_snapshot, find_symbol, open_store};
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
/// Test commands printed in the plain-text packet; `--json` carries the rest.
const PRINTED_TESTS: usize = 6;
/// Shortest bare word worth resolving as a symbol name.
const MIN_WORD_LEN: usize = 3;
/// Identifier-shaped tokens read out of one task string.
const MAX_IDENTIFIERS: usize = 12;
/// A name with more definitions than this grounds nothing in particular.
const MAX_ANCHOR_NODES: usize = 3;
/// Content terms (already stop-filtered by the `ask` parser) offered to `rg`.
const RG_TERMS: usize = 4;

/// Runner-up within the `ask` high-margin band of the top score is still a
/// plausible edit target and gets expanded too.
fn is_contender(score: i64, top: i64) -> bool {
    top > 0 && score * 1000 >= top * (1000 - HIGH_MARGIN_PERMILLE)
}

/// A task token that could be a symbol name, strongest shape first.
/// `Explicit` spells a location out (`Foo::bar`, `src/ask/query.rs`);
/// `Shaped` is written like code (`snake_case`, `CamelCase`); `Bare` is an
/// ordinary lowercase word that only *might* be a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    Explicit,
    Shaped,
    Bare,
}

fn shape_of(token: &str) -> Option<Shape> {
    if token.is_empty() || token.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    // Anything holding punctuation a symbol or path cannot contain
    // ("don't", "field,value") is prose, not a name.
    if !token
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '/' | '.' | '-'))
    {
        return None;
    }
    let path_like = token.contains('/')
        || token
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && ext.chars().all(char::is_alphanumeric));
    if token.contains("::") || path_like {
        return Some(Shape::Explicit);
    }
    if token.contains('_') || token.contains(char::is_uppercase) {
        return Some(Shape::Shaped);
    }
    if token.chars().count() < MIN_WORD_LEN || crate::ask::query::is_stopword(token) {
        return None;
    }
    Some(Shape::Bare)
}

/// Identifier-shaped tokens in the task string, strongest shape first and
/// deduplicated. Deterministic and lexical-free: this decides what *could*
/// be a symbol name; the store decides what actually is one.
fn identifier_candidates(task: &str) -> Vec<(Shape, String)> {
    let mut out: Vec<(Shape, String)> = Vec::new();
    for raw in task.split_whitespace() {
        let token = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        let Some(shape) = shape_of(token) else {
            continue;
        };
        if !out.iter().any(|(_, kept)| kept == token) {
            out.push((shape, token.to_owned()));
        }
        if out.len() == MAX_IDENTIFIERS {
            break;
        }
    }
    out.sort_by_key(|(shape, _)| *shape);
    out
}

/// A task-string token that named a real node.
struct Anchor {
    term: String,
    node: Node,
}

/// Resolve the task string's identifiers against real node names before any
/// lexical scoring runs. Exact and qualified-suffix matches only — the fuzzy
/// path is the guessing this pre-pass exists to replace. Whatever fails to
/// ground is returned so the caller can report it instead of dropping it.
fn anchors_of(store: &Store, task: &str) -> Result<(Vec<Anchor>, Vec<String>)> {
    let scope_index = store.scope_index()?;
    let preferred = ScopeSelection::agent_default().as_set();
    let mut anchors: Vec<Anchor> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    for (shape, term) in identifier_candidates(task) {
        let nodes = match find_symbol(store, &term)? {
            Found::Exact(nodes) if nodes.len() <= MAX_ANCHOR_NODES => nodes,
            _ => {
                unresolved.push(term);
                continue;
            }
        };
        let grounded: Vec<Node> = nodes
            .into_iter()
            // An ordinary word is only a name when it *is* the name: a
            // bare `field` must not be promoted to `Index::field`.
            .filter(|n| shape != Shape::Bare || qualified_of(n.id.as_str()) == term)
            // ...and never through a fixture or vendored copy, which is
            // where lone English verbs like `add` usually live.
            .filter(|n| preferred.contains(&scope_index.scope_of(n)))
            .collect();
        if grounded.is_empty() {
            unresolved.push(term);
            continue;
        }
        for node in grounded {
            if anchors.len() == MAX_FOCUS {
                break;
            }
            if anchors.iter().all(|a| a.node.id != node.id) {
                anchors.push(Anchor {
                    term: term.clone(),
                    node,
                });
            }
        }
    }
    Ok((anchors, unresolved))
}

/// `card` reads its provenance from an `ask` hit; an anchor states its own.
fn anchor_hit(node: &Node, term: &str) -> Value {
    json!({
        "id": node.symbol_key().as_str(),
        "snapshot_id": node.id.as_str(),
        "doc": node.doc,
        "matched": [term],
        "roles": ["anchor"],
        "channels": ["identifier"],
    })
}

/// The node behind one affected-test row, so `impact` can render its
/// runnable command. Only the rows actually kept are resolved.
fn test_node(store: &Store, test: &crate::impact::SymbolRef) -> Option<Node> {
    match find_symbol(store, &format!("{}@{}", test.qualified, test.file)) {
        Ok(Found::Exact(mut nodes)) if !nodes.is_empty() => Some(nodes.remove(0)),
        _ => None,
    }
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
        &ScopeSelection::ask_default(),
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
    let (anchors, unresolved_intents) = anchors_of(store, task)?;
    let hits = ranked_hits(&ask);
    let top = hits.first().and_then(|h| h["score"].as_i64()).unwrap_or(0);

    let filter = EdgeFilter::default();
    let mut candidates = Vec::with_capacity(hits.len() + anchors.len());
    let mut focus: Vec<Node> = Vec::new();
    let mut confidences = Vec::new();
    let mut unresolved = 0usize;
    let mut rank = 0usize;
    for anchor in &anchors {
        let hit = anchor_hit(&anchor.node, &anchor.term);
        let mut entry = card(&root, store, &hit, &anchor.node, &filter, &mut confidences)?;
        rank += 1;
        entry["rank"] = json!(rank);
        entry["score"] = Value::Null;
        entry["focus"] = json!(true);
        entry["anchor"] = json!(anchor.term);
        unresolved += store.unresolved_named(&anchor.node.name)?;
        focus.push(anchor.node.clone());
        candidates.push(entry);
    }
    let anchored: BTreeSet<String> = anchors
        .iter()
        .map(|a| a.node.id.as_str().to_owned())
        .collect();
    for hit in &hits {
        let id = NodeId::new(hit["snapshot_id"].as_str().unwrap_or(""));
        if anchored.contains(id.as_str()) {
            continue;
        }
        let Some(node) = store.node(&id)? else {
            continue;
        };
        let score = hit["score"].as_i64().unwrap_or(0);
        // Resolved identifiers outrank lexical similarity: once anything
        // grounded, a bag-of-words hit is context, never an edit target.
        let expand = anchors.is_empty()
            && focus.len() < MAX_FOCUS
            && (rank == 0 || (!abstain && is_contender(score, top)));
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
        rank += 1;
        entry["rank"] = json!(rank);
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
        .map(|t| {
            let cmd = test_node(store, t).and_then(|n| crate::impact::test_command(&root, &n));
            json!({"qualified": t.qualified, "kind": t.kind, "file": t.file, "cmd": cmd})
        })
        .collect();

    let evidence = crate::coverage::TraversalEvidence::from_confidences(confidences, unresolved);
    let coverage =
        crate::coverage::traversal_json(&root, store, &filter, evidence, !focus.is_empty())?;

    let grounded = !anchors.is_empty();
    let mut next_actions: Vec<String> = Vec::new();
    if !grounded && (abstain || focus.is_empty()) {
        let terms = ask["topics"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|t| t["query_terms"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .filter(|term| term.len() > 2)
            .take(RG_TERMS)
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
        "outcome": if grounded || !(abstain || focus.is_empty()) { "ranked" } else { "abstain" },
        "anchors": anchors
            .iter()
            .map(|a| json!({
                "term": a.term,
                "qualified": qualified_of(a.node.id.as_str()),
                "k": a.node.kind.as_str(),
                "f": a.node.file,
            }))
            .collect::<Vec<_>>(),
        "unresolved_intents": unresolved_intents,
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
    let anchors: Vec<String> = p["anchors"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            format!(
                "{} -> {}",
                a["term"].as_str().unwrap_or(""),
                a["qualified"].as_str().unwrap_or("")
            )
        })
        .collect();
    if !anchors.is_empty() {
        println!("anchors: {}", anchors.join(", "));
    }
    let intents: Vec<&str> = p["unresolved_intents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !intents.is_empty() {
        println!("unresolved intents: {}", intents.join(", "));
    }
    for c in p["candidates"].as_array().into_iter().flatten() {
        let marker = if c["anchor"].is_string() {
            "@"
        } else if c["focus"] == true {
            "*"
        } else {
            " "
        };
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
    let tests = p["tests"].as_array().map_or(&[][..], Vec::as_slice);
    println!(
        "tests ({} affected, {} shown):",
        p["tests_total"],
        tests.len()
    );
    for t in tests.iter().take(PRINTED_TESTS) {
        match t["cmd"].as_str() {
            Some(cmd) => println!("  {cmd}"),
            None => println!(
                "  # {} ({})",
                t["qualified"].as_str().unwrap_or(""),
                t["file"].as_str().unwrap_or("")
            ),
        }
    }
    if let Some(rest) = tests.len().checked_sub(PRINTED_TESTS).filter(|n| *n > 0) {
        println!("  # +{rest} more (--json for all)");
    }
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

#[cfg(test)]
mod tests {
    use super::{Shape, identifier_candidates};

    fn terms(task: &str) -> Vec<String> {
        identifier_candidates(task)
            .into_iter()
            .map(|(_, t)| t)
            .collect()
    }

    #[test]
    fn extraction_keeps_identifier_shapes_and_drops_prose() {
        let got = terms(
            "add a new field to `Decision` and thread it through adjudication, see Store::node in src/ask/query.rs (don't break it)",
        );
        for name in [
            "Decision",
            "adjudication",
            "Store::node",
            "src/ask/query.rs",
        ] {
            assert!(got.iter().any(|t| t == name), "lost `{name}`: {got:?}");
        }
        // Stopwords, sub-`MIN_WORD_LEN` words and prose punctuation never
        // reach the store.
        for prose in ["a", "to", "and", "it", "the", "don't"] {
            assert!(!got.iter().any(|t| t == prose), "kept prose `{prose}`");
        }
    }

    #[test]
    fn strongest_shape_is_resolved_first() {
        let got = identifier_candidates("thread a field through Decision via Store::node");
        assert_eq!(got[0], (Shape::Explicit, "Store::node".to_owned()));
        assert_eq!(got[1], (Shape::Shaped, "Decision".to_owned()));
        // Bare words keep task order behind everything code-shaped.
        assert_eq!(
            got[2..].iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>(),
            ["thread", "field", "through", "via"]
        );
    }

    #[test]
    fn extraction_is_deduplicated_and_bounded() {
        let task = "Node Node ".repeat(20);
        assert_eq!(terms(&task), vec!["Node".to_owned()]);
        let many = (0..40)
            .map(|i| format!("sym_{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(terms(&many).len(), super::MAX_IDENTIFIERS);
    }

    #[test]
    fn extraction_keeps_structured_tokens_stopwords_would_lose() {
        // `use` and `for` are filler as words but real as identifiers.
        assert_eq!(
            terms("use for_each and use::this"),
            ["use::this".to_owned(), "for_each".to_owned()]
        );
    }
}
