//! `sinter context "<task>"`: the smallest evidence packet an agent needs
//! before editing. Pure composition over `ask`, `show`-style cards, depth-1
//! `deps`/`affected`, `impact`'s affected-test selection, `grep`'s literal
//! pass, and one coverage envelope. No new graph machinery, no scoring of
//! its own.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::{Confidence, CorpusScope, Node, NodeId, Relation, SymbolKind};
use sinter_resolve::qualified_of;
use sinter_store::{EdgeFilter, Reached, Store};

use crate::ask::confidence::HIGH_MARGIN_PERMILLE;
use crate::corpus::ScopeSelection;
use crate::lookup::{Found, ensure_snapshot, find_symbol, open_store};
use crate::render::{ellipsize, line_of};

/// Hits `ask` is asked for; the packet keeps every one as a candidate but
/// only expands the contenders (see `is_contender`).
const ASK_LIMIT: usize = 5;
const MAX_FOCUS: usize = 3;
/// Direct dependency/dependent rows kept per focus candidate.
const EDGE_ROWS: usize = 8;
/// Test rows kept; `impact` uses the same per-collection budget.
const TEST_ROWS: usize = crate::impact::DEFAULT_LIMIT;
const EXCERPT_LINES: usize = 12;
/// Test commands printed in the plain-text packet; `--json` carries the rest.
const PRINTED_TESTS: usize = 6;
/// Shortest bare word worth resolving as a symbol name.
const MIN_WORD_LEN: usize = 3;
/// Identifier-shaped tokens read out of one task string.
const MAX_IDENTIFIERS: usize = 12;
/// A name with more definitions than this grounds nothing in particular.
const MAX_ANCHOR_NODES: usize = 3;
/// Content terms (already stop-filtered by the `ask` parser) offered to
/// `sinter grep` when nothing ranked.
const GREP_TERMS: usize = 4;
/// Lexical hits below which an abstaining `ask` also abstains the packet;
/// at or above it the list is an answer whatever the calibration says.
const MIN_LEXICAL_HITS: usize = 3;
/// Trigram candidates consulted for one fuzzy anchor hop, and the
/// shortest word worth a hop (`CLI` lands on anything; `supervisor` does
/// not).
const FUZZY_CANDIDATES: usize = 10;
const MIN_FUZZY_LEN: usize = 5;
/// Literal-pass bounds: hits scanned, rows kept per list.
const LITERAL_CAP: usize = 200;
const LITERAL_ROWS: usize = 12;
/// Package manifests that bound a test command's working directory.
const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "go.mod",
    "package.json",
    "pyproject.toml",
    "setup.py",
];
/// Symbols a file anchor contributes as candidates, highest in-degree first.
const FILE_CANDIDATES: usize = 10;
/// A bare token ending in one of these names a file, never a symbol.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "go", "py", "ts", "tsx", "js", "java", "cs", "c", "cc", "cpp", "h", "hpp", "sql",
    "proto", "md", "sh",
];
/// Task-prose filler that would otherwise become lexical ranking terms;
/// the `ask` parser's own list covers the rest.
const TASK_STOPWORDS: &[&str] = &[
    "a", "an", "the", "in", "on", "for", "to", "of", "and", "or", "make", "add", "new", "use",
    "it", "this", "that", "with", "from", "by", "per", "into", "honor", "account",
];
/// Shortest hyphen fragment (`multi-thread` -> `multi`, `thread`) kept as a
/// term; shorter fragments (`per` of `per-tool`) are noise.
const MIN_FRAGMENT_LEN: usize = 4;

/// `serve.rs`, `src/placement.rs`: names a file by path suffix.
fn is_path_like(token: &str) -> bool {
    token.contains('/')
        || token
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && SOURCE_EXTENSIONS.contains(&ext))
}

fn is_task_stopword(word: &str) -> bool {
    TASK_STOPWORDS.contains(&word) || crate::ask::query::is_stopword(word)
}

/// The task with file names, filler words and short hyphen fragments
/// removed: what `ask` ranks on. Falls back to the task itself when
/// nothing survives so `ask` can still abstain honestly.
fn lexical_query(task: &str) -> String {
    let mut words: Vec<&str> = Vec::new();
    for raw in task.split_whitespace() {
        let token = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if token.is_empty() || is_path_like(token) {
            continue;
        }
        if token.contains('-') {
            words.extend(
                token
                    .split('-')
                    .filter(|part| part.chars().count() >= MIN_FRAGMENT_LEN)
                    .filter(|part| !is_task_stopword(&part.to_lowercase())),
            );
            continue;
        }
        if !is_task_stopword(&token.to_lowercase()) {
            words.push(token);
        }
    }
    if words.is_empty() {
        task.to_owned()
    } else {
        words.join(" ")
    }
}

/// Runner-up within the `ask` high-margin band of the top score is still a
/// plausible edit target and gets expanded too.
fn is_contender(score: i64, top: i64) -> bool {
    top > 0 && score * 1000 >= top * (1000 - HIGH_MARGIN_PERMILLE)
}

/// A task token that could be a symbol name, strongest shape first.
/// `Explicit` spells a location out (`Foo::bar`, `src/ask/query.rs`);
/// `Shaped` is written like code (`snake_case`, `CamelCase`); `Bare` is an
/// ordinary lowercase word that only *might* be a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    Explicit,
    Shaped,
    Bare,
}

fn shape_of(token: &str) -> Option<Shape> {
    if token.is_empty() || token.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    // Anything holding punctuation a symbol or path cannot contain
    // ("don't", "field,value") is prose, not a name.
    if !token
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '/' | '.' | '-'))
    {
        return None;
    }
    if token.contains("::") || is_path_like(token) {
        return Some(Shape::Explicit);
    }
    // A hyphenated or dotted word (`per-tool`, `e.g`, `notes.txt`) is prose;
    // its fragments are terms, not names, and `lexical_query` keeps the
    // long ones.
    if token.contains('-') || token.contains('.') {
        return None;
    }
    if token.contains('_') || token.contains(char::is_uppercase) {
        return Some(Shape::Shaped);
    }
    if token.chars().count() < MIN_WORD_LEN || is_task_stopword(token) {
        return None;
    }
    Some(Shape::Bare)
}

/// Identifier-shaped tokens in the task string, strongest shape first and
/// deduplicated. Deterministic and lexical-free: this decides what *could*
/// be a symbol name; the store decides what actually is one.
fn identifier_candidates(task: &str) -> Vec<(Shape, String)> {
    let mut out: Vec<(Shape, String)> = Vec::new();
    for raw in task.split_whitespace() {
        let token = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        let Some(shape) = shape_of(token) else {
            continue;
        };
        if !out.iter().any(|(_, kept)| kept == token) {
            out.push((shape, token.to_owned()));
        }
        if out.len() == MAX_IDENTIFIERS {
            break;
        }
    }
    out.sort_by_key(|(shape, _)| *shape);
    out
}

/// A task-string token that named a real node.
struct Anchor {
    term: String,
    node: Node,
    /// Symbols defined in an anchored file, highest in-degree first.
    contained: Vec<Node>,
    /// Reached by one trigram hop, not by name.
    fuzzy: bool,
}

/// What the task's identifiers grounded to, and what they did not.
#[derive(Default)]
struct Grounding {
    anchors: Vec<Anchor>,
    unresolved: Vec<(Shape, String)>,
    /// `(term, matching paths)` for a file name that fits several files.
    ambiguous_files: Vec<(String, Vec<String>)>,
}

/// `Name@file` for a symbol, the bare path for a file node (whose name is
/// already its path).
fn handle_of(node: &Node) -> String {
    if node.kind == SymbolKind::File {
        node.file.clone()
    } else {
        format!("{}@{}", node.name, node.file)
    }
}

/// Resolve a file-name token against indexed paths by suffix. `Ok(Err(paths))`
/// is the ambiguous case; `Ok(Ok(None))` names no indexed file.
fn file_anchor(store: &Store, term: &str) -> Result<Result<Option<Anchor>, Vec<String>>> {
    let wanted = term.trim_start_matches("./");
    let suffix = format!("/{wanted}");
    let mut paths: Vec<String> = store
        .file_hashes()?
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| path == wanted || path.ends_with(&suffix))
        .collect();
    paths.sort();
    let path = match paths.as_slice() {
        [] => return Ok(Ok(None)),
        [path] => path.clone(),
        _ => return Ok(Err(paths)),
    };
    let Some(facts) = store.facts(&path)? else {
        return Ok(Ok(None));
    };
    let (files, symbols): (Vec<Node>, Vec<Node>) = facts
        .nodes
        .into_iter()
        .partition(|n| n.kind == SymbolKind::File);
    let Some(node) = files.into_iter().next() else {
        return Ok(Ok(None));
    };
    let ids: Vec<NodeId> = symbols.iter().map(|n| n.id.clone()).collect();
    let in_edges = store.in_edges_many(&ids)?;
    let degree = |n: &Node| {
        in_edges.get(&n.id).map_or(0, |edges| {
            edges
                .iter()
                .filter(|e| e.relation != Relation::Contains)
                .count()
        })
    };
    let mut contained: Vec<(usize, Node)> = symbols.into_iter().map(|n| (degree(&n), n)).collect();
    contained.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    Ok(Ok(Some(Anchor {
        term: term.to_owned(),
        node,
        contained: contained
            .into_iter()
            .take(FILE_CANDIDATES)
            .map(|(_, n)| n)
            .collect(),
        fuzzy: false,
    })))
}

/// Task vocabulary that means "change how this thing behaves", as opposed
/// to a question about where something is defined. Only then does the
/// entry-point prior apply: it prefers where an edit lands, which is the
/// wrong bias for a lookup.
const FEATURE_WORDS: &[&str] = &[
    "add",
    "implement",
    "support",
    "wire",
    "expose",
    "register",
    "cli",
    "command",
    "subcommand",
    "flag",
    "option",
    "feature",
    "handler",
    "endpoint",
];

fn is_feature_task(task: &str) -> bool {
    task.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| FEATURE_WORDS.contains(&word))
}

/// Entry-point seeds consulted, and how far an edit target may sit from
/// one. One bounded BFS per `context` invocation.
const ENTRY_SEEDS: usize = 8;
const ENTRY_HOPS: usize = 3;
/// A tie among ranked candidates goes to the entry point. Small on
/// purpose: a candidate scoring more than this above an entry point still
/// wins outright.
const ENTRY_BIAS_PERMILLE: i64 = 100;

/// Ids that are an entry point by name, or reachable from one within
/// `ENTRY_HOPS`. One BFS from at most `ENTRY_SEEDS` seeds.
fn entry_point_ids(store: &Store) -> Result<BTreeSet<String>> {
    let filter = EdgeFilter::default();
    let mut seeds: Vec<Node> = Vec::new();
    for term in ["main", "run"] {
        for node in store.search(term, FUZZY_CANDIDATES)? {
            if !matches!(node.kind, SymbolKind::File | SymbolKind::Section)
                && is_entry_point(&node.name)
                && !is_test_file(&node.file)
            {
                seeds.push(node);
            }
        }
    }
    seeds.truncate(ENTRY_SEEDS);
    let mut reached = BTreeSet::new();
    for seed in &seeds {
        reached.insert(seed.id.as_str().to_owned());
        for step in store.dependencies(&seed.id, &filter, ENTRY_HOPS)? {
            reached.insert(step.node.id.as_str().to_owned());
        }
    }
    Ok(reached)
}

/// `main`, `run*`, `*Command`: where a task usually starts.
fn is_entry_point(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "main" || lower.starts_with("run") || lower.ends_with("command")
}

/// One trigram hop for an intent word nothing grounded (`supervisor` ->
/// `supervisorCommand`). Only a name that contains the word, in preferred
/// scope; a bare English word may only land on an entry point, since it
/// otherwise names the lexical bait this pre-pass exists to avoid.
fn fuzzy_anchor(
    store: &Store,
    scope_index: &sinter_store::ScopeIndex,
    preferred: &BTreeSet<CorpusScope>,
    shape: Shape,
    term: &str,
) -> Result<Option<Node>> {
    let lower = term.to_lowercase();
    let mut close: Vec<Node> = store
        .search(term, FUZZY_CANDIDATES)?
        .into_iter()
        .filter(|n| !matches!(n.kind, SymbolKind::File | SymbolKind::Section))
        .filter(|n| {
            let name = n.name.to_lowercase();
            name != lower && name.contains(&lower)
        })
        .filter(|n| shape != Shape::Bare || is_entry_point(&n.name))
        .filter(|n| preferred.contains(&scope_index.scope_of(n)))
        .collect();
    close.sort_by(|a, b| {
        is_entry_point(&b.name)
            .cmp(&is_entry_point(&a.name))
            .then_with(|| a.name.len().cmp(&b.name.len()))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(close.into_iter().next())
}

/// Resolve the task string's identifiers against real node names before any
/// lexical scoring runs. Exact and qualified-suffix matches first; only when
/// none grounds does one bounded fuzzy hop run (see `fuzzy_anchor`).
/// Whatever fails to ground is returned so the caller can report it instead
/// of dropping it.
fn anchors_of(store: &Store, task: &str) -> Result<Grounding> {
    let scope_index = store.scope_index()?;
    let preferred = ScopeSelection::agent_default().as_set();
    let mut g = Grounding::default();
    for (shape, term) in identifier_candidates(task) {
        if g.anchors.len() == MAX_FOCUS {
            break;
        }
        if is_path_like(&term) {
            match file_anchor(store, &term)? {
                Ok(Some(anchor)) => g.anchors.push(anchor),
                Ok(None) => g.unresolved.push((shape, term)),
                Err(paths) => g.ambiguous_files.push((term, paths)),
            }
            continue;
        }
        let nodes = match find_symbol(store, &term)? {
            Found::Exact(nodes) if nodes.len() <= MAX_ANCHOR_NODES => nodes,
            _ => {
                g.unresolved.push((shape, term));
                continue;
            }
        };
        let grounded: Vec<Node> = nodes
            .into_iter()
            // An ordinary word is only a name when it *is* the name: a
            // bare `field` must not be promoted to `Index::field`.
            .filter(|n| shape != Shape::Bare || qualified_of(n.id.as_str()) == term)
            // ...and never through a fixture or vendored copy, which is
            // where lone English verbs like `add` usually live.
            .filter(|n| preferred.contains(&scope_index.scope_of(n)))
            .collect();
        if grounded.is_empty() {
            g.unresolved.push((shape, term));
            continue;
        }
        for node in grounded {
            if g.anchors.len() == MAX_FOCUS {
                break;
            }
            if g.anchors.iter().all(|a| a.node.id != node.id) {
                g.anchors.push(Anchor {
                    term: term.clone(),
                    node,
                    contained: Vec::new(),
                    fuzzy: false,
                });
            }
        }
    }
    // Nothing named a node outright: one fuzzy hop per intent word, so a
    // task can still ground before lexical ranking guesses for it.
    if g.anchors.is_empty() {
        for (shape, term) in &g.unresolved {
            if g.anchors.len() == MAX_FOCUS {
                break;
            }
            if is_path_like(term) || term.chars().count() < MIN_FUZZY_LEN {
                continue;
            }
            if let Some(node) = fuzzy_anchor(store, &scope_index, &preferred, *shape, term)?
                && g.anchors.iter().all(|a| a.node.id != node.id)
            {
                g.anchors.push(Anchor {
                    term: term.clone(),
                    node,
                    contained: Vec::new(),
                    fuzzy: true,
                });
            }
        }
    }
    Ok(g)
}

/// `card` reads its provenance from an `ask` hit; an anchor states its own.
fn anchor_hit(anchor: &Anchor) -> Value {
    let node = &anchor.node;
    json!({
        "id": node.symbol_key().as_str(),
        "snapshot_id": node.id.as_str(),
        "doc": node.doc,
        "matched": [anchor.term],
        "roles": ["anchor"],
        "channels": [if anchor.fuzzy { "fuzzy" } else { "identifier" }],
    })
}

/// Language key `testcmd` understands, from a file's extension.
fn language_of(file: &str) -> &'static str {
    match file.rsplit('.').next().unwrap_or("") {
        "rs" => "rust",
        "go" => "go",
        "ts" | "tsx" | "mts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        _ => "",
    }
}

/// Nearest ancestor of `file` holding a package manifest, repository-
/// relative; empty for the root.
fn package_dir_of(root: &Path, file: &str) -> String {
    let mut dir = Path::new(file).parent();
    while let Some(d) = dir {
        if MANIFESTS.iter().any(|m| root.join(d).join(m).is_file()) {
            return d.to_string_lossy().into_owned();
        }
        dir = d.parent();
    }
    String::new()
}

/// Conventional test file names, for a file node the scope index did not
/// mark (`index.test.ts` beside its source).
fn is_test_file(file: &str) -> bool {
    let base = file.rsplit('/').next().unwrap_or(file);
    base.contains(".test.")
        || base.contains(".spec.")
        || base.ends_with("_test.go")
        || base.ends_with("_test.py")
        || base.starts_with("test_")
        || file.starts_with("tests/")
        || file.contains("/tests/")
        || file.contains("/test/")
        || file.contains("/__tests__/")
}

/// The runnable command for one affected test: the exact cargo target
/// when the layout names one, otherwise the ecosystem's runner.
fn command_for(root: &Path, store: &Store, test: &crate::impact::SymbolRef) -> Option<String> {
    let language = language_of(&test.file);
    if language == "rust"
        && let Some(cmd) =
            test_node(store, test).and_then(|n| crate::impact::test_command(root, &n))
    {
        return Some(cmd);
    }
    if language.is_empty() {
        return None;
    }
    let name = if test.kind == "file" {
        ""
    } else {
        test.qualified
            .rsplit("::")
            .next()
            .unwrap_or(&test.qualified)
    };
    Some(crate::testcmd::test_command(
        language,
        &package_dir_of(root, &test.file),
        &test.file,
        name,
    ))
}

/// Affected-test rows for the focus set: `impact`'s selection plus test
/// *files* the radius reached by import (a TS suite keeps its cases in
/// callbacks the graph has no node for, so the file is the test). Each
/// row carries `via`, the first hop, when the test was not a direct
/// dependent; direct rows sort first.
fn test_rows(
    root: &Path,
    store: &Store,
    focus: &[Node],
    filter: &EdgeFilter,
) -> Result<Vec<Value>> {
    let mut reached: BTreeMap<String, (Node, String)> = BTreeMap::new();
    for node in focus {
        for r in store.dependents(&node.id, filter, 25)? {
            reached
                .entry(r.node.id.as_str().to_owned())
                .or_insert((r.node, r.via.dst.as_str().to_owned()));
        }
    }
    for node in focus {
        reached.remove(node.id.as_str());
    }
    let radius: BTreeMap<String, Node> = reached
        .iter()
        .map(|(id, (node, _))| (id.clone(), node.clone()))
        .collect();
    let scope_index = store.scope_index()?;
    let mut tests = crate::impact::affected_tests(store, &radius, focus)?;
    for (node, _) in reached.values() {
        if node.kind == SymbolKind::File
            && (scope_index.scope_of(node) == CorpusScope::Test || is_test_file(&node.file))
        {
            tests.push(crate::impact::SymbolRef {
                qualified: node.file.clone(),
                kind: "file",
                file: node.file.clone(),
            });
        }
    }
    let focus_ids: BTreeSet<&str> = focus.iter().map(|n| n.id.as_str()).collect();
    let via_of: HashMap<(String, String), Option<String>> = reached
        .values()
        .map(|(node, via)| {
            let hop = (!focus_ids.contains(via.as_str())).then(|| qualified_of(via).to_owned());
            (
                (node.file.clone(), qualified_of(node.id.as_str()).to_owned()),
                hop,
            )
        })
        .collect();
    let mut rows: Vec<(Option<String>, Value)> = tests
        .iter()
        .map(|t| {
            let via = via_of
                .get(&(t.file.clone(), t.qualified.clone()))
                .cloned()
                .flatten();
            let row = json!({
                "qualified": t.qualified,
                "kind": t.kind,
                "file": t.file,
                "cmd": command_for(root, store, t),
                "via": via,
            });
            (via, row)
        })
        .collect();
    rows.sort_by_key(|(via, _)| via.is_some());
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

/// The node behind one affected-test row, so `impact` can render its
/// runnable command. Only the rows actually kept are resolved.
fn test_node(store: &Store, test: &crate::impact::SymbolRef) -> Option<Node> {
    match find_symbol(store, &format!("{}@{}", test.qualified, test.file)) {
        Ok(Found::Exact(mut nodes)) if !nodes.is_empty() => Some(nodes.remove(0)),
        _ => None,
    }
}

/// Every `ask` hit across topics, best first, deduplicated by handle.
/// Prose sections sit below code unless the task reaches for docs.
fn ranked_hits(ask: &Value, wants_docs: bool) -> Vec<Value> {
    let mut hits: Vec<Value> = ask["topics"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|topic| topic["hits"].as_array().into_iter().flatten().cloned())
        .collect();
    hits.sort_by(|a, b| b["score"].as_i64().cmp(&a["score"].as_i64()));
    if !wants_docs {
        hits.sort_by_key(|hit| hit["kind"] == "section");
    }
    let mut seen = BTreeSet::new();
    hits.retain(|hit| seen.insert(hit["id"].as_str().unwrap_or("").to_owned()));
    hits
}

fn excerpt(repo: &Path, node: &Node) -> Option<String> {
    let source = std::fs::read_to_string(repo.join(&node.file)).ok()?;
    let start = (node.span.start as usize).min(source.len());
    let end = (node.span.end as usize).min(source.len());
    let body = source.get(start..end)?;
    Some(
        body.lines()
            .take(EXCERPT_LINES)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn edge_row(r: &Reached) -> Value {
    json!({
        "s": qualified_of(r.node.id.as_str()),
        "k": r.node.kind.as_str(),
        "f": r.node.file,
        "e": format!("{}/{}", r.via.relation.as_str(), r.via.evidence.as_str()),
    })
}

fn rows(reached: &[&Reached]) -> Vec<Value> {
    reached
        .iter()
        .take(EDGE_ROWS)
        .map(|r| edge_row(r))
        .collect()
}

/// `show`-style card plus direct deps/affected for one focus candidate.
fn card(
    repo: &Path,
    store: &Store,
    hit: &Value,
    node: &Node,
    filter: &EdgeFilter,
    confidences: &mut Vec<Confidence>,
) -> Result<Value> {
    let deps = store.dependencies(&node.id, filter, 1)?;
    let dependents = store.dependents(&node.id, filter, 1)?;
    confidences.extend(
        deps.iter()
            .chain(dependents.iter())
            .map(|r| r.via.confidence),
    );
    let (callers, importers): (Vec<&Reached>, Vec<&Reached>) = dependents
        .iter()
        .partition(|r| r.via.relation != Relation::Imports);
    let (direct, direct_files) = sinter_store::direct_summary(&dependents);
    let dep_refs: Vec<&Reached> = deps.iter().collect();
    Ok(json!({
        "id": hit["id"],
        "handle": handle_of(node),
        "qualified": qualified_of(node.id.as_str()),
        "name": node.name,
        "kind": node.kind.as_str(),
        "file": node.file,
        "line": line_of(repo, &node.file, node.span.start),
        "end_line": line_of(repo, &node.file, node.span.end),
        "signature": node.signature,
        "doc": hit["doc"],
        "excerpt": excerpt(repo, node),
        "why": {"matched": hit["matched"], "roles": hit["roles"], "channels": hit["channels"]},
        "deps": {"total": deps.len(), "direct": rows(&dep_refs)},
        "affected": {
            "direct": direct,
            "direct_files": direct_files,
            "callers": rows(&callers),
            "importing_files": importers.len(),
            "importers": rows(&importers),
        },
    }))
}

/// The packet. Shared by CLI `--json` and the MCP `context` tool.
pub(crate) fn response(repo: &Path, store: &Store, task: &str) -> Result<Value> {
    let root = crate::pipeline::discover_root(repo).canonicalize()?;
    let snapshot = ensure_snapshot(store, None)?;
    let ask = crate::ask::ask_response_with_store(
        &root,
        store,
        &lexical_query(task),
        ASK_LIMIT,
        &ScopeSelection::ask_default(),
        false,
    )?;
    let abstain = ask["decision"] == "abstain";
    let abstain_reason = ask["topics"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|topic| topic["status"] == "abstain")
        .map(|topic| topic["confidence"]["reason"].clone())
        .unwrap_or(Value::Null);
    let Grounding {
        anchors,
        unresolved: unresolved_intents,
        ambiguous_files,
    } = anchors_of(store, task)?;
    let mut hits = ranked_hits(&ask, crate::ask::query::wants_docs(task));
    // A feature or CLI change is edited at an entry point, so an entry
    // point outranks an equally-scored candidate that is not one.
    if is_feature_task(task) {
        let entries = entry_point_ids(store)?;
        let biased = |hit: &Value| {
            let score = hit["score"].as_i64().unwrap_or(0);
            let entry = is_entry_point(hit["name"].as_str().unwrap_or(""))
                || entries.contains(hit["snapshot_id"].as_str().unwrap_or(""));
            if entry {
                score * (1000 + ENTRY_BIAS_PERMILLE) / 1000
            } else {
                score
            }
        };
        hits.sort_by_key(|hit| std::cmp::Reverse(biased(hit)));
    }
    let hits = hits;
    let top = hits.first().and_then(|h| h["score"].as_i64()).unwrap_or(0);

    let filter = EdgeFilter::default();
    let mut candidates = Vec::with_capacity(hits.len() + anchors.len());
    let mut focus: Vec<Node> = Vec::new();
    let mut confidences = Vec::new();
    let mut unresolved = 0usize;
    let mut rank = 0usize;
    for anchor in &anchors {
        let hit = anchor_hit(anchor);
        let mut entry = card(&root, store, &hit, &anchor.node, &filter, &mut confidences)?;
        rank += 1;
        entry["rank"] = json!(rank);
        entry["score"] = Value::Null;
        entry["focus"] = json!(true);
        entry["anchor"] = json!(anchor.term);
        unresolved += store.unresolved_named(&anchor.node.name)?;
        focus.push(anchor.node.clone());
        candidates.push(entry);
    }
    // A file anchor's own symbols: context rows, never edit targets.
    for anchor in &anchors {
        for node in &anchor.contained {
            if candidates.iter().any(|c| c["handle"] == handle_of(node)) {
                continue;
            }
            rank += 1;
            candidates.push(json!({
                "id": node.symbol_key().as_str(),
                "handle": handle_of(node),
                "qualified": qualified_of(node.id.as_str()),
                "kind": node.kind.as_str(),
                "file": node.file,
                "line": line_of(&root, &node.file, node.span.start),
                "why": {"matched": [anchor.term], "roles": ["contained"], "channels": ["file"]},
                "rank": rank,
                "score": Value::Null,
                "focus": false,
            }));
        }
    }
    let anchored: BTreeSet<String> = anchors
        .iter()
        .flat_map(|a| std::iter::once(&a.node).chain(a.contained.iter()))
        .map(|n| n.id.as_str().to_owned())
        .collect();
    for hit in &hits {
        let id = NodeId::new(hit["snapshot_id"].as_str().unwrap_or(""));
        if anchored.contains(id.as_str()) {
            continue;
        }
        let Some(node) = store.node(&id)? else {
            continue;
        };
        let score = hit["score"].as_i64().unwrap_or(0);
        // Resolved identifiers outrank lexical similarity: once anything
        // grounded, a bag-of-words hit is context, never an edit target.
        let expand = anchors.is_empty()
            && focus.len() < MAX_FOCUS
            && (rank == 0 || (!abstain && is_contender(score, top)));
        let mut entry = if expand {
            card(&root, store, hit, &node, &filter, &mut confidences)?
        } else {
            json!({
                "id": hit["id"],
                "handle": format!("{}@{}", node.name, node.file),
                "qualified": qualified_of(node.id.as_str()),
                "kind": node.kind.as_str(),
                "file": node.file,
                "line": hit["line"],
                "why": {"matched": hit["matched"], "roles": hit["roles"], "channels": hit["channels"]},
            })
        };
        rank += 1;
        entry["rank"] = json!(rank);
        entry["score"] = json!(score);
        entry["focus"] = json!(expand);
        if expand {
            unresolved += store.unresolved_named(&node.name)?;
            focus.push(node);
        }
        candidates.push(entry);
    }

    let mut test_rows = test_rows(&root, store, &focus, &filter)?;
    let tests_total = test_rows.len();
    test_rows.truncate(TEST_ROWS);

    // Literal pass: the task's quoted strings and flags, verbatim, which no
    // symbol name carries. Manifest and completion-table hits are mirrors
    // of a registration made in code.
    let literal_hits =
        crate::grep::literal_scan(&root, &crate::grep::literal_tokens(task), LITERAL_CAP);
    let (mirrors, literals): (Vec<&crate::grep::Hit>, Vec<&crate::grep::Hit>) = literal_hits
        .iter()
        .partition(|h| crate::grep::is_mirror_file(&h.file));

    let evidence = crate::coverage::TraversalEvidence::from_confidences(confidences, unresolved);
    let coverage =
        crate::coverage::traversal_json(&root, store, &filter, evidence, !focus.is_empty())?;

    let grounded = !anchors.is_empty();
    // An anchor or a real candidate list is an answer whatever `ask`'s
    // calibration says about its own top hit.
    let ranked = grounded || hits.len() >= MIN_LEXICAL_HITS || (!abstain && !focus.is_empty());
    // Each action is the MCP call (`tool`, `args`) plus its CLI rendering
    // (`cli`); `tool_calls` / `cli_actions` keep one side each.
    let mut next_actions: Vec<Value> = Vec::new();
    if !ranked {
        let terms = ask["topics"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|t| t["query_terms"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .filter(|term| term.len() > 2)
            .take(GREP_TERMS)
            .collect::<Vec<_>>()
            .join("|");
        next_actions.push(json!({
            "tool": "grep",
            "args": {"pattern": terms},
            "cli": format!("sinter grep '{terms}'"),
        }));
        next_actions.push(json!({"tool": "map", "args": {}, "cli": "sinter map"}));
        next_actions.push(json!({
            "tool": "ask",
            "args": {"question": "<one concrete term from the task>"},
            "cli": "sinter ask \"<one concrete term from the task>\"",
        }));
    }
    for node in &focus {
        let handle = handle_of(node);
        next_actions.push(json!({
            "tool": "show",
            "args": {"symbol": handle},
            "cli": format!("sinter show {handle}"),
        }));
        next_actions.push(json!({
            "tool": "affected",
            "args": {"symbol": handle, "max_depth": 3},
            "cli": format!("sinter affected {handle} --max-depth 3"),
        }));
    }
    next_actions.push(json!({
        "tool": "impact",
        "args": {"rev_range": "HEAD"},
        "cli": "sinter impact  # after editing: changed symbols, blast radius, tests",
    }));

    Ok(json!({
        "task": task,
        "snapshot": snapshot,
        "outcome": if ranked { "ranked" } else { "abstain" },
        "anchors": anchors
            .iter()
            .map(|a| json!({
                "term": a.term,
                "qualified": qualified_of(a.node.id.as_str()),
                "k": a.node.kind.as_str(),
                "f": a.node.file,
                "fuzzy": a.fuzzy,
            }))
            .collect::<Vec<_>>(),
        "unresolved_intents": unresolved_intents.iter().map(|(_, t)| t).collect::<Vec<_>>(),
        "candidates": candidates,
        "tests": test_rows,
        "tests_total": tests_total,
        "literals": literals.iter().take(LITERAL_ROWS).map(|h| h.json()).collect::<Vec<_>>(),
        "literals_total": literals.len(),
        "mirrors": mirrors.iter().take(LITERAL_ROWS).map(|h| h.json()).collect::<Vec<_>>(),
        "mirrors_total": mirrors.len(),
        "gaps": {
            "abstain_reason": abstain_reason,
            "ambiguous_files": ambiguous_files
                .iter()
                .map(|(term, paths)| json!({"term": term, "candidates": paths}))
                .collect::<Vec<_>>(),
            "unresolved_refs_matching_candidates": unresolved,
            "ask_advice": ask["topics"][0]["advice"],
        },
        "coverage": coverage,
        "next_actions": next_actions,
    }))
}

/// Keep only the MCP half of every `next_actions` entry: `{tool, args}`
/// objects a client sends straight back. CLI-only fallbacks are dropped.
pub(crate) fn tool_calls(packet: &mut Value) {
    if let Some(actions) = packet["next_actions"].as_array_mut() {
        actions.retain(|action| action.get("tool").is_some());
        for action in actions {
            action.as_object_mut().map(|a| a.remove("cli"));
        }
    }
}

/// Keep only the CLI rendering of every `next_actions` entry.
pub(crate) fn cli_actions(packet: &mut Value) {
    if let Some(actions) = packet["next_actions"].as_array_mut() {
        for action in actions {
            *action = action["cli"].clone();
        }
    }
}

/// Ok(true) when the packet has a ranked edit target (grep-style exit codes).
pub fn run(repo: &Path, task: &str, json: bool) -> Result<bool> {
    let store = open_store(repo)?;
    let mut packet = response(repo, &store, task)?;
    cli_actions(&mut packet);
    let ranked = packet["outcome"] == "ranked";
    if json {
        crate::agent_protocol::write_json(&packet)?;
        return Ok(ranked);
    }
    print_packet(&packet);
    Ok(ranked)
}

/// Multi-line text as one line, cut at a word boundary past `max` chars.
fn one_line(text: &str, max: usize) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() && out.chars().count() + 1 + word.chars().count() > max {
            out.push('\u{2026}');
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Compact human rendering, bounded to roughly forty lines.
fn print_packet(p: &Value) {
    let list = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .map(|r| {
                format!(
                    "{} ({})",
                    r["s"].as_str().unwrap_or(""),
                    r["f"].as_str().unwrap_or("")
                )
            })
            .collect()
    };
    println!(
        "context: {}  [{}]",
        p["task"].as_str().unwrap_or(""),
        p["outcome"].as_str().unwrap_or("")
    );
    let anchors: Vec<String> = p["anchors"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            format!(
                "{} {} {}",
                a["term"].as_str().unwrap_or(""),
                if a["fuzzy"] == true { "~>" } else { "->" },
                a["qualified"].as_str().unwrap_or("")
            )
        })
        .collect();
    if !anchors.is_empty() {
        println!("anchors: {}", anchors.join(", "));
    }
    for a in p["gaps"]["ambiguous_files"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let paths: Vec<&str> = a["candidates"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        println!(
            "ambiguous file: {} -> {}",
            a["term"].as_str().unwrap_or(""),
            ellipsize(&paths.join(", "), 100)
        );
    }
    for c in p["candidates"].as_array().into_iter().flatten() {
        let marker = if c["anchor"].is_string() {
            "@"
        } else if c["focus"] == true {
            "*"
        } else {
            " "
        };
        println!(
            "{marker}{}. {} {}  {}:{}  [{}]",
            c["rank"],
            c["kind"].as_str().unwrap_or(""),
            c["qualified"].as_str().unwrap_or(""),
            c["file"].as_str().unwrap_or(""),
            c["line"],
            c["why"]["matched"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        );
        if c["focus"] != true {
            continue;
        }
        if let Some(sig) = c["signature"].as_str().filter(|s| !s.is_empty()) {
            println!("     {}", ellipsize(sig, 100));
        }
        if let Some(doc) = c["doc"].as_str() {
            println!("     /// {}", one_line(doc, 100));
        }
        let deps = list(&c["deps"]["direct"]);
        println!(
            "     deps ({}): {}",
            c["deps"]["total"],
            ellipsize(&deps.join(", "), 110)
        );
        let callers = list(&c["affected"]["callers"]);
        println!(
            "     affected: {} direct in {} file(s); {} importing file(s): {}",
            c["affected"]["direct"],
            c["affected"]["direct_files"],
            c["affected"]["importing_files"],
            ellipsize(&callers.join(", "), 90)
        );
    }
    let tests = p["tests"].as_array().map_or(&[][..], Vec::as_slice);
    println!(
        "tests ({} affected, {} shown):",
        p["tests_total"],
        tests.len()
    );
    for t in tests.iter().take(PRINTED_TESTS) {
        let via = t["via"]
            .as_str()
            .map_or(String::new(), |via| format!("  (via {via})"));
        match t["cmd"].as_str() {
            Some(cmd) => println!("  {cmd}{via}"),
            None => println!(
                "  # {} ({}){via}",
                t["qualified"].as_str().unwrap_or(""),
                t["file"].as_str().unwrap_or("")
            ),
        }
    }
    if let Some(rest) = tests.len().checked_sub(PRINTED_TESTS).filter(|n| *n > 0) {
        println!("  # +{rest} more (--json for all)");
    }
    for key in ["literals", "mirrors"] {
        let rows = p[key].as_array().map_or(&[][..], Vec::as_slice);
        if rows.is_empty() {
            continue;
        }
        println!(
            "{key} ({} found, {} shown):",
            p[format!("{key}_total")],
            rows.len()
        );
        for row in rows {
            println!(
                "  {}:{}: {}",
                row["f"].as_str().unwrap_or(""),
                row["l"],
                ellipsize(row["t"].as_str().unwrap_or("").trim(), 100)
            );
        }
    }
    println!(
        "gaps: coverage {}; unresolved refs naming candidates {}; abstain {}",
        p["coverage"]["status"].as_str().unwrap_or("?"),
        p["gaps"]["unresolved_refs_matching_candidates"],
        p["gaps"]["abstain_reason"].as_str().unwrap_or("none")
    );
    println!("next:");
    for a in p["next_actions"].as_array().into_iter().flatten() {
        println!("  {}", a.as_str().unwrap_or(""));
    }
    println!("  snapshot: {}", p["snapshot"].as_str().unwrap_or(""));
}

#[cfg(test)]
mod tests {
    use super::{
        Shape, identifier_candidates, is_entry_point, is_feature_task, is_test_file, language_of,
        lexical_query, one_line,
    };

    #[test]
    fn test_rows_know_their_language_and_file_shape() {
        assert_eq!(language_of("src/index.test.ts"), "typescript");
        assert_eq!(language_of("pkg/a_test.go"), "go");
        assert_eq!(language_of("notes.md"), "");
        assert!(is_test_file("packages/cli/src/index.test.ts"));
        assert!(is_test_file("tests/surface.rs"));
        assert!(!is_test_file("src/testing_helpers.rs"));
        assert!(is_feature_task("add a --json flag to the doctor command"));
        assert!(!is_feature_task("where is the trie node stored"));
        assert!(is_entry_point("supervisorCommand"));
        assert!(is_entry_point("runCLI"));
        assert!(!is_entry_point("NewThreadField"));
    }

    fn terms(task: &str) -> Vec<String> {
        identifier_candidates(task)
            .into_iter()
            .map(|(_, t)| t)
            .collect()
    }

    #[test]
    fn extraction_keeps_identifier_shapes_and_drops_prose() {
        let got = terms(
            "add a new field to `Decision` and thread it through adjudication, see Store::node in src/ask/query.rs (don't break it)",
        );
        for name in [
            "Decision",
            "adjudication",
            "Store::node",
            "src/ask/query.rs",
        ] {
            assert!(got.iter().any(|t| t == name), "lost `{name}`: {got:?}");
        }
        // Stopwords, sub-`MIN_WORD_LEN` words and prose punctuation never
        // reach the store.
        for prose in ["a", "to", "and", "it", "the", "don't"] {
            assert!(!got.iter().any(|t| t == prose), "kept prose `{prose}`");
        }
    }

    #[test]
    fn strongest_shape_is_resolved_first() {
        let got = identifier_candidates("thread a field through Decision via Store::node");
        assert_eq!(got[0], (Shape::Explicit, "Store::node".to_owned()));
        assert_eq!(got[1], (Shape::Shaped, "Decision".to_owned()));
        // Bare words keep task order behind everything code-shaped.
        assert_eq!(
            got[2..].iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>(),
            ["thread", "field", "through", "via"]
        );
    }

    #[test]
    fn extraction_is_deduplicated_and_bounded() {
        let task = "Node Node ".repeat(20);
        assert_eq!(terms(&task), vec!["Node".to_owned()]);
        let many = (0..40)
            .map(|i| format!("sym_{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(terms(&many).len(), super::MAX_IDENTIFIERS);
    }

    #[test]
    fn lexical_query_drops_files_filler_and_short_fragments() {
        assert_eq!(
            lexical_query("make take_budget honor a per-tool default budget in serve.rs"),
            "take_budget tool default budget"
        );
        assert_eq!(
            lexical_query("add a new placement Policy variant in src/placement.rs"),
            "placement Policy variant"
        );
        assert_eq!(lexical_query("multi-thread it"), "multi thread");
        // Nothing left: `ask` gets the task and abstains on its own terms.
        assert_eq!(lexical_query("add a new"), "add a new");
    }

    #[test]
    fn extraction_treats_hyphenated_prose_and_filler_as_noise() {
        assert_eq!(
            terms("honor a per-tool budget in serve.rs"),
            ["serve.rs", "budget"]
        );
        assert!(terms("notes.txt").is_empty());
    }

    #[test]
    fn one_line_joins_and_cuts_on_words() {
        assert_eq!(
            one_line("Pull `x`\n(never\nsees them)", 100),
            "Pull `x` (never sees them)"
        );
        assert_eq!(one_line("alpha beta gamma", 10), "alpha beta\u{2026}");
    }

    #[test]
    fn extraction_keeps_structured_tokens_stopwords_would_lose() {
        // `use` and `for` are filler as words but real as identifiers.
        assert_eq!(
            terms("use for_each and use::this"),
            ["use::this".to_owned(), "for_each".to_owned()]
        );
    }
}
