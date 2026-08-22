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
    json: bool,
) -> Result<bool> {
    let store = open_store(repo)?;
    let refs = store.unresolved_details(file, name)?;
    let total = refs.len();
    let repo = crate::pipeline::discover_root(repo);
    let classifier = Classifier::new(&repo, &store, &refs)?;
    if json {
        crate::agent_protocol::write_json(&to_json(&repo, &classifier, &refs, limit))?;
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
    for (u, category) in ordered(&classifier, &refs).into_iter().take(limit) {
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
    if total > limit {
        println!(
            "{} more below cutoff · `sinter unresolved --limit {}` to widen",
            total - limit,
            total,
        );
    }
    Ok(total > 0)
}
