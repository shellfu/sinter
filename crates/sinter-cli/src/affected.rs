use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use crate::lookup::{edge_filter, open_store, unique_symbol};

/// `sinter affected`: reverse blast radius — everything transitively
/// depending on the symbol, cross-file.
pub fn run(
    repo: &Path,
    symbol: &str,
    evidence: &[String],
    certain: bool,
    max_depth: usize,
) -> Result<()> {
    let store = open_store(repo)?;
    let node = unique_symbol(&store, symbol)?;
    let filter = edge_filter(evidence, certain)?;
    let reached = store.dependents(&node.id, &filter, max_depth)?;
    println!(
        "{} dependents of {} ({})",
        reached.len(),
        qualified_of(node.id.as_str()),
        node.file
    );
    // Render as a real tree: each dependent indents under the node it
    // actually reaches (via.dst), not under whatever BFS printed last.
    let mut children: std::collections::HashMap<&str, Vec<&sinter_store::Reached>> =
        std::collections::HashMap::new();
    for r in &reached {
        children.entry(r.via.dst.as_str()).or_default().push(r);
    }
    let mut stack: Vec<(&sinter_store::Reached, usize)> = Vec::new();
    if let Some(roots) = children.get(node.id.as_str()) {
        for r in roots.iter().rev() {
            stack.push((r, 1));
        }
    }
    while let Some((r, depth)) = stack.pop() {
        println!(
            "  {}{} {}  {}  [{}/{}]",
            "  ".repeat(depth - 1),
            qualified_of(r.node.id.as_str()),
            r.node.kind.as_str(),
            r.node.file,
            r.via.relation.as_str(),
            r.via.evidence.as_str(),
        );
        if let Some(kids) = children.get(r.node.id.as_str()) {
            for kid in kids.iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }
    Ok(())
}

/// `sinter affected --workspace`: cross-repo blast radius over member
/// stores plus boundary links.
pub fn run_workspace(
    manifest: &std::path::Path,
    symbol: &str,
    evidence: &[String],
    certain: bool,
    max_depth: usize,
) -> Result<()> {
    let ws = crate::workspace::load(manifest)?;
    let (member, node) = crate::workspace::find_symbol(&ws, symbol)?;
    let filter = crate::lookup::edge_filter(evidence, certain)?;
    let reached = crate::workspace::dependents(&ws, &member, &node.id, &filter, max_depth)?;
    println!(
        "{} dependents of {member}:{} ({})",
        reached.len(),
        qualified_of(node.id.as_str()),
        node.file
    );
    let mut children: std::collections::HashMap<(&str, &str), Vec<&crate::workspace::WsReached>> =
        std::collections::HashMap::new();
    for r in &reached {
        children
            .entry((r.parent.0.as_str(), r.parent.1.as_str()))
            .or_default()
            .push(r);
    }
    let mut stack: Vec<(&crate::workspace::WsReached, usize)> = Vec::new();
    if let Some(roots) = children.get(&(member.as_str(), node.id.as_str())) {
        for r in roots.iter().rev() {
            stack.push((r, 1));
        }
    }
    while let Some((r, depth)) = stack.pop() {
        println!(
            "  {}{}:{} {}  {}  [{}/{}]",
            "  ".repeat(depth - 1),
            r.member,
            qualified_of(r.node.id.as_str()),
            r.node.kind.as_str(),
            r.node.file,
            r.relation.as_str(),
            r.evidence.as_str(),
        );
        if let Some(kids) = children.get(&(r.member.as_str(), r.node.id.as_str())) {
            for kid in kids.iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }
    Ok(())
}
