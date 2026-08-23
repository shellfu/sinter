//! Shared request decoding and JSON projections for graph traversal tools.
//!
//! Repository and workspace execution differ, but their traversal filters,
//! limits, and summary-first payload contract must remain identical.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::Node;
use sinter_resolve::qualified_of;

pub(crate) fn required_string(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "missing required parameter `{key}` (got: {})",
                args.as_object()
                    .map(|object| object.keys().cloned().collect::<Vec<_>>().join(", "))
                    .filter(|keys| !keys.is_empty())
                    .unwrap_or_else(|| "no arguments".to_string())
            )
        })
}

pub(crate) fn traversal_filter(args: &Value) -> Result<sinter_store::EdgeFilter> {
    let evidence = args
        .get("evidence")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let certain = args
        .get("min_confidence")
        .and_then(Value::as_str)
        .is_some_and(|confidence| confidence == "certain");
    let relations = args
        .get("relations")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut filter = crate::lookup::edge_filter(&evidence, certain)?;
    filter.relations = crate::lookup::relation_set(&relations)?;
    let scopes =
        crate::corpus::ScopeSelection::from_json(args, crate::corpus::ScopeSelection::all())?;
    if !scopes.is_all() {
        filter.scopes = Some(scopes.as_set());
    }
    Ok(filter)
}

pub(crate) fn limit(args: &Value, default: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(default as u64) as usize
}

pub(crate) fn affected_options(args: &Value) -> (usize, bool) {
    (
        limit(args, 50),
        args.get("detail").and_then(Value::as_bool).unwrap_or(false),
    )
}

pub(crate) fn node_json(node: &Node) -> Value {
    json!({
        "id": node.symbol_key().as_str(),
        "snapshot_id": node.id.as_str(),
        "symbol_key": node.symbol_key().as_str(),
        "qualified": qualified_of(node.id.as_str()),
        "name": node.name,
        "kind": node.kind.as_str(),
        "file": node.file,
        "span": {"start": node.span.start, "end": node.span.end},
        "signature": node.signature,
        "doc": node.doc,
    })
}

pub(crate) fn scoped_node_json(node: &Node, scopes: &sinter_store::ScopeIndex) -> Value {
    let mut value = node_json(node);
    value["scope"] = json!(scopes.scope_of(node).as_str());
    value
}

/// Descending per-file dependent counts, capped for an agent's context.
pub(crate) fn by_file(files: impl Iterator<Item = String>) -> Value {
    let mut counts = HashMap::<String, u64>::new();
    for file in files {
        *counts.entry(file).or_default() += 1;
    }
    let mut pairs: Vec<_> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs.truncate(10);
    json!(pairs)
}

/// Summary-first affected payload shared by repository and workspace tools.
pub(crate) fn affected_json(
    symbol: Value,
    unresolved: Value,
    scip: Option<bool>,
    entries: Vec<Value>,
    files: Value,
    counts: (usize, usize, usize),
    limit: usize,
) -> Value {
    let (total, direct, direct_files) = counts;
    let mut out = json!({
        "symbol": symbol,
        "total": total,
        "direct": direct,
        "direct_files": direct_files,
        "unresolved_refs_matching_name": unresolved,
        "by_file": files,
        "dependents": entries,
    });
    if let Some(scip) = scip {
        out["scip_evidence_available"] = json!(scip);
    }
    if total > limit {
        out["truncated"] = json!(total - limit);
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{affected_json, required_string, traversal_filter};

    #[test]
    fn required_parameters_report_the_supplied_keys() {
        let error = required_string(&json!({"wrong": "value"}), "symbol").unwrap_err();
        assert!(error.to_string().contains("got: wrong"));
    }

    #[test]
    fn traversal_arguments_share_one_decoder() {
        let filter = traversal_filter(&json!({
            "evidence": ["scope"],
            "min_confidence": "certain",
            "relations": ["calls"]
        }))
        .unwrap();
        assert_eq!(
            filter.min_confidence,
            Some(sinter_core::Confidence::Certain)
        );
        assert_eq!(filter.relations.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn affected_payload_reports_truncation() {
        let value = affected_json(json!({}), json!(0), None, vec![], json!([]), (3, 1, 1), 2);
        assert_eq!(value["truncated"], 1);
    }
}
