use std::path::Path;

use anyhow::Result;
use sinter_resolve::qualified_of;

use crate::lookup::open_store;
use crate::render::line_of;

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
    if json {
        let entries: Vec<serde_json::Value> = refs
            .iter()
            .take(limit)
            .map(|u| {
                let r = &u.reference;
                serde_json::json!({
                    "name": r.name,
                    "path": r.path,
                    "relation": r.relation.as_str(),
                    "file": r.file,
                    "line": line_of(&repo, &r.file, r.span.start),
                    "enclosing": r.enclosing.as_ref().map(|id| qualified_of(id.as_str())),
                    "reason": u.reason.as_str(),
                })
            })
            .collect();
        let mut out = serde_json::json!({
            "total": total,
            "unresolved": entries,
        });
        if total > limit {
            out["truncated"] = serde_json::json!(total - limit);
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(total > 0);
    }
    println!("{total} unresolved reference(s)");
    for u in refs.iter().take(limit) {
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
            "  {}  {}  {}  in {}  [{}]",
            written,
            r.relation.as_str(),
            location,
            r.enclosing
                .as_ref()
                .map(|id| qualified_of(id.as_str()).to_string())
                .unwrap_or_else(|| "<file scope>".to_string()),
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
