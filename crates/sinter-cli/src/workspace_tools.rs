//! Routing and execution of agent graph tools across workspace members.
//!
//! This boundary owns member addressing, per-member freshness, and traversal
//! across declared repository links. It deliberately does not own MCP framing.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::Node;
use sinter_resolve::qualified_of;

use crate::graph_tool::{
    affected_json, affected_options, by_file, limit, required_string, traversal_filter,
};
use crate::lookup::{ensure_snapshot_token, open_store, unique_symbol};

pub(crate) fn call(manifest: &Path, name: &str, args: &Value) -> Result<Value> {
    let workspace = crate::workspace::load(manifest)?;
    for repo in workspace.members.values() {
        crate::pipeline::build(repo, None)?;
    }
    if !crate::workspace::stale_members(&workspace)?.is_empty() {
        crate::workspace::refresh(&workspace)?;
    }

    let snapshot = matches!(name, "show" | "query" | "affected" | "path")
        .then(|| crate::workspace::snapshot_token(&workspace))
        .transpose()?;
    if let Some(snapshot) = snapshot.as_deref() {
        ensure_snapshot_token(args.get("if_snapshot").and_then(Value::as_str), snapshot)?;
    }

    let mut result = match name {
        "ask" => ask(&workspace, args),
        "show" => show(&workspace, args),
        "impact" => impact(&workspace, args),
        "unresolved" => unresolved(&workspace, args),
        "query" => query(&workspace, args),
        "affected" => affected(&workspace, args),
        "path" => path(&workspace, args),
        other => anyhow::bail!(
            "unknown tool {other} (workspace scope serves: ask, show, query, affected, path, impact)"
        ),
    }?;
    if let Some(snapshot) = snapshot {
        result["snapshot"] = json!(snapshot);
    }
    Ok(result)
}

fn member_node(node: &Node, member: &str, scope: sinter_core::CorpusScope) -> Value {
    json!({
        "member": member,
        "id": format!("{member}:{}", node.symbol_key()),
        "snapshot_id": format!("{member}:{}", node.id),
        "symbol_key": node.symbol_key().as_str(),
        "qualified": format!("{member}:{}", qualified_of(node.id.as_str())),
        "name": node.name,
        "kind": node.kind.as_str(),
        "scope": scope.as_str(),
        "file": node.file,
        "signature": node.signature,
        "doc": node.doc,
    })
}

fn member_scopes(
    workspace: &crate::workspace::Workspace,
) -> Result<BTreeMap<String, std::collections::HashMap<String, sinter_core::CorpusScope>>> {
    workspace
        .members
        .iter()
        .map(|(member, repo)| {
            let store = sinter_store::Store::open(crate::pipeline::db_path(repo))?;
            Ok((member.clone(), store.file_scopes()?))
        })
        .collect()
}

fn scope_of(
    scopes: &BTreeMap<String, std::collections::HashMap<String, sinter_core::CorpusScope>>,
    member: &str,
    node: &Node,
) -> sinter_core::CorpusScope {
    scopes[member]
        .get(&node.file)
        .copied()
        .unwrap_or_else(|| sinter_core::CorpusScope::classify_path(&node.file))
}

fn ask(workspace: &crate::workspace::Workspace, args: &Value) -> Result<Value> {
    let limit = limit(args, 5);
    let question = required_string(args, "question")?;
    let scopes = crate::corpus::ScopeSelection::from_json(
        args,
        crate::corpus::ScopeSelection::agent_default(),
    )?;
    let candidate_limit = crate::ask::workspace_candidate_limit(&question, limit);
    let mut responses = Vec::new();
    for (member, repo) in &workspace.members {
        responses.push((
            member.clone(),
            crate::ask::ask_response_json(repo, &question, candidate_limit, &scopes)?,
        ));
    }
    Ok(crate::ask::merge_workspace_responses(
        &question, limit, &scopes, responses,
    ))
}

fn show(workspace: &crate::workspace::Workspace, args: &Value) -> Result<Value> {
    let (member, node) =
        crate::workspace::find_symbol(workspace, &required_string(args, "symbol")?)?;
    let links = crate::workspace::LinkStore::open(workspace)?;
    let link_json = |member: &str, id: &str, link: &crate::workspace::Link| {
        json!({
            "member": member,
            "symbol": qualified_of(id),
            "relation": link.relation.as_str(),
            "evidence": link.evidence.as_str(),
            "via": link.via,
        })
    };
    let boundary_outgoing: Vec<Value> = links
        .out_links(&member, node.id.as_str())?
        .iter()
        .map(|link| link_json(&link.dst_member, &link.dst_id, link))
        .collect();
    let boundary_incoming: Vec<Value> = links
        .in_links(&member, node.id.as_str())?
        .iter()
        .map(|link| link_json(&link.src_member, &link.src_id, link))
        .collect();
    let member_root = workspace.members[&member].clone();
    let store = sinter_store::Store::open(crate::pipeline::db_path(&member_root))?;
    let scope = store.file_scope(&node.file)?;
    let edge_json = |edge: &sinter_core::Edge, other: &sinter_core::NodeId| {
        json!({
            "symbol": qualified_of(other.as_str()),
            "relation": edge.relation.as_str(),
            "evidence": edge.evidence.as_str(),
            "site": crate::render::site_json(&member_root, edge),
        })
    };
    let outgoing: Vec<Value> = store
        .out_edges(&node.id)?
        .iter()
        .map(|edge| edge_json(edge, &edge.dst))
        .collect();
    let incoming: Vec<Value> = store
        .in_edges(&node.id)?
        .iter()
        .map(|edge| edge_json(edge, &edge.src))
        .collect();
    Ok(json!({
        "symbol": member_node(&node, &member, scope),
        "outgoing": outgoing,
        "incoming": incoming,
        "boundary_outgoing": boundary_outgoing,
        "boundary_incoming": boundary_incoming,
    }))
}

fn impact(workspace: &crate::workspace::Workspace, args: &Value) -> Result<Value> {
    let member = required_string(args, "member")?;
    let repo = workspace
        .members
        .get(&member)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown member `{member}` (members: {})",
                workspace
                    .members
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?
        .clone();
    let mut report = crate::impact::compute(&repo, &required_string(args, "rev_range")?)?;
    let changed_ids: Vec<sinter_core::NodeId> = {
        let store = open_store(&repo)?;
        report
            .changed_symbols
            .iter()
            .filter_map(|changed| {
                unique_symbol(&store, &changed.qualified)
                    .ok()
                    .map(|node| node.id)
            })
            .collect()
    };
    let filter = sinter_store::EdgeFilter::default();
    let mut cross: BTreeMap<String, crate::impact::SymbolRef> = BTreeMap::new();
    for node_id in &changed_ids {
        for reached in crate::workspace::dependents(workspace, &member, node_id, &filter, 25)? {
            if reached.member == member {
                continue;
            }
            let key = format!("{}:{}", reached.member, reached.node.id.as_str());
            let symbol = crate::impact::SymbolRef {
                qualified: qualified_of(reached.node.id.as_str()).to_string(),
                kind: reached.node.kind.as_str(),
                file: format!("{}:{}", reached.member, reached.node.file),
            };
            if crate::impact::is_test(&reached.node) {
                report.affected_tests.push(symbol.clone());
            }
            cross.insert(key, symbol);
        }
    }
    report.blast_radius.extend(cross.into_values());
    Ok(crate::impact::to_json(&report))
}

fn unresolved(workspace: &crate::workspace::Workspace, args: &Value) -> Result<Value> {
    let optional = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let limit = limit(args, 50);
    let wanted_member = optional("member");
    let mut total = 0usize;
    let mut entries = Vec::new();
    for (member, repo) in &workspace.members {
        if wanted_member.is_some_and(|wanted| wanted != member) {
            continue;
        }
        let store = open_store(repo)?;
        let refs = store.unresolved_details(optional("file"), optional("name"))?;
        total += refs.len();
        let room = limit.saturating_sub(entries.len());
        let classifier = crate::coverage::Classifier::new(repo, &store, &refs)?;
        let mut part = crate::unresolved::to_json(repo, &classifier, &refs, room);
        for entry in part["unresolved"].as_array_mut().into_iter().flatten() {
            entry["member"] = json!(member);
            entry["file"] = json!(format!("{member}:{}", entry["file"].as_str().unwrap_or("")));
        }
        entries.extend(part["unresolved"].as_array().cloned().unwrap_or_default());
    }
    let mut out = json!({"total": total, "unresolved": entries});
    if total > limit {
        out["truncated"] = json!(total - limit);
    }
    Ok(out)
}

fn query(workspace: &crate::workspace::Workspace, args: &Value) -> Result<Value> {
    let (member, node) =
        crate::workspace::find_symbol(workspace, &required_string(args, "symbol")?)?;
    let selection =
        crate::corpus::ScopeSelection::from_json(args, crate::corpus::ScopeSelection::all())?;
    let store = sinter_store::Store::open(crate::pipeline::db_path(&workspace.members[&member]))?;
    let scope = store.file_scope(&node.file)?;
    if !selection.contains(scope) {
        anyhow::bail!(
            "symbol `{}` is in scope `{}` which the request excluded",
            required_string(args, "symbol")?,
            scope.as_str()
        );
    }
    Ok(json!({"scope": selection.json(), "result": member_node(&node, &member, scope)}))
}

fn affected(workspace: &crate::workspace::Workspace, args: &Value) -> Result<Value> {
    let filter = traversal_filter(args)?;
    let depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(10) as usize;
    let (limit, detail) = affected_options(args);
    let (member, node) =
        crate::workspace::find_symbol(workspace, &required_string(args, "symbol")?)?;
    let reached = crate::workspace::dependents(workspace, &member, &node.id, &filter, depth)?;
    let scopes = member_scopes(workspace)?;
    let store = sinter_store::Store::open(crate::pipeline::db_path(&workspace.members[&member]))?;
    let unresolved = store.unresolved_named(&node.name)?;
    let entries: Vec<Value> = reached
        .iter()
        .take(limit)
        .map(|reached| {
            if detail {
                json!({
                    "node": member_node(
                        &reached.node,
                        &reached.member,
                        scope_of(&scopes, &reached.member, &reached.node),
                    ),
                    "relation": reached.relation.as_str(),
                    "evidence": reached.evidence.as_str(),
                    "confidence": match reached.evidence.confidence() {
                        sinter_core::Confidence::Certain => "certain",
                        sinter_core::Confidence::Inferred => "possible",
                    },
                    "parent": format!("{}:{}", reached.parent.0, qualified_of(&reached.parent.1)),
                })
            } else {
                json!({
                    "s": format!("{}:{}", reached.member, qualified_of(reached.node.id.as_str())),
                    "k": reached.node.kind.as_str(),
                    "f": reached.node.file,
                    "scope": scope_of(&scopes, &reached.member, &reached.node).as_str(),
                    "e": format!("{}/{}", reached.relation.as_str(), reached.evidence.as_str()),
                    "c": match reached.evidence.confidence() {
                        sinter_core::Confidence::Certain => "certain",
                        sinter_core::Confidence::Inferred => "possible",
                    },
                    "p": format!("{}:{}", reached.parent.0, qualified_of(&reached.parent.1)),
                })
            }
        })
        .collect();
    let direct: Vec<_> = reached
        .iter()
        .filter(|reached| reached.parent.0 == member && reached.parent.1 == node.id.as_str())
        .collect();
    let direct_files = direct
        .iter()
        .map(|reached| (reached.member.as_str(), reached.node.file.as_str()))
        .collect::<HashSet<_>>()
        .len();
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        reached.iter().map(|item| item.evidence.confidence()),
        unresolved,
    );
    drop(store);
    let mut out = affected_json(
        member_node(&node, &member, scope_of(&scopes, &member, &node)),
        json!(unresolved),
        None,
        entries,
        by_file(reached.iter().map(|reached| reached.node.file.clone())),
        (reached.len(), direct.len(), direct_files),
        limit,
    );
    out["coverage"] =
        crate::coverage::workspace_json(workspace, &filter, evidence, !reached.is_empty())?;
    Ok(out)
}

fn path(workspace: &crate::workspace::Workspace, args: &Value) -> Result<Value> {
    let (from_member, from) =
        crate::workspace::find_symbol(workspace, &required_string(args, "from")?)?;
    let (to_member, to) = crate::workspace::find_symbol(workspace, &required_string(args, "to")?)?;
    let filter = traversal_filter(args)?;
    let steps = crate::workspace::shortest_path(
        workspace,
        (&from_member, &from.id),
        (&to_member, &to.id),
        &filter,
    )?;
    let evidence = crate::coverage::TraversalEvidence::from_confidences(
        steps
            .iter()
            .flatten()
            .map(|(_, _, _, evidence, _, _)| evidence.confidence()),
        0,
    );
    let mut out = json!({
        "found": steps.is_some(),
        "steps": steps.iter().flatten().map(
            |(from_member, from_id, relation, evidence, to_member, to_id)| json!({
                "from": format!("{from_member}:{}", qualified_of(from_id)),
                "to": format!("{to_member}:{}", qualified_of(to_id)),
                "relation": relation.as_str(),
                "evidence": evidence.as_str(),
                "confidence": match evidence.confidence() {
                    sinter_core::Confidence::Certain => "certain",
                    sinter_core::Confidence::Inferred => "possible",
                },
            })
        ).collect::<Vec<_>>(),
    });
    out["coverage"] =
        crate::coverage::workspace_json(workspace, &filter, evidence, steps.is_some())?;
    Ok(out)
}
