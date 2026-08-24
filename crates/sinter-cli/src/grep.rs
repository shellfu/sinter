//! `sinter grep`: text search bounded by a graph traversal.
//!
//! The composite an agent otherwise hand-builds: `sinter affected X` to get
//! a file set, then a text search over exactly those files. Structure comes
//! from the graph store, never from the text scan; the text scan never
//! leaves the bound.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Result, bail};
use regex::Regex;
use sinter_store::{EdgeFilter, Store};

use crate::lookup::{ensure_snapshot, open_store, unique_symbol_in};

const SUPPORTED: &str = "supported: affected(SYMBOL), deps(SYMBOL), file(PATH)";

/// One `--within` traversal, before it is resolved against the graph.
#[derive(Debug, PartialEq)]
enum Within {
    Affected(String),
    Deps(String),
    File(String),
}

/// `verb(ARG)` only. Anything else is a usage error: silently ignoring a
/// bound would silently widen the search.
fn parse_within(spec: &str) -> Result<Within> {
    let trimmed = spec.trim();
    let Some((verb, rest)) = trimmed.split_once('(') else {
        bail!("`--within {spec}`: expected `verb(ARG)` — {SUPPORTED}");
    };
    let Some(arg) = rest.strip_suffix(')') else {
        bail!("`--within {spec}`: missing closing `)` — {SUPPORTED}");
    };
    let arg = arg.trim();
    if arg.is_empty() {
        bail!("`--within {spec}`: empty argument — {SUPPORTED}");
    }
    match verb.trim() {
        "affected" => Ok(Within::Affected(arg.to_string())),
        "deps" => Ok(Within::Deps(arg.to_string())),
        "file" => Ok(Within::File(arg.to_string())),
        other => bail!("`--within {spec}`: unknown traversal `{other}` — {SUPPORTED}"),
    }
}

/// Files each traversal reached, unioned and deduplicated. Sorted so a
/// bound is reproducible across runs and across `--within` ordering.
fn union_files(per_within: Vec<Vec<String>>) -> Vec<String> {
    per_within
        .into_iter()
        .flatten()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// Graph traversal to file set. Same store calls `affected`/`deps` make, so
/// the bound is exactly what those verbs would have printed.
fn files_of(
    store: &Store,
    within: &Within,
    filter: &EdgeFilter,
    max_depth: usize,
) -> Result<Vec<String>> {
    let reached = match within {
        Within::File(path) => return Ok(vec![path.clone()]),
        Within::Affected(symbol) => {
            let node = unique_symbol_in(store, symbol, filter.scopes.as_ref())?;
            store.dependents(&node.id, filter, max_depth)?
        }
        Within::Deps(symbol) => {
            let node = unique_symbol_in(store, symbol, filter.scopes.as_ref())?;
            store.dependencies(&node.id, filter, max_depth)?
        }
    };
    Ok(reached.into_iter().map(|r| r.node.file).collect())
}

// ---------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------

struct Hit {
    file: String,
    line: usize,
    text: String,
}

/// Matches in one file, streamed line by line. `keep` bounds what is kept
/// for printing; the returned total counts every match so the summary is
/// honest above the cutoff. `None` when the file is not UTF-8 text.
fn scan(
    file: &str,
    reader: impl BufRead,
    pattern: &Regex,
    keep: usize,
    out: &mut Vec<Hit>,
) -> Option<usize> {
    let mut total = 0;
    for (index, line) in reader.lines().enumerate() {
        let line = line.ok()?;
        if !pattern.is_match(&line) {
            continue;
        }
        total += 1;
        if out.len() < keep {
            out.push(Hit {
                file: file.to_string(),
                line: index + 1,
                text: line.trim_end().to_string(),
            });
        }
    }
    Some(total)
}

/// `sinter grep`: regex search over the files a graph traversal reached.
/// Ok(true) when anything matched (grep-style exit codes).
pub fn run(
    repo: &Path,
    pattern: &str,
    within: &[String],
    filter: &EdgeFilter,
    max_depth: usize,
    limit: usize,
    json: bool,
) -> Result<bool> {
    // One line, not regex's multi-line diagnostic: the JSON failure envelope
    // splits a multi-line message into a candidate list.
    let compiled = Regex::new(pattern).map_err(|error| {
        let text = error.to_string();
        let detail = text
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("invalid pattern")
            .trim()
            .trim_start_matches("error: ");
        anyhow::anyhow!("invalid regex `{pattern}`: {detail}")
    })?;
    let bounds: Vec<Within> = within
        .iter()
        .map(|w| parse_within(w))
        .collect::<Result<_>>()?;
    let store = open_store(repo)?;
    let snapshot = ensure_snapshot(&store, None)?;
    let root = crate::pipeline::discover_root(repo);
    let mut per_within = Vec::with_capacity(bounds.len());
    for bound in &bounds {
        per_within.push(files_of(&store, bound, filter, max_depth)?);
    }
    let files = union_files(per_within);

    let mut hits: Vec<Hit> = Vec::new();
    let mut searched = 0usize;
    let mut total = 0usize;
    for file in &files {
        let Ok(handle) = File::open(root.join(file)) else {
            continue;
        };
        // Binary or non-UTF-8 content is skipped, never fatal: one blob in
        // the bound must not cost the whole answer.
        if let Some(count) = scan(file, BufReader::new(handle), &compiled, limit, &mut hits) {
            searched += 1;
            total += count;
        }
    }

    if json {
        let mut out = serde_json::json!({
            "status": if total > 0 { "found" } else { "not_proven" },
            "snapshot": snapshot,
            "pattern": pattern,
            "within": within,
            "files_in_bound": files.len(),
            "files_searched": searched,
            "total": total,
            "matches": hits.iter().map(|h| serde_json::json!({
                "f": h.file,
                "l": h.line,
                "t": h.text,
            })).collect::<Vec<_>>(),
        });
        if total > hits.len() {
            out["truncated"] = serde_json::json!(total - hits.len());
        }
        crate::agent_protocol::write_json(&out)?;
        return Ok(total > 0);
    }

    println!(
        "{total} matches · bound {} files ({searched} searched)",
        files.len()
    );
    for hit in &hits {
        println!("{}:{}: {}", hit.file, hit.line, hit.text);
    }
    if total > hits.len() {
        println!(
            "{} more matches below cutoff · `sinter grep --limit {total}` to widen",
            total - hits.len()
        );
    }
    Ok(total > 0)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

    use regex::Regex;

    use sinter_store::EdgeFilter;

    use super::{Within, parse_within, scan, union_files};

    #[test]
    fn within_forms_parse() {
        assert_eq!(
            parse_within("affected(Decision)").unwrap(),
            Within::Affected("Decision".into())
        );
        assert_eq!(
            parse_within(" deps( Foo::bar ) ").unwrap(),
            Within::Deps("Foo::bar".into())
        );
        assert_eq!(
            parse_within("file(src/main.rs)").unwrap(),
            Within::File("src/main.rs".into())
        );
    }

    #[test]
    fn unknown_within_forms_are_rejected_with_the_supported_list() {
        for spec in ["callers(X)", "affected", "affected(X", "affected()"] {
            let error = parse_within(spec).unwrap_err().to_string();
            assert!(
                error.contains("affected(SYMBOL)") || error.contains("closing"),
                "{spec}: {error}"
            );
        }
    }

    #[test]
    fn file_sets_union_and_deduplicate() {
        let files = union_files(vec![
            vec!["b.rs".into(), "a.rs".into(), "b.rs".into()],
            vec!["a.rs".into(), "c.rs".into()],
        ]);
        assert_eq!(files, vec!["a.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn matches_are_bounded_but_counted_in_full() {
        let pattern = Regex::new("fn ").unwrap();
        let text = "fn a() {}\nlet x = 1;\nfn b() {}\nfn c() {}\n";
        let mut hits = Vec::new();
        let total = scan("x.rs", Cursor::new(text), &pattern, 2, &mut hits).unwrap();
        assert_eq!(total, 3);
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].line, hits[1].line), (1, 3));
        assert_eq!(hits[0].text, "fn a() {}");
    }

    #[test]
    fn non_utf8_input_is_skipped_not_fatal() {
        let pattern = Regex::new("a").unwrap();
        let mut hits = Vec::new();
        assert!(
            scan(
                "bin",
                Cursor::new([0x61, 0xff, 0x0a]),
                &pattern,
                10,
                &mut hits
            )
            .is_none()
        );
    }

    #[test]
    fn patterns_are_full_regex() {
        let cases = [
            ("fn run", "pub fn run(", true),
            ("fn run", "fn walk(", false),
            ("^pub", "pub fn x", true),
            ("^pub", " pub fn x", false),
            ("x$", "let x", true),
            ("x$", "let x = 1", false),
            (r"\d+", "v12", true),
            (r"\d+", "vee", false),
            ("[A-Z][a-z]*", "Word", true),
            ("fn.*->.*Result", "fn run(x: u8) -> Result<bool>", true),
            // Constructs the hand-rolled matcher used to reject.
            ("dependents|dependencies", "pub fn dependencies(", true),
            ("dependents|dependencies", "pub fn nodes(", false),
            ("(ab)+c", "ababc", true),
            (r"^\s{4}pub", "    pub fn x", true),
        ];
        for (pattern, line, want) in cases {
            assert_eq!(
                Regex::new(pattern).unwrap().is_match(line),
                want,
                "{pattern} vs {line}"
            );
        }
    }

    #[test]
    fn an_invalid_regex_is_a_clean_error() {
        let error = super::run(
            Path::new("."),
            "a[",
            &["file(README.md)".to_string()],
            &EdgeFilter::default(),
            1,
            1,
            false,
        )
        .unwrap_err();
        // regex's own compile diagnostic, on one line.
        let message = format!("{error:#}");
        assert_eq!(message, "invalid regex `a[`: unclosed character class");
    }
}
