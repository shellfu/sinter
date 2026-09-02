//! `sinter show <symbol>`: the "I found it, now orient me" card — grouped,
//! capped, evidence-tagged. One bounded screen, never a BFS dump.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sinter_core::{Edge, Evidence, Node, Relation, Span, SymbolKind};
use sinter_resolve::qualified_of;
use sinter_store::{EdgeFilter, Store};

use crate::lookup::{
    Resolved, also_see, enclosing_at, ensure_snapshot, open_store, resolve_symbol_in, selectors,
    short_list, split_handle, split_line,
};
use crate::render::{ellipsize, line_of, location, node_json, site_json, site_location};

/// Rows shown per relation group before collapsing to `… (+N)`.
pub const DEFAULT_LIMIT: usize = 20;

/// Lines of the excerpt when the caller names none (MCP `body` without
/// `context_lines`), and the half-window around an explicit `:line`.
pub const DEFAULT_BODY_LINES: usize = 10;

/// Byte ceiling of a default `--body` excerpt (`--budget-bytes` overrides).
pub const DEFAULT_BODY_BYTES: usize = 4096;

/// A span this short is printed whole under the default rule.
const WHOLE_BODY_LINES: usize = 60;

/// How much of a span `--body` prints.
#[derive(Clone, Copy, Debug)]
pub enum BodyLimit {
    /// `--context-lines N`; `0` is the whole span.
    Lines(usize),
    /// Whole span up to [`WHOLE_BODY_LINES`], else as many lines as fit the
    /// byte budget (`None` = unlimited).
    Budget(Option<usize>),
}

/// Presentation switches for one `show` card.
pub struct Options {
    pub limit: usize,
    pub json: bool,
    pub if_snapshot: Option<String>,
    pub body: Option<BodyLimit>,
    /// List the used-by files (the default is a one-line tally).
    pub callers: bool,
    /// Print the bodies of the type's `impl` blocks.
    pub impls: bool,
    /// Force the structural outline, whatever the span's size.
    pub outline: bool,
    /// Byte budget for `--impls` bodies (`--budget-bytes`; `None` = unlimited).
    pub budget_bytes: Option<usize>,
}

/// A bounded source excerpt plus how much of the span it left out, so a
/// cut is never silent: the card says "N more lines" and the JSON carries
/// `excerpt_truncated`/`excerpt_total_lines`.
pub(crate) struct Excerpt {
    pub text: String,
    /// Lines in the whole span, shown or not.
    pub total_lines: usize,
    pub truncated: bool,
    /// 1-based line of the first excerpt line, when known.
    pub first_line: Option<usize>,
    /// 1-based line the excerpt was centred on (`X@file:line`).
    pub target_line: Option<usize>,
}

/// Set `excerpt`, `excerpt_truncated` and `excerpt_total_lines` on a
/// `show` envelope. The one writer for CLI `--json` and MCP `show`, so the
/// two stay byte-identical.
pub(crate) fn excerpt_json(out: &mut Value, body: &Excerpt) {
    out["excerpt"] = json!(body.text);
    out["excerpt_truncated"] = json!(body.truncated);
    out["excerpt_total_lines"] = json!(body.total_lines);
    if let Some(line) = body.first_line {
        out["excerpt_first_line"] = json!(line);
    }
    if let Some(line) = body.target_line {
        out["excerpt_target_line"] = json!(line);
    }
}

/// Keep the first lines of `all` that `limit` admits. `truncated` is true
/// iff a line was dropped.
fn cut(all: &[&str], limit: BodyLimit) -> (String, bool) {
    let total = all.len();
    let keep = match limit {
        BodyLimit::Lines(0) => total,
        BodyLimit::Lines(n) => n.min(total),
        BodyLimit::Budget(_) if total <= WHOLE_BODY_LINES => total,
        BodyLimit::Budget(None) => total,
        BodyLimit::Budget(Some(bytes)) => {
            let mut used = 0;
            all.iter()
                .take_while(|line| {
                    used += line.len() + 1;
                    used <= bytes
                })
                .count()
                .max(1)
        }
    };
    (all[..keep].join("\n"), keep < total)
}

/// Start of the line holding `byte`, moved back over any directly
/// preceding `#[...]` attribute lines so a struct excerpt shows its
/// derives. ponytail: one attribute per line; a multi-line attribute stops
/// the walk at its closing line.
fn attribute_start(source: &str, byte: usize) -> usize {
    let mut start = source[..byte].rfind('\n').map_or(0, |i| i + 1);
    while start > 0 {
        let prev = source[..start - 1].rfind('\n').map_or(0, |i| i + 1);
        if !source[prev..start].trim_start().starts_with("#[") {
            break;
        }
        start = prev;
    }
    start
}

/// The span's source under `limit`, attributes included.
pub(crate) fn excerpt(
    repo: &Path,
    file: &str,
    start: u64,
    end: u64,
    limit: BodyLimit,
) -> Option<Excerpt> {
    let source = std::fs::read_to_string(repo.join(file)).ok()?;
    let start = (start as usize).min(source.len());
    let end = (end as usize).min(source.len()).max(start);
    if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
        return None;
    }
    let start = attribute_start(&source, start);
    let all: Vec<&str> = source[start..end].lines().collect();
    let (text, truncated) = cut(&all, limit);
    Some(Excerpt {
        text,
        total_lines: all.len(),
        truncated,
        first_line: Some(source[..start].matches('\n').count() + 1),
        target_line: None,
    })
}

/// [`excerpt`] capped at `lines` (`0` = whole span). The MCP `show` producer.
pub(crate) fn excerpt_lines(
    repo: &Path,
    file: &str,
    start: u64,
    end: u64,
    lines: usize,
) -> Option<Excerpt> {
    excerpt(repo, file, start, end, BodyLimit::Lines(lines))
}

/// `±half` lines around `line`, clipped to the span: the answer to
/// `show X@file:line --body` is that line in context, not the span head.
fn excerpt_around(
    repo: &Path,
    file: &str,
    span: Span,
    line: usize,
    half: usize,
) -> Option<Excerpt> {
    let source = std::fs::read_to_string(repo.join(file)).ok()?;
    let first = line_of(repo, file, span.start)?;
    let last = line_of(repo, file, span.end.max(span.start))?;
    let line = line.clamp(first, last);
    let lo = line.saturating_sub(half).max(first);
    let hi = (line + half).min(last);
    let text = source
        .lines()
        .skip(lo - 1)
        .take(hi - lo + 1)
        .collect::<Vec<_>>()
        .join("\n");
    Some(Excerpt {
        text,
        total_lines: last - first + 1,
        truncated: lo > first || hi < last,
        first_line: Some(lo),
        target_line: Some(line),
    })
}

/// One `impl` block header naming the type, found textually in the files
/// that hold its methods. ponytail: Rust-shaped, whole-word match on the
/// header, brace-counted body; impl nodes in the graph would replace this.
struct ImplBlock {
    file: String,
    line: usize,
    header: String,
    start: u64,
    end: u64,
}

fn is_word_boundary(c: Option<char>) -> bool {
    c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
}

fn header_names(header: &str, name: &str) -> bool {
    header.match_indices(name).any(|(i, _)| {
        is_word_boundary(header[..i].chars().next_back())
            && is_word_boundary(header[i + name.len()..].chars().next())
    })
}

fn impl_blocks(repo: &Path, store: &Store, node: &Node) -> Result<Vec<ImplBlock>> {
    if matches!(
        node.kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::File | SymbolKind::Section
    ) {
        return Ok(Vec::new());
    }
    let mut files = BTreeSet::from([node.file.clone()]);
    for member in store.nodes_glob(&format!("{}::", qualified_of(node.id.as_str())), "")? {
        files.insert(member.file);
    }
    let mut blocks = Vec::new();
    for file in files {
        let Ok(source) = std::fs::read_to_string(repo.join(&file)) else {
            continue;
        };
        let mut offset = 0usize;
        for (index, line) in source.split_inclusive('\n').enumerate() {
            let trimmed = line.trim_start();
            let header = trimmed.split('{').next().unwrap_or(trimmed).trim_end();
            if trimmed.starts_with("impl")
                && matches!(trimmed[4..].chars().next(), Some(' ') | Some('<'))
                && header_names(header, &node.name)
            {
                let start = offset + (line.len() - trimmed.len());
                let mut depth = 0i32;
                let mut end = source.len();
                for (i, c) in source[start..].char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' if depth == 1 => {
                            end = start + i + 1;
                            break;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                blocks.push(ImplBlock {
                    file: file.clone(),
                    line: index + 1,
                    header: header.to_string(),
                    start: start as u64,
                    end: end as u64,
                });
            }
            offset += line.len();
        }
    }
    Ok(blocks)
}

/// A function's local `const`/`static`/`let` is an implementation detail,
/// not a child worth a `contains` row.
fn local_binding(store: &Store, parent: &Node, edge: &Edge) -> Result<bool> {
    if edge.relation != Relation::Contains
        || !matches!(parent.kind, SymbolKind::Function | SymbolKind::Method)
    {
        return Ok(false);
    }
    Ok(store.node(&edge.dst)?.is_some_and(|child| {
        matches!(
            child.kind,
            SymbolKind::Constant | SymbolKind::Static | SymbolKind::Variable
        )
    }))
}

/// The symbol's edges after `--relations` / `--scope`: outgoing first,
/// incoming second. Scope applies to the far end of each edge; contains
/// edges survive only when no relation restriction was given.
pub fn edges(store: &Store, node: &Node, filter: &EdgeFilter) -> Result<(Vec<Edge>, Vec<Edge>)> {
    let scopes = store.scope_index()?;
    let keep = |e: &Edge, other: &str| {
        filter
            .relations
            .as_ref()
            .is_none_or(|set| set.contains(&e.relation))
            && filter.scopes.as_ref().is_none_or(|set| {
                let file = other.split_once('#').map_or(other, |(f, _)| f);
                set.contains(&scopes.scope_of_id(other, file))
            })
    };
    let mut out = Vec::new();
    for e in store.out_edges(&node.id)? {
        if keep(&e, e.dst.as_str()) && !local_binding(store, node, &e)? {
            out.push(e);
        }
    }
    let inn = store
        .in_edges(&node.id)?
        .into_iter()
        .filter(|e| keep(e, e.src.as_str()))
        .collect();
    Ok((out, inn))
}

/// `outgoing`/`incoming` arrays capped at `limit` per relation, plus
/// `totals` and (only when something was cut) `truncated` per group —
/// the same convention as `affected`. Shared by the CLI and MCP `show`.
pub fn edges_json(
    repo: &Path,
    store: &Store,
    node: &Node,
    filter: &EdgeFilter,
    limit: usize,
) -> Result<Value> {
    let (out, inn) = edges(store, node, filter)?;
    let mut totals = json!({});
    let mut truncated = json!({});
    let mut direction = |name: &str, edges: &[Edge], other: fn(&Edge) -> &str| -> Vec<Value> {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        let mut rows = Vec::new();
        for e in edges {
            let n = seen.entry(e.relation.as_str()).or_default();
            *n += 1;
            if *n <= limit {
                let mut row = json!({
                    "symbol": qualified_of(other(e)),
                    "relation": e.relation.as_str(),
                    "evidence": e.evidence.as_str(),
                    "site": site_json(repo, e),
                });
                crate::render::add_sites(&mut row, repo, e);
                rows.push(row);
            }
        }
        for (rel, n) in seen {
            totals[name][rel] = json!(n);
            if n > limit {
                truncated[name][rel] = json!(n - limit);
            }
        }
        rows
    };
    let outgoing = direction("outgoing", &out, |e| e.dst.as_str());
    let incoming = direction("incoming", &inn, |e| e.src.as_str());
    let mut v = json!({"outgoing": outgoing, "incoming": incoming, "totals": totals});
    if truncated.as_object().is_some_and(|m| !m.is_empty()) {
        v["truncated"] = truncated;
    }
    Ok(v)
}

/// Join `shown` exemplars and collapse the rest to `… (+N) · --limit`.
fn listed(shown: Vec<String>, total: usize) -> String {
    let mut out = shown.join(", ");
    if total > shown.len() {
        out.push_str(&format!(", … (+{}) · --limit", total - shown.len()));
    }
    out
}

fn short(id: &str) -> &str {
    let q = qualified_of(id);
    q.rsplit("::").next().unwrap_or(q)
}

fn names(edges: &[&Edge], end: fn(&Edge) -> &str, limit: usize) -> String {
    listed(
        edges
            .iter()
            .take(limit)
            .map(|e| short(end(e)).to_string())
            .collect(),
        edges.len(),
    )
}

fn evidence_tally(edges: &[&Edge]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in edges {
        *counts.entry(e.evidence.as_str()).or_default() += 1;
    }
    counts
        .iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `  | line` rows, the centred line as `  > line`, then the cut marker.
fn print_excerpt(body: &Excerpt, hint: &str) {
    for (i, l) in body.text.lines().enumerate() {
        let here = body
            .first_line
            .zip(body.target_line)
            .is_some_and(|(first, target)| first + i == target);
        println!("  {} {l}", if here { '>' } else { '|' });
    }
    if body.truncated {
        println!(
            "  | … {} more lines ({hint})",
            body.total_lines - body.text.lines().count()
        );
    }
}

/// Ok(true) when the symbol resolved (grep-style exit codes).
pub fn run(repo: &Path, symbol: &str, filter: &EdgeFilter, opts: &Options) -> Result<bool> {
    let repo = repo.canonicalize()?;
    let store = open_store(&repo)?;
    let snapshot = ensure_snapshot(&store, opts.if_snapshot.as_deref())?;
    // `@file:line` asks for whatever encloses that line; `X@file:line`
    // resolves X and keeps the line for the excerpt.
    let (resolved, target_line) = match symbol.strip_prefix('@') {
        Some(at) => (
            Resolved::unique(enclosing_at(&store, &repo, at)?),
            split_line(at).1,
        ),
        None => (
            resolve_symbol_in(&store, symbol, filter.scopes.as_ref())?,
            split_handle(symbol).2,
        ),
    };
    let node = &resolved.node;
    let scope = store.file_scope(&node.file)?;
    let family = also_see(&store, node)?;
    let body = opts.body.and_then(|limit| match target_line {
        Some(line) => {
            let half = match limit {
                BodyLimit::Lines(n) if n > 0 => n,
                _ => DEFAULT_BODY_LINES,
            };
            excerpt_around(&repo, &node.file, node.span, line, half)
        }
        None => excerpt(&repo, &node.file, node.span.start, node.span.end, limit),
    });
    // A span past the outline thresholds is a black hole: no child node,
    // no readable body. Outline it unless `--body` asked for the source.
    let outline = crate::outline::of(&repo, &store, node)?.filter(|outline| {
        !outline.rows.is_empty() && (opts.outline || (opts.body.is_none() && outline.oversized()))
    });
    let impls = impl_blocks(&repo, &store, node)?;
    let impl_body = |block: &ImplBlock| {
        opts.impls
            .then(|| {
                excerpt(
                    &repo,
                    &block.file,
                    block.start,
                    block.end,
                    BodyLimit::Budget(opts.budget_bytes),
                )
            })
            .flatten()
    };
    if opts.json {
        if !resolved.ignored.is_empty() {
            crate::agent_protocol::warn(resolved.note(symbol));
        }
        // Same shape as the MCP `show` tool.
        let mut out = edges_json(&repo, &store, node, filter, opts.limit)?;
        let mut symbol_json = node_json(node);
        symbol_json["scope"] = json!(scope.as_str());
        out["symbol"] = symbol_json;
        out["snapshot"] = json!(snapshot);
        if let Some(body) = &body {
            excerpt_json(&mut out, body);
        }
        if let Some(outline) = &outline {
            out["outline"] = outline.json(opts.limit);
            if outline.rows.len() > opts.limit {
                out["outline_truncated"] = json!(outline.rows.len() - opts.limit);
            }
        }
        if !family.is_empty() {
            out["also_see"] = json!(selectors(&family));
        }
        if !impls.is_empty() {
            out["impls"] = json!(
                impls
                    .iter()
                    .map(|block| {
                        let mut v = json!({
                            "header": block.header,
                            "site": format!("{}:{}", block.file, block.line),
                        });
                        if let Some(body) = impl_body(block) {
                            excerpt_json(&mut v, &body);
                        }
                        v
                    })
                    .collect::<Vec<_>>()
            );
        }
        crate::agent_protocol::write_json(&out)?;
        return Ok(true);
    }
    let line = line_of(&repo, &node.file, node.span.start);

    // A tie-broken pick is part of the answer, not a stderr aside: say
    // which one won, and hand back the selector that re-resolves to it.
    let selector = resolved.selector();
    if !resolved.ignored.is_empty() {
        println!(
            "resolved: {selector} ({} other{} ignored by {}: {})",
            resolved.ignored.len(),
            if resolved.ignored.len() == 1 { "" } else { "s" },
            resolved.reason,
            short_list(&resolved.ignored)
        );
    }
    if !family.is_empty() {
        println!("also_see: {}", short_list(&family));
    }
    println!(
        "{} {}    {} ({}..{}) [{scope}]",
        node.kind.as_str(),
        qualified_of(node.id.as_str()),
        location(&repo, &node.file, line),
        node.span.start,
        node.span.end,
    );
    if let Some(doc) = &node.doc {
        for l in doc.lines().take(3) {
            println!("  /// {l}");
        }
    }
    if !node.signature.is_empty() {
        println!("  {}", ellipsize(&node.signature, 110));
    }
    if let Some(body) = &body {
        let hint = if body.target_line.is_some() {
            "--context-lines N widens"
        } else {
            "--context-lines 0 for all"
        };
        print_excerpt(body, hint);
    }
    if let Some(outline) = &outline {
        println!(
            "outline ({}) of {} lines / {} bytes",
            outline.rows.len(),
            outline.lines,
            outline.bytes
        );
        for row in outline.rows.iter().take(opts.limit) {
            println!("  {:>6}  {:<7} {}", row.line, row.kind, row.text);
        }
        if outline.rows.len() > opts.limit {
            println!("  … (+{}) · --limit", outline.rows.len() - opts.limit);
        }
    }
    println!();

    let (out, inn) = edges(&store, node, filter)?;

    let group =
        |rel: Relation| -> Vec<&Edge> { out.iter().filter(|e| e.relation == rel).collect() };
    let contains = group(Relation::Contains);
    if !contains.is_empty() {
        println!(
            "contains ({})    {}",
            contains.len(),
            names(&contains, |e| e.dst.as_str(), opts.limit)
        );
    }
    if !impls.is_empty() {
        let shown = impls
            .iter()
            .take(opts.limit)
            .map(|b| format!("{} ({})", b.header, location(&repo, &b.file, Some(b.line))))
            .collect();
        println!(
            "impls ({})       {}",
            impls.len(),
            listed(shown, impls.len())
        );
        if opts.impls {
            for block in &impls {
                if let Some(body) = impl_body(block) {
                    print_excerpt(&body, "--budget-bytes N widens");
                }
            }
        }
    }
    let extends = group(Relation::Extends);
    if !extends.is_empty() {
        println!(
            "extends          {}    [{}]",
            names(&extends, |e| e.dst.as_str(), opts.limit),
            evidence_tally(&extends)
        );
    }
    let imports = group(Relation::Imports);
    if !imports.is_empty() {
        println!(
            "imports ({})     {}    [{}]",
            imports.len(),
            names(&imports, |e| e.dst.as_str(), opts.limit),
            evidence_tally(&imports)
        );
    }

    let implements = group(Relation::Implements);
    if !implements.is_empty() {
        println!(
            "implements       {}    [{}]",
            names(&implements, |e| e.dst.as_str(), opts.limit),
            evidence_tally(&implements)
        );
    }
    // Implementors are the answer to "who is behind this trait", not
    // dependents of it — listed by name, kept out of the used-by tally.
    let implementors: Vec<&Edge> = inn
        .iter()
        .filter(|e| e.relation == Relation::Implements)
        .collect();
    if !implementors.is_empty() {
        println!(
            "implemented by ({})    {}    [{}]",
            implementors.len(),
            names(&implementors, |e| e.src.as_str(), opts.limit),
            evidence_tally(&implementors)
        );
    }

    // Per source file: edges into the target, the call-site offsets kept
    // (capped) and how many sites there are in all.
    type FileUse = (usize, BTreeSet<u64>, u32);

    // used by: incoming non-contains, non-implements edges grouped by
    // source file. One tally line by default; `--callers` lists the files.
    let dependents: Vec<&Edge> = inn
        .iter()
        .filter(|e| !matches!(e.relation, Relation::Contains | Relation::Implements))
        .collect();
    if !dependents.is_empty() {
        // Per src file: edge count, the call sites themselves (capped, so a
        // hub file prints a bounded row) and how many sites there are in
        // all — repeated calls from one caller stay visible.
        let mut per_file: BTreeMap<&str, FileUse> = BTreeMap::new();
        for e in &dependents {
            let file = e
                .src
                .as_str()
                .split_once('#')
                .map_or(e.src.as_str(), |(f, _)| f);
            let entry = per_file.entry(file).or_default();
            entry.0 += 1;
            entry.2 += e.sites_total;
            for span in e.sites() {
                entry.1.insert(span.start);
            }
            while entry.1.len() > sinter_core::MAX_SITES {
                let last = *entry.1.iter().next_back().expect("non-empty");
                entry.1.remove(&last);
            }
        }
        if opts.callers {
            println!(
                "used by ({} files, {} edges)",
                per_file.len(),
                dependents.len()
            );
            let mut rows: Vec<(&str, FileUse)> = per_file.into_iter().collect();
            rows.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(b.0)));
            for (file, (count, sites, total)) in rows.iter().take(opts.limit) {
                let lines: Vec<usize> = sites
                    .iter()
                    .filter_map(|b| line_of(&repo, file, *b))
                    .collect();
                let omitted = (*total as usize).saturating_sub(sites.len());
                println!(
                    "  {}   {count} edges",
                    crate::render::lines_text(&repo, file, &lines, omitted)
                );
            }
            if rows.len() > opts.limit {
                println!("  … (+{} files) · --limit", rows.len() - opts.limit);
            }
        } else {
            println!(
                "used by: {} files, {} edges (--callers lists files; sinter affected {selector} for rows)",
                per_file.len(),
                dependents.len()
            );
        }
    }

    // Dynamic call edges fan a trait method out to its implementations.
    // Short names collide by construction (every impl is `speak`), so
    // these are listed qualified, apart from the direct calls.
    let dispatches: Vec<&Edge> = out
        .iter()
        .filter(|e| e.relation == Relation::Calls && e.evidence == Evidence::Dynamic)
        .collect();
    if !dispatches.is_empty() {
        let shown = dispatches
            .iter()
            .take(opts.limit)
            .map(|e| qualified_of(e.dst.as_str()).to_string())
            .collect();
        println!(
            "dispatches to ({})    {}",
            dispatches.len(),
            listed(shown, dispatches.len())
        );
    }

    // One row per relation: a `uses` edge (type reference) is never a call.
    for (label, rel) in [("calls", Relation::Calls), ("uses", Relation::Uses)] {
        let edges: Vec<&Edge> = out
            .iter()
            .filter(|e| e.relation == rel && e.evidence != Evidence::Dynamic)
            .collect();
        if edges.is_empty() {
            continue;
        }
        // Exemplars carry their site (`name (file:line)`) so "A calls B"
        // comes with "at file:line" instead of forcing a follow-up grep.
        let shown = edges
            .iter()
            .take(opts.limit)
            .map(|e| {
                let name = short(e.dst.as_str());
                match site_location(&repo, e) {
                    Some(site) => format!("{name} ({site})"),
                    None => name.to_string(),
                }
            })
            .collect();
        println!(
            "{:<16} {}    [{}]",
            format!("{label} ({})", edges.len()),
            listed(shown, edges.len()),
            evidence_tally(&edges)
        );
    }

    println!();
    println!("Next: sinter affected {selector} --max-depth 3");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{BodyLimit, attribute_start, cut, excerpt, excerpt_around, excerpt_lines};
    use sinter_core::Span;

    fn fixture(body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), body).unwrap();
        dir
    }

    #[test]
    fn excerpt_caps_lines_and_slices_the_span() {
        let dir = fixture("fn a() {\n1\n2\n3\n}\n");
        let got = excerpt_lines(dir.path(), "a.rs", 0, 15, 2)
            .map(|e| e.text)
            .unwrap();
        assert_eq!(got, "fn a() {\n1");
    }

    #[test]
    fn excerpt_lines_reports_the_cut() {
        let dir = fixture("fn a() {\n1\n2\n3\n}\n");
        let cut = excerpt_lines(dir.path(), "a.rs", 0, 17, 2).unwrap();
        assert_eq!((cut.total_lines, cut.truncated), (5, true));
        let whole = excerpt_lines(dir.path(), "a.rs", 0, 17, 5).unwrap();
        assert_eq!((whole.total_lines, whole.truncated), (5, false));
        assert_eq!(whole.text, "fn a() {\n1\n2\n3\n}");
        // `0` is the whole span, not an empty one.
        let all = excerpt_lines(dir.path(), "a.rs", 0, 17, 0).unwrap();
        assert_eq!(
            (all.text.as_str(), all.truncated),
            (whole.text.as_str(), false)
        );
    }

    #[test]
    fn default_rule_prints_short_spans_whole_and_long_ones_to_the_budget() {
        let short: Vec<&str> = (0..60).map(|_| "line").collect();
        assert_eq!(
            cut(&short, BodyLimit::Budget(Some(10))),
            (short.join("\n"), false)
        );
        let long: Vec<&str> = (0..61).map(|_| "0123456789").collect();
        let (text, truncated) = cut(&long, BodyLimit::Budget(Some(33)));
        assert_eq!((text.lines().count(), truncated), (3, true));
        assert!(!cut(&long, BodyLimit::Budget(None)).1);
        // A budget smaller than one line still shows that line.
        assert_eq!(cut(&long, BodyLimit::Budget(Some(1))).0.lines().count(), 1);
    }

    #[test]
    fn excerpt_includes_preceding_attributes() {
        let src = "use x;\n#[derive(Debug)]\n#[repr(C)]\nstruct S;\n";
        assert_eq!(attribute_start(src, src.find("struct").unwrap()), 7);
        let dir = fixture(src);
        let start = src.find("struct").unwrap() as u64;
        let got = excerpt(
            dir.path(),
            "a.rs",
            start,
            src.len() as u64,
            BodyLimit::Lines(0),
        )
        .unwrap();
        assert_eq!(got.text, "#[derive(Debug)]\n#[repr(C)]\nstruct S;");
        assert_eq!(got.first_line, Some(2));
    }

    #[test]
    fn excerpt_around_centres_on_the_line_inside_the_span() {
        let src = "x\nfn a() {\n1\n2\n3\n4\n5\n}\ny\n";
        let dir = fixture(src);
        let span = Span {
            start: 2,
            end: src.len() as u64 - 3,
        };
        let got = excerpt_around(dir.path(), "a.rs", span, 5, 1).unwrap();
        assert_eq!(got.text, "2\n3\n4");
        assert_eq!((got.first_line, got.target_line), (Some(4), Some(5)));
        assert!(got.truncated);
        // The window never leaves the span.
        let got = excerpt_around(dir.path(), "a.rs", span, 1, 50).unwrap();
        assert_eq!(got.text.lines().next(), Some("fn a() {"));
        assert!(!got.truncated);
    }

    #[test]
    fn excerpt_clamps_out_of_range_spans() {
        let dir = fixture("fn a() {}\n");
        // A mid-line start snaps to the start of that line.
        assert_eq!(
            excerpt_lines(dir.path(), "a.rs", 3, 9_999, 10)
                .map(|e| e.text)
                .unwrap(),
            "fn a() {}"
        );
        // Reversed span clamps instead of panicking.
        assert_eq!(
            excerpt_lines(dir.path(), "a.rs", 8, 2, 10)
                .map(|e| e.text)
                .unwrap(),
            "fn a() {"
        );
    }

    #[test]
    fn excerpt_degrades_on_missing_file_and_char_boundaries() {
        let dir = fixture("// \u{e9}\n");
        assert!(
            excerpt_lines(dir.path(), "missing.rs", 0, 4, 10)
                .map(|e| e.text)
                .is_none()
        );
        // Byte 4 lands inside the two-byte \u{e9}.
        assert!(
            excerpt_lines(dir.path(), "a.rs", 0, 4, 10)
                .map(|e| e.text)
                .is_none()
        );
    }
}
