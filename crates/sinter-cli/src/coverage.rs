//! Coverage contract for graph traversals. Positive and negative answers
//! describe the same indexed snapshot, filters, evidence tiers, and gaps so
//! a non-empty syntax-only result is never mistaken for an exhaustive one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sinter_core::{Confidence, Evidence, Relation, UnresolvedReason, UnresolvedReference};
use sinter_store::{EdgeFilter, Store};

/// Evidence represented by one traversal answer. `possible` means inferred
/// graph edges, not a confirmed runtime dependency. `unresolved` is evidence
/// observed by extraction but not bound to a graph target.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TraversalEvidence {
    pub certain: usize,
    pub possible: usize,
    pub unresolved: usize,
}

impl TraversalEvidence {
    pub fn from_confidences(
        confidences: impl IntoIterator<Item = Confidence>,
        unresolved: usize,
    ) -> Self {
        let mut evidence = Self {
            unresolved,
            ..Self::default()
        };
        for confidence in confidences {
            match confidence {
                Confidence::Certain => evidence.certain += 1,
                Confidence::Inferred => evidence.possible += 1,
            }
        }
        evidence
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GraphHealth {
    syntax_error_files: BTreeSet<String>,
    failed_files: BTreeMap<String, String>,
}

fn health_path(repo: &Path) -> std::path::PathBuf {
    repo.join(".sinter").join("health.json")
}

fn read_health(repo: &Path) -> GraphHealth {
    std::fs::read(health_path(repo))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Update extraction health incrementally. Failed files are retried on the
/// next build because their hash stamp is not committed; a later success
/// removes the persisted failure.
pub fn record_health(
    repo: &Path,
    touched: &[&str],
    removed: &[String],
    syntax_errors: &[String],
    failures: &[(String, String)],
) -> Result<()> {
    let mut health = read_health(repo);
    for file in touched
        .iter()
        .copied()
        .chain(removed.iter().map(String::as_str))
    {
        health.syntax_error_files.remove(file);
        health.failed_files.remove(file);
    }
    health
        .syntax_error_files
        .extend(syntax_errors.iter().cloned());
    health.failed_files.extend(failures.iter().cloned());

    let path = health_path(repo);
    let bytes = serde_json::to_vec_pretty(&health)?;
    if std::fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

fn git_output(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// What an unresolved reference most likely means, and whether anything
/// in this repository can be done about it. `reason` records how the miss
/// happened; the category says what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedCategory {
    /// Nothing in the corpus defines the name: standard library, a
    /// dependency, or a shell builtin. Not a graph gap.
    LikelyExternal,
    /// A compiler index would settle it, and the file's language has one
    /// Sinter can run; the index is missing or stale.
    MissingCompilerIndex,
    /// A member call whose receiver type syntax extraction could not see;
    /// the name exists in the corpus.
    MissingReceiverType,
    /// The bare name is defined in several places and nothing picked one.
    AmbiguousInternalTarget,
    /// The reference site itself is not an identifier, or sits in a file
    /// indexed from a partial syntax tree.
    UnsupportedSyntax,
    /// Evidence anchored the reference inside the corpus and the target was
    /// still not found: a real gap worth a look.
    ActionableAnchoredMiss,
}

impl UnresolvedCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LikelyExternal => "likely_external",
            Self::MissingCompilerIndex => "missing_compiler_index",
            Self::MissingReceiverType => "missing_receiver_type",
            Self::AmbiguousInternalTarget => "ambiguous_internal_target",
            Self::UnsupportedSyntax => "unsupported_syntax",
            Self::ActionableAnchoredMiss => "actionable_anchored_miss",
        }
    }

    /// Categories a maintainer of this repository can act on.
    pub const fn is_actionable(self) -> bool {
        matches!(
            self,
            Self::MissingReceiverType
                | Self::AmbiguousInternalTarget
                | Self::ActionableAnchoredMiss
        )
    }
}

/// Repository facts the classifier needs once, not per reference.
pub struct Classifier {
    /// Definition count per bare name across the corpus, for every name
    /// that appears unresolved.
    definitions: std::collections::HashMap<String, usize>,
    syntax_error_files: BTreeSet<String>,
    /// A compiler index is missing or stale for these languages.
    unindexed_languages: Vec<String>,
}

impl Classifier {
    pub fn new(repo: &Path, store: &Store, refs: &[UnresolvedReference]) -> Result<Self> {
        let mut definitions = std::collections::HashMap::new();
        for item in refs {
            let name = item.reference.name.as_str();
            if !definitions.contains_key(name) {
                let count = store.nodes_named(name)?.len();
                definitions.insert(name.to_owned(), count);
            }
        }
        let unindexed_languages = match crate::scip::staleness(repo) {
            crate::scip::Staleness::Fresh => Vec::new(),
            _ => crate::scip::indexable_languages(repo),
        };
        Ok(Self {
            definitions,
            syntax_error_files: read_health(repo).syntax_error_files,
            unindexed_languages,
        })
    }

    pub fn classify(&self, item: &UnresolvedReference) -> UnresolvedCategory {
        let reference = &item.reference;
        let is_identifier = reference
            .name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$');
        if !is_identifier || self.syntax_error_files.contains(&reference.file) {
            return UnresolvedCategory::UnsupportedSyntax;
        }
        let defined = self
            .definitions
            .get(reference.name.as_str())
            .copied()
            .unwrap_or(0);
        if item.reason == UnresolvedReason::CompilerUnresolved || defined == 0 {
            return UnresolvedCategory::LikelyExternal;
        }
        let has_receiver = reference.path.as_deref().is_some_and(|path| {
            path.trim_end_matches(reference.name.as_str())
                .ends_with(['.', ':', '>'])
        });
        if item.reason == UnresolvedReason::SyntaxAnchoredMiss && !has_receiver {
            // Already anchored inside the corpus; an index would only
            // confirm what a reader can check now.
            return UnresolvedCategory::ActionableAnchoredMiss;
        }
        let language = sinter_extract::spec_for_path(&reference.file).map(|spec| spec.name);
        if language.is_some_and(|lang| self.unindexed_languages.iter().any(|l| l == lang)) {
            return UnresolvedCategory::MissingCompilerIndex;
        }
        if reference.relation == Relation::Calls && has_receiver {
            UnresolvedCategory::MissingReceiverType
        } else if item.reason == UnresolvedReason::SyntaxAnchoredMiss {
            UnresolvedCategory::ActionableAnchoredMiss
        } else {
            // defined >= 1 here; one definition with no anchor is still a
            // choice resolution declined to make.
            UnresolvedCategory::AmbiguousInternalTarget
        }
    }
}

/// Count per category, every category present so consumers need no
/// defaulting.
pub fn category_counts(
    classifier: &Classifier,
    refs: &[UnresolvedReference],
) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for item in refs {
        *counts
            .entry(classifier.classify(item).as_str())
            .or_default() += 1;
    }
    counts
}

pub(crate) fn repository_coverage(repo: &Path, store: &Store) -> Result<serde_json::Value> {
    let repo = crate::pipeline::discover_root(repo);
    let health = read_health(&repo);
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    // Sinter's own artifacts must not make the tree look dirty.
    let dirty = git_output(
        &repo,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )
    .map(|status| {
        status
            .lines()
            .any(|line| !line.get(3..).unwrap_or("").starts_with(".sinter/"))
    });
    let indexing_projects = crate::scip::indexing_projects(&repo);
    let indexable_languages: BTreeSet<&str> = indexing_projects
        .iter()
        .flat_map(|project| project.languages.iter().map(String::as_str))
        .collect();
    let runnable_indexing = indexing_projects
        .iter()
        .any(|project| project.recommendation.is_some());
    let unavailable_indexing = indexing_projects
        .iter()
        .any(|project| project.status == "indexer_unavailable");
    let unconfigured_languages = crate::scip::unconfigured_indexable_languages(&repo);
    let (scip_state, stale_inputs) = match crate::scip::staleness(&repo) {
        crate::scip::Staleness::Fresh => ("fresh", 0),
        crate::scip::Staleness::Missing => ("missing", 0),
        crate::scip::Staleness::Stale(n) => ("stale", n),
    };
    let unresolved = store.all_unresolved_details()?;
    let mut reasons = BTreeMap::<&str, usize>::new();
    for item in &unresolved {
        *reasons.entry(item.reason.as_str()).or_default() += 1;
    }
    let classifier = Classifier::new(&repo, store, &unresolved)?;
    let categories = category_counts(&classifier, &unresolved);
    let actionable = unresolved
        .iter()
        .filter(|item| classifier.classify(item).is_actionable())
        .count();
    // Refs a compiler index would settle. Not actionable by hand, but
    // the headline must not let `actionable` read as "nearly complete".
    let waiting_on_scip = categories
        .get(UnresolvedCategory::MissingCompilerIndex.as_str())
        .copied()
        .unwrap_or(0);
    let waiting_suffix = if waiting_on_scip > 0 {
        format!(" · {waiting_on_scip} refs waiting on `sinter scip`")
    } else {
        String::new()
    };

    let mut limitations = vec![
        "a missing graph edge is not proof that no runtime path exists".to_string(),
        "dynamic dispatch edges are conservative candidates, not dependency-injection proof"
            .to_string(),
    ];
    if scip_state == "missing" && runnable_indexing {
        limitations.push(format!(
            "compiler index missing for configured {} project(s); run `sinter scip`{waiting_suffix}",
            indexable_languages
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    } else if scip_state == "missing" && !indexing_projects.is_empty() {
        limitations.push(
            "compiler index missing for configured projects, but their indexers are unavailable; inspect compiler_index.projects for install guidance"
                .to_string(),
        );
    } else if scip_state == "missing" && !unconfigured_languages.is_empty() {
        limitations.push(format!(
            "compiler index missing for {} source files, but no configured SCIP project was detected; no indexing command is recommended",
            unconfigured_languages.join(", ")
        ));
    } else if scip_state == "stale" && runnable_indexing {
        limitations.push(format!(
            "compiler index is stale ({stale_inputs} newer source/config inputs); run `sinter scip`{waiting_suffix}"
        ));
    } else if scip_state == "stale" && unavailable_indexing {
        limitations.push(format!(
            "compiler index is stale ({stale_inputs} newer source/config inputs), but the required indexers are unavailable; inspect compiler_index.projects for install guidance"
        ));
    } else if scip_state == "stale" {
        limitations.push(format!(
            "compiler index is stale ({stale_inputs} newer source/config inputs), but no configured SCIP project needs a runnable refresh"
        ));
    }
    if !health.failed_files.is_empty() {
        limitations.push("one or more files failed extraction and are unindexed".to_string());
    }
    if !health.syntax_error_files.is_empty() {
        limitations.push("one or more files were indexed from partial syntax trees".to_string());
        if health
            .syntax_error_files
            .iter()
            .any(|file| file.ends_with(".sql"))
        {
            limitations.push(
                "partially parsed .sql files skip statements the SQL grammar cannot parse (e.g. CREATE PROCEDURE); those objects are absent from the graph"
                    .to_string(),
            );
        }
    }
    if actionable > 0 {
        limitations.push(format!(
            "{actionable} unresolved references point inside this repository; `sinter unresolved` lists them by category"
        ));
    }

    let completeness = if scip_state == "fresh"
        && health.failed_files.is_empty()
        && health.syntax_error_files.is_empty()
        && actionable == 0
    {
        "complete_for_indexed_snapshot"
    } else {
        "partial"
    };
    let available_sources = [
        ("structural", "available", "certain"),
        ("scope", "available", "possible"),
        ("import", "available", "possible"),
        ("dynamic", "available", "possible"),
        (
            "scip",
            if scip_state == "fresh" {
                "available"
            } else {
                scip_state
            },
            "certain",
        ),
    ]
    .into_iter()
    .map(|(kind, status, certainty)| {
        serde_json::json!({
            "kind": kind,
            "status": status,
            "certainty": certainty,
        })
    })
    .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "completeness": completeness,
        "conclusive": false,
        "universe": {
            "mode": "repository",
            "root": repo,
        },
        "snapshot": {
            "head": head,
            "dirty": dirty,
            "working_tree_indexed": true,
            "node_id_scope": "snapshot",
            "graph_schema": Store::CURRENT_SCHEMA,
        },
        "compiler_index": {
            "state": scip_state,
            "indexable_languages": indexable_languages.into_iter().collect::<Vec<_>>(),
            "stale_inputs": stale_inputs,
            "projects": indexing_projects,
            "unconfigured_languages": unconfigured_languages,
        },
        "graph": {
            "unresolved_references": unresolved.len(),
            "unresolved_by_reason": reasons,
            "unresolved_by_category": categories,
            "actionable_unresolved": actionable,
            "missing_compiler_index": waiting_on_scip,
            "syntax_error_files": health.syntax_error_files,
            "unindexed_files": health.failed_files.keys().collect::<Vec<_>>(),
            "excluded_derived_roots": crate::corpus::DERIVED_ROOTS,
            "excluded_derived_dirs": crate::corpus::DERIVED_DIRS,
        },
        "available_sources": available_sources,
        "limitations": limitations,
    }))
}

/// Compact repository-health summary for the orientation card. The complete
/// traversal contract stays private to this module; Map needs only enough
/// evidence to stop a structural inventory from looking exhaustive.
pub(crate) fn orientation_health_json(repo: &Path, store: &Store) -> Result<serde_json::Value> {
    let coverage = repository_coverage(repo, store)?;
    let graph = &coverage["graph"];
    let count = |field: &str| graph[field].as_array().map_or(0, std::vec::Vec::len);
    Ok(serde_json::json!({
        "status": coverage["completeness"].clone(),
        "universe": coverage["universe"].clone(),
        "snapshot": coverage["snapshot"].clone(),
        "compiler_index": {
            "state": coverage["compiler_index"]["state"].clone(),
            "indexable_languages": coverage["compiler_index"]["indexable_languages"].clone(),
            "stale_inputs": coverage["compiler_index"]["stale_inputs"].clone(),
        },
        "graph": {
            "unresolved_references": graph["unresolved_references"].clone(),
            "actionable_unresolved": graph["actionable_unresolved"].clone(),
            "missing_compiler_index": graph["missing_compiler_index"].clone(),
            "syntax_error_files": count("syntax_error_files"),
            "unindexed_files": count("unindexed_files"),
        },
        "limitations": coverage["limitations"].clone(),
    }))
}

fn filter_json(filter: &EdgeFilter) -> serde_json::Value {
    let relation_values = filter
        .relations
        .as_ref()
        .map(|relations| {
            relations
                .iter()
                .map(|relation| relation.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            [
                Relation::Calls,
                Relation::Uses,
                Relation::Imports,
                Relation::Implements,
                Relation::Extends,
            ]
            .into_iter()
            .map(Relation::as_str)
            .collect()
        });
    let evidence_values = filter
        .evidence
        .as_ref()
        .map(|evidence| {
            evidence
                .iter()
                .map(|item| item.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            [
                Evidence::Structural,
                Evidence::Scope,
                Evidence::Import,
                Evidence::Scip,
                Evidence::Declared,
                Evidence::Dynamic,
            ]
            .into_iter()
            .map(Evidence::as_str)
            .collect()
        });
    let scope_values = filter
        .scopes
        .as_ref()
        .map(|scopes| {
            scopes
                .iter()
                .map(|scope| scope.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            sinter_core::CorpusScope::ALL
                .into_iter()
                .map(sinter_core::CorpusScope::as_str)
                .collect()
        });
    serde_json::json!({
        "relations": {
            "mode": if filter.relations.is_some() { "restricted" } else { "all_dependencies" },
            "values": relation_values,
        },
        "evidence": {
            "mode": if filter.evidence.is_some() { "restricted" } else { "all_available" },
            "values": evidence_values,
        },
        "min_confidence": if filter.min_confidence == Some(Confidence::Certain) {
            "certain"
        } else {
            "any"
        },
        "scope": {
            "mode": if filter.scopes.is_some() { "restricted" } else { "all" },
            "values": scope_values,
        },
    })
}

/// What to tell a reader about unresolved references naming the queried
/// symbol: `sinter scip` binds them only while the index is missing or
/// stale; against a fresh index they are macro bodies, unsupported syntax,
/// or external names.
pub fn unresolved_hint(repo: &Path) -> &'static str {
    match crate::scip::staleness(repo) {
        crate::scip::Staleness::Fresh => {
            "macro body / unsupported syntax / external; not bindable by scip"
        }
        _ => "`sinter scip` would bind them",
    }
}

/// Full footer and full `coverage` JSON on demand; the default is the
/// compact agent-facing form. `doctor` is always full.
pub fn verbose() -> bool {
    std::env::var_os("SINTER_VERBOSE_COVERAGE").is_some()
}

/// Resource URI serving the repository-wide half of a coverage envelope,
/// so a collapsed `ref` can be resolved back to what it names.
pub(crate) const COVERAGE_URI: &str = "sinter://coverage";

/// The half of a traversal envelope that describes the repository rather
/// than the query. Identical for every answer computed from one graph
/// state, and therefore what a long-lived MCP session pays for again on
/// every call.
const SHARED_FIELDS: [&str; 8] = [
    "completeness",
    "conclusive",
    "universe",
    "snapshot",
    "compiler_index",
    "graph",
    "available_sources",
    "limitations",
];

/// Claim qualifiers must survive reference collapsing. They are small and
/// interpreting a negative answer without them can silently widen its scope.
const ALWAYS_CARRIED_FIELDS: [&str; 3] = ["completeness", "conclusive", "universe"];

/// Fingerprint of the repository-wide half, `None` when `coverage` is not a
/// traversal envelope. Session-scoped identity only: it must change when
/// the shared half changes, never survive the process.
fn shared_fingerprint(coverage: &serde_json::Value) -> Option<String> {
    use std::hash::{Hash, Hasher};

    let object = coverage.as_object()?;
    // Workspace servers do not expose a workspace coverage resource. Let
    // the ordinary byte-budget collapse retain the essential qualifiers
    // instead of emitting a reference the client cannot resolve.
    if coverage
        .pointer("/universe/mode")
        .and_then(serde_json::Value::as_str)
        == Some("workspace")
    {
        return None;
    }
    if !object.contains_key("completeness") {
        return None;
    }
    let shared: BTreeMap<&str, &serde_json::Value> = SHARED_FIELDS
        .iter()
        .filter_map(|field| object.get(*field).map(|value| (*field, value)))
        .collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&shared).ok()?.hash(&mut hasher);
    Some(format!("cov-{:016x}", hasher.finish()))
}

/// Stamp every coverage envelope in `data` with its `ref`, and replace the
/// repository-wide half with that reference alone when the caller has
/// already been given it. Returns the fingerprint the answer carries, so a
/// session can decide what the next answer owes.
pub(crate) fn collapse_repeated(
    data: &mut serde_json::Value,
    known: Option<&str>,
) -> Option<String> {
    let mut carried = None;
    collapse_into(data, known, &mut carried);
    carried
}

fn collapse_into(value: &mut serde_json::Value, known: Option<&str>, carried: &mut Option<String>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collapse_into(item, known, carried);
            }
        }
        serde_json::Value::Object(object) => {
            if let Some(coverage) = object.get_mut("coverage")
                && let Some(fingerprint) = shared_fingerprint(coverage)
                && let Some(envelope) = coverage.as_object_mut()
            {
                if carried.is_none() {
                    *carried = Some(fingerprint.clone());
                }
                if known == Some(fingerprint.as_str()) {
                    envelope.retain(|field, _| {
                        !SHARED_FIELDS.contains(&field.as_str())
                            || ALWAYS_CARRIED_FIELDS.contains(&field.as_str())
                    });
                    envelope.insert(
                        "ref_note".to_string(),
                        serde_json::json!(format!(
                            "repository-wide coverage unchanged since the last full block \
                             in this session; read resource {COVERAGE_URI} for it"
                        )),
                    );
                }
                envelope.insert("ref".to_string(), serde_json::json!(fingerprint));
            }
            for (_, child) in object.iter_mut() {
                collapse_into(child, known, carried);
            }
        }
        _ => {}
    }
}

/// The repository-wide half on its own, shaped exactly as a traversal
/// envelope carries it: what a collapsed `ref` names.
pub(crate) fn shared_document(repo: &Path, store: &Store) -> Result<serde_json::Value> {
    let mut coverage = repository_coverage(repo, store)?;
    if !verbose() {
        slim_for_traversal(&mut coverage);
    }
    if let Some(object) = coverage.as_object_mut() {
        object.retain(|field, _| SHARED_FIELDS.contains(&field.as_str()));
    }
    if let Some(fingerprint) = shared_fingerprint(&coverage) {
        coverage["ref"] = serde_json::json!(fingerprint);
    }
    Ok(coverage)
}

const SYNTAX_ERROR_FILES_SHOWN: usize = 5;

/// Per-query envelope: repository-wide detail (every indexing project,
/// every partial-syntax file) belongs to `doctor`, not to each answer.
fn slim_for_traversal(coverage: &mut serde_json::Value) {
    crate::agent_protocol::slim_compiler_index(coverage);
    let graph = &mut coverage["graph"];
    if let Some(files) = graph["syntax_error_files"].as_array_mut() {
        let total = files.len();
        if total > SYNTAX_ERROR_FILES_SHOWN {
            files.truncate(SYNTAX_ERROR_FILES_SHOWN);
            graph["syntax_error_files_total"] = serde_json::json!(total);
        }
    }
}

/// Fields of the full envelope a default traversal answer keeps: the claim
/// qualifiers, the indexed commit, the compiler-index state that changes
/// what the query could have seen, and the query's own evidence counts.
/// Everything repository-wide (projects, graph totals, sources,
/// limitations, filters) is identical on every answer and belongs to
/// `--coverage` / `include_coverage` or `sinter doctor`.
const SUMMARY_FIELDS: [&str; 6] = [
    "status",
    "completeness",
    "conclusive",
    "snapshot",
    "compiler_index",
    "evidence",
];

/// Default per-answer trust envelope: `traversal_json` cut to
/// `SUMMARY_FIELDS`, with `compiler_index` reduced to its state.
pub fn summary_json(
    repo: &Path,
    store: &Store,
    filter: &EdgeFilter,
    evidence: TraversalEvidence,
    found: bool,
) -> Result<serde_json::Value> {
    let mut coverage = traversal_json(repo, store, filter, evidence, found)?;
    if let Some(object) = coverage.as_object_mut() {
        object.retain(|field, _| SUMMARY_FIELDS.contains(&field.as_str()));
    }
    coverage["compiler_index"] = serde_json::json!({
        "state": coverage["compiler_index"]["state"].clone(),
    });
    Ok(coverage)
}

/// `traversal_json` when `full`, else `summary_json`: one call for a verb
/// whose caller chose with `--coverage` / `include_coverage`.
pub fn coverage_json(
    repo: &Path,
    store: &Store,
    filter: &EdgeFilter,
    evidence: TraversalEvidence,
    found: bool,
    full: bool,
) -> Result<serde_json::Value> {
    if full {
        traversal_json(repo, store, filter, evidence, found)
    } else {
        summary_json(repo, store, filter, evidence, found)
    }
}

/// Machine-readable trust envelope carried by every traversal answer: the
/// full block. `summary_json` is the default answer's cut of it.
pub fn traversal_json(
    repo: &Path,
    store: &Store,
    filter: &EdgeFilter,
    evidence: TraversalEvidence,
    found: bool,
) -> Result<serde_json::Value> {
    let mut coverage = repository_coverage(repo, store)?;
    coverage["status"] = serde_json::json!(if found { "found" } else { "not_proven" });
    if !verbose() {
        slim_for_traversal(&mut coverage);
    }
    coverage["filters"] = filter_json(filter);
    coverage["evidence"] = serde_json::json!({
        "count_scope": "all_matches_before_limit",
        "certain": {"results": evidence.certain},
        "possible": {"results": evidence.possible},
        "unresolved": {
            "matching_query": evidence.unresolved,
            "repository_total": coverage["graph"]["unresolved_references"],
            "actionable": coverage["graph"]["actionable_unresolved"],
            "missing_compiler_index": coverage["graph"]["missing_compiler_index"],
        },
    });
    Ok(coverage)
}

/// Unresolved references inside the files a traversal actually touched.
/// Repository-wide unresolved totals live in the envelope already; this is
/// the loud, radius-scoped version: gaps in *these* files mean *this*
/// answer may be missing rows. Only `actionable` gaps degrade the answer;
/// the rest are external names, unsupported syntax, or refs a compiler
/// index would settle.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RadiusUnresolved {
    pub references: usize,
    pub sql: usize,
    pub actionable: usize,
    pub missing_compiler_index: usize,
}

/// Classify a set of unresolved references into radius counts.
pub fn tally(repo: &Path, store: &Store, refs: &[UnresolvedReference]) -> Result<RadiusUnresolved> {
    let classifier = Classifier::new(repo, store, refs)?;
    let mut out = RadiusUnresolved {
        references: refs.len(),
        ..RadiusUnresolved::default()
    };
    for item in refs {
        if item.reference.file.ends_with(".sql") {
            out.sql += 1;
        }
        let category = classifier.classify(item);
        if category.is_actionable() {
            out.actionable += 1;
        } else if category == UnresolvedCategory::MissingCompilerIndex {
            out.missing_compiler_index += 1;
        }
    }
    Ok(out)
}

/// Count and classify unresolved references in the given (deduplicated)
/// files.
pub fn radius_unresolved<'a>(
    repo: &Path,
    store: &Store,
    files: impl IntoIterator<Item = &'a str>,
) -> Result<RadiusUnresolved> {
    let files: BTreeSet<&str> = files.into_iter().collect();
    let mut refs = Vec::new();
    for file in files {
        refs.extend(store.unresolved_details_in(file)?);
    }
    tally(repo, store, &refs)
}

/// Extend a traversal coverage envelope with the radius-scoped gap counts.
/// Additive: the existing `evidence.unresolved` fields are untouched.
pub fn attach_radius(coverage: &mut serde_json::Value, radius: RadiusUnresolved) {
    coverage["evidence"]["unresolved"]["within_radius"] = serde_json::json!({
        "references": radius.references,
        "sql": radius.sql,
        "actionable": radius.actionable,
        "missing_compiler_index": radius.missing_compiler_index,
    });
}

/// Human coverage line for radius-scoped gaps; `None` when the radius has
/// no unresolved references (nothing degraded, nothing to say). Only
/// actionable gaps read as partial coverage.
pub fn radius_note(radius: RadiusUnresolved) -> Option<String> {
    if radius.references == 0 {
        return None;
    }
    if radius.actionable > 0 {
        let sql = if radius.sql > 0 {
            format!(" ({} in SQL)", radius.sql)
        } else {
            String::new()
        };
        return Some(format!(
            "  {} reference(s) unresolved within this radius{sql}; coverage partial — see `sinter unresolved`",
            radius.actionable
        ));
    }
    let sql = if radius.sql > 0 {
        format!(", {} in SQL", radius.sql)
    } else {
        String::new()
    };
    let index = if radius.missing_compiler_index > 0 {
        format!(
            "; {} waiting on `sinter scip`",
            radius.missing_compiler_index
        )
    } else {
        String::new()
    };
    Some(format!(
        "  unresolved in radius: 0 actionable ({} external/unsupported refs excluded{sql}{index})",
        radius.references - radius.missing_compiler_index
    ))
}

/// Text footer. Default: one `coverage:` line plus gaps specific to this
/// query (a missing/stale compiler index). `SINTER_VERBOSE_COVERAGE=1`
/// restores filters and every repository-wide limitation.
pub fn print_footer(
    repo: &Path,
    store: &Store,
    filter: &EdgeFilter,
    evidence: TraversalEvidence,
    found: bool,
    snapshot: Option<&str>,
) -> Result<()> {
    let coverage = traversal_json(repo, store, filter, evidence, found)?;
    print_traversal_footer(&coverage, snapshot);
    Ok(())
}

/// The same footer from an envelope already built (and already carried in a
/// `--json` payload), so a verb that has the coverage object does not
/// recompute it to print one line.
pub fn print_traversal_footer(coverage: &serde_json::Value, snapshot: Option<&str>) {
    let verbose = verbose();
    println!("{}", footer_line(coverage, snapshot));
    if verbose {
        let relations = coverage["filters"]["relations"]["values"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "  filters: relations={relations} min_confidence={} scope={}",
            coverage["filters"]["min_confidence"]
                .as_str()
                .unwrap_or("any"),
            coverage["filters"]["scope"]["values"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    if verbose {
        for text in query_gaps(coverage, true) {
            println!("  gap: {text}");
        }
    } else if let Some(text) = gap_line(coverage) {
        println!("  gap: {text}");
    }
}

/// The one default gap line: names the degraded relation instead of
/// restating the repository-wide limitation, whose input counts change on
/// every edit and never change a decision. `None` when no compiler index
/// applies to this repository.
fn gap_line(coverage: &serde_json::Value) -> Option<String> {
    query_gaps(coverage, false).first()?;
    let state = coverage["compiler_index"]["state"].as_str()?;
    Some(format!(
        "scip {state} — receiver/method calls may be missing"
    ))
}

fn footer_line(coverage: &serde_json::Value, snapshot: Option<&str>) -> String {
    let n = |v: &serde_json::Value| v.as_u64().unwrap_or(0);
    let mut line = format!(
        "  coverage: {} · {} certain · {} possible · {} unresolved naming query",
        coverage["completeness"].as_str().unwrap_or("partial"),
        n(&coverage["evidence"]["certain"]["results"]),
        n(&coverage["evidence"]["possible"]["results"]),
        n(&coverage["evidence"]["unresolved"]["matching_query"]),
    );
    if let Some(snapshot) = snapshot {
        line.push_str(&format!(" · snapshot {snapshot}"));
    }
    line
}

/// Limitations worth a line under this answer. Verbose keeps all of them;
/// the default keeps only compiler-index state, which changes what the
/// query could have seen. The generic disclaimers never print by default.
fn query_gaps(coverage: &serde_json::Value, verbose: bool) -> Vec<&str> {
    coverage["limitations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter(|text| verbose || text.starts_with("compiler index"))
        .collect()
}

/// Aggregate member coverage without flattening away which repository owns
/// a gap. Boundary evidence is declared separately because it comes from the
/// workspace manifest/link store, not a member compiler index.
pub fn workspace_json(
    workspace: &crate::workspace::Workspace,
    filter: &EdgeFilter,
    evidence: TraversalEvidence,
    found: bool,
) -> Result<serde_json::Value> {
    let mut members = serde_json::Map::new();
    let mut gaps = Vec::new();
    let mut partial = false;
    for (name, repo) in &workspace.members {
        let store = Store::open(crate::pipeline::db_path(repo))?;
        let member = repository_coverage(repo, &store)?;
        partial |= member["completeness"] == "partial";
        if let Some(items) = member["limitations"].as_array() {
            gaps.extend(items.iter().filter_map(|item| {
                item.as_str()
                    .map(|text| serde_json::json!({"member": name, "message": text}))
            }));
        }
        members.insert(name.clone(), member);
    }
    Ok(serde_json::json!({
        "status": if found { "found" } else { "not_proven" },
        "completeness": if partial { "partial" } else { "complete_for_indexed_snapshot" },
        "conclusive": false,
        "universe": {
            "mode": "workspace",
            "name": workspace.manifest.workspace.name,
            "manifest": workspace.manifest_path,
            "members": workspace.members.iter().map(|(name, root)| {
                (name.clone(), serde_json::json!({"root": root}))
            }).collect::<serde_json::Map<String, serde_json::Value>>(),
        },
        "filters": filter_json(filter),
        "evidence": {
            "count_scope": "all_matches_before_limit",
            "certain": {"results": evidence.certain},
            "possible": {"results": evidence.possible},
            "unresolved": {"matching_query": evidence.unresolved},
        },
        "available_sources": {
            "member_graphs": "available",
            "boundary_imports": "available",
            "declared_manifest_links": "available",
        },
        "members": members,
        "gaps": gaps,
        "limitations": [
            "a workspace graph path is bounded by member extraction/index coverage and declared boundary links",
            "undeclared runtime coupling cannot be inferred as an exhaustive dependency path",
        ],
    }))
}

pub fn print_workspace_traversal(
    workspace: &crate::workspace::Workspace,
    filter: &EdgeFilter,
    evidence: TraversalEvidence,
    found: bool,
) -> Result<()> {
    print_workspace_footer(workspace, filter, evidence, found, None)
}

pub fn print_workspace_footer(
    workspace: &crate::workspace::Workspace,
    filter: &EdgeFilter,
    evidence: TraversalEvidence,
    found: bool,
    snapshot: Option<&str>,
) -> Result<()> {
    let coverage = workspace_json(workspace, filter, evidence, found)?;
    let verbose = verbose();
    println!("{}", footer_line(&coverage, snapshot));
    if let Some(gaps) = coverage["gaps"].as_array() {
        for gap in gaps {
            let message = gap["message"].as_str().unwrap_or("coverage unavailable");
            if !verbose && !message.starts_with("compiler index") {
                continue;
            }
            println!(
                "  gap: {}: {message}",
                gap["member"].as_str().unwrap_or("unknown"),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use sinter_core::{
        Confidence, Evidence, Reference, Relation, Span, UnresolvedReason, UnresolvedReference,
    };
    use sinter_store::{EdgeFilter, Store};

    use super::{
        Classifier, TraversalEvidence, UnresolvedCategory, footer_line, gap_line,
        orientation_health_json, query_gaps, slim_for_traversal, summary_json, traversal_json,
    };

    fn item(name: &str, path: Option<&str>, reason: UnresolvedReason) -> UnresolvedReference {
        UnresolvedReference {
            reference: Reference {
                file: "src/lib.rs".into(),
                name: name.into(),
                path: path.map(str::to_owned),
                relation: Relation::Calls,
                span: Span { start: 0, end: 1 },
                enclosing: None,
                alias: None,
            },
            reason,
        }
    }

    fn classifier(defined: &[(&str, usize)], unindexed: &[&str]) -> Classifier {
        Classifier {
            definitions: defined
                .iter()
                .map(|(name, count)| ((*name).to_owned(), *count))
                .collect(),
            syntax_error_files: Default::default(),
            unindexed_languages: unindexed.iter().map(|l| (*l).to_owned()).collect(),
        }
    }

    #[test]
    fn undefined_names_are_external_and_anchored_misses_stay_actionable() {
        let c = classifier(&[("walk", 2), ("run", 1)], &["rust"]);
        assert_eq!(
            c.classify(&item("unwrap", None, UnresolvedReason::SyntaxOnly)),
            UnresolvedCategory::LikelyExternal
        );
        assert_eq!(
            c.classify(&item("walk", None, UnresolvedReason::SyntaxAnchoredMiss)),
            UnresolvedCategory::ActionableAnchoredMiss
        );
        assert_eq!(
            c.classify(&item("walk", None, UnresolvedReason::SyntaxOnly)),
            UnresolvedCategory::MissingCompilerIndex
        );
        assert_eq!(
            c.classify(&item(":", None, UnresolvedReason::SyntaxOnly)),
            UnresolvedCategory::UnsupportedSyntax
        );
    }

    #[test]
    fn receiver_calls_and_bare_names_split_when_no_index_applies() {
        let c = classifier(&[("walk", 2), ("run", 1)], &[]);
        assert_eq!(
            c.classify(&item(
                "run",
                Some("self.job.run"),
                UnresolvedReason::SyntaxOnly
            )),
            UnresolvedCategory::MissingReceiverType
        );
        assert_eq!(
            c.classify(&item("walk", None, UnresolvedReason::SyntaxOnly)),
            UnresolvedCategory::AmbiguousInternalTarget
        );
    }

    #[test]
    fn traversal_evidence_never_folds_possible_into_certain() {
        let evidence = TraversalEvidence::from_confidences(
            [
                Confidence::Certain,
                Confidence::Inferred,
                Confidence::Inferred,
            ],
            4,
        );
        assert_eq!(evidence.certain, 1);
        assert_eq!(evidence.possible, 2);
        assert_eq!(evidence.unresolved, 4);
    }

    #[test]
    fn positive_scip_backed_result_is_certain_but_only_snapshot_complete() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(repo.join(".sinter")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub fn source() {}\n").unwrap();
        std::fs::write(repo.join(".sinter/index.scip"), []).unwrap();
        let store = Store::create(repo.join(".sinter/graph.redb")).unwrap();
        let filter = EdgeFilter {
            evidence: Some(BTreeSet::from([Evidence::Scip])),
            min_confidence: Some(Confidence::Certain),
            relations: Some(BTreeSet::from([Relation::Calls])),
            scopes: None,
        };
        let coverage = traversal_json(
            repo,
            &store,
            &filter,
            TraversalEvidence::from_confidences([Confidence::Certain], 0),
            true,
        )
        .unwrap();

        assert_eq!(coverage["status"], "found");
        assert_eq!(coverage["completeness"], "complete_for_indexed_snapshot");
        assert_eq!(coverage["conclusive"], false);
        assert_eq!(coverage["universe"]["mode"], "repository");
        assert_eq!(
            coverage["universe"]["root"],
            repo.canonicalize().unwrap().to_string_lossy().as_ref()
        );
        assert_eq!(coverage["evidence"]["certain"]["results"], 1);
        assert_eq!(coverage["evidence"]["possible"]["results"], 0);
        assert_eq!(coverage["filters"]["evidence"]["values"][0], "scip");
        assert!(
            coverage["available_sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|source| source["kind"] == "scip" && source["status"] == "available")
        );
    }

    #[test]
    fn orientation_health_is_compact_and_names_complete_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(repo.join(".sinter")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub fn source() {}\n").unwrap();
        std::fs::write(repo.join(".sinter/index.scip"), []).unwrap();
        let store = Store::create(repo.join(".sinter/graph.redb")).unwrap();

        let health = orientation_health_json(repo, &store).unwrap();

        assert_eq!(health["status"], "complete_for_indexed_snapshot");
        assert_eq!(health["universe"]["mode"], "repository");
        assert_eq!(health["compiler_index"]["state"], "fresh");
        assert_eq!(health["graph"]["actionable_unresolved"], 0);
        assert!(health["compiler_index"].get("projects").is_none());
        assert!(health.get("available_sources").is_none());
    }

    #[test]
    fn compact_footer_is_one_line_and_keeps_only_index_gaps() {
        let coverage = serde_json::json!({
            "completeness": "partial",
            "evidence": {
                "certain": {"results": 10},
                "possible": {"results": 0},
                "unresolved": {"matching_query": 2},
            },
            "limitations": [
                "a missing graph edge is not proof that no runtime path exists",
                "compiler index is stale (3 newer source/config inputs); run `sinter scip`",
                "one or more files were indexed from partial syntax trees",
            ],
        });
        assert_eq!(
            footer_line(&coverage, Some("graph-v12-abc")),
            "  coverage: partial · 10 certain · 0 possible · 2 unresolved naming query · snapshot graph-v12-abc"
        );
        assert_eq!(
            query_gaps(&coverage, false),
            ["compiler index is stale (3 newer source/config inputs); run `sinter scip`"]
        );
        assert_eq!(query_gaps(&coverage, true).len(), 3);
    }

    #[test]
    fn default_gap_line_names_the_index_state_without_input_counts() {
        let mut coverage = serde_json::json!({
            "compiler_index": {"state": "stale"},
            "limitations": [
                "compiler index is stale (14 newer source/config inputs); run `sinter scip`",
            ],
        });
        assert_eq!(
            gap_line(&coverage).as_deref(),
            Some("scip stale — receiver/method calls may be missing")
        );
        coverage["compiler_index"]["state"] = serde_json::json!("missing");
        assert_eq!(
            gap_line(&coverage).as_deref(),
            Some("scip missing — receiver/method calls may be missing")
        );
        // No compiler-index limitation: nothing is degraded, no line.
        coverage["limitations"] = serde_json::json!([]);
        assert_eq!(gap_line(&coverage), None);
    }

    #[test]
    fn summary_keeps_the_claim_qualifiers_and_drops_repository_bulk() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(repo.join(".sinter")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub fn source() {}\n").unwrap();
        let store = Store::create(repo.join(".sinter/graph.redb")).unwrap();
        let filter = EdgeFilter::default();
        let evidence = TraversalEvidence::from_confidences([Confidence::Inferred], 2);
        let summary = summary_json(repo, &store, &filter, evidence, false).unwrap();
        let mut keys: Vec<_> = summary.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "compiler_index",
                "completeness",
                "conclusive",
                "evidence",
                "snapshot",
                "status"
            ]
        );
        assert_eq!(summary["status"], "not_proven");
        assert_eq!(summary["conclusive"], false);
        assert_eq!(
            summary["compiler_index"],
            serde_json::json!({"state": "missing"})
        );
        assert_eq!(summary["evidence"]["possible"]["results"], 1);
        assert_eq!(summary["evidence"]["unresolved"]["matching_query"], 2);
        let full = traversal_json(repo, &store, &filter, evidence, false).unwrap();
        assert!(full.get("limitations").is_some());
        assert!(full.get("filters").is_some());
    }

    #[test]
    fn traversal_json_slims_projects_and_caps_syntax_error_files() {
        let files: Vec<String> = (0..8).map(|i| format!("q{i}.sql")).collect();
        let mut coverage = serde_json::json!({
            "compiler_index": {
                "state": "fresh",
                "stale_inputs": 0,
                "projects": [{"freshness": "fresh", "languages": ["rust"], "root": "."}],
            },
            "graph": {"syntax_error_files": files},
        });
        slim_for_traversal(&mut coverage);
        assert!(coverage["compiler_index"].get("projects").is_none());
        assert_eq!(coverage["compiler_index"]["state"], "fresh");
        assert_eq!(
            coverage["graph"]["syntax_error_files"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert_eq!(coverage["graph"]["syntax_error_files_total"], 8);
    }
}
