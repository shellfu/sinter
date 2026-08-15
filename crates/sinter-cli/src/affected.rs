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
    for r in &reached {
        println!(
            "  {}{} {}  {}  [{}/{:?}]",
            "  ".repeat(r.depth - 1),
            qualified_of(r.node.id.as_str()),
            r.node.kind.as_str(),
            r.node.file,
            r.via.relation.as_str(),
            r.via.confidence,
        );
    }
    Ok(())
}
