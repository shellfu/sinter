//! Score one candidate symbol against a parsed `Query`. Policy lives in
//! the point constants; a value change requires an evaluation run.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::Serialize;
use sinter_core::{CorpusScope, Node, Relation, SymbolKind};
use sinter_resolve::qualified_of;
use sinter_store::{EdgeFilter, Store};

use super::query::{Query, identifier_tokens};
use crate::corpus::ScopeSelection;

/// Every field match is weighed by the term's rarity, ln(1 + N/df) in
/// permille, so `cedar` (a handful of symbols) outweighs `events` (a
/// thousand docs). Clamped: glue words never vanish, one-off words never
/// dominate on their own.
const IDF_FLOOR_PERMILLE: i64 = 500;
const IDF_CEILING_PERMILLE: i64 = 4000;
/// The rarest query term names the domain (`cedar`, `online`); a hit that
/// never mentions it keeps half its score, however many common terms it
/// matched. Only applies when the term exists somewhere in the corpus.
const RAREST_MISS_PERMILLE: i64 = 500;
/// Relational questions (two or more topics): the leading hits per topic
/// are checked for a graph connection within this many hops, and a
/// connected pair is boosted. 5 × 5 = 25 pair checks per topic pair.
const PLANNER_TOP: usize = 5;
const PLANNER_HOPS: usize = 3;
const CONNECTED_PERMILLE: i64 = 1300;

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
/// A query term that appears only inside the symbol's body (a local, a
/// callee argument, a comment): a quarter of a doc match at full IDF,
/// scaled toward zero for words most bodies use.
const PT_BODY: i64 = 10;
/// A term matched only through a query synonym (`budget` for "cap") earns
/// this share of its weight, so a literal match always outranks it.
const SYNONYM_PERMILLE: i64 = 800;
/// Body-only candidates pulled in per term variant; retrieval, not recall.
const BODY_RETRIEVAL_CAP: usize = 50;
/// A body word carried by more than this share of nodes is glue, not topic.
const BODY_DF_CEILING_PERMILLE: u64 = 100;
const HUB_CAP: i64 = 20;
const FAMILY_MIN_CHILDREN: usize = 2;
/// Scope prior, permille. An agent asking "how does X reach Y" wants the
/// production symbol; a test fixture named `request()` matches the same
/// words but is not the answer. Docs sit just under production (prose
/// about the thing is a fair second), tests at roughly half (they name
/// the behavior but do not implement it), fixtures/examples lower still
/// (they imitate production names wholesale), generated/vendor last
/// (copied or machine-written, rarely what a question is about).
const PRIOR_PRODUCTION: i64 = 1000;
const PRIOR_DOCS: i64 = 900;
const PRIOR_TEST: i64 = 600;
const PRIOR_FIXTURE: i64 = 400;
const PRIOR_GENERATED: i64 = 300;

fn scope_prior(scope: CorpusScope) -> i64 {
    match scope {
        CorpusScope::Production => PRIOR_PRODUCTION,
        CorpusScope::Docs => PRIOR_DOCS,
        CorpusScope::Test => PRIOR_TEST,
        CorpusScope::Fixture | CorpusScope::Example => PRIOR_FIXTURE,
        CorpusScope::Generated | CorpusScope::Vendor => PRIOR_GENERATED,
    }
}

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
    body: i64,
    evidence: i64,
    /// Terms matched only in the body count half toward coverage.
    pub(super) body_only: usize,
    pub(super) coverage_numerator: usize,
    pub(super) coverage_denominator: usize,
    kind_numerator: i64,
    kind_denominator: i64,
    penalty_numerator: i64,
    penalty_denominator: i64,
    hub_bonus: i64,
    family_bonus: i64,
    /// Per-term IDF weight, permille, in query-term order.
    idf_permille: Vec<i64>,
    /// The rarest query term is absent from every field of this hit.
    rarest_miss: bool,
    /// Boosted because a hit in the neighbouring topic is graph-connected.
    connected: bool,
    final_score: i64,
}

/// The query term that names the domain: the one with the fewest field
/// hits, when it is at least `RAREST_MARGIN` times rarer than the runner-up.
/// Nouns only: an inflected verb (`configured`) can be just as rare without
/// naming anything, and two equally rare words (`sub`, `mounted`) do not
/// single out one.
const RAREST_MARGIN: usize = 2;

fn rarest_term<'q>(query: &'q Query, df: &[usize]) -> Option<&'q str> {
    let mut ranked = (0..df.len())
        .filter(|&index| df[index] >= 1 && !query.terms()[index].is_action())
        .collect::<Vec<_>>();
    ranked.sort_by_key(|&index| df[index]);
    match ranked.as_slice() {
        [] => None,
        [only] => Some(query.terms()[*only].surface()),
        [first, second, ..] if df[*first] * RAREST_MARGIN <= df[*second] => {
            Some(query.terms()[*first].surface())
        }
        _ => None,
    }
}

/// ln(1 + N/df) in permille, clamped; df 0 weighs like df 1.
fn idf_permille(total: u64, df: usize) -> i64 {
    let ratio = total.max(1) as f64 / df.max(1) as f64;
    (((1.0 + ratio).ln() * 1000.0) as i64).clamp(IDF_FLOOR_PERMILLE, IDF_CEILING_PERMILLE)
}

/// `points` scaled by a permille weight.
fn weigh(points: i64, permille: i64) -> i64 {
    points * permille / 1000
}

pub(super) struct Hit {
    pub(super) node: Node,
    pub(super) scope: CorpusScope,
    pub(super) score: i64,
    pub(super) matched: Vec<String>,
    /// Subset of `matched` whose only evidence is a body word.
    body_matched: Vec<String>,
    pub(super) channels: Vec<&'static str>,
    /// Query roles this hit satisfied: "action", "phrase", "owner".
    pub(super) roles: Vec<&'static str>,
    pub(super) total_terms: usize,
    pub(super) breakdown: ScoreBreakdown,
    /// Lower-ranked hits with the same bare name, folded into this one so
    /// the visible top-k carries distinct names (`build` absorbs
    /// `Index::build`).
    pub(super) variants: Vec<String>,
    parent: Option<String>,
}

impl Hit {
    /// Term coverage with body-only terms at half credit.
    pub(super) fn coverage(&self) -> (usize, usize) {
        coverage(&self.breakdown)
    }
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

fn kind_prior(kind: SymbolKind, query: &Query) -> (i64, i64) {
    if kind == SymbolKind::Section {
        // Prose describes behavior; it never is the behavior. Only a
        // question that reaches for prose lets a section stand level.
        return if query.wants_docs() {
            (1, 1)
        } else if query.is_engineering() {
            (1, 2)
        } else {
            (7, 10)
        };
    }
    if query.is_action() {
        return match kind {
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Macro => (3, 2),
            SymbolKind::Struct
            | SymbolKind::Class
            | SymbolKind::Enum
            | SymbolKind::Interface
            | SymbolKind::Trait
            | SymbolKind::TypeAlias
            | SymbolKind::Table
            | SymbolKind::View
            | SymbolKind::Module
            | SymbolKind::File => (1, 1),
            _ => (7, 10),
        };
    }
    match kind {
        SymbolKind::Struct
        | SymbolKind::Class
        | SymbolKind::Enum
        | SymbolKind::Interface
        | SymbolKind::Trait
        | SymbolKind::TypeAlias
        | SymbolKind::Table
        | SymbolKind::View => (3, 2),
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

/// Trigram closeness alone credits `sync` to `syntax_error_files`; a
/// close name must also share a four-character prefix with one token.
const FUZZY_PREFIX_CHARS: usize = 4;

fn shares_prefix(variants: &[String], tokens: &[String]) -> bool {
    variants.iter().any(|variant| {
        tokens.iter().any(|token| {
            let shared = variant
                .chars()
                .zip(token.chars())
                .take_while(|(a, b)| a == b)
                .count();
            shared >= FUZZY_PREFIX_CHARS.min(variant.len())
        })
    })
}

/// Owner segment of a qualified name: `JsonReader` in `JsonReader::peek`.
fn owner_of(qualified: &str) -> Option<&str> {
    qualified
        .rsplit_once("::")
        .map(|(prefix, _)| prefix.rsplit("::").next().unwrap_or(prefix))
}

/// Summed weight (permille, rarer term of each pair) of adjacent query
/// pairs whose tokens appear consecutively, in order, in `tokens`.
fn name_phrase_hits(tokens: &[String], query: &Query, idf: &[i64]) -> i64 {
    query
        .phrases()
        .iter()
        .filter(|(first, second)| {
            let (first, second) = (&query.terms()[*first], &query.terms()[*second]);
            tokens
                .windows(2)
                .any(|pair| first.matches_token(&pair[0]) && second.matches_token(&pair[1]))
        })
        .map(|(first, second)| idf[*first].max(idf[*second]))
        .sum()
}

/// Summed weight of adjacent query pairs that appear verbatim ("a b") in
/// `doc`.
fn doc_phrase_hits(doc: &str, query: &Query, idf: &[i64]) -> i64 {
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
        .map(|(first, second)| idf[*first].max(idf[*second]))
        .sum()
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
    /// The term appears only inside the body (see `FileFacts::body_terms`).
    body: bool,
    /// Every field hit came through a synonym; no literal form occurs.
    synonym_only: bool,
}

impl TermEvidence {
    fn any(self) -> bool {
        self.name_exact || self.name_close || self.owner || self.doc || self.signature || self.path
    }
}

/// Per-term body evidence: which node ids carry a variant as a body word,
/// and that variant's IDF weight in permille (0 for glue words).
struct BodyEvidence {
    ids: HashSet<String>,
    idf_permille: i64,
}

struct Candidate {
    node: Node,
    name_tokens: Vec<String>,
    doc: String,
    evidence: Vec<TermEvidence>,
    matched_name_tokens: Vec<bool>,
}

fn gather(
    node: Node,
    query: &Query,
    close_ids: &[HashSet<String>],
    body: &[BodyEvidence],
) -> Candidate {
    let name = node.name.to_lowercase();
    let qualified = qualified_of(node.id.as_str()).to_lowercase();
    let owner = owner_of(&qualified).unwrap_or("").to_owned();
    let name_tokens = identifier_tokens(&node.name);
    let doc = doc_summary(node.doc.as_deref().unwrap_or(""));
    // The signature always echoes the name; credit it only for what it
    // adds (parameters, return type), or a bare name match on a common
    // word like `request` double-counts and beats a real doc match.
    let signature = node.signature.to_lowercase().replacen(&name, "", 1);
    let file = node.file.to_lowercase();
    let mut matched_name_tokens = vec![false; name_tokens.len()];
    let evidence = query
        .terms()
        .iter()
        .enumerate()
        .map(|(index, term)| {
            let token_hit = name_tokens.iter().any(|token| term.matches_token(token));
            let mut hit = TermEvidence {
                name_exact: term.variants().contains(&name),
                name_close: token_hit
                    || term.occurs_in(&name)
                    || (close_ids[index].contains(node.id.as_str())
                        && shares_prefix(term.variants(), &name_tokens)),
                owner: !owner.is_empty() && term.occurs_in(&owner),
                doc: !doc.is_empty() && term.occurs_in(&doc),
                signature: term.occurs_in(&signature),
                path: file
                    .split(['/', '.'])
                    .any(|segment| term.occurs_in(segment)),
                body: body[index].ids.contains(node.id.as_str()),
                ..TermEvidence::default()
            };
            let literal = [&name, &owner, &doc, &signature]
                .iter()
                .any(|field| term.core_occurs_in(field))
                || file
                    .split(['/', '.'])
                    .any(|segment| term.core_occurs_in(segment))
                || name_tokens.iter().any(|token| term.abbreviates(token));
            hit.synonym_only = hit.any() && !literal;
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

pub(super) fn score_candidates(
    store: &Store,
    query: &Query,
    scopes: &ScopeSelection,
) -> Result<Vec<Hit>> {
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
    let body = body_evidence(store, query, &mut nodes, &mut seen)?;
    // A question about tests wants test symbols even though the default
    // scope (production, docs) leaves them out.
    let test_query = terms
        .iter()
        .any(|term| term.variants().iter().any(|variant| variant == "test"));
    let file_scopes = store.file_scopes()?;
    let scope_of = |node: &Node| {
        file_scopes
            .get(&node.file)
            .copied()
            .unwrap_or_else(|| CorpusScope::classify_path(&node.file))
    };
    nodes.retain(|node| {
        let scope = scope_of(node);
        scopes.contains(scope) || (test_query && scope == CorpusScope::Test)
    });
    // A question about tests, or a `--scope` that excludes production,
    // asked for those symbols on purpose: rank them on evidence alone.
    let flat_prior = test_query || !scopes.contains(CorpusScope::Production);
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    let candidate_ids = nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>();
    let incoming = store.in_edges_many(&candidate_ids)?;

    let candidates = nodes
        .into_iter()
        .map(|node| gather(node, query, &close_ids, &body))
        .collect::<Vec<_>>();
    // Document frequency over the retrieved pool: token retrieval is
    // exhaustive per term, so a field hit count here is the corpus count
    // (plus the few fuzzy extras), without a second index walk.
    let df = (0..terms.len())
        .map(|index| {
            candidates
                .iter()
                .filter(|candidate| candidate.evidence[index].any())
                .count()
        })
        .collect::<Vec<_>>();
    let total = store.node_count()?;
    let idf = df
        .iter()
        .map(|&df| idf_permille(total, df))
        .collect::<Vec<_>>();
    let rarest = rarest_term(query, &df);

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
        let mut body_matched = Vec::new();
        let mut channels = Vec::new();
        let mut roles = Vec::new();
        for (index, term) in terms.iter().enumerate() {
            let hit = evidence[index];
            let w = if hit.synonym_only {
                weigh(idf[index], SYNONYM_PERMILLE)
            } else {
                idf[index]
            };
            if hit.name_exact {
                breakdown.name += weigh(PT_EXACT_NAME, w);
                channels.push("name");
            } else if hit.name_close {
                breakdown.name += weigh(PT_NAME_CLOSE, w);
                channels.push("name");
            } else if hit.owner {
                breakdown.name += weigh(PT_OWNER, w);
                channels.push("owner");
                roles.push("owner");
            }
            if term.is_action() && hit.action_token {
                breakdown.action_name_bonus += weigh(PT_ACTION_TOKEN, w);
                channels.push("action-name");
                roles.push("action");
            } else if term.is_action() && (hit.name_exact || hit.name_close) {
                breakdown.action_name_bonus += weigh(PT_ACTION_NAME, w);
                channels.push("action-name");
                roles.push("action");
            }
            if hit.doc {
                breakdown.doc += weigh(PT_DOC, w);
                channels.push("doc");
            }
            if hit.signature {
                breakdown.signature += weigh(PT_SIGNATURE, w);
                channels.push("sig");
            }
            if hit.path {
                breakdown.path += weigh(PT_PATH, w);
                channels.push("path");
            }
            if hit.any() {
                matched.push(term.surface().to_owned());
            } else if hit.body && body[index].idf_permille > 0 {
                breakdown.body += PT_BODY * body[index].idf_permille / 1000;
                breakdown.body_only += 1;
                channels.push("body");
                matched.push(term.surface().to_owned());
                body_matched.push(term.surface().to_owned());
            }
        }
        let matched_tokens = matched_name_tokens.iter().filter(|hit| **hit).count() as i64;
        if matched_tokens > 0 {
            let total = name_tokens.len() as i64;
            breakdown.name_precision = PT_NAME_PRECISION * (2 * matched_tokens - total) / total;
        }
        let name_phrases = name_phrase_hits(&name_tokens, query, &idf);
        let doc_phrases = doc_phrase_hits(&doc, query, &idf);
        breakdown.phrase = weigh(PT_PHRASE_NAME, name_phrases) + weigh(PT_PHRASE_DOC, doc_phrases);
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
            + breakdown.body
            == 0
        {
            continue;
        }

        let (kind_numerator, kind_denominator) = kind_prior(node.kind, query);
        let scope = scope_of(&node);
        let penalty_numerator = if flat_prior {
            PRIOR_PRODUCTION
        } else if scope == CorpusScope::Production && is_vendor_path(&node.file) {
            PRIOR_GENERATED
        } else if scope == CorpusScope::Production
            && (is_test_path(&node.file) || is_test_name(qualified_of(node.id.as_str())))
        {
            // Store classification missed it; the path or name says test.
            PRIOR_TEST
        } else {
            scope_prior(scope)
        };
        let penalty_denominator = PRIOR_PRODUCTION;
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
        breakdown.idf_permille = idf.clone();
        breakdown.rarest_miss = rarest.is_some_and(|term| !matched.iter().any(|m| m == term));
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
            scope,
            node,
            score,
            matched,
            body_matched,
            channels,
            roles,
            total_terms: terms.len(),
            breakdown,
            variants: Vec::new(),
            parent,
        });
    }
    // A question about behavior wants the member, never the type that
    // owns it; the family boost only serves noun questions.
    if !action_query {
        apply_family_boost(&mut hits);
    }
    sort_hits(&mut hits);
    apply_callee_evidence(store, query, rarest, &mut hits)?;
    sort_hits(&mut hits);
    Ok(cluster_same_name(collapse_same_name(hits)))
}

/// Relational planner: for each neighbouring topic pair, a leading hit on
/// one side that reaches (or is reached by) a leading hit on the other
/// within `PLANNER_HOPS` is the pair the question is about. Both are
/// boosted and the pair is reported as `"left -> right"`. `connected`
/// answers whether two ids are graph-connected; the store-backed version
/// is `connect_topics`.
pub(super) fn boost_connected(
    groups: &mut [(String, Vec<Hit>)],
    mut connected: impl FnMut(&Node, &Node) -> Result<bool>,
) -> Result<Vec<String>> {
    let mut pairs = Vec::new();
    for at in 1..groups.len() {
        let (left, right) = groups.split_at_mut(at);
        let left = &mut left[at - 1].1;
        let right = &mut right[0].1;
        let mut boosted_left = vec![false; left.len().min(PLANNER_TOP)];
        let mut boosted_right = vec![false; right.len().min(PLANNER_TOP)];
        for (li, lhit) in left.iter().take(PLANNER_TOP).enumerate() {
            for (ri, rhit) in right.iter().take(PLANNER_TOP).enumerate() {
                if connected(&lhit.node, &rhit.node)? {
                    boosted_left[li] = true;
                    boosted_right[ri] = true;
                    pairs.push(format!(
                        "{} -> {}",
                        qualified_of(lhit.node.id.as_str()),
                        qualified_of(rhit.node.id.as_str())
                    ));
                }
            }
        }
        for (hits, boosted) in [(&mut *left, boosted_left), (&mut *right, boosted_right)] {
            for (hit, boost) in hits.iter_mut().zip(boosted) {
                if boost {
                    hit.score = hit.score * CONNECTED_PERMILLE / 1000;
                    hit.breakdown.connected = true;
                    hit.breakdown.final_score = hit.score;
                    hit.channels.push("connected");
                    hit.channels.sort_unstable();
                }
            }
            sort_hits(hits);
        }
    }
    Ok(pairs)
}

/// `boost_connected` over the store graph, either direction, any relation
/// but containment.
pub(super) fn connect_topics(
    store: &Store,
    groups: &mut [(String, Vec<Hit>)],
) -> Result<Vec<String>> {
    // ponytail: one 3-hop closure per left hit (10 per topic pair); a hub
    // node's closure can be large on a big repo. Cap the closure size if
    // it shows up in timings.
    let filter = EdgeFilter::default();
    let mut closures: HashMap<String, HashSet<String>> = HashMap::new();
    boost_connected(groups, |left, right| {
        if !closures.contains_key(left.id.as_str()) {
            let mut reached = HashSet::new();
            for step in store
                .dependencies(&left.id, &filter, PLANNER_HOPS)?
                .into_iter()
                .chain(store.dependents(&left.id, &filter, PLANNER_HOPS)?)
            {
                reached.insert(step.node.id.as_str().to_owned());
            }
            closures.insert(left.id.as_str().to_owned(), reached);
        }
        Ok(closures[left.id.as_str()].contains(right.id.as_str()))
    })
}

/// Keep the best hit per bare name among weak matches; later same-name
/// weak hits become its `variants` so the next distinct name moves up. A
/// hit covering at least half the query is an answer in its own right
/// (`App::add_url_rule` beside `Blueprint::add_url_rule`) and stays.
fn collapse_same_name(hits: Vec<Hit>) -> Vec<Hit> {
    let mut kept: Vec<Hit> = Vec::with_capacity(hits.len());
    let mut index_by_name: HashMap<String, usize> = HashMap::new();
    for hit in hits {
        let weak = hit.matched.len() * 2 < hit.total_terms;
        if !weak {
            kept.push(hit);
            continue;
        }
        match index_by_name.get(hit.node.name.as_str()) {
            Some(&at) => kept[at]
                .variants
                .push(qualified_of(hit.node.id.as_str()).to_owned()),
            None => {
                index_by_name.insert(hit.node.name.clone(), kept.len());
                kept.push(hit);
            }
        }
    }
    kept
}

/// One bare name is one answer with several homes: `add_url_rule` on four
/// types, `getRawType` on two. Rank order scatters that family, so the
/// member a question means can sit past the visible top-k behind its own
/// namesakes, separated by evidence too thin to tell them apart. Emit each
/// name as one contiguous run at its best rank.
///
/// Bounded two ways, because an unbounded pull drags junk into the window:
/// a namesake joins only from within `NAME_RUN_SPAN` ranks of the run's
/// leader (further down it lost on evidence, not on tie-breaking), and only
/// from a different file, since same file and same name is an overload
/// rather than a second answer.
const NAME_RUN_SPAN: usize = 4;

fn cluster_same_name(hits: Vec<Hit>) -> Vec<Hit> {
    let mut runs: Vec<Vec<Hit>> = Vec::new();
    let mut leader_rank: Vec<usize> = Vec::new();
    let mut run_of: HashMap<String, usize> = HashMap::new();
    for (rank, hit) in hits.into_iter().enumerate() {
        let joins = run_of.get(hit.node.name.as_str()).copied().filter(|&at| {
            rank - leader_rank[at] <= NAME_RUN_SPAN
                && runs[at].iter().all(|kept| kept.node.file != hit.node.file)
        });
        match joins {
            Some(at) => runs[at].push(hit),
            None => {
                run_of.insert(hit.node.name.clone(), runs.len());
                leader_rank.push(rank);
                runs.push(vec![hit]);
            }
        }
    }
    runs.into_iter().flatten().collect()
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

/// Term coverage with body-only terms at half credit, as a fraction.
fn coverage(breakdown: &ScoreBreakdown) -> (usize, usize) {
    if breakdown.body_only == 0 {
        return (breakdown.coverage_numerator, breakdown.coverage_denominator);
    }
    (
        2 * breakdown.coverage_numerator - breakdown.body_only,
        2 * breakdown.coverage_denominator,
    )
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
        + breakdown.callee
        + breakdown.body;
    let (covered, total) = coverage(breakdown);
    let mut score = breakdown.evidence
        * covered as i64
        * breakdown.kind_numerator
        * breakdown.penalty_numerator
        / (total as i64 * breakdown.kind_denominator * breakdown.penalty_denominator);
    if breakdown.rarest_miss {
        score = score * RAREST_MISS_PERMILLE / 1000;
    }
    score += breakdown.hub_bonus;
    breakdown.final_score = score;
    score
}

/// Rerank the leading hits with one hop of call evidence: a term the
/// symbol itself never mentions, but a direct callee is named after.
fn apply_callee_evidence(
    store: &Store,
    query: &Query,
    rarest: Option<&str>,
    hits: &mut [Hit],
) -> Result<()> {
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
            let body_only = hit.body_matched.iter().position(|m| m == term.surface());
            if body_only.is_none() && hit.matched.iter().any(|m| m == term.surface()) {
                continue;
            }
            if callee_tokens.iter().any(|token| term.matches_token(token)) {
                // A callee named after the term outranks a body mention:
                // the term graduates from half to full coverage.
                match body_only {
                    Some(at) => {
                        hit.body_matched.remove(at);
                        hit.breakdown.body_only -= 1;
                    }
                    None => hit.matched.push(term.surface().to_owned()),
                }
                hit.breakdown.callee += PT_CALLEE;
                gained = true;
            }
        }
        if gained {
            hit.breakdown.coverage_numerator = hit.matched.len();
            hit.breakdown.rarest_miss =
                rarest.is_some_and(|term| !hit.matched.iter().any(|m| m == term));
            hit.channels.push("callee");
            hit.roles.push("callee");
            hit.score = combine(&mut hit.breakdown);
        }
    }
    Ok(())
}

/// Body-word retrieval and evidence for every query term. Glue words (df
/// above the ceiling) neither retrieve nor score; the rest pull in at
/// most `BODY_RETRIEVAL_CAP` new candidates per variant and weigh by IDF.
fn body_evidence(
    store: &Store,
    query: &Query,
    nodes: &mut Vec<Node>,
    seen: &mut HashSet<String>,
) -> Result<Vec<BodyEvidence>> {
    let total = store.node_count()?.max(2);
    let ceiling = total * BODY_DF_CEILING_PERMILLE / 1000;
    let log_total = (total as f64).ln();
    let mut out = Vec::with_capacity(query.terms().len());
    for term in query.terms() {
        let mut evidence = BodyEvidence {
            ids: HashSet::new(),
            idf_permille: 0,
        };
        // Agent nouns name the actor, bodies name the act: "importers"
        // reaches `is_import`/`importing_files` through the bare stem.
        let stems = term.variants().iter().flat_map(|variant| {
            let stem = ["ers", "er", "ors", "or"]
                .iter()
                .find_map(|suffix| variant.strip_suffix(suffix))
                .filter(|stem| stem.len() >= 4)
                .map(str::to_owned);
            std::iter::once(variant.clone()).chain(stem)
        });
        for variant in stems {
            let variant = variant.as_str();
            let df = store.body_term_df(variant)?;
            if df == 0 || df > ceiling {
                continue;
            }
            let idf = ((total as f64 / df as f64).ln() / log_total * 1000.0) as i64;
            evidence.idf_permille = evidence.idf_permille.max(idf);
            let ids = store.body_term_ids(variant)?;
            for id in ids.iter().take(BODY_RETRIEVAL_CAP) {
                if !seen.contains(id)
                    && let Some(node) = store.node(&sinter_core::NodeId::new(id))?
                {
                    seen.insert(id.clone());
                    nodes.push(node);
                }
            }
            evidence.ids.extend(ids);
        }
        out.push(evidence);
    }
    Ok(out)
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
    use super::{
        CONNECTED_PERMILLE, Hit, IDF_CEILING_PERMILLE, IDF_FLOOR_PERMILLE, NAME_RUN_SPAN,
        PRIOR_PRODUCTION, PT_DOC, ScoreBreakdown, boost_connected, cluster_same_name, combine,
        doc_phrase_hits, idf_permille, name_phrase_hits, owner_of, scope_prior, shares_prefix,
        weigh,
    };
    use sinter_core::{CorpusScope, Node, NodeId, Span, SymbolKind};

    fn full_coverage(evidence_name: i64) -> ScoreBreakdown {
        ScoreBreakdown {
            name: evidence_name,
            coverage_numerator: 1,
            coverage_denominator: 1,
            kind_numerator: 1,
            kind_denominator: 1,
            penalty_numerator: PRIOR_PRODUCTION,
            penalty_denominator: PRIOR_PRODUCTION,
            ..ScoreBreakdown::default()
        }
    }

    #[test]
    fn idf_is_clamped_and_monotone() {
        assert_eq!(idf_permille(29_000, 0), IDF_CEILING_PERMILLE);
        assert_eq!(idf_permille(29_000, 20), IDF_CEILING_PERMILLE);
        assert!(idf_permille(29_000, 1_000) < idf_permille(29_000, 100));
        assert_eq!(idf_permille(10, 10), IDF_FLOOR_PERMILLE.max(693));
        assert_eq!(idf_permille(10, 10_000), IDF_FLOOR_PERMILLE);
    }

    #[test]
    fn rare_term_beats_three_common_terms() {
        // 29k nodes, "adjudicate trajectory events against cedar policies":
        // "cedar" in 20 docs, the other three in 3000 each. The Cedar
        // engine's doc says "cedar policies"; the Python harness doc says
        // "adjudicate ... events ... policies" and never "cedar".
        let rare = weigh(PT_DOC, idf_permille(29_000, 20));
        let common = weigh(PT_DOC, idf_permille(29_000, 3_000));
        assert!(rare > common, "rare {rare} common {common}");
        let mut engine = full_coverage(rare + common);
        engine.coverage_numerator = 2;
        engine.coverage_denominator = 4;
        let mut harness = full_coverage(3 * common);
        harness.coverage_numerator = 3;
        harness.coverage_denominator = 4;
        harness.rarest_miss = true;
        assert!(combine(&mut engine) > combine(&mut harness));
        // Without the rarest-miss penalty the harness still wins.
        harness.rarest_miss = false;
        assert!(combine(&mut engine) < combine(&mut harness));
    }

    #[test]
    fn missing_the_rarest_term_halves_the_score() {
        let mut with = full_coverage(200);
        let mut without = full_coverage(200);
        without.rarest_miss = true;
        assert_eq!(combine(&mut with), 200);
        assert_eq!(combine(&mut without), 100);
    }

    fn hit(id: &str, score: i64) -> Hit {
        Hit {
            node: Node {
                id: NodeId::new(id),
                name: id.to_owned(),
                kind: SymbolKind::Function,
                file: "a.rs".to_owned(),
                span: Span { start: 0, end: 0 },
                signature: String::new(),
                doc: None,
            },
            scope: CorpusScope::Production,
            score,
            matched: Vec::new(),
            body_matched: Vec::new(),
            channels: Vec::new(),
            roles: Vec::new(),
            total_terms: 1,
            breakdown: ScoreBreakdown::default(),
            variants: Vec::new(),
            parent: None,
        }
    }

    fn named(name: &str, file: &str) -> Hit {
        let mut hit = hit(name, 0);
        hit.node.id = NodeId::new(format!("{file}#{name}"));
        hit.node.file = file.to_owned();
        hit
    }

    #[test]
    fn same_name_hits_are_emitted_as_one_run() {
        // `add_url_rule` on three types, scattered by evidence: the run
        // leader pulls the two below it up behind itself. `route`, which
        // sat between them, drops to the end of the run.
        let clustered = cluster_same_name(vec![
            named("add_url_rule", "blueprints.py"),
            named("route", "scaffold.py"),
            named("add_url_rule", "scaffold.py"),
            named("add_url_rule", "app.py"),
        ]);
        let files = clustered
            .iter()
            .map(|hit| hit.node.file.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            files,
            ["blueprints.py", "scaffold.py", "app.py", "scaffold.py"]
        );
    }

    #[test]
    fn a_run_takes_neither_overloads_nor_distant_namesakes() {
        let mut hits = vec![named("parseReader", "JsonParser.java")];
        // Same name, same file: an overload, not a second answer.
        hits.push(named("parseReader", "JsonParser.java"));
        // Same name, different file, but past NAME_RUN_SPAN.
        for step in 0..NAME_RUN_SPAN {
            hits.push(named(&format!("filler{step}"), "other.java"));
        }
        hits.push(named("parseReader", "Streams.java"));
        let clustered = cluster_same_name(hits);
        let names = clustered
            .iter()
            .map(|hit| hit.node.id.as_str().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names[0], "JsonParser.java#parseReader");
        assert_eq!(names[1], "JsonParser.java#parseReader");
        assert_eq!(names.last().unwrap(), "Streams.java#parseReader");
    }

    #[test]
    fn planner_boosts_the_connected_pair_across_topics() {
        // Graph: b -> y. Topic one prefers a (100) over b (90); topic two
        // prefers x over y. The connected pair b/y should win both topics.
        let edges = [("b", "y")];
        let mut groups = vec![
            ("one".to_owned(), vec![hit("a", 100), hit("b", 90)]),
            ("two".to_owned(), vec![hit("x", 100), hit("y", 90)]),
        ];
        let pairs = boost_connected(&mut groups, |l, r| {
            Ok(edges.contains(&(l.id.as_str(), r.id.as_str())))
        })
        .unwrap();
        assert_eq!(pairs, vec!["b -> y".to_owned()]);
        assert_eq!(groups[0].1[0].node.name, "b");
        assert_eq!(groups[0].1[0].score, 90 * CONNECTED_PERMILLE / 1000);
        assert_eq!(groups[1].1[0].node.name, "y");
        assert!(groups[1].1[0].breakdown.connected);
        assert!(groups[1].1[0].channels.contains(&"connected"));
    }

    #[test]
    fn fuzzy_closeness_needs_a_shared_prefix() {
        let tokens = identifier_tokens("syntax_error_files");
        assert!(!shares_prefix(&["sync".into()], &tokens));
        assert!(shares_prefix(
            &["completion".into()],
            &identifier_tokens("GenZshCompleation")
        ));
        assert!(shares_prefix(&["arg".into()], &identifier_tokens("args")));
    }

    #[test]
    fn phrases_reward_ordered_adjacent_tokens() {
        let query = Query::parse("where is zsh completion generated");
        let idf = vec![1000; query.len()];
        assert_eq!(
            name_phrase_hits(&identifier_tokens("GenZshCompletion"), &query, &idf),
            1000
        );
        assert_eq!(
            name_phrase_hits(&identifier_tokens("CompletionZsh"), &query, &idf),
            0
        );
        assert_eq!(
            doc_phrase_hits("writes zsh completion to w", &query, &idf),
            1000
        );
    }

    #[test]
    fn scope_prior_prefers_production_over_tests_and_fixtures() {
        let order = [
            CorpusScope::Production,
            CorpusScope::Docs,
            CorpusScope::Test,
            CorpusScope::Fixture,
            CorpusScope::Example,
            CorpusScope::Generated,
            CorpusScope::Vendor,
        ];
        for pair in order.windows(2) {
            assert!(scope_prior(pair[0]) >= scope_prior(pair[1]), "{pair:?}");
        }
        assert!(scope_prior(CorpusScope::Fixture) < scope_prior(CorpusScope::Test));
        assert_eq!(scope_prior(CorpusScope::Production), PRIOR_PRODUCTION);
    }

    #[test]
    fn equal_evidence_ranks_fixture_below_production() {
        let scored = |scope: CorpusScope| {
            let mut breakdown = ScoreBreakdown {
                name: 100,
                coverage_numerator: 1,
                coverage_denominator: 1,
                kind_numerator: 1,
                kind_denominator: 1,
                penalty_numerator: scope_prior(scope),
                penalty_denominator: PRIOR_PRODUCTION,
                ..ScoreBreakdown::default()
            };
            combine(&mut breakdown)
        };
        assert_eq!(scored(CorpusScope::Production), 100);
        assert!(scored(CorpusScope::Fixture) < scored(CorpusScope::Test));
        assert!(scored(CorpusScope::Test) < scored(CorpusScope::Production));
    }

    #[test]
    fn owner_is_last_qualifier() {
        assert_eq!(owner_of("jsonreader::peek"), Some("jsonreader"));
        assert_eq!(owner_of("a::b::c"), Some("b"));
        assert_eq!(owner_of("peek"), None);
    }
}
