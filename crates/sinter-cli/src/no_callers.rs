//! Snapshot-scoped assertions that a symbol has no observed incoming edges
//! of a given shape (`no-callers`, `no-writers`, `no-dependents`).
//!
//! This is deliberately narrower than `affected`: rows are depth-one
//! edges of the spec's relations, the requested corpus scope is explicit, and an empty
//! traversal becomes a claim only when the indexed snapshot is complete and
//! no unresolved reference still names the symbol. It never claims runtime
//! exhaustiveness; the ordinary coverage envelope keeps `conclusive: false`.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::{Confidence, CorpusScope, Node, Relation, SymbolKind};
use sinter_resolve::qualified_of;
use sinter_store::{EdgeFilter, Reached, Store};

use crate::coverage::{TraversalEvidence, traversal_json, workspace_json};
use crate::lookup::{ensure_snapshot, ensure_snapshot_token, open_store, unique_symbol_in};

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
}

pub(crate) const NO_CALLERS: AssertionSpec = AssertionSpec {
    kind: "no_callers",
    label: "no-callers",
    noun: "caller",
    rows: "callers",
    meaning: "no observed depth-one call edges in the requested corpus scope",
    relations: &[Relation::Calls],
};

pub(crate) const NO_WRITERS: AssertionSpec = AssertionSpec {
    kind: "no_writers",
    label: "no-writers",
    noun: "writer",
    rows: "callers",
    meaning: "no observed depth-one writes/alters/drops edges in the requested corpus scope",
    relations: &[Relation::Writes, Relation::Alters, Relation::Drops],
};

/// Every non-containment incoming relation. `imports` edges only ever
/// count as `possible`: a `use` line names a symbol without proving a
/// dependency on it.
pub(crate) const NO_DEPENDENTS: AssertionSpec = AssertionSpec {
    kind: "no_dependents",
    label: "no-dependents",
    noun: "dependent",
    rows: "dependents",
    meaning: "no observed depth-one non-containment edges (calls, uses, imports as possible, implements, extends, reads, writes, creates, alters, drops) in the requested corpus scope",
    relations: &[
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
    ],
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

/// Per-query envelope minus the repository-wide `graph` block that
/// `doctor` already reports; `--verbose` keeps it.
fn trim_coverage(coverage: &mut Value, verbose: bool) {
    if verbose || crate::coverage::verbose() {
        return;
    }
    if let Some(object) = coverage.as_object_mut() {
        object.remove("graph");
        if let Some(members) = object.get_mut("members").and_then(Value::as_object_mut) {
            for member in members.values_mut() {
                if let Some(member) = member.as_object_mut() {
                    member.remove("graph");
                }
            }
        }
    }
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

fn status(callers: usize, completeness: &Value, unresolved: usize) -> &'static str {
    if callers > 0 {
        "violated"
    } else if completeness == "complete_for_indexed_snapshot" && unresolved == 0 {
        "holds_for_indexed_snapshot"
    } else {
        "not_proven"
    }
}

fn caller_row(repo: &Path, reached: &Reached, scopes: &sinter_store::ScopeIndex) -> Value {
    let mut node = crate::graph_tool::scoped_node_json(&reached.node, scopes);
    node["relation"] = json!(reached.via.relation.as_str());
    node["evidence"] = json!(reached.via.evidence.as_str());
    node["confidence"] = json!(
        match edge_confidence(reached.via.relation, reached.via.confidence) {
            Confidence::Certain => "certain",
            Confidence::Inferred => "possible",
        }
    );
    if let Some(site) = crate::render::site_location(repo, &reached.via) {
        node["site"] = json!(site);
    }
    node
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
    let coverage = traversal_json(&root, store, &filter, evidence, !callers.is_empty())?;
    let assertion_status = status(callers.len(), &coverage["completeness"], unresolved);
    let scope_index = store.scope_index()?;
    let mut out = json!({
        "status": assertion_status,
        "assertion": {
            "kind": spec.kind,
            "meaning": spec.meaning,
            "runtime_exhaustive": false,
        },
        "symbol": crate::graph_tool::scoped_node_json(&node, &scope_index),
        format!("observed_{}", spec.rows): callers.len(),
        spec.rows: callers.iter().take(limit).map(|r| caller_row(&root, r, &scope_index)).collect::<Vec<_>>(),
        "ignored_out_of_scope": {
            "count": ignored.len(),
            "by_scope": ignored.iter().fold(std::collections::BTreeMap::<&str, usize>::new(), |mut counts, r| {
                *counts.entry(scope_index.scope_of(&r.node).as_str()).or_default() += 1;
                counts
            }),
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

fn workspace_node(member: &str, node: &Node, scope: CorpusScope) -> Value {
    json!({
        "member": member,
        "id": format!("{member}:{}", node.symbol_key()),
        "symbol_key": node.symbol_key().as_str(),
        "qualified": format!("{member}:{}", qualified_of(node.id.as_str())),
        "name": node.name,
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
    let assertion_status = status(callers.len(), &coverage["completeness"], unresolved);
    let mut out = json!({
        "status": assertion_status,
        "assertion": {
            "kind": spec.kind,
            "meaning": spec.meaning,
            "runtime_exhaustive": false,
        },
        "symbol": workspace_node(&member, &node, owner_scope),
        format!("observed_{}", spec.rows): callers.len(),
        spec.rows: callers.iter().take(limit).map(|r| {
            let mut row = workspace_node(&r.member, &r.node, scope_of(&r.member, &r.node));
            row["relation"] = json!(r.relation.as_str());
            row["evidence"] = json!(r.evidence.as_str());
            row["confidence"] = json!(match edge_confidence(r.relation, r.evidence.confidence()) {
                Confidence::Certain => "certain",
                Confidence::Inferred => "possible",
            });
            row
        }).collect::<Vec<_>>(),
        "ignored_out_of_scope": {
            "count": ignored.len(),
            "by_scope": ignored.iter().fold(std::collections::BTreeMap::<&str, usize>::new(), |mut counts, r| {
                *counts.entry(scope_of(&r.member, &r.node).as_str()).or_default() += 1;
                counts
            }),
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
    for caller in response[spec.rows].as_array().into_iter().flatten() {
        println!(
            "  {}  {}  [{} / {}]",
            caller["qualified"].as_str().unwrap_or("?"),
            caller["site"]
                .as_str()
                .or_else(|| caller["file"].as_str())
                .unwrap_or("?"),
            caller["relation"].as_str().unwrap_or("calls"),
            caller["evidence"].as_str().unwrap_or("?"),
        );
    }
    println!(
        "  ignored out of scope: {}; unresolved refs matching name: {}",
        response["ignored_out_of_scope"]["count"], response["unresolved_refs_matching_name"],
    );
    if let Some(hint) = response["hint"].as_str() {
        println!("  hint: {hint}");
    }
    crate::coverage::print_traversal_footer(&response["coverage"], response["snapshot"].as_str());
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
    trim_coverage(&mut response["coverage"], verbose);
    response["snapshot"] = json!(snapshot);
    let holds = response["status"] == "holds_for_indexed_snapshot";
    if json_output {
        crate::agent_protocol::write_json(&response)?;
    } else {
        print_response(spec, &response);
    }
    Ok(holds)
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
    trim_coverage(&mut response["coverage"], verbose);
    response["snapshot"] = json!(snapshot);
    let holds = response["status"] == "holds_for_indexed_snapshot";
    if json_output {
        crate::agent_protocol::write_json(&response)?;
    } else {
        print_response(spec, &response);
    }
    Ok(holds)
}
