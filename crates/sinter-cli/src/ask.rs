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

use crate::lookup::open_store;
use crate::render::{ellipsize, line_of, location};

mod ranking;

use ranking::{Hit, QueryTerm, clauses_of, score_candidates, terms_of};

fn term_text(terms: &[QueryTerm], separator: &str) -> String {
    terms
        .iter()
        .map(QueryTerm::surface)
        .collect::<Vec<_>>()
        .join(separator)
}

/// Score each clause independently, then dedup: a node hit by several
/// clauses shows once, in its best clause (highest score; earlier clause
/// on ties). Per-clause cap keeps total output near the single-topic limit.
fn multi_hits(
    store: &Store,
    clauses: &[(String, Vec<QueryTerm>)],
    limit: usize,
) -> Result<Vec<(String, Vec<Hit>)>> {
    let per = limit.div_ceil(clauses.len()).max(2);
    let mut groups: Vec<(String, Vec<Hit>)> = Vec::with_capacity(clauses.len());
    for (label, terms) in clauses {
        groups.push((label.clone(), score_candidates(store, terms)?));
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
        hits.truncate(per);
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
pub fn run_workspace(manifest: &Path, question: &str, limit: usize) -> Result<bool> {
    let ws = crate::workspace::load(manifest)?;
    let terms = terms_of(question);
    if terms.is_empty() {
        bail!("no searchable terms in {question:?} — try naming the thing you're looking for");
    }
    let mut all: Vec<(String, std::path::PathBuf, Hit)> = Vec::new();
    for (name, repo) in &ws.members {
        let store = crate::lookup::open_store(repo)?;
        for hit in score_candidates(&store, &terms)? {
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
        println!("no match for {:?} in any member", term_text(&terms, " "));
        return Ok(false);
    }
    println!(
        "Best matches across {} members ({} terms: {}):
",
        ws.members.len(),
        terms.len(),
        term_text(&terms, ", ")
    );
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
        "id": h.node.id.as_str(),
        "qualified": qualified_of(h.node.id.as_str()),
        "name": h.node.name,
        "kind": h.node.kind.as_str(),
        "file": h.node.file,
        "span": {"start": h.node.span.start, "end": h.node.span.end},
        "line": line_of(repo, &h.node.file, h.node.span.start),
        "signature": h.node.signature,
        "doc": h.node.doc,
        "score": h.score,
        "matched": h.matched,
        "channels": h.channels,
        "score_breakdown": h.breakdown,
    })
}

/// Structured hits — the single shape behind `ask --json` and the MCP
/// `ask` tool. A multi-topic question keeps the flat array (least breaking
/// for existing consumers) and adds a "topic" field per hit; single-topic
/// output is unchanged.
pub fn ask_json(repo: &Path, question: &str, limit: usize) -> Result<Vec<serde_json::Value>> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
    ask_json_with_store(&repo, &store, question, limit)
}

pub(crate) fn ask_json_current(
    repo: &Path,
    question: &str,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let repo = repo.canonicalize()?;
    let store = crate::lookup::open_current(&repo)?;
    ask_json_with_store(&repo, &store, question, limit)
}

fn ask_json_with_store(
    repo: &Path,
    store: &Store,
    question: &str,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let clauses = clauses_of(question);
    if clauses.len() >= 2 {
        let mut out = Vec::new();
        for (topic, hits) in multi_hits(store, &clauses, limit)? {
            for h in &hits {
                let mut v = hit_json(repo, h);
                v["topic"] = json!(topic);
                out.push(v);
            }
        }
        return Ok(out);
    }
    let terms = terms_of(question);
    if terms.is_empty() {
        bail!("no searchable terms in {question:?} — try naming the thing you're looking for");
    }
    let hits = score_candidates(store, &terms)?;
    Ok(hits.iter().take(limit).map(|h| hit_json(repo, h)).collect())
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
    clauses: &[(String, Vec<QueryTerm>)],
    limit: usize,
) -> Result<bool> {
    let groups = multi_hits(store, clauses, limit)?;
    println!("Best matches ({} topics):\n", groups.len());
    let mut best: Option<(i64, &Hit)> = None;
    for (topic, hits) in &groups {
        println!("## {topic}");
        if hits.is_empty() {
            println!("no match\n");
            continue;
        }
        for (rank, hit) in hits.iter().enumerate() {
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
pub fn run(repo: &Path, question: &str, limit: usize, json: bool) -> Result<bool> {
    let repo = repo.canonicalize()?;
    if json {
        // ask_json opens the store itself; must run before this function
        // takes its own handle (redb forbids a second in-process open).
        let hits = ask_json(&repo, question, limit)?;
        println!("{}", serde_json::to_string_pretty(&hits)?);
        return Ok(!hits.is_empty());
    }
    let store = open_store(&repo)?;
    let clauses = clauses_of(question);
    if clauses.len() >= 2 {
        return run_multi(&repo, &store, &clauses, limit);
    }
    let terms = terms_of(question);
    if terms.is_empty() {
        bail!("no searchable terms in {question:?} — try naming the thing you're looking for");
    }
    let hits = score_candidates(&store, &terms)?;

    if hits.is_empty() {
        println!("no match for {:?}", term_text(&terms, " "));
        let close = store.search(&term_text(&terms, ""), 5)?;
        if !close.is_empty() {
            let names: Vec<&str> = close.iter().map(|n| n.name.as_str()).collect();
            println!("closest symbols: {}", names.join(", "));
        }
        return Ok(false);
    }

    println!(
        "Best matches ({} terms: {}):\n",
        terms.len(),
        term_text(&terms, ", ")
    );
    // Verbose multi-topic questions dilute term coverage; a top hit
    // matching almost nothing is noise wearing a ranking. Say so instead
    // of letting it pass as an answer.
    if terms.len() >= 4 && hits[0].matched.len() * 3 <= terms.len() {
        println!(
            "weak match: best hit covers {}/{} terms — this graph indexes code \
             symbols, not prose docs. Ask one topic at a time with the terms \
             you expect in an identifier or doc comment.\n",
            hits[0].matched.len(),
            terms.len()
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
