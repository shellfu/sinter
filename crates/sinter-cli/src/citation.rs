//! Durable symbol citations and Markdown citation verification.
//!
//! A managed citation carries a stable `symbol_key` in an HTML comment and
//! renders its current `file#Lline` location for humans. Verification resolves
//! the key again, so line movement is detectable. Bare `path:line` references
//! are checked for existence/range but remain `location_only`: their semantic
//! target cannot be proven after edits.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use sinter_resolve::qualified_of;
use sinter_store::Store;

use crate::lookup::{Found, ensure_snapshot, find_symbol, open_store, unique_symbol};

#[derive(Deserialize)]
struct CitationIdentity {
    symbol_key: String,
}

fn managed_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\[[^\]\n]*\]\((?P<target>[^)\n]+)\)\s*<!--\s*sinter-cite:v1\s+(?P<meta>\{[^>\n]*\})\s*-->"#,
        )
        .expect("managed citation regex")
    })
}

fn location_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?P<path>(?:[A-Za-z0-9_.-]+/)*[A-Za-z0-9_.-]+\.[A-Za-z0-9]+)(?::(?P<colon>[0-9]+)|#L(?P<anchor>[0-9]+))",
        )
        .expect("source location regex")
    })
}

fn document_line(text: &str, offset: usize) -> usize {
    text[..offset].bytes().filter(|byte| *byte == b'\n').count() + 1
}

fn target(file: &str, line: usize) -> String {
    format!("{file}#L{line}")
}

fn canonical_document(repo: &Path, document: &Path) -> Result<PathBuf> {
    let root = crate::pipeline::discover_root(repo).canonicalize()?;
    let joined = if document.is_absolute() {
        document.to_path_buf()
    } else {
        root.join(document)
    };
    let canonical = joined
        .canonicalize()
        .with_context(|| format!("document {}", joined.display()))?;
    if canonical.strip_prefix(&root).is_err() {
        bail!(
            "document {} is outside searched repository {}",
            canonical.display(),
            root.display()
        );
    }
    Ok(canonical)
}

fn cite_response(repo: &Path, store: &Store, symbol: &str) -> Result<Value> {
    let root = crate::pipeline::discover_root(repo).canonicalize()?;
    let node = unique_symbol(store, symbol)?;
    let line = crate::render::line_of(&root, &node.file, node.span.start)
        .with_context(|| format!("read cited source {}", node.file))?;
    let end_line = crate::render::line_of(&root, &node.file, node.span.end).unwrap_or(line);
    let identity = json!({"symbol_key": node.symbol_key().as_str()});
    let metadata = serde_json::to_string(&identity)?;
    let location = target(&node.file, line);
    let markdown = format!(
        "[{}]({location}) <!-- sinter-cite:v1 {metadata} -->",
        qualified_of(node.id.as_str())
    );
    Ok(json!({
        "status": "current",
        "symbol": crate::graph_tool::node_json(&node),
        "citation": {
            "markdown": markdown,
            "target": location,
            "target_base": "repository_root",
            "identity": identity,
            "line": line,
            "end_line": end_line,
        },
        "universe": {"mode": "repository", "root": root},
    }))
}

fn resolve_managed(
    root: &Path,
    store: &Store,
    identity: &CitationIdentity,
    cited_target: &str,
    doc_line: usize,
) -> Value {
    match find_symbol(store, &identity.symbol_key) {
        Ok(Found::Exact(nodes)) if nodes.len() == 1 => {
            let node = &nodes[0];
            let Some(line) = crate::render::line_of(root, &node.file, node.span.start) else {
                return json!({
                    "document_line": doc_line,
                    "source": "managed",
                    "status": "missing_source",
                    "symbol_key": identity.symbol_key,
                    "cited_target": cited_target,
                    "file": node.file,
                });
            };
            let current = target(&node.file, line);
            json!({
                "document_line": doc_line,
                "source": "managed",
                "status": if cited_target == current { "current" } else { "moved" },
                "symbol_key": identity.symbol_key,
                "cited_target": cited_target,
                "current_target": current,
                "qualified": qualified_of(node.id.as_str()),
            })
        }
        Ok(Found::Exact(nodes)) => json!({
            "document_line": doc_line,
            "source": "managed",
            "status": "ambiguous",
            "symbol_key": identity.symbol_key,
            "cited_target": cited_target,
            "candidates": nodes.iter().map(crate::graph_tool::node_json).collect::<Vec<_>>(),
        }),
        Ok(Found::Relocated(nodes)) => json!({
            "document_line": doc_line,
            "source": "managed",
            "status": "identity_relocated",
            "symbol_key": identity.symbol_key,
            "cited_target": cited_target,
            "candidates": nodes.iter().map(crate::graph_tool::node_json).collect::<Vec<_>>(),
        }),
        Ok(Found::Suggestions(_)) => json!({
            "document_line": doc_line,
            "source": "managed",
            "status": "missing",
            "symbol_key": identity.symbol_key,
            "cited_target": cited_target,
        }),
        Err(error) => json!({
            "document_line": doc_line,
            "source": "managed",
            "status": "invalid_identity",
            "symbol_key": identity.symbol_key,
            "cited_target": cited_target,
            "error": format!("{error:#}"),
        }),
    }
}

fn location_only(root: &Path, path: &str, line: usize, doc_line: usize) -> Value {
    let joined = root.join(path);
    let canonical = joined.canonicalize();
    if canonical
        .as_ref()
        .is_ok_and(|source| source.strip_prefix(root).is_err())
    {
        return json!({
            "document_line": doc_line,
            "source": "unmanaged",
            "status": "outside_universe",
            "cited_target": target(path, line),
        });
    }
    let (status, lines) = match canonical.and_then(std::fs::read_to_string) {
        Ok(text) => {
            let lines = text.lines().count();
            (
                if (1..=lines).contains(&line) {
                    "location_only"
                } else {
                    "out_of_range"
                },
                Some(lines),
            )
        }
        Err(_) => ("missing_file", None),
    };
    json!({
        "document_line": doc_line,
        "source": "unmanaged",
        "status": status,
        "cited_target": target(path, line),
        "file_lines": lines,
        "note": "path/line exists, but no stable symbol identity proves what it cites",
    })
}

fn verify_response(repo: &Path, store: &Store, document: &Path) -> Result<Value> {
    let root = crate::pipeline::discover_root(repo).canonicalize()?;
    let document = canonical_document(&root, document)?;
    let text = std::fs::read_to_string(&document)
        .with_context(|| format!("read document {}", document.display()))?;
    let mut entries = Vec::new();
    let mut managed_ranges = Vec::new();

    for capture in managed_pattern().captures_iter(&text) {
        let whole = capture.get(0).expect("whole match");
        managed_ranges.push((whole.start(), whole.end()));
        let cited_target = capture.name("target").expect("target").as_str();
        let doc_line = document_line(&text, whole.start());
        let entry = match serde_json::from_str::<CitationIdentity>(
            capture.name("meta").expect("metadata").as_str(),
        ) {
            Ok(identity) => resolve_managed(&root, store, &identity, cited_target, doc_line),
            Err(error) => json!({
                "document_line": doc_line,
                "source": "managed",
                "status": "invalid_metadata",
                "cited_target": cited_target,
                "error": error.to_string(),
            }),
        };
        entries.push(entry);
    }

    for capture in location_pattern().captures_iter(&text) {
        let whole = capture.get(0).expect("whole match");
        if text[..whole.start()].ends_with("://") {
            continue;
        }
        if managed_ranges
            .iter()
            .any(|(start, end)| whole.start() >= *start && whole.start() < *end)
        {
            continue;
        }
        let line = capture
            .name("colon")
            .or_else(|| capture.name("anchor"))
            .and_then(|value| value.as_str().parse::<usize>().ok())
            .unwrap_or(0);
        entries.push(location_only(
            &root,
            capture.name("path").expect("path").as_str(),
            line,
            document_line(&text, whole.start()),
        ));
    }
    entries.sort_by_key(|entry| entry["document_line"].as_u64().unwrap_or(0));

    let count = |wanted: &str| {
        entries
            .iter()
            .filter(|entry| entry["status"] == wanted)
            .count()
    };
    let current = count("current");
    let unmanaged = count("location_only");
    let stale = entries.len().saturating_sub(current + unmanaged);
    let status = if entries.is_empty() {
        "no_citations"
    } else if stale > 0 {
        "stale"
    } else if unmanaged > 0 {
        "not_proven"
    } else {
        "current"
    };
    Ok(json!({
        "status": status,
        "document": document.strip_prefix(&root).unwrap_or(&document),
        "summary": {
            "total": entries.len(),
            "managed_current": current,
            "unmanaged_location_only": unmanaged,
            "stale_or_invalid": stale,
        },
        "citations": entries,
        "universe": {"mode": "repository", "root": root},
    }))
}

pub(crate) fn run_cite(
    repo: &Path,
    symbol: &str,
    json_output: bool,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let mut response = cite_response(repo, &store, symbol)?;
    response["snapshot"] = json!(snapshot);
    if json_output {
        crate::agent_protocol::write_json(&response)?;
    } else {
        println!(
            "{}",
            response["citation"]["markdown"].as_str().unwrap_or("")
        );
    }
    Ok(true)
}

pub(crate) fn run_verify(
    repo: &Path,
    document: &Path,
    json_output: bool,
    if_snapshot: Option<&str>,
) -> Result<bool> {
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, if_snapshot)?;
    let mut response = verify_response(repo, &store, document)?;
    response["snapshot"] = json!(snapshot);
    let current = response["status"] == "current";
    if json_output {
        crate::agent_protocol::write_json(&response)?;
    } else {
        println!(
            "verify-doc {}: {} ({} current, {} location-only, {} stale/invalid)",
            response["document"].as_str().unwrap_or("?"),
            response["status"].as_str().unwrap_or("stale"),
            response["summary"]["managed_current"],
            response["summary"]["unmanaged_location_only"],
            response["summary"]["stale_or_invalid"],
        );
        for citation in response["citations"].as_array().into_iter().flatten() {
            println!(
                "  line {}: {}  {}",
                citation["document_line"],
                citation["status"].as_str().unwrap_or("invalid"),
                citation["cited_target"].as_str().unwrap_or("?"),
            );
        }
    }
    Ok(current)
}
