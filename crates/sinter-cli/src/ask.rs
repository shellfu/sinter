//! `sinter ask "<question>"`: a vague question gives a ranked, grouped,
//! content-bearing starting point. Keyword scoring only — no NLP, no LLM;
//! the doc comment is the prose. Design: docs/design-human-query.md.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};
use serde_json::json;
use sinter_core::{Node, Relation, SymbolKind};
use sinter_resolve::qualified_of;
use sinter_store::Store;

use crate::lookup::open_store;
use crate::render::{ellipsize, line_of, location};

// ---- Scoring policy: one table, golden-disciplined (see design §1c). ----
// Changing ANY value below requires a fixture that motivates it.
const PT_EXACT_NAME: i64 = 100;
const PT_NAME_CLOSE: i64 = 60;
const PT_DOC: i64 = 40;
const PT_SIGNATURE: i64 = 30;
const PT_PATH: i64 = 25;
const HUB_CAP: i64 = 20;
/// Family boost: a candidate containing >= this many other candidates
/// inherits its best child's score + 1 — the concept the hits share,
/// bounded by the children's own evidence
/// (fixture: ask_family_boost_surfaces_parent).
const FAMILY_MIN_CHILDREN: usize = 2;
/// Kind prior as (numerator, denominator).
fn kind_prior(kind: SymbolKind) -> (i64, i64) {
    match kind {
        SymbolKind::Struct
        | SymbolKind::Class
        | SymbolKind::Enum
        | SymbolKind::Interface
        | SymbolKind::Trait
        | SymbolKind::TypeAlias => (3, 2),
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Macro => (6, 5),
        SymbolKind::Module | SymbolKind::File => (1, 1),
        _ => (7, 10),
    }
}
const TEST_PENALTY: (i64, i64) = (1, 2);
/// Vendored/generated third-party source: indexed for blast radius, but a
/// vague question wants project code (fixture: ask_dampens_vendored_paths).
const VENDOR_PENALTY: (i64, i64) = (1, 2);

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "at", "be", "been", "by", "can", "could", "do", "does", "find", "for",
    "how", "i", "in", "is", "it", "its", "located", "may", "me", "might", "must", "my", "of", "on",
    "or", "our", "shall", "should", "show", "that", "the", "these", "this", "those", "to", "was",
    "we", "were", "what", "where", "which", "who", "whom", "will", "with", "would", "you", "your",
];

/// Weak verbs that inflate term coverage on unrelated symbols ("work"
/// matching Workspace). Soft: dropped only when a real term remains, so
/// asking for a symbol literally named `work` still works.
/// (fixture: ask_drops_weak_verbs_when_real_terms_remain)
const SOFT_STOPWORDS: &[&str] = &[
    "code", "going", "happen", "happens", "stuff", "thing", "things", "use", "used", "uses",
    "using", "work", "working", "works",
];

/// Question -> distinct lowercase terms, stopworded (design §1a).
fn terms_of(question: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let terms: Vec<String> = question
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty() && !STOPWORDS.contains(t))
        .filter(|t| seen.insert(t.to_string()))
        .map(str::to_string)
        .collect();
    let hard: Vec<String> = terms
        .iter()
        .filter(|t| !SOFT_STOPWORDS.contains(&t.as_str()))
        .cloned()
        .collect();
    if hard.is_empty() { terms } else { hard }
}

/// Term matches with a trailing-`s` second chance (variant, not replacement).
fn contains_term(haystack_lower: &str, term: &str) -> bool {
    haystack_lower.contains(term)
        || term
            .strip_suffix('s')
            .is_some_and(|singular| !singular.is_empty() && haystack_lower.contains(singular))
}

fn is_test_path(file: &str) -> bool {
    file.starts_with("tests/")
        || file.contains("/tests/")
        || file.contains("_test.")
        || file.contains(".test.")
        || file.contains("test_")
}

fn is_vendor_path(file: &str) -> bool {
    let lower = file.to_lowercase();
    lower.split('/').any(|seg| {
        matches!(seg, "vendor" | "third_party" | "node_modules") || seg.contains("generated")
    })
}

struct Hit {
    node: Node,
    score: i64,
    matched: Vec<String>,
    channels: Vec<&'static str>,
    total_terms: usize,
    /// Structural Contains-parent, for the family post-pass.
    parent: Option<String>,
}

fn score_candidates(store: &Store, terms: &[String]) -> Result<Vec<Hit>> {
    // Candidate recall via the TOKENS index (design §4 v2): keyed reads,
    // never a corpus scan. Trigram extras add fuzzy-name candidates —
    // tracked PER TERM: closeness to one term is never credit for another
    // (fixture: ask_trigram_credit_is_per_term).
    let mut nodes = store.candidates_for_terms(terms)?;
    let mut seen: HashSet<String> = nodes.iter().map(|n| n.id.as_str().to_string()).collect();
    let mut close_ids: Vec<HashSet<String>> = Vec::with_capacity(terms.len());
    for term in terms {
        let mut close = HashSet::new();
        for node in store.search(term, 25)? {
            close.insert(node.id.as_str().to_string());
            if seen.insert(node.id.as_str().to_string()) {
                nodes.push(node);
            }
        }
        close_ids.push(close);
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));

    let mut hits = Vec::new();
    for node in nodes {
        let name_l = node.name.to_lowercase();
        let doc_l = node.doc.as_deref().unwrap_or("").to_lowercase();
        let sig_l = node.signature.to_lowercase();
        let file_l = node.file.to_lowercase();
        let mut base = 0i64;
        let mut matched = Vec::new();
        let mut channels: Vec<&'static str> = Vec::new();
        for (ti, term) in terms.iter().enumerate() {
            let mut term_hit = false;
            if name_l == *term || term.strip_suffix('s') == Some(name_l.as_str()) {
                base += PT_EXACT_NAME;
                channels.push("name");
                term_hit = true;
            } else if contains_term(&name_l, term) || close_ids[ti].contains(node.id.as_str()) {
                base += PT_NAME_CLOSE;
                channels.push("name");
                term_hit = true;
            }
            if !doc_l.is_empty() && contains_term(&doc_l, term) {
                base += PT_DOC;
                channels.push("doc");
                term_hit = true;
            }
            if contains_term(&sig_l, term) {
                base += PT_SIGNATURE;
                channels.push("sig");
                term_hit = true;
            }
            if file_l
                .split(['/', '.'])
                .any(|segment| contains_term(segment, term))
            {
                base += PT_PATH;
                channels.push("path");
                term_hit = true;
            }
            if term_hit {
                matched.push(term.clone());
            }
        }
        if base == 0 {
            continue;
        }
        // score = ⌊ base × t × Kn × Pn / (T × Kd × Pd) ⌋ + min(in_degree, cap)
        let (kn, kd) = kind_prior(node.kind);
        let (mut pn, mut pd) = if is_test_path(&node.file) && !terms.iter().any(|t| t == "test") {
            TEST_PENALTY
        } else {
            (1, 1)
        };
        if is_vendor_path(&node.file) {
            pn *= VENDOR_PENALTY.0;
            pd *= VENDOR_PENALTY.1;
        }
        let t = matched.len() as i64;
        let total = terms.len() as i64;
        let mut score = base * t * kn * pn / (total * kd * pd);
        let in_edges = store.in_edges(&node.id)?;
        score += (in_edges.len() as i64).min(HUB_CAP);
        let parent = in_edges
            .iter()
            .find(|e| e.relation == Relation::Contains)
            .map(|e| e.src.as_str().to_string());
        channels.sort();
        channels.dedup();
        hits.push(Hit {
            node,
            score,
            matched,
            channels,
            total_terms: terms.len(),
            parent,
        });
    }
    // Family post-pass: a candidate containing other candidates is the
    // concept those hits share; it inherits its best child's score + 1.
    // Children link structurally (Contains) or — for out-of-class
    // definitions — via a qualified prefix naming exactly one candidate
    // of a member-scope kind.
    let member_scope = |k: SymbolKind| {
        matches!(
            k,
            SymbolKind::Class
                | SymbolKind::Struct
                | SymbolKind::Interface
                | SymbolKind::Trait
                | SymbolKind::Enum
        )
    };
    let mut by_name: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for hit in &hits {
        if member_scope(hit.node.kind) {
            by_name
                .entry(hit.node.name.as_str())
                .or_default()
                .push(hit.node.id.as_str());
        }
    }
    let mut families: std::collections::HashMap<String, (usize, i64)> =
        std::collections::HashMap::new();
    for hit in &hits {
        let structural = hit.parent.clone();
        let named = qualified_of(hit.node.id.as_str())
            .rsplit_once("::")
            .map(|(prefix, _)| prefix.rsplit("::").next().unwrap_or(prefix))
            .and_then(|owner| match by_name.get(owner).map(Vec::as_slice) {
                Some([unique]) if *unique != hit.node.id.as_str() => Some(unique.to_string()),
                _ => None,
            });
        for parent in [structural, named].into_iter().flatten() {
            let entry = families.entry(parent).or_insert((0, 0));
            entry.0 += 1;
            entry.1 = entry.1.max(hit.score);
        }
    }
    for hit in &mut hits {
        if !matches!(hit.node.kind, SymbolKind::File | SymbolKind::Module)
            && let Some((count, best_child)) = families.get(hit.node.id.as_str())
            && *count >= FAMILY_MIN_CHILDREN
            && *best_child + 1 > hit.score
        {
            hit.score = *best_child + 1;
            hit.channels.push("family");
            hit.channels.sort();
        }
    }
    // Deterministic order: score desc, then kind order, file, span start.
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| (a.node.kind as u8).cmp(&(b.node.kind as u8)))
            .then_with(|| a.node.file.cmp(&b.node.file))
            .then_with(|| a.node.span.start.cmp(&b.node.span.start))
    });
    Ok(hits)
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
/// member name.
pub fn run_workspace(manifest: &Path, question: &str, limit: usize) -> Result<()> {
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
        println!("no match for {:?} in any member", terms.join(" "));
        return Ok(());
    }
    println!(
        "Best matches across {} members ({} terms: {}):
",
        ws.members.len(),
        terms.len(),
        terms.join(", ")
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
    Ok(())
}

/// Structured hits — the single shape behind `ask --json` and the MCP
/// `ask` tool.
pub fn ask_json(repo: &Path, question: &str, limit: usize) -> Result<Vec<serde_json::Value>> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
    let terms = terms_of(question);
    if terms.is_empty() {
        bail!("no searchable terms in {question:?} — try naming the thing you're looking for");
    }
    let hits = score_candidates(&store, &terms)?;
    Ok(hits
        .iter()
        .take(limit)
        .map(|h| {
            json!({
                "id": h.node.id.as_str(),
                "qualified": qualified_of(h.node.id.as_str()),
                "name": h.node.name,
                "kind": h.node.kind.as_str(),
                "file": h.node.file,
                "span": {"start": h.node.span.start, "end": h.node.span.end},
                "line": line_of(&repo, &h.node.file, h.node.span.start),
                "signature": h.node.signature,
                "doc": h.node.doc,
                "score": h.score,
                "matched": h.matched,
            })
        })
        .collect())
}

pub fn run(repo: &Path, question: &str, limit: usize, json: bool) -> Result<()> {
    let repo = repo.canonicalize()?;
    if json {
        // ask_json opens the store itself; must run before this function
        // takes its own handle (redb forbids a second in-process open).
        println!(
            "{}",
            serde_json::to_string_pretty(&ask_json(&repo, question, limit)?)?
        );
        return Ok(());
    }
    let store = open_store(&repo)?;
    let terms = terms_of(question);
    if terms.is_empty() {
        bail!("no searchable terms in {question:?} — try naming the thing you're looking for");
    }
    let hits = score_candidates(&store, &terms)?;

    if hits.is_empty() {
        println!("no match for {:?}", terms.join(" "));
        let close = store.search(&terms.join(""), 5)?;
        if !close.is_empty() {
            let names: Vec<&str> = close.iter().map(|n| n.name.as_str()).collect();
            println!("closest symbols: {}", names.join(", "));
        }
        return Ok(());
    }

    println!(
        "Best matches ({} terms: {}):\n",
        terms.len(),
        terms.join(", ")
    );
    for (rank, hit) in hits.iter().take(limit).enumerate() {
        let line = line_of(&repo, &hit.node.file, hit.node.span.start);
        println!(
            "{}. {} {}    [{} {}/{} terms]",
            rank + 1,
            hit.node.kind.as_str(),
            qualified_of(hit.node.id.as_str()),
            hit.channels.join("+"),
            hit.matched.len(),
            hit.total_terms,
        );
        println!("   {}", location(&repo, &hit.node.file, line));
        if let Some(doc) = &hit.node.doc
            && let Some(first) = doc.lines().next()
        {
            println!("   /// {first}");
        }
        if !hit.node.signature.is_empty() {
            println!("   {}", ellipsize(&hit.node.signature, 100));
        }
        let (contains, used_by, extends) = adjacency_counts(&store, &hit.node)?;
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
    Ok(())
}
