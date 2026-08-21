//! Score one candidate symbol against a parsed `Query`. Policy lives in
//! the point constants; a value change requires an evaluation run.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::Serialize;
use sinter_core::{Node, Relation, SymbolKind};
use sinter_resolve::qualified_of;
use sinter_store::Store;

use super::query::{Query, identifier_tokens};

// Scoring policy. A value change requires an evaluation or focused fixture.
const PT_EXACT_NAME: i64 = 100;
const PT_NAME_CLOSE: i64 = 60;
const PT_OWNER: i64 = 45;
const PT_DOC: i64 = 40;
const PT_SIGNATURE: i64 = 30;
const PT_PATH: i64 = 25;
/// Action verb appears inside the name (`nextNull` for "next").
const PT_ACTION_NAME: i64 = 25;
/// Action verb is a whole identifier token (`peek`, `do_teardown_request`).
const PT_ACTION_TOKEN: i64 = 70;
/// Full credit when every identifier token is a query term; each unmatched
/// token costs as much as a matched one earns, so `ExactArgs` beats
/// `ExactValidArgs` and `render_template` beats `render_template_string`.
const PT_NAME_PRECISION: i64 = 40;
/// Per adjacent query pair that appears in the same order in the name.
const PT_PHRASE_NAME: i64 = 20;
/// Per adjacent query pair that appears verbatim in the doc comment.
const PT_PHRASE_DOC: i64 = 10;
/// Doc credit reads only the leading summary; a long type-level essay
/// should not outscore the member that does the work.
const DOC_SUMMARY_CHARS: usize = 400;
/// A query term that appears only in the name of something this symbol
/// calls: the entry point that orchestrates `preprocess_request` answers
/// "dispatched with preprocessing" even though its own name does not.
const PT_CALLEE: i64 = 30;
/// Only the leading hits earn callee credit; it is a rerank, not retrieval.
const CALLEE_RERANK_DEPTH: usize = 30;
const HUB_CAP: i64 = 20;
const FAMILY_MIN_CHILDREN: usize = 2;
const TEST_PENALTY: (i64, i64) = (1, 3);
const VENDOR_PENALTY: (i64, i64) = (1, 2);

#[derive(Debug, Default, Serialize)]
pub(super) struct ScoreBreakdown {
    name: i64,
    doc: i64,
    signature: i64,
    path: i64,
    action_name_bonus: i64,
    name_precision: i64,
    phrase: i64,
    callee: i64,
    evidence: i64,
    coverage_numerator: usize,
    coverage_denominator: usize,
    kind_numerator: i64,
    kind_denominator: i64,
    penalty_numerator: i64,
    penalty_denominator: i64,
    hub_bonus: i64,
    family_bonus: i64,
    final_score: i64,
}

pub(super) struct Hit {
    pub(super) node: Node,
    pub(super) score: i64,
    pub(super) matched: Vec<String>,
    pub(super) channels: Vec<&'static str>,
    /// Query roles this hit satisfied: "action", "phrase", "owner".
    pub(super) roles: Vec<&'static str>,
    pub(super) total_terms: usize,
    pub(super) breakdown: ScoreBreakdown,
    parent: Option<String>,
}

fn is_test_path(file: &str) -> bool {
    file.starts_with("tests/")
        || file.starts_with("test/")
        || file.contains("/tests/")
        || file.contains("/test/")
        || file.contains("/testing.")
        || file.contains("_test.")
        || file.contains(".test.")
        || file.contains("test_")
}

/// `TestFoo`, `FooTest::bar`, `test_foo`: a test symbol outside a test path.
fn is_test_name(qualified: &str) -> bool {
    qualified.split("::").any(|segment| {
        let lower = segment.to_lowercase();
        lower.starts_with("test") || lower.ends_with("test") || lower.ends_with("tests")
    })
}

fn is_vendor_path(file: &str) -> bool {
    file.to_lowercase().split('/').any(|segment| {
        matches!(segment, "vendor" | "third_party" | "node_modules")
            || segment.contains("generated")
    })
}

fn kind_prior(kind: SymbolKind, action_query: bool) -> (i64, i64) {
    if action_query {
        return match kind {
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Macro => (3, 2),
            SymbolKind::Struct
            | SymbolKind::Class
            | SymbolKind::Enum
            | SymbolKind::Interface
            | SymbolKind::Trait
            | SymbolKind::TypeAlias
            | SymbolKind::Module
            | SymbolKind::File => (1, 1),
            // Prose describes behavior; it never is the behavior.
            SymbolKind::Section => (1, 2),
            _ => (7, 10),
        };
    }
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

fn doc_summary(doc: &str) -> String {
    let lower = doc.to_lowercase();
    match lower.char_indices().nth(DOC_SUMMARY_CHARS) {
        Some((cut, _)) => lower[..cut].to_owned(),
        None => lower,
    }
}

/// Owner segment of a qualified name: `JsonReader` in `JsonReader::peek`.
fn owner_of(qualified: &str) -> Option<&str> {
    qualified
        .rsplit_once("::")
        .map(|(prefix, _)| prefix.rsplit("::").next().unwrap_or(prefix))
}

/// Count of adjacent query pairs whose tokens appear consecutively, in
/// order, in `tokens`.
fn name_phrase_hits(tokens: &[String], query: &Query) -> usize {
    query
        .phrases()
        .iter()
        .filter(|(first, second)| {
            let (first, second) = (&query.terms()[*first], &query.terms()[*second]);
            tokens
                .windows(2)
                .any(|pair| first.matches_token(&pair[0]) && second.matches_token(&pair[1]))
        })
        .count()
}

/// Count of adjacent query pairs that appear verbatim ("a b") in `doc`.
fn doc_phrase_hits(doc: &str, query: &Query) -> usize {
    if doc.is_empty() {
        return 0;
    }
    query
        .phrases()
        .iter()
        .filter(|(first, second)| {
            let (first, second) = (&query.terms()[*first], &query.terms()[*second]);
            first.variants().iter().any(|a| {
                second
                    .variants()
                    .iter()
                    .any(|b| doc.contains(&format!("{a} {b}")))
            })
        })
        .count()
}

/// Where one query term touched one candidate.
#[derive(Clone, Copy, Default)]
struct TermEvidence {
    name_exact: bool,
    name_close: bool,
    owner: bool,
    /// An identifier token equals an action-verb variant of the term.
    action_token: bool,
    doc: bool,
    signature: bool,
    path: bool,
}

impl TermEvidence {
    fn any(self) -> bool {
        self.name_exact || self.name_close || self.owner || self.doc || self.signature || self.path
    }
}

struct Candidate {
    node: Node,
    name_tokens: Vec<String>,
    doc: String,
    evidence: Vec<TermEvidence>,
    matched_name_tokens: Vec<bool>,
}

fn gather(node: Node, query: &Query, close_ids: &[HashSet<String>]) -> Candidate {
    let name = node.name.to_lowercase();
    let qualified = qualified_of(node.id.as_str()).to_lowercase();
    let owner = owner_of(&qualified).unwrap_or("").to_owned();
    let name_tokens = identifier_tokens(&node.name);
    let doc = doc_summary(node.doc.as_deref().unwrap_or(""));
    let signature = node.signature.to_lowercase();
    let file = node.file.to_lowercase();
    let mut matched_name_tokens = vec![false; name_tokens.len()];
    let evidence = query
        .terms()
        .iter()
        .enumerate()
        .map(|(index, term)| {
            let token_hit = name_tokens.iter().any(|token| term.matches_token(token));
            let mut hit = TermEvidence {
                name_exact: term.variants().iter().any(|variant| *variant == name),
                name_close: token_hit
                    || term.occurs_in(&name)
                    || close_ids[index].contains(node.id.as_str()),
                owner: !owner.is_empty() && term.occurs_in(&owner),
                doc: !doc.is_empty() && term.occurs_in(&doc),
                signature: term.occurs_in(&signature),
                path: file
                    .split(['/', '.'])
                    .any(|segment| term.occurs_in(segment)),
                ..TermEvidence::default()
            };
            for (token, flag) in name_tokens.iter().zip(matched_name_tokens.iter_mut()) {
                if term.matches_token(token) {
                    *flag = true;
                    if term.is_action() && term.is_core_token(token) {
                        hit.action_token = true;
                    }
                }
            }
            hit
        })
        .collect();
    Candidate {
        node,
        name_tokens,
        doc,
        evidence,
        matched_name_tokens,
    }
}

pub(super) fn score_candidates(store: &Store, query: &Query) -> Result<Vec<Hit>> {
    let terms = query.terms();
    let variants = terms
        .iter()
        .map(|term| term.variants().to_vec())
        .collect::<Vec<_>>();
    let action_query = query.is_action();

    let mut nodes = store.candidates_for_term_variants(&variants)?;
    let mut seen = nodes
        .iter()
        .map(|node| node.id.as_str().to_owned())
        .collect::<HashSet<_>>();
    let mut close_ids = Vec::with_capacity(terms.len());
    for term in terms {
        let mut close = HashSet::new();
        for variant in term.variants() {
            for node in store.search(variant, 25)? {
                close.insert(node.id.as_str().to_owned());
                if seen.insert(node.id.as_str().to_owned()) {
                    nodes.push(node);
                }
            }
        }
        close_ids.push(close);
    }
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    let candidate_ids = nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>();
    let incoming = store.in_edges_many(&candidate_ids)?;

    let candidates = nodes
        .into_iter()
        .map(|node| gather(node, query, &close_ids))
        .collect::<Vec<_>>();
    let test_query = terms.iter().any(|term| term.surface() == "test");

    let mut hits = Vec::new();
    for candidate in candidates {
        let Candidate {
            node,
            name_tokens,
            doc,
            evidence,
            matched_name_tokens,
        } = candidate;
        let mut breakdown = ScoreBreakdown::default();
        let mut matched = Vec::new();
        let mut channels = Vec::new();
        let mut roles = Vec::new();
        for (index, term) in terms.iter().enumerate() {
            let hit = evidence[index];
            if hit.name_exact {
                breakdown.name += PT_EXACT_NAME;
                channels.push("name");
            } else if hit.name_close {
                breakdown.name += PT_NAME_CLOSE;
                channels.push("name");
            } else if hit.owner {
                breakdown.name += PT_OWNER;
                channels.push("owner");
                roles.push("owner");
            }
            if term.is_action() && hit.action_token {
                breakdown.action_name_bonus += PT_ACTION_TOKEN;
                channels.push("action-name");
                roles.push("action");
            } else if term.is_action() && (hit.name_exact || hit.name_close) {
                breakdown.action_name_bonus += PT_ACTION_NAME;
                channels.push("action-name");
                roles.push("action");
            }
            if hit.doc {
                breakdown.doc += PT_DOC;
                channels.push("doc");
            }
            if hit.signature {
                breakdown.signature += PT_SIGNATURE;
                channels.push("sig");
            }
            if hit.path {
                breakdown.path += PT_PATH;
                channels.push("path");
            }
            if hit.any() {
                matched.push(term.surface().to_owned());
            }
        }
        let matched_tokens = matched_name_tokens.iter().filter(|hit| **hit).count() as i64;
        if matched_tokens > 0 {
            let total = name_tokens.len() as i64;
            breakdown.name_precision = PT_NAME_PRECISION * (2 * matched_tokens - total) / total;
        }
        let name_phrases = name_phrase_hits(&name_tokens, query);
        let doc_phrases = doc_phrase_hits(&doc, query);
        breakdown.phrase =
            PT_PHRASE_NAME * name_phrases as i64 + PT_PHRASE_DOC * doc_phrases as i64;
        if name_phrases + doc_phrases > 0 {
            channels.push("phrase");
            roles.push("phrase");
        }
        if breakdown.name
            + breakdown.doc
            + breakdown.signature
            + breakdown.path
            + breakdown.action_name_bonus
            + breakdown.name_precision
            + breakdown.phrase
            == 0
        {
            continue;
        }

        let (kind_numerator, kind_denominator) = kind_prior(node.kind, action_query);
        let (mut penalty_numerator, mut penalty_denominator) = if !test_query
            && (is_test_path(&node.file) || is_test_name(qualified_of(node.id.as_str())))
        {
            TEST_PENALTY
        } else {
            (1, 1)
        };
        if is_vendor_path(&node.file) {
            penalty_numerator *= VENDOR_PENALTY.0;
            penalty_denominator *= VENDOR_PENALTY.1;
        }
        let in_edges = incoming
            .get(&node.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        breakdown.hub_bonus = (in_edges.len() as i64).min(HUB_CAP);
        breakdown.coverage_numerator = matched.len();
        breakdown.coverage_denominator = terms.len();
        breakdown.kind_numerator = kind_numerator;
        breakdown.kind_denominator = kind_denominator;
        breakdown.penalty_numerator = penalty_numerator;
        breakdown.penalty_denominator = penalty_denominator;
        let score = combine(&mut breakdown);
        let parent = in_edges
            .iter()
            .find(|edge| edge.relation == Relation::Contains)
            .map(|edge| edge.src.as_str().to_owned());
        channels.sort_unstable();
        channels.dedup();
        roles.sort_unstable();
        roles.dedup();
        hits.push(Hit {
            node,
            score,
            matched,
            channels,
            roles,
            total_terms: terms.len(),
            breakdown,
            parent,
        });
    }
    // A question about behavior wants the member, never the type that
    // owns it; the family boost only serves noun questions.
    if !action_query {
        apply_family_boost(&mut hits);
    }
    sort_hits(&mut hits);
    apply_callee_evidence(store, query, &mut hits)?;
    sort_hits(&mut hits);
    Ok(hits)
}

fn sort_hits(hits: &mut [Hit]) {
    hits.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| (left.node.kind as u8).cmp(&(right.node.kind as u8)))
            .then_with(|| left.node.file.cmp(&right.node.file))
            .then_with(|| left.node.span.start.cmp(&right.node.span.start))
    });
}

/// evidence × coverage × kind prior × penalties + hub bonus.
fn combine(breakdown: &mut ScoreBreakdown) -> i64 {
    breakdown.evidence = breakdown.name
        + breakdown.doc
        + breakdown.signature
        + breakdown.path
        + breakdown.action_name_bonus
        + breakdown.name_precision
        + breakdown.phrase
        + breakdown.callee;
    let score = breakdown.evidence
        * breakdown.coverage_numerator as i64
        * breakdown.kind_numerator
        * breakdown.penalty_numerator
        / (breakdown.coverage_denominator as i64
            * breakdown.kind_denominator
            * breakdown.penalty_denominator)
        + breakdown.hub_bonus;
    breakdown.final_score = score;
    score
}

/// Rerank the leading hits with one hop of call evidence: a term the
/// symbol itself never mentions, but a direct callee is named after.
fn apply_callee_evidence(store: &Store, query: &Query, hits: &mut [Hit]) -> Result<()> {
    let depth = hits.len().min(CALLEE_RERANK_DEPTH);
    let ids = hits[..depth]
        .iter()
        .map(|hit| hit.node.id.clone())
        .collect::<Vec<_>>();
    let outgoing = store.out_edges_many(&ids)?;
    for hit in &mut hits[..depth] {
        let Some(edges) = outgoing.get(&hit.node.id) else {
            continue;
        };
        let callee_tokens = edges
            .iter()
            .filter(|edge| edge.relation == Relation::Calls)
            .flat_map(|edge| {
                let qualified = qualified_of(edge.dst.as_str());
                identifier_tokens(qualified.rsplit("::").next().unwrap_or(qualified))
            })
            .collect::<HashSet<_>>();
        if callee_tokens.is_empty() {
            continue;
        }
        let mut gained = false;
        for term in query.terms() {
            if hit.matched.iter().any(|m| m == term.surface()) {
                continue;
            }
            if callee_tokens.iter().any(|token| term.matches_token(token)) {
                hit.matched.push(term.surface().to_owned());
                hit.breakdown.callee += PT_CALLEE;
                gained = true;
            }
        }
        if gained {
            hit.breakdown.coverage_numerator = hit.matched.len();
            hit.channels.push("callee");
            hit.roles.push("callee");
            hit.score = combine(&mut hit.breakdown);
        }
    }
    Ok(())
}

fn apply_family_boost(hits: &mut [Hit]) {
    let member_scope = |kind: SymbolKind| {
        matches!(
            kind,
            SymbolKind::Class
                | SymbolKind::Struct
                | SymbolKind::Interface
                | SymbolKind::Trait
                | SymbolKind::Enum
        )
    };
    let mut by_name: HashMap<&str, Vec<&str>> = HashMap::new();
    for hit in hits.iter() {
        if member_scope(hit.node.kind) {
            by_name
                .entry(hit.node.name.as_str())
                .or_default()
                .push(hit.node.id.as_str());
        }
    }
    let mut families: HashMap<String, (usize, i64)> = HashMap::new();
    for hit in hits.iter() {
        let structural = hit.parent.clone();
        let named = qualified_of(hit.node.id.as_str())
            .rsplit_once("::")
            .map(|(prefix, _)| prefix.rsplit("::").next().unwrap_or(prefix))
            .and_then(|owner| match by_name.get(owner).map(Vec::as_slice) {
                Some([unique]) if *unique != hit.node.id.as_str() => Some((*unique).to_owned()),
                _ => None,
            });
        for parent in [structural, named].into_iter().flatten() {
            let entry = families.entry(parent).or_insert((0, 0));
            entry.0 += 1;
            entry.1 = entry.1.max(hit.score);
        }
    }
    for hit in hits {
        if matches!(hit.node.kind, SymbolKind::File | SymbolKind::Module) {
            continue;
        }
        if let Some((count, best_child)) = families.get(hit.node.id.as_str())
            && *count >= FAMILY_MIN_CHILDREN
            && *best_child + 1 > hit.score
        {
            let boosted = *best_child + 1;
            hit.breakdown.family_bonus = boosted - hit.score;
            hit.breakdown.final_score = boosted;
            hit.score = boosted;
            hit.channels.push("family");
            hit.channels.sort_unstable();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::query::{Query, identifier_tokens};
    use super::{doc_phrase_hits, name_phrase_hits, owner_of};

    #[test]
    fn phrases_reward_ordered_adjacent_tokens() {
        let query = Query::parse("where is zsh completion generated");
        assert_eq!(
            name_phrase_hits(&identifier_tokens("GenZshCompletion"), &query),
            1
        );
        assert_eq!(
            name_phrase_hits(&identifier_tokens("CompletionZsh"), &query),
            0
        );
        assert_eq!(doc_phrase_hits("writes zsh completion to w", &query), 1);
    }

    #[test]
    fn owner_is_last_qualifier() {
        assert_eq!(owner_of("jsonreader::peek"), Some("jsonreader"));
        assert_eq!(owner_of("a::b::c"), Some("b"));
        assert_eq!(owner_of("peek"), None);
    }
}
