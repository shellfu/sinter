use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use crate::coverage::{Classifier, category_counts};
use crate::lookup::open_store;
use crate::render::line_of;

/// Rows per page, whatever `--limit` asks: a reader wants the shape of
/// the gaps, not the gaps; `next_cursor` continues.
pub const MAX_ROWS: usize = 50;

/// JSON shape shared by `sinter unresolved --json` and the MCP `unresolved`
/// tool: `total`, `by_category`, `actionable` (user gaps), `resolver_gap`,
/// `unresolved` (one page from `cursor`, capped at `MAX_ROWS`, actionable
/// categories first), `next_cursor` when more remain.
pub fn to_json(
    repo: &Path,
    classifier: &Classifier,
    refs: &[sinter_core::UnresolvedReference],
    limit: usize,
) -> serde_json::Value {
    to_json_page(repo, classifier, refs, 0, limit)
}

fn to_json_page(
    repo: &Path,
    classifier: &Classifier,
    refs: &[sinter_core::UnresolvedReference],
    cursor: usize,
    limit: usize,
) -> serde_json::Value {
    let total = refs.len();
    let by_category = category_counts(classifier, refs);
    let actionable = user_gaps(&by_category);
    let resolver_gap = by_category
        .get(crate::coverage::UnresolvedCategory::ResolverGap.as_str())
        .copied()
        .unwrap_or(0);
    let limit = limit.min(MAX_ROWS);
    let entries: Vec<serde_json::Value> = ordered(classifier, refs)
        .into_iter()
        .skip(cursor)
        .take(limit)
        .map(|(u, category)| {
            let r = &u.reference;
            serde_json::json!({
                "name": r.name,
                "path": r.path,
                "relation": r.relation.as_str(),
                "file": r.file,
                "line": line_of(repo, &r.file, r.span.start),
                "enclosing": r.enclosing.as_ref().map(|id| qualified_of(id.as_str())),
                "reason": u.reason.as_str(),
                "category": category.as_str(),
            })
        })
        .collect();
    let mut out = serde_json::json!({
        "total": total,
        "by_category": by_category,
        "actionable": actionable,
        "resolver_gap": resolver_gap,
        "unresolved": entries,
    });
    let end = cursor + limit;
    if total > end {
        out["truncated"] = serde_json::json!(total - end);
        out["next_cursor"] = serde_json::json!(end);
    }
    out
}

/// Rows worth a reader's time by default: user gaps only. External
/// names, resolver gaps, unsupported syntax, and refs waiting on a
/// compiler index are counted, never listed, unless `--all`.
fn shown_by_default(category: crate::coverage::UnresolvedCategory) -> bool {
    category.is_actionable()
}

/// `to_json` over the default rows only; `total` and `by_category` still
/// describe every reference so the counts stay honest.
pub fn to_json_default(
    repo: &Path,
    classifier: &Classifier,
    refs: &[sinter_core::UnresolvedReference],
    cursor: usize,
    limit: usize,
) -> serde_json::Value {
    let shown: Vec<_> = refs
        .iter()
        .filter(|u| shown_by_default(classifier.classify(u)))
        .cloned()
        .collect();
    let mut out = to_json_page(repo, classifier, &shown, cursor, limit);
    out["total"] = serde_json::json!(refs.len());
    out["by_category"] = serde_json::json!(category_counts(classifier, refs));
    out["shown"] = serde_json::json!("default");
    out
}

fn category_is_actionable(category: &str) -> bool {
    matches!(
        category,
        "missing_receiver_type" | "ambiguous_internal_target" | "actionable_anchored_miss"
    )
}

/// Count of user gaps: unresolved references a maintainer of this repo
/// can act on. Resolver gaps are sinter's to fix and never counted here.
fn user_gaps(by_category: &std::collections::BTreeMap<&'static str, usize>) -> usize {
    by_category
        .iter()
        .filter(|(category, _)| category_is_actionable(category))
        .map(|(_, count)| count)
        .sum()
}

/// Records with their category, actionable ones first, original order
/// within each group — the raw list never loses a row.
fn ordered<'a>(
    classifier: &Classifier,
    refs: &'a [sinter_core::UnresolvedReference],
) -> Vec<(
    &'a sinter_core::UnresolvedReference,
    crate::coverage::UnresolvedCategory,
)> {
    let mut rows: Vec<_> = refs.iter().map(|u| (u, classifier.classify(u))).collect();
    rows.sort_by_key(|(_, category)| !category.is_actionable());
    rows
}

/// `sinter unresolved`: list the references extraction saw but resolution
/// never bound — the graph's honest gaps, first-class (R2). Ok(true) when
/// any matched (grep-style exit codes).
pub fn run(
    repo: &Path,
    file: Option<&str>,
    name: Option<&str>,
    cursor: usize,
    limit: usize,
    all: bool,
    json: bool,
) -> Result<bool> {
    // Asking for one name is asking to see its rows, whatever their category.
    let all = all || name.is_some();
    let store = open_store(repo)?;
    let refs = store.unresolved_details(file, name)?;
    let total = refs.len();
    let repo = crate::pipeline::discover_root(repo);
    let classifier = Classifier::new(&repo, &store, &refs)?;
    if json {
        let out = if all {
            to_json_page(&repo, &classifier, &refs, cursor, limit)
        } else {
            to_json_default(&repo, &classifier, &refs, cursor, limit)
        };
        crate::agent_protocol::write_json(&out)?;
        return Ok(total > 0);
    }
    let by_category = category_counts(&classifier, &refs);
    let actionable = user_gaps(&by_category);
    let resolver = by_category
        .get(crate::coverage::UnresolvedCategory::ResolverGap.as_str())
        .copied()
        .unwrap_or(0);
    println!(
        "{total} unresolved reference(s), {actionable} user gap(s), {resolver} resolver gap(s)"
    );
    for (category, count) in &by_category {
        println!(
            "  {count:>6}  {category}{}",
            if category_is_actionable(category) {
                "  *"
            } else {
                ""
            }
        );
    }
    println!();
    let rows: Vec<_> = ordered(&classifier, &refs)
        .into_iter()
        .filter(|(_, category)| all || shown_by_default(*category))
        .collect();
    let shown = rows.len();
    let limit = limit.min(MAX_ROWS);
    for (u, category) in rows.into_iter().skip(cursor).take(limit) {
        let r = &u.reference;
        let location =
            crate::render::location(&repo, &r.file, line_of(&repo, &r.file, r.span.start));
        // Written path text can span lines (chained calls); keep one row
        // per reference.
        let written = r
            .path
            .as_deref()
            .unwrap_or(&r.name)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "  {}  {}  {}  in {}  [{} · {}]",
            written,
            r.relation.as_str(),
            location,
            r.enclosing
                .as_ref()
                .map(|id| qualified_of(id.as_str()).to_string())
                .unwrap_or_else(|| "<file scope>".to_string()),
            category.as_str(),
            u.reason.as_str(),
        );
    }
    let end = cursor + limit;
    if shown > end {
        println!(
            "{} more · `sinter unresolved --cursor {end}` for the next page",
            shown - end,
        );
    }
    if !all && total > shown {
        println!(
            "{} non-user-gap row(s) hidden · `sinter unresolved --all` to list",
            total - shown
        );
    }
    Ok(total > 0)
}
