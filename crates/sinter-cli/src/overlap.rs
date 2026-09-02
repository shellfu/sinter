//! `sinter overlap <range>...`: map several in-flight changes (open PRs)
//! onto one graph and rank pairwise merge risk. Each range is what `git
//! diff` accepts (`main...pr-1`); PR enumeration stays outside — sinter
//! is offline, so a forge CLI supplies the ranges:
//!
//!   gh pr list --json headRefName -q '.[].headRefName' \
//!     | xargs -I{} echo "main...{}" | xargs sinter overlap
//!
//! Spans are matched against the graph of the current working tree (build
//! at the merge base for best fidelity — same caveat as `impact`, whose
//! endpoint attribution this shares).
//! Risk tiers per pair:
//!   direct — both PRs touch the same node: textual or semantic collision
//!   radius — one PR touches a node the other's touched code depends on:
//!            merges clean, breaks semantically (invisible to the forge)
//!   file   — same file only, disjoint nodes: usually fine
//! A pair where one range's endpoint is an ancestor of the other's is
//! sequential, not concurrent: its overlap is the earlier diff counted
//! twice, so it is reported as such and not scored.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::impact;

#[derive(Serialize)]
pub struct PrMap {
    pub label: String,
    rev_range: String,
    pub touched: BTreeSet<String>,
    pub radius: BTreeSet<String>,
    pub files: BTreeSet<String>,
}

#[derive(Serialize)]
pub struct Pair {
    a: String,
    b: String,
    pub risk: &'static str,
    direct: Vec<String>,
    radius: Vec<String>,
    files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// Radius tier default: call/use edges only, so file-level import noise
/// does not turn every pair into MEDIUM.
fn default_filter() -> sinter_store::EdgeFilter {
    sinter_store::EdgeFilter {
        relations: Some(
            [sinter_core::Relation::Calls, sinter_core::Relation::Uses]
                .into_iter()
                .collect(),
        ),
        ..sinter_store::EdgeFilter::default()
    }
}

fn endpoint(range: &str) -> &str {
    let end = range
        .split_once("...")
        .or_else(|| range.split_once(".."))
        .map_or(range, |(_, end)| end);
    if end.is_empty() { "HEAD" } else { end }
}

/// `LABEL=RANGE` or bare `RANGE` (label = the range itself).
fn parse_arg(arg: &str) -> (String, String) {
    match arg.split_once('=') {
        Some((label, range)) if !label.contains("...") && !label.contains("..") => {
            (label.to_string(), range.to_string())
        }
        _ => (arg.to_string(), arg.to_string()),
    }
}

fn map_one(
    repo: &Path,
    store: &sinter_store::Store,
    filter: &sinter_store::EdgeFilter,
    label: String,
    range: String,
) -> Result<PrMap> {
    let report = impact::compute_with_store(repo, &range, filter, store)?;
    let key = |s: &impact::SymbolRef| format!("{}:{}", s.file, s.qualified);
    Ok(PrMap {
        touched: report.changed_symbols.iter().map(key).collect(),
        radius: report.blast_radius.iter().map(key).collect(),
        files: report
            .changed_symbols
            .iter()
            .map(|s| s.file.clone())
            .collect(),
        label,
        rev_range: range,
    })
}

fn pair(repo: &Path, a: &PrMap, b: &PrMap) -> Pair {
    let (ea, eb) = (endpoint(&a.rev_range), endpoint(&b.rev_range));
    // Both directions true means the same commit, which is a duplicate
    // range, not a sequence.
    let (a_in_b, b_in_a) = (
        impact::is_ancestor(repo, ea, eb),
        impact::is_ancestor(repo, eb, ea),
    );
    let sequential = match (a_in_b, b_in_a) {
        (true, false) => Some(format!("{} contains {}'s endpoint", b.label, a.label)),
        (false, true) => Some(format!("{} contains {}'s endpoint", a.label, b.label)),
        _ => None,
    };
    if let Some(note) = sequential {
        return Pair {
            a: a.label.clone(),
            b: b.label.clone(),
            risk: "sequential",
            direct: Vec::new(),
            radius: Vec::new(),
            files: Vec::new(),
            note: Some(format!(
                "{note}; overlap would be the earlier diff itself, skipped"
            )),
        };
    }
    let direct: Vec<String> = a.touched.intersection(&b.touched).cloned().collect();
    let radius: Vec<String> = a
        .touched
        .intersection(&b.radius)
        .chain(b.touched.intersection(&a.radius))
        .filter(|s| !direct.contains(s))
        .cloned()
        .collect();
    let symbol_files: BTreeSet<&str> = direct
        .iter()
        .chain(radius.iter())
        .filter_map(|s| s.split(':').next())
        .collect();
    let files: Vec<String> = a
        .files
        .intersection(&b.files)
        .filter(|f| !symbol_files.contains(f.as_str()))
        .cloned()
        .collect();
    let risk = if !direct.is_empty() {
        "high"
    } else if !radius.is_empty() {
        "medium"
    } else if !files.is_empty() {
        "low"
    } else {
        "clean"
    };
    Pair {
        a: a.label.clone(),
        b: b.label.clone(),
        risk,
        direct,
        radius,
        files,
        note: None,
    }
}

/// Parse, map, pair, rank — shared by the CLI verb and the MCP tool.
/// `relations` empty means the calls/uses default.
pub fn compute(
    repo: &Path,
    args: &[String],
    relations: &[String],
) -> Result<(Vec<PrMap>, Vec<Pair>)> {
    let store = crate::lookup::open_store(repo)?;
    compute_with_store(repo, &store, args, relations)
}

pub(crate) fn compute_current(repo: &Path, args: &[String]) -> Result<(Vec<PrMap>, Vec<Pair>)> {
    let store = crate::lookup::open_current(repo)?;
    compute_with_store(repo, &store, args, &[])
}

fn compute_with_store(
    repo: &Path,
    store: &sinter_store::Store,
    args: &[String],
    relations: &[String],
) -> Result<(Vec<PrMap>, Vec<Pair>)> {
    if args.len() < 2 {
        bail!("need at least two rev-ranges (e.g. `sinter overlap main...pr-1 main...pr-2`)");
    }
    let filter = match crate::lookup::relation_set(relations)? {
        Some(relations) => sinter_store::EdgeFilter {
            relations: Some(relations),
            ..sinter_store::EdgeFilter::default()
        },
        None => default_filter(),
    };
    let mut maps = Vec::new();
    for arg in args {
        let (label, range) = parse_arg(arg);
        maps.push(map_one(repo, store, &filter, label, range)?);
    }
    let mut pairs = Vec::new();
    for i in 0..maps.len() {
        for j in i + 1..maps.len() {
            pairs.push(pair(repo, &maps[i], &maps[j]));
        }
    }
    // Riskiest first; stable label order inside each tier.
    let rank = |r: &str| match r {
        "high" => 0,
        "medium" => 1,
        "low" => 2,
        "clean" => 3,
        _ => 4,
    };
    pairs.sort_by_key(|p| rank(p.risk));
    Ok((maps, pairs))
}

pub fn run(repo: &Path, args: &[String], relations: &[String], json: bool) -> Result<()> {
    let (maps, pairs) = compute(repo, args, relations)?;

    if json {
        crate::agent_protocol::write_json(&to_json(&maps, &pairs))?;
        return Ok(());
    }
    println!("{} changes mapped, {} pairs:", maps.len(), pairs.len());
    for m in &maps {
        println!(
            "  {}  {} nodes touched, {} in radius",
            m.label,
            m.touched.len(),
            m.radius.len()
        );
    }
    println!();
    for p in &pairs {
        if p.risk == "clean" {
            println!("{} × {}: clean", p.a, p.b);
            continue;
        }
        if let Some(note) = &p.note {
            println!("{} × {}: {}  ({note})", p.a, p.b, p.risk);
            continue;
        }
        println!(
            "{} × {}: {}  ({} direct, {} radius, {} file-only)",
            p.a,
            p.b,
            p.risk.to_uppercase(),
            p.direct.len(),
            p.radius.len(),
            p.files.len()
        );
        for s in &p.direct {
            println!("  direct  {s}");
        }
        for s in &p.radius {
            println!("  radius  {s}");
        }
        for f in &p.files {
            println!("  file    {f}");
        }
    }
    Ok(())
}

/// One compatibility-preserving payload for CLI JSON and MCP. `prs` keeps
/// the full historical CLI surface; `changes` is the bounded summary older
/// MCP clients consumed. Agents can choose detail without transport drift.
pub fn to_json(maps: &[PrMap], pairs: &[Pair]) -> serde_json::Value {
    serde_json::json!({
        "prs": maps,
        "changes": maps.iter().map(|p| serde_json::json!({
            "label": p.label,
            "touched": p.touched.len(),
            "radius": p.radius.len(),
            "files": p.files.len(),
        })).collect::<Vec<_>>(),
        "pairs": pairs,
    })
}
