//! Snapshot-scoped assertions that a symbol has no observed incoming edges
//! of a given shape (`no-callers`, `no-writers`, `no-dependents`), plus the
//! `deletable` tally over every scope.
//!
//! This is deliberately narrower than `affected`: rows are depth-one
//! edges of the spec's relations, the requested corpus scope is explicit, and an empty
//! traversal becomes a claim only when the indexed snapshot is complete for
//! that scope and no unresolved reference still names the symbol. It never
//! claims runtime exhaustiveness; the ordinary coverage envelope keeps
//! `conclusive: false`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::{Confidence, CorpusScope, Node, Relation, SymbolKind};
use sinter_resolve::qualified_of;
use sinter_store::{EdgeFilter, Reached, Store};

use crate::coverage::{TraversalEvidence, traversal_json, workspace_json};
use crate::lookup::{
    also_see, ensure_snapshot, ensure_snapshot_token, open_store, selectors, unique_symbol_in,
};

pub(crate) const DEFAULT_LIMIT: usize = 50;

/// Which depth-one edge relations an assertion forbids, and how it names
/// itself. `assert no-writers` is `assert no-callers` over write-shaped
/// relations: same traversal, same JSON shape, same exit codes.
pub(crate) struct AssertionSpec {
    pub kind: &'static str,
    pub label: &'static str,
    pub noun: &'static str,
    /// JSON key for the row list; `observed_<rows>` is the count.
    pub rows: &'static str,
    pub meaning: &'static str,
    pub relations: &'static [Relation],
    /// A tally over every scope (`has_dependents` / `none_observed`), not a
    /// scoped negative proof.
    pub tally: bool,
}

pub(crate) const NO_CALLERS: AssertionSpec = AssertionSpec {
    kind: "no_callers",
    label: "no-callers",
    noun: "caller",
    rows: "callers",
    meaning: "no observed depth-one call edges in the requested corpus scope",
    relations: &[Relation::Calls],
    tally: false,
};

pub(crate) const NO_WRITERS: AssertionSpec = AssertionSpec {
    kind: "no_writers",
    label: "no-writers",
    noun: "writer",
    rows: "callers",
    meaning: "no observed depth-one writes/alters/drops edges in the requested corpus scope",
    relations: &[Relation::Writes, Relation::Alters, Relation::Drops],
    tally: false,
};

const DEPENDENT_RELATIONS: &[Relation] = &[
    Relation::Calls,
    Relation::Uses,
    Relation::Imports,
    Relation::Implements,
    Relation::Extends,
    Relation::Reads,
    Relation::Writes,
    Relation::Creates,
    Relation::Alters,
    Relation::Drops,
];

/// Every non-containment incoming relation. `imports` edges only ever
/// count as `possible`: a `use` line names a symbol without proving a
/// dependency on it.
pub(crate) const NO_DEPENDENTS: AssertionSpec = AssertionSpec {
    kind: "no_dependents",
    label: "no-dependents",
    noun: "dependent",
    rows: "dependents",
    meaning: "no observed depth-one non-containment edges (calls, uses, imports as possible, implements, extends, reads, writes, creates, alters, drops) in the requested corpus scope",
    relations: DEPENDENT_RELATIONS,
    tally: false,
};

/// "Can I delete this?": every dependent in every scope, grouped by scope.
pub(crate) const DELETABLE: AssertionSpec = AssertionSpec {
    kind: "deletable",
    label: "deletable",
    noun: "dependent",
    rows: "dependents",
    meaning: "depth-one non-containment dependents observed in any corpus scope, grouped by scope",
    relations: DEPENDENT_RELATIONS,
    tally: true,
};

fn edge_confidence(relation: Relation, confidence: Confidence) -> Confidence {
    match relation {
        Relation::Imports => Confidence::Inferred,
        _ => confidence,
    }
}

/// `no-callers` on a non-callable kind is answered by an edge relation
/// the symbol never receives; say which assertion does.
fn kind_hint(spec: &AssertionSpec, kind: SymbolKind) -> Option<String> {
    let callable = matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Macro
    );
    (spec.kind == NO_CALLERS.kind && !callable).then(|| {
        format!(
            "no-callers counts calls edges only; use assert no-dependents for {}",
            kind.as_str()
        )
    })
}

/// Default `--json` is the decision and its qualifiers: status, rows as
/// `{name, site, relation, evidence, confidence, scope}`, the compiler
/// index state and the limitations. `--verbose` keeps the full envelope.
fn compact(response: &mut Value, spec: &AssertionSpec, verbose: bool) {
    if verbose || crate::coverage::verbose() {
        return;
    }
    let keep = |value: &mut Value, keys: &[&str]| {
        if let Some(object) = value.as_object_mut() {
            object.retain(|key, _| keys.contains(&key.as_str()));
        }
    };
    keep(&mut response["assertion"], &["kind", "runtime_exhaustive"]);
    keep(
        &mut response["symbol"],
        &["qualified", "kind", "file", "scope"],
    );
    if let Some(rows) = response[spec.rows].as_array_mut() {
        for row in rows {
            keep(
                row,
                &[
                    "name",
                    "site",
                    "relation",
                    "evidence",
                    "confidence",
                    "scope",
                ],
            );
        }
    }
    keep(
        &mut response["coverage"],
        &[
            "completeness",
            "conclusive",
            "universe",
            "compiler_index",
            "limitations",
            "members",
        ],
    );
    keep(&mut response["coverage"]["compiler_index"], &["state"]);
    codes(&mut response["coverage"]["limitations"]);
    // A zero count qualifies nothing: silence says the same and costs no
    // bytes. Both keep their shape when they carry a number.
    let ignored = response["ignored_out_of_scope"]["count"].clone();
    let unresolved = response["unresolved_refs_matching_name"].clone();
    if let Some(object) = response.as_object_mut() {
        if ignored == 0 {
            object.remove("ignored_out_of_scope");
        }
        if unresolved == 0 {
            object.remove("unresolved_refs_matching_name");
        }
        if let Some(ignored) = object.get_mut("ignored_out_of_scope") {
            keep(ignored, &["count", "by_scope"]);
        }
    }
    if let Some(members) = response["coverage"]
        .get_mut("members")
        .and_then(Value::as_object_mut)
    {
        for member in members.values_mut() {
            keep(member, &["completeness", "compiler_index", "limitations"]);
            keep(&mut member["compiler_index"], &["state"]);
            codes(&mut member["limitations"]);
        }
    }
}

/// Prose limitation → short code. The two constant disclaimers are
/// `assertion.runtime_exhaustive: false` in prose and repository-wide
/// unresolved counts do not qualify a scoped claim, so both drop; the rest
/// become codes an agent can branch on. `--verbose` keeps the sentences.
/// ponytail: matched by wording, since the sentences are built in
/// `coverage`; an unrecognised line survives verbatim rather than silently
/// vanishing.
const LIMITATION_CODES: &[(&str, &str)] = &[
    ("a missing graph edge is not proof", ""),
    ("dynamic dispatch edges are conservative", ""),
    ("unresolved references point inside this repository", ""),
    ("compiler index missing", "scip_missing"),
    ("compiler index is stale", "scip_stale"),
    ("one or more files failed extraction", "unindexed_files"),
    (
        "one or more files were indexed from partial syntax trees",
        "",
    ),
    ("partially parsed .sql files", "sql_partial_statements"),
    (
        "partially indexed file(s) in the asserted scope",
        "partial_syntax_in_scope",
    ),
];

fn codes(limitations: &mut Value) {
    let Some(lines) = limitations.as_array_mut() else {
        return;
    };
    let mut out = Vec::new();
    for line in lines.iter() {
        let text = line.as_str().unwrap_or("");
        match LIMITATION_CODES
            .iter()
            .find(|(prose, _)| text.contains(prose))
        {
            Some((_, "")) => {}
            Some((_, code)) => out.push(json!(code)),
            None => out.push(line.clone()),
        }
    }
    *limitations = json!(out);
}

fn edge_filter(spec: &AssertionSpec, scopes: BTreeSet<CorpusScope>, certain: bool) -> EdgeFilter {
    EdgeFilter {
        relations: Some(spec.relations.iter().copied().collect()),
        scopes: Some(scopes),
        min_confidence: certain.then_some(sinter_core::Confidence::Certain),
        ..Default::default()
    }
}

fn unresolved_in_scopes(
    store: &Store,
    name: &str,
    scopes: &BTreeSet<CorpusScope>,
) -> Result<usize> {
    let scope_index = store.scope_index()?;
    let mut count = 0;
    for unresolved in store.unresolved_details(None, Some(name))? {
        let scope = match unresolved.reference.enclosing.as_ref() {
            Some(id) => store
                .node(id)?
                .map(|node| scope_index.scope_of(&node))
                .unwrap_or(store.file_scope(&unresolved.reference.file)?),
            None => store.file_scope(&unresolved.reference.file)?,
        };
        count += usize::from(scopes.contains(&scope));
    }
    Ok(count)
}

/// Partially indexed files inside the asserted scopes. A partial file in
/// another scope (a fixture with a syntax error) cannot hide an in-scope
/// edge, so it does not block the claim.
fn index_gaps(root: &Path, store: &Store, scopes: &BTreeSet<CorpusScope>) -> Result<Vec<String>> {
    let mut gaps = Vec::new();
    for file in crate::coverage::unindexed_files(root) {
        if scopes.contains(&store.file_scope(&file)?) {
            gaps.push(file);
        }
    }
    Ok(gaps)
}

fn status(spec: &AssertionSpec, callers: usize, complete: bool, unresolved: usize) -> &'static str {
    match (spec.tally, callers > 0) {
        (true, true) => "has_dependents",
        (true, false) => "none_observed",
        (false, true) => "violated",
        (false, false) if complete && unresolved == 0 => "holds_for_indexed_snapshot",
        (false, false) => "not_proven",
    }
}

fn caller_row(repo: &Path, reached: &Reached, scopes: &sinter_store::ScopeIndex) -> Value {
    let mut node = crate::graph_tool::scoped_node_json(&reached.node, scopes);
    node["name"] = json!(qualified_of(reached.node.id.as_str()));
    node["relation"] = json!(reached.via.relation.as_str());
    node["evidence"] = json!(reached.via.evidence.as_str());
    node["confidence"] = json!(
        match edge_confidence(reached.via.relation, reached.via.confidence) {
            Confidence::Certain => "certain",
            Confidence::Inferred => "possible",
        }
    );
    let site = crate::render::site_json(repo, &reached.via);
    if !site.is_null() {
        node["site"] = site;
        crate::render::add_sites(&mut node, repo, &reached.via);
    }
    node
}

fn by_scope<'a>(scopes: impl Iterator<Item = &'a str>) -> BTreeMap<&'a str, usize> {
    scopes.fold(BTreeMap::new(), |mut counts, scope| {
        *counts.entry(scope).or_default() += 1;
        counts
    })
}

/// Repository assertion producer.
fn repository_response(
    repo: &Path,
    store: &Store,
    spec: &AssertionSpec,
    symbol: &str,
    scopes: BTreeSet<CorpusScope>,
    certain: bool,
    limit: usize,
) -> Result<Value> {
    let root = crate::pipeline::discover_root(repo).canonicalize()?;
    let filter = edge_filter(spec, scopes.clone(), certain);
    let node = unique_symbol_in(store, symbol, Some(&scopes))?;
    let mut callers = store.dependents(&node.id, &filter, 1)?;
    if certain {
        callers
            .retain(|r| edge_confidence(r.via.relation, r.via.confidence) == Confidence::Certain);
    }

    let mut all_filter = filter.clone();
    all_filter.scopes = None;
    let all_callers = store.dependents(&node.id, &all_filter, 1)?;
    let included: HashSet<&str> = callers.iter().map(|r| r.node.id.as_str()).collect();
    let ignored: Vec<&Reached> = all_callers
        .iter()
        .filter(|r| !included.contains(r.node.id.as_str()))
        .collect();

    let unresolved = unresolved_in_scopes(store, &node.name, &scopes)?;
    let evidence = TraversalEvidence::from_confidences(
        callers
            .iter()
            .map(|item| edge_confidence(item.via.relation, item.via.confidence)),
        unresolved,
    );
    let mut coverage = traversal_json(&root, store, &filter, evidence, !callers.is_empty())?;
    // Completeness for *this* claim: a fresh compiler index and no partial
    // file inside the asserted scopes. Repository-wide unresolved counts
    // stay in the envelope; only refs naming the symbol qualify the claim.
    let gaps = index_gaps(&root, store, &scopes)?;
    let complete = coverage["compiler_index"]["state"] == "fresh" && gaps.is_empty();
    coverage["completeness"] = json!(if complete {
        "complete_for_indexed_snapshot"
    } else {
        "partial"
    });
    if !gaps.is_empty()
        && let Some(limitations) = coverage["limitations"].as_array_mut()
    {
        limitations.push(json!(format!(
            "{} partially indexed file(s) in the asserted scope: {}",
            gaps.len(),
            gaps.join(", ")
        )));
    }
    let assertion_status = status(spec, callers.len(), complete, unresolved);
    let scope_index = store.scope_index()?;
    let rows: Vec<Value> = callers
        .iter()
        .take(limit)
        .map(|r| caller_row(&root, r, &scope_index))
        .collect();
    let mut out = json!({
        "status": assertion_status,
        "assertion": {
            "kind": spec.kind,
            "meaning": spec.meaning,
            "runtime_exhaustive": false,
        },
        "symbol": crate::graph_tool::scoped_node_json(&node, &scope_index),
        format!("observed_{}", spec.rows): callers.len(),
        format!("{}_by_scope", spec.rows): by_scope(callers.iter().map(|r| scope_index.scope_of(&r.node).as_str())),
        spec.rows: rows,
        "ignored_out_of_scope": {
            "count": ignored.len(),
            "by_scope": by_scope(ignored.iter().map(|r| scope_index.scope_of(&r.node).as_str())),
        },
        "unresolved_refs_matching_name": unresolved,
        "coverage": coverage,
    });
    if callers.len() > limit {
        out["truncated"] = json!(callers.len() - limit);
    }
    if let Some(hint) = kind_hint(spec, node.kind) {
        out["hint"] = json!(hint);
    }
    let family = also_see(store, &node)?;
    if !family.is_empty() {
        out["also_see"] = json!(selectors(&family));
    }
    Ok(out)
}

fn workspace_node(member: &str, node: &Node, scope: CorpusScope) -> Value {
    json!({
        "member": member,
        "id": format!("{member}:{}", node.symbol_key()),
        "symbol_key": node.symbol_key().as_str(),
        "qualified": format!("{member}:{}", qualified_of(node.id.as_str())),
        "name": format!("{member}:{}", qualified_of(node.id.as_str())),
        "kind": node.kind.as_str(),
        "scope": scope.as_str(),
        "file": node.file,
        "signature": node.signature,
    })
}

/// Workspace assertion producer.
fn workspace_response(
    workspace: &crate::workspace::Workspace,
    spec: &AssertionSpec,
    symbol: &str,
    scopes: BTreeSet<CorpusScope>,
    certain: bool,
    limit: usize,
) -> Result<Value> {
    let filter = edge_filter(spec, scopes.clone(), certain);
    let (member, node) = crate::workspace::find_symbol(workspace, symbol)?;
    let mut callers = crate::workspace::dependents(workspace, &member, &node.id, &filter, 1)?;
    if certain {
        callers.retain(|r| {
            edge_confidence(r.relation, r.evidence.confidence()) == Confidence::Certain
        });
    }

    let mut all_filter = filter.clone();
    all_filter.scopes = None;
    let all_callers = crate::workspace::dependents(workspace, &member, &node.id, &all_filter, 1)?;
    let included: HashSet<(&str, &str)> = callers
        .iter()
        .map(|r| (r.member.as_str(), r.node.id.as_str()))
        .collect();
    let ignored = all_callers
        .iter()
        .filter(|r| !included.contains(&(r.member.as_str(), r.node.id.as_str())))
        .collect::<Vec<_>>();

    let member_scopes = workspace
        .members
        .iter()
        .map(|(member, repo)| {
            let store = Store::open(crate::pipeline::db_path(repo))?;
            Ok((member.clone(), store.scope_index()?))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
    let scope_of = |member: &str, node: &Node| member_scopes[member].scope_of(node);

    let owner_store = Store::open(crate::pipeline::db_path(&workspace.members[&member]))?;
    let mut unresolved = unresolved_in_scopes(&owner_store, &node.name, &scopes)?;
    let owner_scope = owner_store.file_scope(&node.file)?;
    drop(owner_store);
    for (other_member, repo) in &workspace.members {
        if other_member == &member {
            continue;
        }
        let store = Store::open(crate::pipeline::db_path(repo))?;
        unresolved += unresolved_in_scopes(&store, &node.name, &scopes)?;
    }
    let evidence = TraversalEvidence::from_confidences(
        callers
            .iter()
            .map(|item| edge_confidence(item.relation, item.evidence.confidence())),
        unresolved,
    );
    let coverage = workspace_json(workspace, &filter, evidence, !callers.is_empty())?;
    let complete = coverage["completeness"] == "complete_for_indexed_snapshot";
    let assertion_status = status(spec, callers.len(), complete, unresolved);
    let rows: Vec<Value> = callers
        .iter()
        .take(limit)
        .map(|r| {
            let mut row = workspace_node(&r.member, &r.node, scope_of(&r.member, &r.node));
            row["relation"] = json!(r.relation.as_str());
            row["evidence"] = json!(r.evidence.as_str());
            row["confidence"] = json!(match edge_confidence(r.relation, r.evidence.confidence()) {
                Confidence::Certain => "certain",
                Confidence::Inferred => "possible",
            });
            row
        })
        .collect();
    let mut out = json!({
        "status": assertion_status,
        "assertion": {
            "kind": spec.kind,
            "meaning": spec.meaning,
            "runtime_exhaustive": false,
        },
        "symbol": workspace_node(&member, &node, owner_scope),
        format!("observed_{}", spec.rows): callers.len(),
        format!("{}_by_scope", spec.rows): by_scope(callers.iter().map(|r| scope_of(&r.member, &r.node).as_str())),
        spec.rows: rows,
        "ignored_out_of_scope": {
            "count": ignored.len(),
            "by_scope": by_scope(ignored.iter().map(|r| scope_of(&r.member, &r.node).as_str())),
        },
        "unresolved_refs_matching_name": unresolved,
        "coverage": coverage,
    });
    if callers.len() > limit {
        out["truncated"] = json!(callers.len() - limit);
    }
    if let Some(hint) = kind_hint(spec, node.kind) {
        out["hint"] = json!(hint);
    }
    Ok(out)
}

/// `test 4, fixture 1` from a `by_scope` object; empty when nothing counted.
fn scope_tally(counts: &Value) -> String {
    counts
        .as_object()
        .into_iter()
        .flatten()
        .map(|(scope, n)| format!("{scope} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_row(indent: &str, row: &Value) {
    println!(
        "{indent}{}  {}  [{} / {}]",
        row["name"].as_str().unwrap_or("?"),
        row["site"]
            .as_str()
            .or_else(|| row["file"].as_str())
            .unwrap_or("?"),
        row["relation"].as_str().unwrap_or("calls"),
        row["evidence"].as_str().unwrap_or("?"),
    );
}

fn print_response(spec: &AssertionSpec, response: &Value) {
    let symbol = response["symbol"]["qualified"].as_str().unwrap_or("?");
    let count = response[format!("observed_{}", spec.rows)]
        .as_u64()
        .unwrap_or(0);
    println!(
        "assert {} {symbol}: {} ({count} observed {}(s))",
        spec.label,
        response["status"].as_str().unwrap_or("not_proven"),
        spec.noun,
    );
    if let Some(family) = response["also_see"].as_array() {
        let list: Vec<&str> = family.iter().filter_map(Value::as_str).collect();
        println!("  also_see: {}", list.join(", "));
    }
    let rows = response[spec.rows].as_array().into_iter().flatten();
    if spec.tally {
        // Grouped by scope, in the order the tally lists them.
        let by_scope = &response[format!("{}_by_scope", spec.rows)];
        let rows: Vec<&Value> = rows.collect();
        for (scope, n) in by_scope.as_object().into_iter().flatten() {
            println!("  {scope} ({n})");
            for row in rows.iter().filter(|r| r["scope"] == scope.as_str()) {
                print_row("    ", row);
            }
        }
    } else {
        for row in rows {
            print_row("  ", row);
        }
        let ignored = &response["ignored_out_of_scope"];
        let ignored = match ignored["count"].as_u64().unwrap_or(0) {
            0 => "0".to_string(),
            _ => scope_tally(&ignored["by_scope"]),
        };
        println!(
            "  ignored out of scope: {ignored}; unresolved refs matching name: {}",
            response["unresolved_refs_matching_name"],
        );
    }
    if let Some(hint) = response["hint"].as_str() {
        println!("  hint: {hint}");
    }
    crate::coverage::print_traversal_footer(&response["coverage"], response["snapshot"].as_str());
}

/// Exit 0 for `holds_for_indexed_snapshot` and `none_observed`.
fn passes(response: &Value) -> bool {
    matches!(
        response["status"].as_str(),
        Some("holds_for_indexed_snapshot" | "none_observed")
    )
}

#[allow(clippy::too_many_arguments)] // mirrors the clap subcommand one-to-one
pub(crate) fn run_repository(
    repo: &Path,
    spec: &AssertionSpec,
    symbol: &str,
    scopes: BTreeSet<CorpusScope>,
    certain: bool,
    limit: usize,
    json_output: bool,
    verbose: bool,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let mut response = repository_response(repo, &store, spec, symbol, scopes, certain, limit)?;
    // Compaction is the agent's budget, not the human card's: the text
    // renderer keeps the full sentences and per-scope tallies.
    if json_output {
        compact(&mut response, spec, verbose);
    }
    response["snapshot"] = json!(snapshot);
    if json_output {
        crate::agent_protocol::write_json(&response)?;
    } else {
        print_response(spec, &response);
    }
    Ok(passes(&response))
}

#[allow(clippy::too_many_arguments)] // mirrors the clap subcommand one-to-one
pub(crate) fn run_workspace(
    manifest: &Path,
    spec: &AssertionSpec,
    symbol: &str,
    scopes: BTreeSet<CorpusScope>,
    certain: bool,
    limit: usize,
    json_output: bool,
    verbose: bool,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let workspace = crate::workspace::load(manifest)?;
    for repo in workspace.members.values() {
        crate::pipeline::build(repo, None)?;
    }
    if !crate::workspace::stale_members(&workspace)?.is_empty() {
        crate::workspace::refresh(&workspace)?;
    }
    let snapshot = crate::workspace::snapshot_token(&workspace)?;
    ensure_snapshot_token(if_snapshot, &snapshot)?;
    let mut response = workspace_response(&workspace, spec, symbol, scopes, certain, limit)?;
    if json_output {
        compact(&mut response, spec, verbose);
    }
    response["snapshot"] = json!(snapshot);
    if json_output {
        crate::agent_protocol::write_json(&response)?;
    } else {
        print_response(spec, &response);
    }
    Ok(passes(&response))
}

#[cfg(test)]
mod tests {
    use super::{DELETABLE, NO_CALLERS, compact, scope_tally, status};
    use serde_json::json;

    #[test]
    fn status_words_follow_the_spec() {
        assert_eq!(status(&NO_CALLERS, 1, true, 0), "violated");
        assert_eq!(
            status(&NO_CALLERS, 0, true, 0),
            "holds_for_indexed_snapshot"
        );
        assert_eq!(status(&NO_CALLERS, 0, true, 1), "not_proven");
        assert_eq!(status(&NO_CALLERS, 0, false, 0), "not_proven");
        assert_eq!(status(&DELETABLE, 2, false, 3), "has_dependents");
        assert_eq!(status(&DELETABLE, 0, false, 3), "none_observed");
    }

    #[test]
    fn compact_keeps_the_decision_and_its_qualifiers() {
        let mut response = json!({
            "status": "violated",
            "assertion": {"kind": "no_callers", "meaning": "long", "runtime_exhaustive": false},
            "symbol": {"qualified": "f", "kind": "function", "file": "a.rs", "scope": "production", "signature": "fn f()", "doc": "x"},
            "callers": [{"name": "g", "site": "a.rs:3", "relation": "calls", "evidence": "structural", "confidence": "certain", "scope": "test", "signature": "fn g()", "id": "k"}],
            "callers_by_scope": {"test": 1},
            "ignored_out_of_scope": {"count": 0, "by_scope": {}},
            "unresolved_refs_matching_name": 0,
            "coverage": {"completeness": "partial", "conclusive": false, "universe": {"mode": "repository"}, "compiler_index": {"state": "missing", "projects": []}, "limitations": ["compiler index missing for configured rust project(s); run `sinter scip`", "a missing graph edge is not proof that no runtime path exists", "hand-written"], "graph": {}, "snapshot": {}, "evidence": {}},
        });
        compact(&mut response, &NO_CALLERS, false);
        assert_eq!(
            response["assertion"],
            json!({"kind": "no_callers", "runtime_exhaustive": false})
        );
        assert!(response["symbol"].get("signature").is_none());
        assert_eq!(
            response["callers"][0],
            json!({"name": "g", "site": "a.rs:3", "relation": "calls", "evidence": "structural", "confidence": "certain", "scope": "test"})
        );
        assert_eq!(
            response["coverage"]["compiler_index"],
            json!({"state": "missing"})
        );
        assert!(response["coverage"].get("graph").is_none());
        // Prose becomes codes; a constant disclaimer drops; an unrecognised
        // line survives verbatim rather than vanishing.
        assert_eq!(
            response["coverage"]["limitations"],
            json!(["scip_missing", "hand-written"])
        );
        // A zero count qualifies nothing and costs bytes.
        assert!(response.get("ignored_out_of_scope").is_none());
        assert!(response.get("unresolved_refs_matching_name").is_none());
        let mut verbose = response.clone();
        compact(&mut verbose, &NO_CALLERS, true);
        assert_eq!(verbose, response);
    }

    #[test]
    fn compact_keeps_the_counts_that_qualify_a_negative_proof() {
        let mut response = json!({
            "status": "not_proven",
            "callers": [],
            "callers_by_scope": {},
            "ignored_out_of_scope": {"count": 2, "by_scope": {"test": 2}},
            "unresolved_refs_matching_name": 3,
            "coverage": {"limitations": []},
        });
        compact(&mut response, &NO_CALLERS, false);
        assert_eq!(response["ignored_out_of_scope"]["count"], json!(2));
        assert_eq!(response["unresolved_refs_matching_name"], json!(3));
    }

    #[test]
    fn scope_tally_lists_counts_inline() {
        assert_eq!(
            scope_tally(&json!({"fixture": 1, "test": 4})),
            "fixture 1, test 4"
        );
        assert_eq!(scope_tally(&json!({})), "");
    }
}
