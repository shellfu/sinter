//! Shared human-output helpers: span→line, signature ellipsis, terminal
//! hyperlinks. Every human-facing verb renders through these so all verbs
//! look alike.

use std::io::IsTerminal;
use std::path::Path;

use serde_json::{Value, json};
use sinter_core::{Edge, Node};
use sinter_resolve::qualified_of;

/// One node in the shape the MCP tools use — `--json` mirrors it.
///
/// `id` is the durable agent handle. The byte-offset identity remains
/// available as `snapshot_id` for diagnostics and explicit relocation flows.
pub fn node_json(node: &Node) -> Value {
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

/// (file, line) of an edge's call site: the file derives from the src node
/// id, the line from the site span. None when the edge carries no site
/// (containment, dynamic fan-out, implements/extends, declared links).
pub fn site_of(repo: &Path, edge: &Edge) -> Option<(String, Option<usize>)> {
    let span = edge.site?;
    let file = edge
        .src
        .as_str()
        .split_once('#')
        .map_or(edge.src.as_str(), |(f, _)| f);
    Some((file.to_string(), line_of(repo, file, span.start)))
}

/// `file:line` call-site text for JSON (`null` when the edge has none).
pub fn site_json(repo: &Path, edge: &Edge) -> Value {
    match site_of(repo, edge) {
        Some((file, Some(line))) => json!(format!("{file}:{line}")),
        Some((file, None)) => json!(file),
        None => Value::Null,
    }
}

/// Hyperlinked call site(s) for human output; None when the edge has none.
/// One site reads exactly as before (`file:line`); several read
/// `file:12, :48, :91 (+4 more)` — the extra lines stay short because they
/// are all in the src node's own file.
pub fn site_location(repo: &Path, edge: &Edge) -> Option<String> {
    let (file, _) = site_of(repo, edge)?;
    let lines: Vec<usize> = edge
        .sites()
        .filter_map(|span| line_of(repo, &file, span.start))
        .collect();
    Some(lines_text(
        repo,
        &file,
        &lines,
        edge.sites_omitted() as usize,
    ))
}

/// `file:12, :48, :91 (+4 more)`: the first line hyperlinked as usual, the
/// rest bare. Lines are deduplicated (two sites on one line read once) and
/// assumed ascending. One line and nothing omitted renders exactly like a
/// plain `file:line` location.
pub fn lines_text(repo: &Path, file: &str, lines: &[usize], omitted: usize) -> String {
    let mut kept: Vec<usize> = lines.to_vec();
    kept.dedup();
    let mut text = location(repo, file, kept.first().copied());
    for line in kept.iter().skip(1) {
        text.push_str(&format!(", :{line}"));
    }
    if omitted > 0 {
        text.push_str(&format!(" (+{omitted} more)"));
    }
    text
}

/// Every kept site as `file:line`, with the total distinct count. Empty
/// (and 0) when the edge carries no site.
pub fn sites_of(repo: &Path, edge: &Edge) -> (Vec<String>, u32) {
    let Some((file, _)) = site_of(repo, edge) else {
        return (Vec::new(), 0);
    };
    let mut sites: Vec<String> = edge
        .sites()
        .map(|span| match line_of(repo, &file, span.start) {
            Some(line) => format!("{file}:{line}"),
            None => file.clone(),
        })
        .collect();
    // Two sites on one line are one answer to "where"; the count stays whole.
    sites.dedup();
    (sites, edge.sites_total)
}

/// Adds `sites`/`sites_total` to a row that already carries `site` — only
/// when the edge has more than one, so single-site payloads are unchanged.
pub fn add_sites(row: &mut Value, repo: &Path, edge: &Edge) {
    if edge.sites_total <= 1 {
        return;
    }
    let (sites, total) = sites_of(repo, edge);
    row["sites"] = json!(sites);
    row["sites_total"] = json!(total);
}

/// 1-based line of a byte offset, from the file's current content.
pub fn line_of(repo: &Path, file: &str, byte: u64) -> Option<usize> {
    let source = std::fs::read_to_string(repo.join(file)).ok()?;
    let upto = source.get(..(byte as usize).min(source.len()))?;
    Some(upto.bytes().filter(|b| *b == b'\n').count() + 1)
}

/// Middle-ellipsize past `max` chars, snapping both cuts to token
/// boundaries so an identifier is never split in half (`, bac… stdout`).
/// Snapping searches at most a quarter of the window; with no separator
/// in reach it falls back to the raw cut.
pub fn ellipsize(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max || max < 5 {
        return s.to_string();
    }
    let keep = max - 1;
    let slack = (keep / 4).max(1);
    let is_sep = |c: char| matches!(c, ',' | ' ' | '(' | ')' | '{');
    let mut head_end = keep / 2;
    if let Some(i) = (head_end.saturating_sub(slack)..head_end)
        .rev()
        .find(|&i| is_sep(chars[i]))
    {
        head_end = i + 1;
    }
    let mut tail_start = chars.len() - (keep - keep / 2);
    if let Some(i) = (tail_start..(tail_start + slack).min(chars.len())).find(|&i| is_sep(chars[i]))
    {
        tail_start = (i + 1).min(chars.len());
        while tail_start < chars.len() && chars[tail_start] == ' ' {
            tail_start += 1;
        }
    }
    let head: String = chars[..head_end].iter().collect();
    let tail: String = chars[tail_start..].iter().collect();
    format!("{}…{tail}", head.trim_end())
}

/// `file:line`, as an OSC 8 hyperlink when stdout is a terminal.
pub fn location(repo: &Path, file: &str, line: Option<usize>) -> String {
    let text = match line {
        Some(line) => format!("{file}:{line}"),
        None => file.to_string(),
    };
    if std::io::stdout().is_terminal() {
        let target = repo.join(file);
        format!(
            "\u{1b}]8;;file://{}\u{1b}\\{text}\u{1b}]8;;\u{1b}\\",
            target.display()
        )
    } else {
        text
    }
}

/// Pluralized count: `1 file`, `3 files`.
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}
