//! `sinter overlap <range>...`: map several in-flight changes (open PRs)
//! onto one graph and rank pairwise merge risk. Each range is what `git
//! diff` accepts (`main...pr-1`); PR enumeration stays outside — sinter
//! is offline, so a forge CLI supplies the ranges:
//!
//!   gh pr list --json headRefName -q '.[].headRefName' \
//!     | xargs -I{} echo "main...{}" | xargs sinter overlap
//!
//! Spans are matched against the graph of the current working tree (build
//! at the merge base for best fidelity — same caveat as `impact`).
//! Risk tiers per pair:
//!   direct — both PRs touch the same node: textual or semantic collision
//!   radius — one PR touches a node the other's touched code depends on:
//!            merges clean, breaks semantically (invisible to the forge)
//!   file   — same file only, disjoint nodes: usually fine

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::impact;

#[derive(Serialize)]
struct PrMap {
    label: String,
    rev_range: String,
    touched: BTreeSet<String>,
    radius: BTreeSet<String>,
    files: BTreeSet<String>,
}

#[derive(Serialize)]
struct Pair {
    a: String,
    b: String,
    risk: &'static str,
    direct: Vec<String>,
    radius: Vec<String>,
    files: Vec<String>,
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

fn map_one(repo: &Path, label: String, range: String) -> Result<PrMap> {
    let report = impact::compute(repo, &range)?;
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

fn pair(a: &PrMap, b: &PrMap) -> Pair {
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
    }
}

pub fn run(repo: &Path, args: &[String], json: bool) -> Result<()> {
    if args.len() < 2 {
        bail!("need at least two rev-ranges (e.g. `sinter overlap main...pr-1 main...pr-2`)");
    }
    let mut maps = Vec::new();
    for arg in args {
        let (label, range) = parse_arg(arg);
        maps.push(map_one(repo, label, range)?);
    }
    let mut pairs = Vec::new();
    for i in 0..maps.len() {
        for j in i + 1..maps.len() {
            pairs.push(pair(&maps[i], &maps[j]));
        }
    }
    // Riskiest first; stable label order inside each tier.
    let rank = |r: &str| match r {
        "high" => 0,
        "medium" => 1,
        "low" => 2,
        _ => 3,
    };
    pairs.sort_by_key(|p| rank(p.risk));

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "prs": maps, "pairs": pairs }))?
        );
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
