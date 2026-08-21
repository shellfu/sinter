use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::Serialize;
use sinter_core::{Node, Relation, SymbolKind};
use sinter_resolve::qualified_of;
use sinter_store::Store;

// Scoring policy. A value change requires an evaluation or focused fixture.
const PT_EXACT_NAME: i64 = 100;
const PT_NAME_CLOSE: i64 = 60;
const PT_DOC: i64 = 40;
const PT_SIGNATURE: i64 = 30;
const PT_PATH: i64 = 25;
const PT_ACTION_NAME: i64 = 40;
const HUB_CAP: i64 = 20;
const FAMILY_MIN_CHILDREN: usize = 2;
const TEST_PENALTY: (i64, i64) = (1, 2);
const VENDOR_PENALTY: (i64, i64) = (1, 2);

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "by", "can", "could", "do", "does", "find",
    "for", "how", "i", "in", "is", "it", "its", "located", "may", "me", "might", "must", "my",
    "of", "on", "or", "our", "shall", "should", "show", "that", "the", "these", "this", "those",
    "to", "was", "we", "were", "what", "where", "which", "who", "whom", "will", "with", "would",
    "you", "your",
];

const SOFT_STOPWORDS: &[&str] = &[
    "code",
    "going",
    "happen",
    "happens",
    "stuff",
    "thing",
    "things",
    "use",
    "used",
    "uses",
    "using",
    "work",
    "working",
    "works",
    "compared",
    "comparison",
    "comparisons",
    "describe",
    "described",
    "describes",
    "docs",
    "documentation",
    "documented",
    "explain",
    "explained",
    "explains",
    "overview",
    "related",
];

const ACTION_VERBS: &[&str] = &[
    "add",
    "apply",
    "assemble",
    "associate",
    "build",
    "calculate",
    "call",
    "check",
    "collect",
    "configure",
    "convert",
    "create",
    "dispatch",
    "enforce",
    "execute",
    "finalize",
    "generate",
    "handle",
    "initialize",
    "list",
    "load",
    "match",
    "parse",
    "print",
    "process",
    "register",
    "remove",
    "render",
    "run",
    "search",
    "select",
    "serialize",
    "start",
    "traverse",
    "validate",
    "write",
];

#[derive(Clone, Debug)]
pub(super) struct QueryTerm {
    surface: String,
    variants: Vec<String>,
    is_action: bool,
}

impl QueryTerm {
    pub(super) fn surface(&self) -> &str {
        &self.surface
    }
}

#[derive(Debug, Default, Serialize)]
pub(super) struct ScoreBreakdown {
    name: i64,
    doc: i64,
    signature: i64,
    path: i64,
    action_name_bonus: i64,
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
    pub(super) total_terms: usize,
    pub(super) breakdown: ScoreBreakdown,
    parent: Option<String>,
    action_name_matches: usize,
}

pub(super) fn terms_of(question: &str) -> Vec<QueryTerm> {
    let mut seen = HashSet::new();
    let lower = question.to_lowercase();
    let terms = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty() && !STOPWORDS.contains(term))
        .filter(|term| seen.insert((*term).to_owned()))
        .map(normalize_term)
        .collect::<Vec<_>>();
    let hard = terms
        .iter()
        .filter(|term| !SOFT_STOPWORDS.contains(&term.surface.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let mut selected = if hard.is_empty() { terms } else { hard };
    if lower.contains("command line interface")
        && let Some(interface) = selected.iter_mut().find(|term| term.surface == "interface")
    {
        interface.variants.push("cli".into());
    }
    selected
}

pub(super) fn clauses_of(question: &str) -> Vec<(String, Vec<QueryTerm>)> {
    let lower = question.to_lowercase();
    let mut seen = HashSet::new();
    lower
        .split([',', ';'])
        .flat_map(|segment| segment.split(" or "))
        .filter_map(|clause| {
            let terms = terms_of(clause);
            let label = terms
                .iter()
                .map(QueryTerm::surface)
                .collect::<Vec<_>>()
                .join(" ");
            (!terms.is_empty()).then_some((label, terms))
        })
        .filter(|(label, _)| seen.insert(label.clone()))
        .collect()
}

fn normalize_term(surface: &str) -> QueryTerm {
    let mut variants = vec![surface.to_owned()];
    match surface {
        "built" => variants.push("build".into()),
        "ran" => variants.push("run".into()),
        "written" | "wrote" => variants.push("write".into()),
        _ => {}
    }
    if let Some(stem) = surface.strip_suffix("ied") {
        variants.push(format!("{stem}y"));
    }
    if let Some(stem) = surface.strip_suffix("ing") {
        add_verb_stems(&mut variants, stem);
    }
    if let Some(stem) = surface.strip_suffix("ed") {
        add_verb_stems(&mut variants, stem);
    }
    if let Some(singular) = surface
        .strip_suffix('s')
        .filter(|singular| !singular.is_empty() && !surface.ends_with("ss"))
    {
        variants.push(singular.to_owned());
    }
    let is_action = variants
        .iter()
        .any(|variant| ACTION_VERBS.contains(&variant.as_str()));
    add_query_synonyms(&mut variants);
    variants.sort();
    variants.dedup();
    QueryTerm {
        surface: surface.to_owned(),
        variants,
        is_action,
    }
}

fn add_query_synonyms(variants: &mut Vec<String>) {
    let originals = variants.clone();
    for variant in originals {
        let synonyms: &[&str] = match variant.as_str() {
            "application" => &["app"],
            "argument" => &["arg", "args"],
            "calculate" => &["get"],
            "check" => &["validate"],
            "exactly" => &["exact"],
            "flag" => &["arg", "args"],
            "output" => &["sink"],
            "register" => &["add"],
            "route" => &["rule"],
            "subcommand" => &["command", "traverse"],
            "validate" => &["check"],
            _ => &[],
        };
        variants.extend(synonyms.iter().map(|synonym| (*synonym).to_owned()));
    }
}

fn add_verb_stems(variants: &mut Vec<String>, stem: &str) {
    let mut candidates = vec![stem.to_owned(), format!("{stem}e")];
    let chars = stem.chars().collect::<Vec<_>>();
    if chars.len() >= 2 && chars[chars.len() - 1] == chars[chars.len() - 2] {
        candidates.push(chars[..chars.len() - 1].iter().collect());
    }
    variants.extend(
        candidates
            .into_iter()
            .filter(|candidate| ACTION_VERBS.contains(&candidate.as_str())),
    );
}

fn contains_term(haystack: &str, term: &QueryTerm) -> bool {
    term.variants
        .iter()
        .any(|variant| haystack.contains(variant))
}

fn is_test_path(file: &str) -> bool {
    file.starts_with("tests/")
        || file.contains("/tests/")
        || file.contains("_test.")
        || file.contains(".test.")
        || file.contains("test_")
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

pub(super) fn score_candidates(store: &Store, terms: &[QueryTerm]) -> Result<Vec<Hit>> {
    let variants = terms
        .iter()
        .map(|term| term.variants.clone())
        .collect::<Vec<_>>();
    let action_query = terms.iter().any(|term| term.is_action);

    let mut nodes = store.candidates_for_term_variants(&variants)?;
    let mut seen = nodes
        .iter()
        .map(|node| node.id.as_str().to_owned())
        .collect::<HashSet<_>>();
    let mut close_ids = Vec::with_capacity(terms.len());
    for term in terms {
        let mut close = HashSet::new();
        for variant in &term.variants {
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

    let mut hits = Vec::new();
    for node in nodes {
        let name = node.name.to_lowercase();
        let qualified = qualified_of(node.id.as_str()).to_lowercase();
        let doc = node.doc.as_deref().unwrap_or("").to_lowercase();
        let signature = node.signature.to_lowercase();
        let file = node.file.to_lowercase();
        let mut breakdown = ScoreBreakdown::default();
        let mut matched = Vec::new();
        let mut channels = Vec::new();
        let mut action_name_matches = 0;
        for (index, term) in terms.iter().enumerate() {
            let mut term_hit = false;
            let name_exact = term.variants.contains(&name);
            let name_close = contains_term(&name, term)
                || contains_term(&qualified, term)
                || close_ids[index].contains(node.id.as_str());
            if name_exact {
                breakdown.name += PT_EXACT_NAME;
                channels.push("name");
                term_hit = true;
            } else if name_close {
                breakdown.name += PT_NAME_CLOSE;
                channels.push("name");
                term_hit = true;
            }
            if term.is_action && (name_exact || name_close) {
                breakdown.action_name_bonus += PT_ACTION_NAME;
                action_name_matches += 1;
                channels.push("action-name");
            }
            if !doc.is_empty() && contains_term(&doc, term) {
                breakdown.doc += PT_DOC;
                channels.push("doc");
                term_hit = true;
            }
            if contains_term(&signature, term) {
                breakdown.signature += PT_SIGNATURE;
                channels.push("sig");
                term_hit = true;
            }
            if file
                .split(['/', '.'])
                .any(|segment| contains_term(segment, term))
            {
                breakdown.path += PT_PATH;
                channels.push("path");
                term_hit = true;
            }
            if term_hit {
                matched.push(term.surface.clone());
            }
        }
        breakdown.evidence = breakdown.name
            + breakdown.doc
            + breakdown.signature
            + breakdown.path
            + breakdown.action_name_bonus;
        if breakdown.evidence == 0 {
            continue;
        }

        let (kind_numerator, kind_denominator) = kind_prior(node.kind, action_query);
        let (mut penalty_numerator, mut penalty_denominator) =
            if is_test_path(&node.file) && !terms.iter().any(|term| term.surface == "test") {
                TEST_PENALTY
            } else {
                (1, 1)
            };
        if is_vendor_path(&node.file) {
            penalty_numerator *= VENDOR_PENALTY.0;
            penalty_denominator *= VENDOR_PENALTY.1;
        }
        let mut score =
            breakdown.evidence * matched.len() as i64 * kind_numerator * penalty_numerator
                / (terms.len() as i64 * kind_denominator * penalty_denominator);
        let in_edges = incoming
            .get(&node.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        breakdown.hub_bonus = (in_edges.len() as i64).min(HUB_CAP);
        score += breakdown.hub_bonus;
        breakdown.coverage_numerator = matched.len();
        breakdown.coverage_denominator = terms.len();
        breakdown.kind_numerator = kind_numerator;
        breakdown.kind_denominator = kind_denominator;
        breakdown.penalty_numerator = penalty_numerator;
        breakdown.penalty_denominator = penalty_denominator;
        breakdown.final_score = score;
        let parent = in_edges
            .iter()
            .find(|edge| edge.relation == Relation::Contains)
            .map(|edge| edge.src.as_str().to_owned());
        channels.sort_unstable();
        channels.dedup();
        hits.push(Hit {
            node,
            score,
            matched,
            channels,
            total_terms: terms.len(),
            breakdown,
            parent,
            action_name_matches,
        });
    }
    apply_family_boost(&mut hits, action_query);
    hits.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| (left.node.kind as u8).cmp(&(right.node.kind as u8)))
            .then_with(|| left.node.file.cmp(&right.node.file))
            .then_with(|| left.node.span.start.cmp(&right.node.span.start))
    });
    Ok(hits)
}

fn apply_family_boost(hits: &mut [Hit], action_query: bool) {
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
        if matches!(hit.node.kind, SymbolKind::File | SymbolKind::Module)
            || (action_query && hit.action_name_matches == 0)
        {
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
    use super::{clauses_of, normalize_term, terms_of};

    #[test]
    fn conservative_morphology_normalizes_code_actions() {
        assert!(normalize_term("parsed").variants.contains(&"parse".into()));
        assert!(
            normalize_term("matching")
                .variants
                .contains(&"match".into())
        );
        assert!(normalize_term("built").variants.contains(&"build".into()));
        assert_eq!(normalize_term("string").variants, vec!["string"]);
    }

    #[test]
    fn code_vocabulary_expands_directionally() {
        assert!(
            normalize_term("registered")
                .variants
                .contains(&"add".into())
        );
        assert!(
            normalize_term("arguments")
                .variants
                .contains(&"args".into())
        );
        assert!(normalize_term("route").variants.contains(&"rule".into()));
    }

    #[test]
    fn command_line_interface_adds_initialism_variant() {
        let terms = terms_of("where does the command line interface load the application");
        let interface = terms
            .iter()
            .find(|term| term.surface == "interface")
            .unwrap();
        assert!(interface.variants.contains(&"cli".into()));
    }

    #[test]
    fn weak_terms_drop_when_specific_terms_remain() {
        let terms = terms_of("where does this code work for parsed arguments");
        let surfaces = terms.iter().map(|term| term.surface()).collect::<Vec<_>>();
        assert_eq!(surfaces, vec!["parsed", "arguments"]);
    }

    #[test]
    fn clauses_are_deduplicated_after_normalization() {
        let clauses = clauses_of("parser, parser or matcher");
        assert_eq!(clauses.len(), 2);
        assert_eq!(clauses[0].0, "parser");
        assert_eq!(clauses[1].0, "matcher");
    }
}
