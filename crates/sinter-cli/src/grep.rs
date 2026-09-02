//! `sinter grep`: text search bounded by a graph traversal, or by the
//! indexed corpus when no traversal is given.
//!
//! The composite an agent otherwise hand-builds: `sinter affected X` to get
//! a file set, then a text search over exactly those files. Structure comes
//! from the graph store, never from the text scan; the text scan never
//! leaves the bound. Without `--within` the bound is every indexed file in
//! scope, so the verb replaces `rg` for the common repo-wide case too.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Result, bail};
use regex::Regex;
use sinter_core::CorpusScope;
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

/// Indexed files whose file-level scope the filter admits.
fn indexed_files(store: &Store, filter: &EdgeFilter) -> Result<Vec<String>> {
    let scopes = store.scope_index()?;
    Ok(store
        .file_hashes()?
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| {
            filter
                .scopes
                .as_ref()
                .is_none_or(|admitted| admitted.contains(&scopes.file_scope(path)))
        })
        .collect())
}

/// Graph traversal to file set. Same store calls `affected`/`deps` make, so
/// the bound is exactly what those verbs would have printed. `file(DIR)`
/// is every indexed file under the directory; a path that is neither a
/// file nor a directory bounds nothing and says so through `warnings`.
fn files_of(
    store: &Store,
    root: &Path,
    within: &Within,
    filter: &EdgeFilter,
    max_depth: usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<String>> {
    let reached = match within {
        Within::File(path) => {
            let full = root.join(path);
            if full.is_dir() {
                let prefix = format!("{}/", path.trim_end_matches('/'));
                return Ok(indexed_files(store, filter)?
                    .into_iter()
                    .filter(|file| file.starts_with(&prefix))
                    .collect());
            }
            if !full.is_file() {
                warnings.push(format!("file({path}): no such file or directory"));
                return Ok(Vec::new());
            }
            return Ok(vec![path.clone()]);
        }
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

pub(crate) struct Hit {
    pub(crate) file: String,
    pub(crate) line: usize,
    pub(crate) text: String,
}

impl Hit {
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::json!({"f": self.file, "l": self.line, "t": self.text})
    }
}

// ---------------------------------------------------------------------
// Literal pass: the task's quoted strings and flags, found verbatim
// ---------------------------------------------------------------------

/// Files skipped by the literal pass above this size.
const LITERAL_MAX_FILE_BYTES: u64 = 1 << 20;
/// Longest literal worth scanning for; anything longer is prose.
const LITERAL_MAX_CHARS: usize = 80;

/// Quoted spans (`"query"`, `'x'`, `` `x` ``) and `--flag` tokens in a task
/// or question: things that occur verbatim in a corpus, which lexical
/// ranking over symbol names never sees. Deduplicated, in order.
pub(crate) fn literal_tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |token: &str| {
        let token = token.trim();
        if token.chars().count() >= 2
            && token.chars().count() <= LITERAL_MAX_CHARS
            && !out.iter().any(|kept| kept == token)
        {
            out.push(token.to_owned());
        }
    };
    for quote in ['"', '\'', '`'] {
        let mut parts = text.split(quote);
        // Text before the first quote, then alternating inner/outer spans.
        parts.next();
        while let Some(inner) = parts.next() {
            push(inner);
            if parts.next().is_none() {
                break;
            }
        }
    }
    for word in text.split_whitespace() {
        if word.starts_with("--") && word.len() > 2 {
            push(word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-'));
        }
    }
    out
}

/// Config and manifest files that mirror a registration made in code
/// (`package.json` exports, completion tables, guides): a hit there
/// answers "where else is X registered".
pub(crate) fn is_mirror_file(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    let ext = base.rsplit('.').next().unwrap_or("");
    matches!(
        base,
        "package.json"
            | "jsr.json"
            | "deno.json"
            | "deno.jsonc"
            | "Cargo.toml"
            | "pyproject.toml"
            | "setup.py"
            | "go.mod"
            | "Makefile"
    ) || matches!(ext, "zsh" | "fish" | "bash")
        || base.ends_with("GUIDE.md")
}

/// Every line under `root` containing one of `tokens` verbatim, bounded:
/// files over `LITERAL_MAX_FILE_BYTES` are skipped, derived state and
/// ignored paths are skipped, and at most `cap` hits come back. Walk
/// order is sorted so the rows are reproducible.
// ponytail: reads the whole corpus once per call; only runs when the task
// carries a literal. Index literals in the store if it shows in timings.
pub(crate) fn literal_scan(root: &Path, tokens: &[String], cap: usize) -> Vec<Hit> {
    let mut hits = Vec::new();
    if tokens.is_empty() || cap == 0 {
        return hits;
    }
    let alternation = tokens
        .iter()
        .map(|t| regex::escape(t))
        .collect::<Vec<_>>()
        .join("|");
    let Ok(pattern) = Regex::new(&alternation) else {
        return hits;
    };
    let mut walker = ignore::WalkBuilder::new(root);
    walker.add_custom_ignore_filename(".sinterignore");
    walker.sort_by_file_path(|a, b| a.cmp(b));
    for entry in walker.build().flatten() {
        if hits.len() >= cap {
            break;
        }
        let Some(size) = entry
            .metadata()
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len())
        else {
            continue;
        };
        if size > LITERAL_MAX_FILE_BYTES {
            continue;
        }
        let rel = sinter_core::rel_display(entry.path().strip_prefix(root).unwrap_or(entry.path()));
        if crate::corpus::excluded(&rel) {
            continue;
        }
        let Ok(handle) = File::open(entry.path()) else {
            continue;
        };
        scan(&rel, BufReader::new(handle), &pattern, cap, &mut hits);
    }
    hits
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

/// One line, not regex's multi-line diagnostic: the JSON failure envelope
/// splits a multi-line message into a candidate list.
fn compile(pattern: &str) -> Result<Regex> {
    Regex::new(pattern).map_err(|error| {
        let text = error.to_string();
        let detail = text
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("invalid pattern")
            .trim()
            .trim_start_matches("error: ");
        anyhow::anyhow!("invalid regex `{pattern}`: {detail}")
    })
}

/// The `sinter grep` payload: what CLI `--json` prints and what the MCP
/// `grep` tool returns, produced once so the two cannot drift.
///
/// The store handle belongs to the caller. `serve` opens one per call with
/// `open_current` after its own freshness pass; a reader opened here would
/// outlive nothing but would bypass that ownership, and a session-lived one
/// holds redb's lock against the next rebuild.
pub(crate) fn json(
    store: &Store,
    repo: &Path,
    pattern: &str,
    within: &[String],
    filter: &EdgeFilter,
    max_depth: usize,
    limit: usize,
) -> Result<serde_json::Value> {
    json_with(
        store, repo, pattern, within, filter, max_depth, limit, false,
    )
}

/// `json` plus `--no-tests`: test-scoped files leave the bound before the
/// scan, whichever traversal (or none) produced it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn json_with(
    store: &Store,
    repo: &Path,
    pattern: &str,
    within: &[String],
    filter: &EdgeFilter,
    max_depth: usize,
    limit: usize,
    no_tests: bool,
) -> Result<serde_json::Value> {
    let compiled = compile(pattern)?;
    let bounds: Vec<Within> = within
        .iter()
        .map(|w| parse_within(w))
        .collect::<Result<_>>()?;
    let snapshot = ensure_snapshot(store, None)?;
    let root = crate::pipeline::discover_root(repo);
    let mut warnings = Vec::new();
    let mut files = if bounds.is_empty() {
        indexed_files(store, filter)?
    } else {
        let mut per_within = Vec::with_capacity(bounds.len());
        for bound in &bounds {
            per_within.push(files_of(
                store,
                &root,
                bound,
                filter,
                max_depth,
                &mut warnings,
            )?);
        }
        union_files(per_within)
    };
    if no_tests {
        let scopes = store.scope_index()?;
        files.retain(|file| scopes.file_scope(file) != CorpusScope::Test);
    }

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
    if !warnings.is_empty() {
        out["warnings"] = serde_json::json!(warnings);
    }
    // An empty bound is a coverage answer, not a search answer: zero files
    // searched must carry the same trust envelope the bounding traversal
    // would have printed, or `0 matches` reads as proof.
    if files.is_empty() {
        out["coverage"] = crate::coverage::traversal_json(
            &root,
            store,
            filter,
            crate::coverage::TraversalEvidence::default(),
            false,
        )?;
    }
    Ok(out)
}

/// `sinter grep`: regex search over the files a graph traversal reached,
/// or every indexed file in scope. Ok(true) when anything matched
/// (grep-style exit codes).
#[allow(clippy::too_many_arguments)]
pub fn run(
    repo: &Path,
    pattern: &str,
    within: &[String],
    filter: &EdgeFilter,
    max_depth: usize,
    limit: usize,
    no_tests: bool,
    as_json: bool,
) -> Result<bool> {
    // Validated before the store is opened: `open_store` runs an incremental
    // build scan, and an unusable pattern or `--within` must report as itself
    // rather than pay for one. `json` re-checks for its own callers.
    compile(pattern)?;
    for spec in within {
        parse_within(spec)?;
    }
    let store = open_store(repo)?;
    let out = json_with(
        &store, repo, pattern, within, filter, max_depth, limit, no_tests,
    )?;
    let total = out["total"].as_u64().unwrap_or(0) as usize;
    for warning in out["warnings"].as_array().into_iter().flatten() {
        eprintln!("sinter: warning: {}", warning.as_str().unwrap_or_default());
    }

    if as_json {
        crate::agent_protocol::write_json(&out)?;
        return Ok(total > 0);
    }

    let hits = out["matches"].as_array().map_or(&[][..], Vec::as_slice);
    let bound = if within.is_empty() {
        "repo-wide"
    } else {
        "bound"
    };
    println!(
        "{total} matches · {bound} {} files ({} searched)",
        out["files_in_bound"], out["files_searched"]
    );
    for hit in hits {
        println!(
            "{}:{}: {}",
            hit["f"].as_str().unwrap_or_default(),
            hit["l"],
            hit["t"].as_str().unwrap_or_default()
        );
    }
    if total > hits.len() {
        println!(
            "{} more matches below cutoff · `sinter grep --limit {total}` to widen",
            total - hits.len()
        );
    }
    if let Some(coverage) = out.get("coverage") {
        crate::coverage::print_traversal_footer(coverage, out["snapshot"].as_str());
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
    fn literal_tokens_are_quoted_spans_and_flags() {
        assert_eq!(
            super::literal_tokens(
                r#"wire --resolution and `cmd == "explain"` through 'query' (see --json)."#
            ),
            [
                "explain",
                "query",
                "cmd == \"explain\"",
                "--resolution",
                "--json"
            ]
        );
        assert!(super::literal_tokens("plain prose without literals").is_empty());
        assert!(super::is_mirror_file("completions/_sinter.zsh"));
        assert!(super::is_mirror_file("packages/cli/package.json"));
        assert!(!super::is_mirror_file("src/main.rs"));
    }

    #[test]
    fn literal_scan_is_bounded_and_skips_derived_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let x = \"--flag\";\n--flag\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".sinter")).unwrap();
        std::fs::write(dir.path().join(".sinter/notes.txt"), "--flag\n").unwrap();
        let hits = super::literal_scan(dir.path(), &["--flag".to_owned()], 10);
        assert_eq!(
            hits.len(),
            2,
            "{:?}",
            hits.iter().map(|h| &h.file).collect::<Vec<_>>()
        );
        assert!(hits.iter().all(|h| h.file == "a.rs"));
        assert_eq!(
            super::literal_scan(dir.path(), &["--flag".to_owned()], 1).len(),
            1
        );
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
            false,
        )
        .unwrap_err();
        // regex's own compile diagnostic, on one line.
        let message = format!("{error:#}");
        assert_eq!(message, "invalid regex `a[`: unclosed character class");
    }
}
