use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use sinter_core::{CorpusScope, Node, SymbolKind};
use sinter_store::{ScopeIndex, Store};

use crate::corpus::ScopeSelection;
use crate::lookup::{Found, ensure_snapshot, find_symbol, open_store};
use crate::render::{line_of, location, node_json};

/// Wide trigram net before local ranking: the store cuts at `limit`
/// *before* its majority filter, so a tight limit drops good names.
const SUGGESTION_POOL: usize = 64;

/// A markdown section carries its whole body as `doc`; a finder shows
/// this much of it and leaves the rest to `show`.
const SECTION_DOC_CHARS: usize = 200;

fn trigrams(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

/// Close names, best first: prefix match, then shared trigrams, then the
/// shorter name (a closer edit), then id for determinism.
fn rank_suggestions(store: &Store, symbol: &str) -> Result<Vec<Node>> {
    let query = symbol.to_lowercase();
    let grams = trigrams(&query);
    let mut nodes = store.search(symbol, SUGGESTION_POOL)?;
    let key = |n: &Node| {
        let name = n.name.to_lowercase();
        let shared = grams.iter().filter(|g| name.contains(g.as_str())).count();
        (!name.starts_with(&query), usize::MAX - shared, name.len())
    };
    nodes.sort_by(|a, b| key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id)));
    Ok(nodes)
}

/// Shipped code first, whatever produced the list: an exact `Env` is the
/// one in `src/`, not nine test-local copies; a fuzzy list is not all
/// test names. Stable, so the finer ranking within a tier survives.
fn production_first(nodes: &mut [Node], scopes: &ScopeIndex) {
    let tier = |n: &Node| match scopes.scope_of(n) {
        CorpusScope::Production => 0,
        CorpusScope::Docs => 1,
        CorpusScope::Generated | CorpusScope::Vendor => 3,
        _ => 2,
    };
    nodes.sort_by_key(tier);
}

/// `head*tail` when the symbol holds exactly one `*`; `None` for exact
/// lookup. Only `*` is supported.
fn glob_parts(symbol: &str) -> Result<Option<(&str, &str)>> {
    match symbol.split_once('*') {
        None => Ok(None),
        Some((_, tail)) if tail.contains('*') => {
            anyhow::bail!("glob `{symbol}` may contain only one `*`")
        }
        Some(parts) => Ok(Some(parts)),
    }
}

/// The node's doc, a section body cut to [`SECTION_DOC_CHARS`].
fn capped_doc(node: &Node) -> Option<String> {
    let doc = node.doc.as_deref()?;
    if node.kind != SymbolKind::Section || doc.chars().count() <= SECTION_DOC_CHARS {
        return Some(doc.to_string());
    }
    Some(format!(
        "{}…",
        doc.chars().take(SECTION_DOC_CHARS).collect::<String>()
    ))
}

/// `file:LINE (start..end)` header plus signature and doc, matching `show`.
fn print_node(repo: &Path, node: &Node) {
    let line = line_of(repo, &node.file, node.span.start);
    println!(
        "{} {}  {} ({}..{})",
        node.kind.as_str(),
        qualified_of(node.id.as_str()),
        location(repo, &node.file, line),
        node.span.start,
        node.span.end
    );
    if !node.signature.is_empty() {
        println!("    {}", node.signature);
    }
    if let Some(doc) = capped_doc(node) {
        for l in doc.lines().take(3) {
            println!("    /// {l}");
        }
    }
}

/// `sinter query`: exact + trigram symbol search, content-bearing results.
/// Ok(true) when the symbol matched (grep-style exit codes): a fuzzy
/// suggestion list is a miss with hints, not a hit.
pub fn run(
    repo: &Path,
    symbol: &str,
    limit: usize,
    json: bool,
    if_snapshot: Option<&str>,
    scopes: &ScopeSelection,
) -> Result<bool> {
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let (resolution, exact, mut nodes) = match glob_parts(symbol)? {
        Some((head, tail)) => {
            let mut nodes = store.nodes_glob(head, tail)?;
            nodes.sort_by(|a, b| (&a.file, a.span.start).cmp(&(&b.file, b.span.start)));
            ("glob", true, nodes)
        }
        None => match find_symbol(&store, symbol)? {
            Found::Exact(nodes) => ("exact", true, nodes),
            Found::Relocated(nodes) => ("relocated", false, nodes),
            Found::Suggestions(_) => ("suggestions", false, rank_suggestions(&store, symbol)?),
        },
    };
    let scope_index = store.scope_index()?;
    scopes.narrow(&mut nodes, &scope_index);
    production_first(&mut nodes, &scope_index);
    let found = exact && !nodes.is_empty();
    if json {
        // Same shape as the MCP `query` tool.
        let mut out = serde_json::json!({
            "exact": exact,
            "resolution": resolution,
            "snapshot": snapshot,
            "scope": scopes.json(),
            "results": nodes.iter().take(limit).map(|node| {
                let mut value = node_json(node);
                value["doc"] = serde_json::json!(capped_doc(node));
                value["scope"] = serde_json::json!(scope_index.scope_of(node).as_str());
                value
            }).collect::<Vec<_>>(),
        });
        if nodes.len() > limit {
            out["truncated"] = serde_json::json!(nodes.len() - limit);
        }
        crate::agent_protocol::write_json(&out)?;
        return Ok(found);
    }
    if nodes.is_empty() {
        println!("no matches for `{symbol}`");
    } else if exact {
        for node in nodes.iter().take(limit) {
            print_node(repo, node);
        }
    } else if resolution == "relocated" {
        println!("snapshot-local id `{symbol}` moved; current candidate(s):");
        for node in nodes.iter().take(limit) {
            print_node(repo, node);
        }
    } else {
        println!("no exact match for `{symbol}`; close names:");
        for node in nodes.iter().take(limit) {
            print_node(repo, node);
        }
    }
    if nodes.len() > limit {
        println!(
            "{} more matches below cutoff · `sinter query --limit {}` to widen",
            nodes.len() - limit,
            nodes.len(),
        );
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::{SECTION_DOC_CHARS, capped_doc, glob_parts};
    use sinter_core::{Node, NodeId, Span, SymbolKind};

    #[test]
    fn glob_parts_splits_single_star() {
        assert_eq!(glob_parts("Hooks::*").unwrap(), Some(("Hooks::", "")));
        assert_eq!(glob_parts("*::run").unwrap(), Some(("", "::run")));
        assert_eq!(glob_parts("resolve_*").unwrap(), Some(("resolve_", "")));
        assert_eq!(glob_parts("run").unwrap(), None);
        assert!(glob_parts("a*b*").is_err());
    }

    fn node(kind: SymbolKind, doc: &str) -> Node {
        Node {
            id: NodeId::new("README.md#Intro@0"),
            kind,
            name: "Intro".into(),
            file: "README.md".into(),
            span: Span { start: 0, end: 1 },
            signature: String::new(),
            doc: Some(doc.into()),
        }
    }

    #[test]
    fn section_bodies_are_capped_and_code_docs_are_not() {
        let long = "x".repeat(SECTION_DOC_CHARS + 50);
        let section = capped_doc(&node(SymbolKind::Section, &long)).unwrap();
        assert_eq!(section.chars().count(), SECTION_DOC_CHARS + 1);
        assert!(section.ends_with('…'));
        assert_eq!(
            capped_doc(&node(SymbolKind::Function, &long)).as_deref(),
            Some(long.as_str())
        );
    }
}
