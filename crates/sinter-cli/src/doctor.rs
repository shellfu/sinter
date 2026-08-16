//! `sinter doctor`: diagnose the installation and (optionally) a repo's
//! graph. Every finding names its fix; exit 1 when anything needs action.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use sinter_extract::LANGUAGES;
use sinter_store::Store;

use crate::{install, pipeline};

struct Report {
    problems: usize,
}

impl Report {
    fn ok(&mut self, msg: &str) {
        println!("  ok    {msg}");
    }
    fn warn(&mut self, msg: &str, fix: &str) {
        self.problems += 1;
        println!("  FIX   {msg}\n        -> {fix}");
    }
}

pub fn run(repo: &Path) -> Result<bool> {
    let mut r = Report { problems: 0 };

    println!("sinter {}", env!("CARGO_PKG_VERSION"));
    let names: Vec<&str> = LANGUAGES.iter().map(|l| l.name).collect();
    r.ok(&format!("languages: {}", names.join(", ")));

    // Skill card: installed and current with this binary.
    match install::default_dir() {
        Some(dir) => match std::fs::read_to_string(dir.join("SKILL.md")) {
            Ok(card) if card == install::SKILL => r.ok("skill card installed and current"),
            Ok(_) => r.warn(
                "skill card is stale (differs from this binary's embedded copy)",
                "run `sinter install`",
            ),
            Err(_) => r.warn("skill card not installed", "run `sinter install`"),
        },
        None => r.warn(
            "cannot locate home directory for skill card",
            "pass --dir to `sinter install`",
        ),
    }

    // Repo checks.
    let repo = repo.canonicalize()?;
    let db = pipeline::db_path(&repo);
    if !db.exists() {
        r.warn(
            &format!("no graph at {}", db.display()),
            "run `sinter build`",
        );
        println!("{} problem(s)", r.problems);
        return Ok(r.problems == 0);
    }
    match Store::schema_of(&db)? {
        Some(v) if v == Store::CURRENT_SCHEMA => r.ok(&format!("graph schema v{v} (current)")),
        Some(v) => r.warn(
            &format!(
                "graph schema v{v}, binary writes v{}",
                Store::CURRENT_SCHEMA
            ),
            "run `sinter build` (rebuilds automatically)",
        ),
        None => r.warn("graph has no schema stamp", "run `sinter build`"),
    }

    let store = Store::open(&db)?;
    let stored: HashMap<String, String> = store.file_hashes()?.into_iter().collect();
    let current = pipeline::scan_hashes(&repo, &stored)?;
    let stale = current
        .iter()
        .filter(|(f, h)| stored.get(f) != Some(h))
        .count();
    let removed = {
        let live: std::collections::HashSet<&str> =
            current.iter().map(|(f, _)| f.as_str()).collect();
        stored.keys().filter(|f| !live.contains(f.as_str())).count()
    };
    if stale == 0 && removed == 0 {
        r.ok(&format!("graph fresh ({} files indexed)", stored.len()));
    } else {
        r.warn(
            &format!("graph stale: {stale} changed, {removed} removed files"),
            "run `sinter build`",
        );
    }

    r.ok(&format!(
        "{} nodes, {} edges, {} unresolved refs",
        store.node_count()?,
        store.edge_count()?,
        store.unresolved_count()?,
    ));

    if repo.join(".git").exists() {
        let hook = repo.join(".git/hooks/post-commit");
        let installed = std::fs::read_to_string(&hook).is_ok_and(|s| s.contains("sinter build"));
        if installed {
            r.ok("git hooks installed");
        } else {
            r.warn(
                "git hooks not installed (graph won't refresh on commit/checkout)",
                "run `sinter hooks install`",
            );
        }
    }
    if repo.join("index.scip").exists() {
        r.ok("SCIP index present (compiler-grade evidence tier active)");
    } else {
        r.ok("no SCIP index (optional; a compiler indexer would bind external/method refs)");
    }

    println!("{} problem(s)", r.problems);
    Ok(r.problems == 0)
}
