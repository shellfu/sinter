//! Coverage contract for negative graph answers. A graph miss is evidence
//! about the indexed snapshot, never proof that source/runtime behavior is
//! absent. This module makes the limits machine-readable and keeps the CLI
//! and MCP wording aligned.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sinter_store::Store;

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

/// Machine-readable trust envelope for a negative answer.
pub fn negative_json(repo: &Path, store: &Store) -> Result<serde_json::Value> {
    let repo = crate::pipeline::discover_root(repo);
    let health = read_health(&repo);
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let dirty = git_output(
        &repo,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )
    .map(|s| !s.is_empty());
    let indexable_languages = crate::scip::indexable_languages(&repo);
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

    let mut limitations = vec![
        "a missing graph edge is not proof that no runtime path exists".to_string(),
        "dynamic dispatch edges are conservative candidates, not dependency-injection proof"
            .to_string(),
    ];
    if scip_state == "missing" && !indexable_languages.is_empty() {
        limitations.push(format!(
            "compiler index missing for {}; run `sinter scip`",
            indexable_languages.join(", ")
        ));
    } else if scip_state == "stale" {
        limitations.push(format!(
            "compiler index is stale ({stale_inputs} newer source/config inputs); run `sinter scip`"
        ));
    }
    if !health.failed_files.is_empty() {
        limitations.push("one or more files failed extraction and are unindexed".to_string());
    }
    if !health.syntax_error_files.is_empty() {
        limitations.push("one or more files were indexed from partial syntax trees".to_string());
    }

    Ok(serde_json::json!({
        "status": "not_proven",
        "conclusive": false,
        "snapshot": {
            "head": head,
            "dirty": dirty,
            "working_tree_indexed": true,
            "node_id_scope": "snapshot",
            "graph_schema": Store::CURRENT_SCHEMA,
        },
        "compiler_index": {
            "state": scip_state,
            "indexable_languages": indexable_languages,
            "stale_inputs": stale_inputs,
        },
        "graph": {
            "unresolved_references": unresolved.len(),
            "unresolved_by_reason": reasons,
            "syntax_error_files": health.syntax_error_files,
            "unindexed_files": health.failed_files.keys().collect::<Vec<_>>(),
            "excluded_derived_roots": crate::corpus::DERIVED_ROOTS,
        },
        "limitations": limitations,
    }))
}

pub fn print_negative(repo: &Path, store: &Store) -> Result<()> {
    let coverage = negative_json(repo, store)?;
    println!("  status: not proven (graph coverage is not an absence proof)");
    if let Some(items) = coverage["limitations"].as_array() {
        for item in items {
            if let Some(text) = item.as_str() {
                println!("  coverage: {text}");
            }
        }
    }
    Ok(())
}

pub fn print_workspace_negative(workspace: &crate::workspace::Workspace) -> Result<()> {
    println!("  status: not proven (workspace graph coverage is not an absence proof)");
    for (name, repo) in &workspace.members {
        let Ok(store) = Store::open(crate::pipeline::db_path(repo)) else {
            println!("  coverage: {name}: graph unavailable");
            continue;
        };
        let coverage = negative_json(repo, &store)?;
        println!(
            "  coverage: {name}: SCIP {}, {} unresolved, dirty {}",
            coverage["compiler_index"]["state"]
                .as_str()
                .unwrap_or("unknown"),
            coverage["graph"]["unresolved_references"]
                .as_u64()
                .unwrap_or(0),
            coverage["snapshot"]["dirty"]
                .as_bool()
                .map_or("unknown".to_string(), |dirty| dirty.to_string()),
        );
    }
    Ok(())
}
