//! `sinter ask "<question>"`: a vague question gives a ranked, grouped,
//! content-bearing starting point. Keyword scoring only — no NLP, no LLM;
//! the doc comment is the prose.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};
use serde_json::json;
use sinter_core::{Node, Relation};
use sinter_resolve::qualified_of;
use sinter_store::Store;

use crate::corpus::ScopeSelection;
use crate::lookup::open_store;
use crate::render::{ellipsize, line_of, location};

pub(crate) mod confidence;
pub(crate) mod query;
mod ranking;

use query::{Query, clauses_of};
use ranking::{Hit, score_candidates};

/// Add agent-safety metadata to an already ordered list of hit objects.
/// Runs after any merge so the ranking margin reflects the list the caller
/// will actually see. Only rank one receives a calibrated ranking assessment.
pub(crate) fn annotate(hits: &mut [serde_json::Value]) {
    let scores = hits
        .iter()
        .map(|hit| hit["score"].as_i64().unwrap_or(0))
        .collect::<Vec<_>>();
    let names = hits
        .iter()
        .map(|hit| hit["name"].as_str().unwrap_or("").to_owned())
        .collect::<Vec<_>>();
    for (rank, hit) in hits.iter_mut().enumerate() {
        if let Some(object) = hit.as_object_mut() {
            for stale in [
                "confidence",
                "ranking_bucket",
                "ranking_margin",
                "calibration",
                "term_coverage",
                "verify_required",
                "abstain",
                "confidence_reason",
                "ranking_reason",
                "family_size",
            ] {
                object.remove(stale);
            }
        }
        hit["rank"] = json!(rank + 1);
    }
    let Some(top) = hits.first_mut() else {
        return;
    };
    let (matched, total) = coverage_of_json(top);
    let assessment = confidence::assess_top(&scores, matched, total);
    let name = top["name"].as_str().unwrap_or("");
    let family_size = names.iter().filter(|other| *other == name).count();
    // `confidence` and `confidence_reason` are v1 compatibility aliases.
    // New consumers should use the explicitly named ranking fields and the
    // topic-level decision.
    top["ranking_bucket"] = json!(assessment.ranking_bucket);
    top["confidence"] = json!(assessment.ranking_bucket);
    top["ranking_margin"] = json!(assessment.ranking_margin);
    top["calibration"] = json!(assessment.calibration);
    top["term_coverage"] = json!(assessment.term_coverage);
    top["verify_required"] = json!(assessment.verify_required);
    top["abstain"] = json!(assessment.abstain);
    top["ranking_reason"] = json!(assessment.reason);
    top["confidence_reason"] = json!(assessment.reason);
    top["family_size"] = json!(family_size);
}

/// Caveat line for an annotated list, or None when the top hit stands clear.
/// Term coverage of a serialized hit, body-only terms at half credit
/// (mirrors `ranking::coverage`). Falls back to the matched list when the
/// breakdown carries no numerator.
fn coverage_of_json(hit: &serde_json::Value) -> (usize, usize) {
    let breakdown = &hit["score_breakdown"];
    let field = |name: &str| breakdown[name].as_u64().map(|n| n as usize);
    let total = field("coverage_denominator").unwrap_or(0);
    let matched = field("coverage_numerator")
        .unwrap_or_else(|| hit["matched"].as_array().map_or(0, std::vec::Vec::len));
    match field("body_only") {
        Some(body_only) if body_only > 0 => (2 * matched - body_only, 2 * total),
        _ => (matched, total),
    }
}

pub(crate) fn advice_for(hits: &[serde_json::Value]) -> Option<String> {
    let hit = hits.first()?;
    let ranking_bucket = confidence::RankingBucket::from_label(
        hit["ranking_bucket"]
            .as_str()
            .or_else(|| hit["confidence"].as_str())
            .unwrap_or(""),
    )?;
    let top = confidence::Assessment {
        ranking_bucket,
        ranking_margin: confidence::RankingMargin {
            absolute: hit["ranking_margin"]["absolute"].as_i64(),
            permille: hit["ranking_margin"]["permille"].as_i64(),
        },
        calibration: confidence::Calibration {
            version: confidence::CALIBRATION_VERSION,
            sample_size: hit["calibration"]["sample_size"].as_u64().unwrap_or(0) as usize,
            correct: hit["calibration"]["correct"].as_u64().unwrap_or(0) as usize,
            measured_precision: hit["calibration"]["measured_precision"]
                .as_f64()
                .unwrap_or(0.0),
            precision_interval_95: confidence::wilson_95(
                hit["calibration"]["correct"].as_u64().unwrap_or(0) as usize,
                hit["calibration"]["sample_size"].as_u64().unwrap_or(0) as usize,
            ),
            in_calibration: hit["calibration"]["in_calibration"]
                .as_bool()
                .unwrap_or(false),
        },
        term_coverage: confidence::TermCoverage {
            matched: hit["term_coverage"]["matched"].as_u64().unwrap_or(0) as usize,
            total: hit["term_coverage"]["total"].as_u64().unwrap_or(0) as usize,
            permille: hit["term_coverage"]["permille"].as_u64().unwrap_or(0) as u16,
        },
        verify_required: hit["verify_required"].as_bool().unwrap_or(true),
        abstain: hit["abstain"].as_bool().unwrap_or(true),
        reason: match hit["ranking_reason"]
            .as_str()
            .or_else(|| hit["confidence_reason"].as_str())
            .unwrap_or("")
        {
            "no_match" => "no_match",
            "no_runner_up" => "no_runner_up",
            "non_positive_score" => "non_positive_score",
            "weak_term_coverage" => "weak_term_coverage",
            "insufficient_calibration_sample" => "insufficient_calibration_sample",
            "calibrated_ranking" => "calibrated_ranking",
            _ => "unknown_confidence_state",
        },
    };
    let family_size = hit["family_size"].as_u64().unwrap_or(1) as usize;
    confidence::advice(top, family_size)
}

fn family_size(hits: &[Hit], rank: usize) -> usize {
    hits.iter()
        .filter(|other| other.node.name == hits[rank].node.name)
        .count()
}

/// Score each clause independently, then dedup: a node hit by several
/// clauses shows once, in its best clause (highest score; earlier clause
/// on ties). Output budgeting happens after grouping and is globally strict.
/// Ranked hits per topic label.
type TopicHits = Vec<(String, Vec<Hit>)>;

fn multi_hits(
    store: &Store,
    clauses: &[(String, Query)],
    scopes: &ScopeSelection,
) -> Result<(TopicHits, Vec<String>)> {
    let mut groups: TopicHits = Vec::with_capacity(clauses.len());
    for (label, query) in clauses {
        groups.push((label.clone(), score_candidates(store, query, scopes)?));
    }
    let mut best: std::collections::HashMap<String, (i64, usize)> =
        std::collections::HashMap::new();
    for (ci, (_, hits)) in groups.iter().enumerate() {
        for hit in hits {
            let entry = best
                .entry(hit.node.id.as_str().to_string())
                .or_insert((hit.score, ci));
            if hit.score > entry.0 {
                *entry = (hit.score, ci);
            }
        }
    }
    for (ci, (_, hits)) in groups.iter_mut().enumerate() {
        hits.retain(|h| best[h.node.id.as_str()].1 == ci);
    }
    let connects = ranking::connect_topics(store, &mut groups)?;
    Ok((groups, connects))
}

fn adjacency_counts(store: &Store, node: &Node) -> Result<(usize, usize, Vec<String>)> {
    let out = store.out_edges(&node.id)?;
    let contains = out
        .iter()
        .filter(|e| e.relation == Relation::Contains)
        .count();
    let extends: Vec<String> = out
        .iter()
        .filter(|e| e.relation == Relation::Extends)
        .map(|e| qualified_of(e.dst.as_str()).to_string())
        .collect();
    let used_by_files: HashSet<String> = store
        .in_edges(&node.id)?
        .iter()
        .filter(|e| e.relation != Relation::Contains)
        .map(|e| {
            e.src
                .as_str()
                .split_once('#')
                .map_or(e.src.as_str(), |(f, _)| f)
                .to_string()
        })
        .collect();
    Ok((contains, used_by_files.len(), extends))
}

/// `sinter ask --workspace`: fan candidate gathering out across members,
/// merge-rank with the same deterministic formula, tie-break extended by
/// member name. Stays single-topic: clause splitting (clauses_of) is
/// repo-scope only until a workspace question demands it.
pub fn run_workspace(
    manifest: &Path,
    question: &str,
    limit: usize,
    json: bool,
    explain: bool,
    scopes: &ScopeSelection,
) -> Result<bool> {
    if json {
        let response = crate::workspace_tools::call(
            manifest,
            "ask",
            &json!({
                "question": question,
                "limit": limit,
                "scope": scopes.labels(),
                "explain": explain,
            }),
        )?;
        let found = response["returned"].as_u64().unwrap_or(0) > 0;
        crate::agent_protocol::write_json(&response)?;
        return Ok(found);
    }
    let ws = crate::workspace::load(manifest)?;
    let query = Query::parse(question);
    if query.is_empty() {
        bail!("no searchable terms in {question:?} — try naming the thing you're looking for");
    }
    let mut all: Vec<(String, std::path::PathBuf, Hit)> = Vec::new();
    for (name, repo) in &ws.members {
        let store = crate::lookup::open_store(repo)?;
        for hit in score_candidates(&store, &query, scopes)? {
            all.push((name.clone(), repo.clone(), hit));
        }
    }
    all.sort_by(|a, b| {
        b.2.score
            .cmp(&a.2.score)
            .then_with(|| (a.2.node.kind as u8).cmp(&(b.2.node.kind as u8)))
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.2.node.file.cmp(&b.2.node.file))
            .then_with(|| a.2.node.span.start.cmp(&b.2.node.span.start))
    });
    if all.is_empty() {
        println!("no match for {:?} in any member", query.surface_text(" "));
        return Ok(false);
    }
    println!(
        "Best matches across {} members ({} terms: {}):\n",
        ws.members.len(),
        query.len(),
        query.surface_text(", ")
    );
    let scores = all
        .iter()
        .take(limit + 1)
        .map(|(_, _, h)| h.score)
        .collect::<Vec<_>>();
    let family = all
        .iter()
        .take(limit)
        .filter(|(_, _, h)| h.node.name == all[0].2.node.name)
        .count();
    let assessment = confidence::assess_top(&scores, all[0].2.coverage().0, all[0].2.coverage().1);
    print_caveat(assessment, family, explain);
    for (rank, (member, repo, hit)) in all.iter().take(limit).enumerate() {
        let line = line_of(repo, &hit.node.file, hit.node.span.start);
        println!(
            "{}. {} {}:{}    [{} {}/{} terms]",
            rank + 1,
            hit.node.kind.as_str(),
            member,
            qualified_of(hit.node.id.as_str()),
            hit.channels.join("+"),
            hit.matched.len(),
            hit.total_terms,
        );
        println!("   {}:{}", member, location(repo, &hit.node.file, line));
        if let Some(doc) = &hit.node.doc
            && let Some(first) = doc.lines().next()
        {
            println!("   /// {}", ellipsize(first, 160));
        }
        if !hit.node.signature.is_empty() {
            println!("   {}", ellipsize(&hit.node.signature, 100));
        }
        if explain {
            let next = all.get(rank + 1).map(|(_, _, next)| next);
            println!("   {}", explain_hit_line(hit, next));
        }
        println!();
    }
    if all.len() > limit {
        println!("{} more matches below cutoff", all.len() - limit);
    }
    Ok(true)
}

/// Doc text an `ask` result carries: the first sentence, capped. The full
/// doc stays one `show` away; a result is a pointer, not the page.
const DOC_EXCERPT_CHARS: usize = 200;

fn doc_excerpt(doc: &str) -> String {
    let paragraph = doc.split("\n\n").next().unwrap_or("").trim();
    let sentence = paragraph
        .find(". ")
        .map_or(paragraph, |end| &paragraph[..=end])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    match sentence.char_indices().nth(DOC_EXCERPT_CHARS) {
        Some((cut, _)) => format!("{}…", sentence[..cut].trim_end()),
        None => sentence,
    }
}

fn hit_json(repo: &Path, h: &Hit) -> serde_json::Value {
    json!({
        "id": h.node.symbol_key().as_str(),
        "snapshot_id": h.node.id.as_str(),
        "symbol_key": h.node.symbol_key().as_str(),
        "qualified": qualified_of(h.node.id.as_str()),
        "name": h.node.name,
        "kind": h.node.kind.as_str(),
        "scope": h.scope.as_str(),
        "file": h.node.file,
        "span": {"start": h.node.span.start, "end": h.node.span.end},
        "line": line_of(repo, &h.node.file, h.node.span.start),
        "signature": h.node.signature,
        "doc": h.node.doc.as_deref().map(doc_excerpt),
        "score": h.score,
        "matched": h.matched,
        "channels": h.channels,
        "roles": h.roles,
        "variants": h.variants,
        "score_breakdown": h.breakdown,
    })
}

/// Per-hit fields an agent needs to act on a result. Everything else
/// (scores, spans, calibration, ranking diagnostics) is `--explain` detail.
const LEAN_HIT_FIELDS: &[&str] = &[
    "rank",
    "id",
    "name",
    "qualified",
    "kind",
    "file",
    "line",
    "signature",
    "doc",
    "matched",
    "channels",
    "confidence",
    "scope",
    "member",
];

/// Strip a full response down to the default wire shape. Applied at the
/// CLI/MCP boundary only; internal consumers (`context`, workspace merge)
/// keep the full shape.
fn lean_response(response: &mut serde_json::Value) {
    for topic in response["topics"].as_array_mut().into_iter().flatten() {
        let confidence = json!({
            "level": topic["confidence"]["level"],
            "reason": topic["confidence"]["reason"],
        });
        if let Some(object) = topic.as_object_mut() {
            for field in ["ranking_margin", "term_coverage", "advice"] {
                object.remove(field);
            }
            object.insert("confidence".into(), confidence);
        }
        for hit in topic["hits"].as_array_mut().into_iter().flatten() {
            if let Some(object) = hit.as_object_mut() {
                object.retain(|key, _| LEAN_HIT_FIELDS.contains(&key.as_str()));
            }
        }
    }
}

pub fn ask_response_json(
    repo: &Path,
    question: &str,
    limit: usize,
    scopes: &ScopeSelection,
    explain: bool,
) -> Result<serde_json::Value> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
    let mut response = ask_response_with_store(&repo, &store, question, limit, scopes, explain)?;
    if !explain {
        lean_response(&mut response);
    }
    Ok(response)
}

pub(crate) fn ask_response_json_current(
    repo: &Path,
    question: &str,
    limit: usize,
    scopes: &ScopeSelection,
    explain: bool,
) -> Result<serde_json::Value> {
    let repo = repo.canonicalize()?;
    let store = crate::lookup::open_current(&repo)?;
    let mut response = ask_response_with_store(&repo, &store, question, limit, scopes, explain)?;
    if !explain {
        lean_response(&mut response);
    }
    Ok(response)
}

pub(crate) fn ask_response_with_store(
    repo: &Path,
    store: &Store,
    question: &str,
    limit: usize,
    scopes: &ScopeSelection,
    explain: bool,
) -> Result<serde_json::Value> {
    let clauses = clauses_of(question);
    if clauses.is_empty() {
        bail!("no searchable terms in {question:?} — try naming the thing you're looking for");
    }
    let (groups, connects) = multi_hits(store, &clauses, scopes)?;
    let limits = distribute_limit(limit, groups.len());
    let mut topics = Vec::with_capacity(groups.len());
    for (((label, query), (_, hits)), topic_limit) in clauses.iter().zip(groups.iter()).zip(limits)
    {
        topics.push(topic_json(repo, label, query, hits, topic_limit, explain));
    }
    Ok(response_json(question, limit, scopes, topics, connects))
}

fn response_json(
    question: &str,
    limit: usize,
    scopes: &ScopeSelection,
    topics: Vec<serde_json::Value>,
    connects: Vec<String>,
) -> serde_json::Value {
    let returned = topics
        .iter()
        .map(|topic| topic["returned"].as_u64().unwrap_or(0) as usize)
        .sum::<usize>();
    let candidate_count = topics
        .iter()
        .map(|topic| topic["candidate_count"].as_u64().unwrap_or(0) as usize)
        .sum::<usize>();
    let any_abstain = topics.iter().any(|topic| topic["status"] == "abstain");
    let verify_required = topics
        .iter()
        .any(|topic| topic["verify_required"].as_bool() == Some(true));
    json!({
        "question": question,
        "limit": limit,
        "scope": scopes.json(),
        "returned": returned,
        "truncated": candidate_count.saturating_sub(returned),
        "decision": if any_abstain { "abstain" } else if verify_required { "verify" } else { "answer" },
        "verify_required": verify_required,
        "connects": connects,
        "topics": topics,
    })
}

fn distribute_limit(limit: usize, topics: usize) -> Vec<usize> {
    if topics == 0 {
        return Vec::new();
    }
    let base = limit / topics;
    let remainder = limit % topics;
    (0..topics)
        .map(|index| base + usize::from(index < remainder))
        .collect()
}

fn topic_json(
    repo: &Path,
    label: &str,
    query: &Query,
    hits: &[Hit],
    limit: usize,
    explain: bool,
) -> serde_json::Value {
    let rendered = hits
        .iter()
        .map(|hit| hit_json(repo, hit))
        .collect::<Vec<_>>();
    topic_from_rendered(
        label,
        query
            .surface_text(" ")
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        rendered,
        limit,
        hits.len(),
        explain,
    )
}

fn topic_from_rendered(
    label: &str,
    query_terms: Vec<String>,
    mut hits: Vec<serde_json::Value>,
    limit: usize,
    candidate_count: usize,
    explain: bool,
) -> serde_json::Value {
    if hits.is_empty() || limit == 0 {
        let assessment = confidence::assess_top(&[], 0, query_terms.len());
        let reason = if limit == 0 && !hits.is_empty() {
            "limit_exhausted"
        } else {
            assessment.reason
        };
        return json!({
            "topic": label,
            "query_terms": query_terms,
            "status": "abstain",
            "verify_required": true,
            "confidence": {
                "assessment_type": "ranking_margin_bucket",
                "ranking_bucket": assessment.ranking_bucket,
                "level": assessment.ranking_bucket,
                "reason": reason,
                "calibration": assessment.calibration,
            },
            "ranking_margin": assessment.ranking_margin,
            "term_coverage": assessment.term_coverage,
            "advice": format!("abstain: {reason}; refine the topic or increase the limit"),
            "candidate_count": candidate_count,
            "returned": 0,
            "truncated": candidate_count,
            "hits": [],
        });
    }

    // Keep one unreturned candidate long enough to measure the visible top
    // hit against a real runner-up.
    hits.truncate(limit.saturating_add(1));
    annotate(&mut hits);
    let status = if hits[0]["abstain"].as_bool() == Some(true) {
        "abstain"
    } else {
        "ranked"
    };
    let verify_required = hits[0]["verify_required"].as_bool().unwrap_or(true);
    let advice = advice_for(&hits);
    let confidence = json!({
        "assessment_type": "ranking_margin_bucket",
        "ranking_bucket": hits[0]["ranking_bucket"],
        "level": hits[0]["confidence"],
        "reason": hits[0]["ranking_reason"],
        "calibration": hits[0]["calibration"],
    });
    let ranking_margin = hits[0]["ranking_margin"].clone();
    let term_coverage = hits[0]["term_coverage"].clone();
    hits.truncate(limit);
    for hit in &mut hits {
        if let Some(object) = hit.as_object_mut() {
            // Calibration describes the topic-level ranking decision, not an
            // individual candidate. Keep the one authoritative calibration
            // above. Other per-hit assessment fields remain as v1 aliases;
            // removing them requires a versioned wire-contract change.
            object.remove("calibration");
            if !explain {
                object.remove("score_breakdown");
            }
        }
    }
    json!({
        "topic": label,
        "query_terms": query_terms,
        "status": status,
        "verify_required": verify_required,
        "confidence": confidence,
        "ranking_margin": ranking_margin,
        "term_coverage": term_coverage,
        "advice": advice,
        "candidate_count": candidate_count,
        "returned": hits.len(),
        "truncated": candidate_count.saturating_sub(hits.len()),
        "hits": hits,
    })
}

/// Merge member-local ask responses, rerank per topic, and recalibrate only
/// after the workspace ranking is final. This keeps workspace CLI and MCP on
/// the same topic contract as repository scope.
pub(crate) fn merge_workspace_responses(
    question: &str,
    limit: usize,
    scopes: &ScopeSelection,
    responses: Vec<(String, serde_json::Value)>,
    explain: bool,
) -> serde_json::Value {
    let mut groups: Vec<(String, Vec<String>, Vec<serde_json::Value>, usize)> = Vec::new();
    for (member, response) in responses {
        for topic in response["topics"].as_array().into_iter().flatten() {
            let label = topic["topic"].as_str().unwrap_or("").to_owned();
            let terms = topic["query_terms"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let index = groups
                .iter()
                .position(|(existing, _, _, _)| existing == &label)
                .unwrap_or_else(|| {
                    groups.push((label.clone(), terms, Vec::new(), 0));
                    groups.len() - 1
                });
            groups[index].3 = groups[index]
                .3
                .saturating_add(topic["candidate_count"].as_u64().unwrap_or(0) as usize);
            for mut hit in topic["hits"].as_array().into_iter().flatten().cloned() {
                for field in ["id", "snapshot_id"] {
                    if let Some(value) = hit[field].as_str() {
                        hit[field] = json!(format!("{member}:{value}"));
                    }
                }
                hit["member"] = json!(member);
                groups[index].2.push(hit);
            }
        }
    }
    let limits = distribute_limit(limit, groups.len());
    let topics = groups
        .into_iter()
        .zip(limits)
        .map(|((label, terms, mut hits, candidate_count), topic_limit)| {
            hits.sort_by(|a, b| {
                b["score"]
                    .as_i64()
                    .cmp(&a["score"].as_i64())
                    .then_with(|| a["member"].as_str().cmp(&b["member"].as_str()))
                    .then_with(|| a["file"].as_str().cmp(&b["file"].as_str()))
                    .then_with(|| {
                        a["span"]["start"]
                            .as_u64()
                            .cmp(&b["span"]["start"].as_u64())
                    })
            });
            topic_from_rendered(&label, terms, hits, topic_limit, candidate_count, explain)
        })
        .collect();
    let mut response = response_json(question, limit, scopes, topics, Vec::new());
    if !explain {
        lean_response(&mut response);
    }
    response
}

pub(crate) fn workspace_candidate_limit(question: &str, limit: usize) -> usize {
    limit
        .saturating_add(1)
        .saturating_mul(clauses_of(question).len().max(1))
}

/// `--explain` per-hit line: where the terms matched, the score, and the
/// lead over the next hit.
fn explain_hit_line(hit: &Hit, next: Option<&Hit>) -> String {
    format!(
        "explain: channels {} · score {} · margin {}",
        hit.channels.join("+"),
        hit.score,
        next.map_or("n/a".to_owned(), |n| (hit.score - n.score).to_string()),
    )
}

/// Confidence line (one word) and, under `--explain`, the calibration
/// statistics behind it.
fn print_caveat(assessment: confidence::Assessment, family: usize, explain: bool) {
    let caveat = confidence::advice(assessment, family);
    if let Some(caveat) = &caveat {
        println!("{caveat}");
    }
    if explain {
        println!("{}", confidence::explain_line(assessment));
    }
    if caveat.is_some() || explain {
        println!();
    }
}

fn print_hit(
    repo: &Path,
    store: &Store,
    rank: usize,
    hit: &Hit,
    explain: Option<Option<&Hit>>,
) -> Result<()> {
    let line = line_of(repo, &hit.node.file, hit.node.span.start);
    println!(
        "{}. {} {}    [{} {}/{} terms]",
        rank + 1,
        hit.node.kind.as_str(),
        qualified_of(hit.node.id.as_str()),
        hit.channels.join("+"),
        hit.matched.len(),
        hit.total_terms,
    );
    println!("   {}", location(repo, &hit.node.file, line));
    if let Some(doc) = &hit.node.doc
        && let Some(first) = doc.lines().next()
    {
        println!("   /// {}", ellipsize(first, 160));
    }
    if !hit.node.signature.is_empty() {
        println!("   {}", ellipsize(&hit.node.signature, 100));
    }
    let (contains, used_by, extends) = adjacency_counts(store, &hit.node)?;
    let mut facts = Vec::new();
    if contains > 0 {
        facts.push(format!("contains {contains}"));
    }
    if used_by > 0 {
        facts.push(format!("used by {used_by} files"));
    }
    if !extends.is_empty() {
        facts.push(format!("extends {}", extends.join(", ")));
    }
    if !facts.is_empty() {
        println!("   {}", facts.join(" · "));
    }
    if !hit.variants.is_empty() {
        println!(
            "   {} same-name variants collapsed: {}",
            hit.variants.len(),
            hit.variants.join(", ")
        );
    }
    if let Some(next) = explain {
        println!("   {}", explain_hit_line(hit, next));
    }
    println!();
    Ok(())
}

/// Multi-topic path: one heading per clause, top hits under each,
/// per-clause "no match" lines keep honesty.
fn run_multi(
    repo: &Path,
    store: &Store,
    clauses: &[(String, Query)],
    limit: usize,
    explain: bool,
    scopes: &ScopeSelection,
) -> Result<bool> {
    let (groups, connects) = multi_hits(store, clauses, scopes)?;
    let limits = distribute_limit(limit, groups.len());
    println!("Best matches ({} topics):\n", groups.len());
    if !connects.is_empty() {
        println!("connects: {}\n", connects.join("; "));
    }
    let mut best: Option<(i64, &Hit)> = None;
    for ((topic, hits), topic_limit) in groups.iter().zip(limits) {
        println!("## {topic}");
        if hits.is_empty() {
            println!("no match\n");
            continue;
        }
        if topic_limit == 0 {
            println!("no results within the global limit; increase --limit\n");
            continue;
        }
        let scores = hits
            .iter()
            .take(topic_limit.saturating_add(1))
            .map(|hit| hit.score)
            .collect::<Vec<_>>();
        print_caveat(
            confidence::assess_top(&scores, hits[0].coverage().0, hits[0].coverage().1),
            family_size(&hits[..topic_limit.min(hits.len())], 0),
            explain,
        );
        for (rank, hit) in hits.iter().take(topic_limit).enumerate() {
            print_hit(repo, store, rank, hit, explain.then(|| hits.get(rank + 1)))?;
            if best.is_none_or(|(s, _)| hit.score > s) {
                best = Some((hit.score, hit));
            }
        }
    }
    if let Some((_, top)) = best {
        let q = qualified_of(top.node.id.as_str());
        println!("Next: sinter show {q} · sinter affected {q}");
        return Ok(true);
    }
    Ok(false)
}

/// Ok(true) when any hit surfaced (grep-style exit codes).
pub fn run(
    repo: &Path,
    question: &str,
    limit: usize,
    json: bool,
    explain: bool,
    scopes: &ScopeSelection,
) -> Result<bool> {
    let repo = repo.canonicalize()?;
    if json {
        // ask_response_json opens the store itself; must run before this function
        // takes its own handle (redb forbids a second in-process open).
        let response = ask_response_json(&repo, question, limit, scopes, explain)?;
        let found = response["returned"].as_u64().unwrap_or(0) > 0;
        crate::agent_protocol::write_json(&response)?;
        return Ok(found);
    }
    let store = open_store(&repo)?;
    let clauses = clauses_of(question);
    if clauses.len() >= 2 {
        return run_multi(&repo, &store, &clauses, limit, explain, scopes);
    }
    let query = Query::parse(question);
    if query.is_empty() {
        bail!("no searchable terms in {question:?} — try naming the thing you're looking for");
    }
    let hits = score_candidates(&store, &query, scopes)?;

    if hits.is_empty() {
        println!("no match for {:?}", query.surface_text(" "));
        let close = store.search(&query.surface_text(""), 5)?;
        if !close.is_empty() {
            let names: Vec<&str> = close.iter().map(|n| n.name.as_str()).collect();
            println!("closest symbols: {}", names.join(", "));
        }
        return Ok(false);
    }

    println!(
        "Best matches ({} terms: {}):\n",
        query.len(),
        query.surface_text(", ")
    );
    let scores = hits
        .iter()
        .take(limit + 1)
        .map(|h| h.score)
        .collect::<Vec<_>>();
    let shown = limit.min(hits.len());
    if shown > 0 {
        print_caveat(
            confidence::assess_top(&scores, hits[0].coverage().0, hits[0].coverage().1),
            family_size(&hits[..shown], 0),
            explain,
        );
    }
    // Verbose multi-topic questions dilute term coverage; a top hit
    // matching almost nothing is noise wearing a ranking. Say so instead
    // of letting it pass as an answer.
    if query.len() >= 4 && hits[0].matched.len() * 3 <= query.len() {
        println!(
            "weak match: best hit covers {}/{} terms — this graph indexes code \
             symbols, not prose docs. Ask one topic at a time with the terms \
             you expect in an identifier or doc comment.\n",
            hits[0].matched.len(),
            query.len()
        );
    }
    for (rank, hit) in hits.iter().take(limit).enumerate() {
        print_hit(
            &repo,
            &store,
            rank,
            hit,
            explain.then(|| hits.get(rank + 1)),
        )?;
    }
    if hits.len() > limit {
        println!(
            "{} more matches below cutoff · `sinter ask --limit {}` to widen",
            hits.len() - limit,
            (limit * 2).max(hits.len().min(20)),
        );
    }
    if let Some(top) = hits.first() {
        let q = qualified_of(top.node.id.as_str());
        println!("Next: sinter show {q} · sinter affected {q}");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{advice_for, doc_excerpt, topic_from_rendered};

    #[test]
    fn doc_excerpt_keeps_the_first_sentence_and_caps_length() {
        assert_eq!(
            doc_excerpt("One incremental build pass. `only` narrows\nthe scan."),
            "One incremental build pass."
        );
        assert_eq!(
            doc_excerpt("# sinter\nThis project uses sinter\n\nMore"),
            "# sinter This project uses sinter"
        );
        let long = "word ".repeat(100);
        let excerpt = doc_excerpt(&long);
        assert!(excerpt.ends_with('…'));
        assert!(excerpt.chars().count() <= 201);
    }

    #[test]
    fn advice_uses_annotation_when_only_one_hit_is_returned() {
        let hits = vec![json!({
            "score": 100,
            "ranking_bucket": "unrated",
            "ranking_margin": {"absolute": null, "permille": null},
            "calibration": {
                "version": "ask-holdout-2026-08-23.v2",
                "sample_size": 0,
                "measured_precision": 0.0,
                "in_calibration": false
            },
            "term_coverage": {"matched": 1, "total": 1, "permille": 1000},
            "verify_required": true,
            "abstain": true,
            "ranking_reason": "no_runner_up",
            "family_size": 1
        })];

        assert_eq!(advice_for(&hits).as_deref(), Some("confidence: unrated"));
    }

    #[test]
    fn topic_names_the_ranking_assessment_and_retains_v1_hit_aliases() {
        let topic = topic_from_rendered(
            "request flow",
            vec!["request".to_string(), "flow".to_string()],
            vec![
                json!({
                    "name": "dispatch",
                    "qualified": "dispatch",
                    "score": 400,
                    "matched": ["request", "flow"],
                    "score_breakdown": {"coverage_denominator": 2}
                }),
                json!({
                    "name": "fallback",
                    "qualified": "fallback",
                    "score": 300,
                    "matched": ["flow"],
                    "score_breakdown": {"coverage_denominator": 2}
                }),
            ],
            1,
            2,
            false,
        );

        assert_eq!(
            topic["confidence"]["assessment_type"],
            "ranking_margin_bucket"
        );
        assert_eq!(topic["confidence"]["ranking_bucket"], "high");
        assert_eq!(topic["confidence"]["level"], "high");
        let hit = &topic["hits"][0];
        assert_eq!(hit["ranking_bucket"], "high");
        assert_eq!(hit["confidence"], "high");
        assert_eq!(hit["ranking_reason"], "calibrated_ranking");
        assert_eq!(hit["confidence_reason"], "calibrated_ranking");
        assert!(hit.get("calibration").is_none());
    }
}
