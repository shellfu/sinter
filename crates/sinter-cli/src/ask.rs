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
mod query;
mod ranking;

use query::{Query, clauses_of};
use ranking::{Hit, score_candidates};

/// Add agent-safety metadata to an already ordered list of hit objects.
/// Runs after any merge so the ranking margin reflects the list the caller
/// will actually see. Only rank one receives an empirical confidence label.
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
                "ranking_margin",
                "calibration",
                "term_coverage",
                "verify_required",
                "abstain",
                "confidence_reason",
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
    let matched = top["matched"].as_array().map_or(0, std::vec::Vec::len);
    let total = top["score_breakdown"]["coverage_denominator"]
        .as_u64()
        .unwrap_or(0) as usize;
    let assessment = confidence::assess_top(&scores, matched, total);
    let name = top["name"].as_str().unwrap_or("");
    let family_size = names.iter().filter(|other| *other == name).count();
    top["confidence"] = json!(assessment.level);
    top["ranking_margin"] = json!(assessment.ranking_margin);
    top["calibration"] = json!(assessment.calibration);
    top["term_coverage"] = json!(assessment.term_coverage);
    top["verify_required"] = json!(assessment.verify_required);
    top["abstain"] = json!(assessment.abstain);
    top["confidence_reason"] = json!(assessment.reason);
    top["family_size"] = json!(family_size);
}

/// Caveat line for an annotated list, or None when the top hit stands clear.
pub(crate) fn advice_for(hits: &[serde_json::Value]) -> Option<String> {
    let hit = hits.first()?;
    let level = confidence::Level::from_label(hit["confidence"].as_str().unwrap_or(""))?;
    let top = confidence::Assessment {
        level,
        ranking_margin: confidence::RankingMargin {
            absolute: hit["ranking_margin"]["absolute"].as_i64(),
            permille: hit["ranking_margin"]["permille"].as_i64(),
        },
        calibration: confidence::Calibration {
            version: confidence::CALIBRATION_VERSION,
            sample_size: hit["calibration"]["sample_size"].as_u64().unwrap_or(0) as usize,
            measured_precision: hit["calibration"]["measured_precision"]
                .as_f64()
                .unwrap_or(0.0),
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
        reason: match hit["confidence_reason"].as_str().unwrap_or("") {
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
fn multi_hits(
    store: &Store,
    clauses: &[(String, Query)],
    scopes: &ScopeSelection,
) -> Result<Vec<(String, Vec<Hit>)>> {
    let mut groups: Vec<(String, Vec<Hit>)> = Vec::with_capacity(clauses.len());
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
    Ok(groups)
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
    if let Some(caveat) = confidence::advice(
        confidence::assess_top(&scores, all[0].2.matched.len(), all[0].2.total_terms),
        family,
    ) {
        println!("{caveat}\n");
    }
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
            println!("   /// {first}");
        }
        if !hit.node.signature.is_empty() {
            println!("   {}", ellipsize(&hit.node.signature, 100));
        }
        println!();
    }
    if all.len() > limit {
        println!("{} more matches below cutoff", all.len() - limit);
    }
    Ok(true)
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
        "doc": h.node.doc,
        "score": h.score,
        "matched": h.matched,
        "channels": h.channels,
        "roles": h.roles,
        "score_breakdown": h.breakdown,
    })
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
    ask_response_with_store(&repo, &store, question, limit, scopes, explain)
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
    ask_response_with_store(&repo, &store, question, limit, scopes, explain)
}

fn ask_response_with_store(
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
    let groups = multi_hits(store, &clauses, scopes)?;
    let limits = distribute_limit(limit, groups.len());
    let mut topics = Vec::with_capacity(groups.len());
    for (((label, query), (_, hits)), topic_limit) in clauses.iter().zip(groups.iter()).zip(limits)
    {
        topics.push(topic_json(repo, label, query, hits, topic_limit, explain));
    }
    Ok(response_json(question, limit, scopes, topics))
}

fn response_json(
    question: &str,
    limit: usize,
    scopes: &ScopeSelection,
    topics: Vec<serde_json::Value>,
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
                "level": assessment.level,
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
        "level": hits[0]["confidence"],
        "reason": hits[0]["confidence_reason"],
        "calibration": hits[0]["calibration"],
    });
    let ranking_margin = hits[0]["ranking_margin"].clone();
    let term_coverage = hits[0]["term_coverage"].clone();
    hits.truncate(limit);
    for hit in &mut hits {
        if let Some(object) = hit.as_object_mut() {
            // Calibration describes the topic-level ranking decision, not an
            // individual candidate. Keep the one authoritative copy above.
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
    response_json(question, limit, scopes, topics)
}

pub(crate) fn workspace_candidate_limit(question: &str, limit: usize) -> usize {
    limit
        .saturating_add(1)
        .saturating_mul(clauses_of(question).len().max(1))
}

fn print_hit(repo: &Path, store: &Store, rank: usize, hit: &Hit) -> Result<()> {
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
        println!("   /// {first}");
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
    scopes: &ScopeSelection,
) -> Result<bool> {
    let groups = multi_hits(store, clauses, scopes)?;
    let limits = distribute_limit(limit, groups.len());
    println!("Best matches ({} topics):\n", groups.len());
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
        if let Some(caveat) = confidence::advice(
            confidence::assess_top(&scores, hits[0].matched.len(), hits[0].total_terms),
            family_size(&hits[..topic_limit.min(hits.len())], 0),
        ) {
            println!("{caveat}\n");
        }
        for (rank, hit) in hits.iter().take(topic_limit).enumerate() {
            print_hit(repo, store, rank, hit)?;
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
        return run_multi(&repo, &store, &clauses, limit, scopes);
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
    if shown > 0
        && let Some(caveat) = confidence::advice(
            confidence::assess_top(&scores, hits[0].matched.len(), hits[0].total_terms),
            family_size(&hits[..shown], 0),
        )
    {
        println!("{caveat}\n");
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
        print_hit(&repo, &store, rank, hit)?;
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

    use super::advice_for;

    #[test]
    fn advice_uses_annotation_when_only_one_hit_is_returned() {
        let hits = vec![json!({
            "score": 100,
            "confidence": "unrated",
            "ranking_margin": {"absolute": null, "permille": null},
            "calibration": {
                "version": "ask-holdout-2026-08-21.v1",
                "sample_size": 0,
                "measured_precision": 0.0,
                "in_calibration": false
            },
            "term_coverage": {"matched": 1, "total": 1, "permille": 1000},
            "verify_required": true,
            "abstain": true,
            "confidence_reason": "no_runner_up",
            "family_size": 1
        })];

        assert_eq!(
            advice_for(&hits).as_deref(),
            Some("abstain: no_runner_up; refine the topic or inspect multiple candidates")
        );
    }
}
