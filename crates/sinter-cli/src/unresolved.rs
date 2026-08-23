use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use crate::coverage::{Classifier, category_counts};
use crate::lookup::open_store;
use crate::render::line_of;

/// JSON shape shared by `sinter unresolved --json` and the MCP `unresolved`
/// tool: `total`, `by_category`, `actionable`, `unresolved` (capped at
/// `limit`, actionable categories first), `truncated` when capped.
pub fn to_json(
    repo: &Path,
    classifier: &Classifier,
    refs: &[sinter_core::UnresolvedReference],
    limit: usize,
) -> serde_json::Value {
    let total = refs.len();
    let by_category = category_counts(classifier, refs);
    let actionable = by_category
        .iter()
        .filter(|(category, _)| category_is_actionable(category))
        .map(|(_, count)| count)
        .sum::<usize>();
    let entries: Vec<serde_json::Value> = ordered(classifier, refs)
        .into_iter()
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
        "unresolved": entries,
    });
    if total > limit {
        out["truncated"] = serde_json::json!(total - limit);
    }
    out
}

/// Rows worth a reader's time by default: the actionable categories plus
/// refs a compiler index would settle. External names and unsupported
/// syntax are counted, never listed, unless `--all`.
fn shown_by_default(category: crate::coverage::UnresolvedCategory) -> bool {
    category.is_actionable()
        || category == crate::coverage::UnresolvedCategory::MissingCompilerIndex
}

/// `to_json` over the default rows only; `total` and `by_category` still
/// describe every reference so the counts stay honest.
pub fn to_json_default(
    repo: &Path,
    classifier: &Classifier,
    refs: &[sinter_core::UnresolvedReference],
    limit: usize,
) -> serde_json::Value {
    let shown: Vec<_> = refs
        .iter()
        .filter(|u| shown_by_default(classifier.classify(u)))
        .cloned()
        .collect();
    let mut out = to_json(repo, classifier, &shown, limit);
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
            to_json(&repo, &classifier, &refs, limit)
        } else {
            to_json_default(&repo, &classifier, &refs, limit)
        };
        crate::agent_protocol::write_json(&out)?;
        return Ok(total > 0);
    }
    let by_category = category_counts(&classifier, &refs);
    let actionable = by_category
        .iter()
        .filter(|(category, _)| category_is_actionable(category))
        .map(|(_, count)| count)
        .sum::<usize>();
    println!("{total} unresolved reference(s), {actionable} actionable");
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
    for (u, category) in rows.into_iter().take(limit) {
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
    if shown > limit {
        println!(
            "{} more below cutoff · `sinter unresolved --limit {}` to widen",
            shown - limit,
            shown,
        );
    }
    if !all && total > shown {
        println!(
            "{} external / unsupported-syntax row(s) hidden · `sinter unresolved --all` to list",
            total - shown
        );
    }
    Ok(total > 0)
}
