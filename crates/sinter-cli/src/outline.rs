//! Structural landmarks inside one symbol's span: what `show --body`
//! cannot answer once the span is a black hole.
//!
//! A 173 KB `dispatch_command` whose every subcommand is an
//! `elif cmd == "query":` branch has no child node in the graph and no
//! readable body. The outline is the map of that span: nested definitions
//! the extractor did know about, plus the branch and literal landmarks a
//! bounded text pass can see.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::{Node, Relation, SymbolKind};
use sinter_store::Store;

/// A span at least this large is outlined instead of dumped.
pub const OUTLINE_BYTES: u64 = 8 * 1024;

/// …and so is one at least this many lines long.
pub const OUTLINE_LINES: usize = 200;

/// One landmark: `kind` is `def` (a nested symbol from the graph),
/// `branch` (a conditional or match arm on a literal) or `literal`
/// (a bare command/flag string).
pub struct Row {
    pub line: usize,
    pub kind: &'static str,
    pub text: String,
}

/// The landmarks of a span plus its measured size.
pub struct Outline {
    pub rows: Vec<Row>,
    pub lines: usize,
    pub bytes: u64,
}

impl Outline {
    /// Past either threshold the body is not worth printing.
    pub fn oversized(&self) -> bool {
        self.bytes >= OUTLINE_BYTES || self.lines >= OUTLINE_LINES
    }

    /// The first `limit` rows; the caller reports what it cut.
    pub fn json(&self, limit: usize) -> Value {
        json!(
            self.rows
                .iter()
                .take(limit)
                .map(|row| json!({"line": row.line, "kind": row.kind, "text": row.text}))
                .collect::<Vec<_>>()
        )
    }
}

fn is_definition(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function
            | SymbolKind::Method
            | SymbolKind::Class
            | SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::Trait
            | SymbolKind::Interface
            | SymbolKind::Module
            | SymbolKind::Macro
    )
}

/// Heads of a conditional in the languages sinter indexes.
const BRANCH_HEADS: &[&str] = &[
    "if ",
    "elif ",
    "elsif ",
    "else if ",
    "} else if ",
    "case ",
    "when ",
    "match ",
    "switch ",
];

/// A conditional line that discriminates on a string or path-qualified
/// enum literal — the shape a hand-rolled dispatcher is made of.
fn is_branch(line: &str) -> bool {
    let discriminates = line.contains('"') || line.contains('\'') || line.contains("::");
    if BRANCH_HEADS.iter().any(|head| line.starts_with(head)) {
        return discriminates;
    }
    // A match arm discriminates on its pattern side, never on the value
    // side: `Ok(true) => ExitCode::SUCCESS` decides nothing about the input.
    let arm = line.contains("=>");
    let pattern = line.split("=>").next().unwrap_or(line);
    // The pattern carries a literal, or is a path-qualified variant — whose
    // `=>` may be lines away once it binds fields (`Command::Show {`).
    let variant = pattern.starts_with(char::is_uppercase)
        && pattern.contains("::")
        && (arm || line.ends_with('{'));
    (arm && pattern.contains(['"', '\''])) || variant
}

/// A line that is nothing but a command- or flag-shaped string literal,
/// as a dispatch table or an argument list spells one per line.
fn bare_literal(line: &str) -> bool {
    let body = line.trim_end_matches([',', ']', ')', ';']).trim_end();
    let Some(inner) = body
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            body.strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
    else {
        return false;
    };
    inner.len() >= 2
        && !inner.contains(['"', '\'', ' '])
        && inner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The branch and literal landmarks of `span`, whose first line is `first`.
/// Lines in `defined` already have a `def` row.
fn scan(span: &str, first: usize, defined: &HashSet<usize>) -> Vec<Row> {
    span.lines()
        .enumerate()
        .filter_map(|(offset, raw)| {
            let line = first + offset;
            let trimmed = raw.trim();
            let kind = match () {
                () if defined.contains(&line) => return None,
                () if is_branch(trimmed) => "branch",
                () if bare_literal(trimmed) => "literal",
                () => return None,
            };
            Some(Row {
                line,
                kind,
                text: crate::render::ellipsize(trimmed, 90),
            })
        })
        .collect()
}

/// Nested definitions from the graph, then a single bounded text pass over
/// the span for the landmarks no extractor emits a node for.
///
/// ponytail: the text pass is a per-line keyword match, not a parse. It
/// over-reports a commented-out branch and under-reports a multi-line
/// condition; a real dispatch node in the extractor would replace it.
pub fn of(repo: &Path, store: &Store, node: &Node) -> Result<Option<Outline>> {
    let source = match std::fs::read_to_string(repo.join(&node.file)) {
        Ok(source) => source,
        Err(_) => return Ok(None),
    };
    let start = (node.span.start as usize).min(source.len());
    let end = (node.span.end as usize).min(source.len()).max(start);
    if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
        return Ok(None);
    }
    let line_at = |byte: usize| source[..byte].matches('\n').count() + 1;
    let first = line_at(start);

    let mut rows: Vec<Row> = Vec::new();
    for edge in store.out_edges(&node.id)? {
        if edge.relation != Relation::Contains {
            continue;
        }
        let Some(child) = store.node(&edge.dst)? else {
            continue;
        };
        if !is_definition(child.kind) || child.file != node.file {
            continue;
        }
        let byte = (child.span.start as usize).min(source.len());
        if byte < start || byte >= end || !source.is_char_boundary(byte) {
            continue;
        }
        rows.push(Row {
            line: line_at(byte),
            kind: "def",
            text: format!("{} {}", child.kind.as_str(), child.name),
        });
    }

    let defined: HashSet<usize> = rows.iter().map(|row| row.line).collect();
    rows.extend(scan(&source[start..end], first, &defined));
    rows.sort_by_key(|row| row.line);
    Ok(Some(Outline {
        rows,
        lines: source[start..end].lines().count(),
        bytes: (end - start) as u64,
    }))
}

#[cfg(test)]
mod tests {
    use super::{Outline, Row, bare_literal, is_branch, scan};
    use std::collections::HashSet;

    #[test]
    fn scan_numbers_lines_from_the_span_start_and_yields_to_def_rows() {
        let span =
            "def dispatch(cmd):\n    if cmd == \"query\":\n        run()\n    \"--limit\",\n";
        let rows = scan(span, 805, &HashSet::from([805]));
        let got: Vec<(usize, &str)> = rows.iter().map(|r| (r.line, r.kind)).collect();
        assert_eq!(got, vec![(806, "branch"), (808, "literal")]);
        assert_eq!(rows[0].text, "if cmd == \"query\":");
        // Without a def row on it, line 805 is scanned like any other.
        assert_eq!(scan(span, 805, &HashSet::new()).len(), 2);
    }

    #[test]
    fn either_threshold_makes_a_span_oversized() {
        let outline = |lines, bytes| Outline {
            rows: vec![Row {
                line: 1,
                kind: "branch",
                text: String::new(),
            }],
            lines,
            bytes,
        };
        assert!(!outline(10, 100).oversized());
        assert!(outline(10, super::OUTLINE_BYTES).oversized());
        assert!(outline(super::OUTLINE_LINES, 100).oversized());
    }

    #[test]
    fn branches_need_a_literal_to_discriminate_on() {
        assert!(is_branch(r#"elif cmd == "query":"#));
        assert!(is_branch(r#"if (cmd === 'path') {"#));
        assert!(is_branch("match Command::Query {"));
        assert!(is_branch(r#""show" => run_show(args),"#));
        assert!(is_branch(r#"Some("--limit") => limit = next(),"#));
        // A variant pattern that binds fields: its `=>` is lines below.
        assert!(is_branch("Command::Show {"));
        assert!(is_branch(r#"Self::Function => "function","#));
        assert!(!is_branch("Options {"));
        assert!(!is_branch("let f = crate::render::line_of(x);"));
        // A conditional on no literal is not a landmark.
        assert!(!is_branch("if depth > limit {"));
        assert!(!is_branch("let x = compute();"));
        // `=>` with the literal on the value side is a body, not a pattern.
        assert!(!is_branch(r#"other => println!("unknown"),"#));
    }

    #[test]
    fn bare_literals_are_command_or_flag_shaped() {
        assert!(bare_literal(r#""--limit","#));
        assert!(bare_literal("'no-callers',"));
        assert!(bare_literal(r#""verify_doc""#));
        assert!(!bare_literal(r#""a""#));
        assert!(!bare_literal(r#""two words","#));
        assert!(!bare_literal(r#"name = "query""#));
        assert!(!bare_literal("plain_identifier,"));
    }
}
