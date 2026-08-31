//! Snapshot-scoped assertion that a symbol has no observed call edges.
//!
//! This is deliberately narrower than `affected`: callers are depth-one
//! `calls` edges, the requested corpus scope is explicit, and an empty
//! traversal becomes a claim only when the indexed snapshot is complete and
//! no unresolved reference still names the symbol. It never claims runtime
//! exhaustiveness; the ordinary coverage envelope keeps `conclusive: false`.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::{CorpusScope, Node, Relation};
use sinter_resolve::qualified_of;
use sinter_store::{EdgeFilter, Reached, Store};

use crate::coverage::{TraversalEvidence, traversal_json, workspace_json};
use crate::lookup::{ensure_snapshot, ensure_snapshot_token, open_store, unique_symbol_in};

pub(crate) const DEFAULT_LIMIT: usize = 50;

fn call_filter(scopes: BTreeSet<CorpusScope>, certain: bool) -> EdgeFilter {
    EdgeFilter {
        relations: Some(BTreeSet::from([Relation::Calls])),
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
    node["confidence"] = json!(match reached.via.confidence {
        sinter_core::Confidence::Certain => "certain",
        sinter_core::Confidence::Inferred => "possible",
    });
    if let Some(site) = crate::render::site_location(repo, &reached.via) {
        node["site"] = json!(site);
    }
    node
}

/// Repository assertion producer.
fn repository_response(
    repo: &Path,
    store: &Store,
    symbol: &str,
    scopes: BTreeSet<CorpusScope>,
    certain: bool,
    limit: usize,
) -> Result<Value> {
    let root = crate::pipeline::discover_root(repo).canonicalize()?;
    let filter = call_filter(scopes.clone(), certain);
    let node = unique_symbol_in(store, symbol, Some(&scopes))?;
    let callers = store.dependents(&node.id, &filter, 1)?;

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
        callers.iter().map(|item| item.via.confidence),
        unresolved,
    );
    let coverage = traversal_json(&root, store, &filter, evidence, !callers.is_empty())?;
    let assertion_status = status(callers.len(), &coverage["completeness"], unresolved);
    let scope_index = store.scope_index()?;
    let mut out = json!({
        "status": assertion_status,
        "assertion": {
            "kind": "no_callers",
            "meaning": "no observed depth-one call edges in the requested corpus scope",
            "runtime_exhaustive": false,
        },
        "symbol": crate::graph_tool::scoped_node_json(&node, &scope_index),
        "observed_callers": callers.len(),
        "callers": callers.iter().take(limit).map(|r| caller_row(&root, r, &scope_index)).collect::<Vec<_>>(),
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
    symbol: &str,
    scopes: BTreeSet<CorpusScope>,
    certain: bool,
    limit: usize,
) -> Result<Value> {
    let filter = call_filter(scopes.clone(), certain);
    let (member, node) = crate::workspace::find_symbol(workspace, symbol)?;
    let callers = crate::workspace::dependents(workspace, &member, &node.id, &filter, 1)?;

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
        callers.iter().map(|item| item.evidence.confidence()),
        unresolved,
    );
    let coverage = workspace_json(workspace, &filter, evidence, !callers.is_empty())?;
    let assertion_status = status(callers.len(), &coverage["completeness"], unresolved);
    let mut out = json!({
        "status": assertion_status,
        "assertion": {
            "kind": "no_callers",
            "meaning": "no observed depth-one call edges in the requested corpus scope",
            "runtime_exhaustive": false,
        },
        "symbol": workspace_node(&member, &node, owner_scope),
        "observed_callers": callers.len(),
        "callers": callers.iter().take(limit).map(|r| {
            let mut row = workspace_node(&r.member, &r.node, scope_of(&r.member, &r.node));
            row["relation"] = json!(r.relation.as_str());
            row["evidence"] = json!(r.evidence.as_str());
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
    Ok(out)
}

fn print_response(response: &Value) {
    let symbol = response["symbol"]["qualified"].as_str().unwrap_or("?");
    let count = response["observed_callers"].as_u64().unwrap_or(0);
    println!(
        "assert no-callers {symbol}: {} ({count} observed caller(s))",
        response["status"].as_str().unwrap_or("not_proven")
    );
    for caller in response["callers"].as_array().into_iter().flatten() {
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
    crate::coverage::print_traversal_footer(&response["coverage"], response["snapshot"].as_str());
}

pub(crate) fn run_repository(
    repo: &Path,
    symbol: &str,
    scopes: BTreeSet<CorpusScope>,
    certain: bool,
    limit: usize,
    json_output: bool,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let mut response = repository_response(repo, &store, symbol, scopes, certain, limit)?;
    response["snapshot"] = json!(snapshot);
    let holds = response["status"] == "holds_for_indexed_snapshot";
    if json_output {
        crate::agent_protocol::write_json(&response)?;
    } else {
        print_response(&response);
    }
    Ok(holds)
}

pub(crate) fn run_workspace(
    manifest: &Path,
    symbol: &str,
    scopes: BTreeSet<CorpusScope>,
    certain: bool,
    limit: usize,
    json_output: bool,
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
    let mut response = workspace_response(&workspace, symbol, scopes, certain, limit)?;
    response["snapshot"] = json!(snapshot);
    let holds = response["status"] == "holds_for_indexed_snapshot";
    if json_output {
        crate::agent_protocol::write_json(&response)?;
    } else {
        print_response(&response);
    }
    Ok(holds)
}
