//! `sinter impact <rev-range>`: changed symbols -> blast radius -> affected
//! tests. Line hunks come from `git diff -U0`; spans are matched against the
//! graph built from the working tree, so build before asking.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sinter_core::{Node, SymbolKind};
use sinter_resolve::qualified_of;
use sinter_store::EdgeFilter;

use crate::lookup::open_store;

#[derive(Serialize)]
pub struct ImpactReport {
    pub rev_range: String,
    pub changed_symbols: Vec<SymbolRef>,
    pub blast_radius: Vec<SymbolRef>,
    pub affected_tests: Vec<SymbolRef>,
}

#[derive(Serialize, Clone)]
pub struct SymbolRef {
    pub qualified: String,
    pub kind: &'static str,
    pub file: String,
}

fn symbol_ref(node: &Node) -> SymbolRef {
    SymbolRef {
        qualified: qualified_of(node.id.as_str()).to_string(),
        kind: node.kind.as_str(),
        file: node.file.clone(),
    }
}

/// Test detection heuristic: conventional test files and names.
fn is_test(node: &Node) -> bool {
    let f = &node.file;
    f.ends_with("_test.go")
        || f.starts_with("tests/")
        || f.contains("/tests/")
        || f.contains("/test/")
        || node.name.starts_with("test_")
        || node.name.starts_with("Test")
}

pub fn compute(repo: &Path, rev_range: &str) -> Result<ImpactReport> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;

    // New-side hunks per file from git.
    let output = Command::new("git")
        .args([
            "-c",
            "diff.noprefix=false",
            "diff",
            "--no-ext-diff",
            "-U0",
            "--no-color",
            rev_range,
        ])
        .current_dir(&repo)
        .output()
        .context("run git diff")?;
    if !output.status.success() {
        bail!(
            "git diff {rev_range} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut hunks: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            // Deletions ("+++ /dev/null") and quoted/unexpected paths must
            // clear the current file, never inherit the previous one.
            current = rest.strip_prefix("b/").map(str::to_string);
        } else if let (Some(file), Some(rest)) = (&current, line.strip_prefix("@@ ")) {
            // "@@ -a,b +c,d @@" — take the new-side c,d.
            if let Some(plus) = rest.split_whitespace().find(|t| t.starts_with('+')) {
                let mut parts = plus[1..].splitn(2, ',');
                let start: usize = parts.next().unwrap_or("0").parse().unwrap_or(0);
                let count: usize = parts.next().map_or(1, |c| c.parse().unwrap_or(1));
                if start > 0 {
                    hunks
                        .entry(file.clone())
                        .or_default()
                        .push((start, count.max(1)));
                }
            }
        }
    }

    // Changed symbols: nodes whose byte span overlaps a changed line range.
    let mut changed: Vec<Node> = Vec::new();
    for (file, ranges) in &hunks {
        let Some(facts) = store.facts(file)? else {
            continue; // not a language file, or not built
        };
        let Ok(source) = std::fs::read_to_string(repo.join(file)) else {
            continue;
        };
        let mut line_starts = vec![0u64];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u64 + 1);
            }
        }
        let byte_range = |line: usize, count: usize| -> (u64, u64) {
            let start = line_starts
                .get(line - 1)
                .copied()
                .unwrap_or(source.len() as u64);
            let end = line_starts
                .get(line - 1 + count)
                .copied()
                .unwrap_or(source.len() as u64);
            (start, end)
        };
        for node in &facts.nodes {
            if node.kind == SymbolKind::File {
                continue;
            }
            let touched = ranges.iter().any(|&(l, c)| {
                let (s, e) = byte_range(l, c);
                node.span.start < e && s < node.span.end
            });
            if touched {
                changed.push(node.clone());
            }
        }
    }

    // Blast radius: union of dependents of every changed symbol.
    let filter = EdgeFilter::default();
    let mut radius: BTreeMap<String, Node> = BTreeMap::new();
    for node in &changed {
        for reached in store.dependents(&node.id, &filter, 25)? {
            radius.insert(reached.node.id.as_str().to_string(), reached.node);
        }
    }
    for node in &changed {
        radius.remove(node.id.as_str());
    }

    let affected_tests: Vec<SymbolRef> = radius
        .values()
        .chain(changed.iter())
        .filter(|n| is_test(n))
        .map(symbol_ref)
        .collect();

    Ok(ImpactReport {
        rev_range: rev_range.to_string(),
        changed_symbols: changed.iter().map(symbol_ref).collect(),
        blast_radius: radius.values().map(symbol_ref).collect(),
        affected_tests,
    })
}

pub fn run(repo: &Path, rev_range: &str, manifest: Option<&Path>) -> Result<()> {
    let mut report = compute(repo, rev_range)?;
    // Workspace mode: follow boundary links out of the changed member and
    // continue the blast radius inside the other members.
    if let Some(manifest) = manifest {
        let ws = crate::workspace::load(manifest)?;
        let repo_canon = repo.canonicalize()?;
        let member = ws
            .members
            .iter()
            .find(|(_, path)| **path == repo_canon)
            .map(|(name, _)| name.clone())
            .ok_or_else(|| anyhow::anyhow!("--repo is not a member of this workspace"))?;
        // Resolve changed symbols to node ids first, then drop the handle:
        // workspace traversal opens every member store itself, and redb
        // forbids a second open of the same file in-process.
        let changed_ids: Vec<sinter_core::NodeId> = {
            let store = open_store(&repo_canon)?;
            report
                .changed_symbols
                .iter()
                .filter_map(|c| {
                    crate::lookup::unique_symbol(&store, &c.qualified)
                        .ok()
                        .map(|n| n.id)
                })
                .collect()
        };
        let filter = EdgeFilter::default();
        let mut cross: std::collections::BTreeMap<String, SymbolRef> =
            std::collections::BTreeMap::new();
        for node_id in &changed_ids {
            for reached in crate::workspace::dependents(&ws, &member, node_id, &filter, 25)? {
                if reached.member == member {
                    continue; // local radius already counted
                }
                let key = format!("{}:{}", reached.member, reached.node.id.as_str());
                let mut sym = symbol_ref(&reached.node);
                sym.file = format!("{}:{}", reached.member, sym.file);
                if is_test(&reached.node) {
                    report.affected_tests.push(sym.clone());
                }
                cross.insert(key, sym);
            }
        }
        report.blast_radius.extend(cross.into_values());
    }
    println!(
        "impact {}: {} changed symbols, {} in blast radius, {} tests affected",
        report.rev_range,
        report.changed_symbols.len(),
        report.blast_radius.len(),
        report.affected_tests.len()
    );
    println!("changed:");
    for s in &report.changed_symbols {
        println!("  {} {}  {}", s.kind, s.qualified, s.file);
    }
    println!("blast radius:");
    for s in &report.blast_radius {
        println!("  {} {}  {}", s.kind, s.qualified, s.file);
    }
    println!("affected tests:");
    for s in &report.affected_tests {
        println!("  {} {}  {}", s.kind, s.qualified, s.file);
    }
    Ok(())
}

/// Also usable by the MCP server.
pub fn to_json(report: &ImpactReport) -> serde_json::Value {
    serde_json::to_value(report).expect("impact report serializes")
}
