//! `sinter init`: onboard a repository in one command. Pure composition of
//! existing verbs — build, git hooks, agent integration, MCP registration —
//! finished by a doctor pass so the outcome is verified, not assumed.

use std::path::Path;

use anyhow::Result;

use crate::{doctor, hooks, install, pipeline};

pub fn run(repo: &Path, cursor: bool) -> Result<bool> {
    let repo = repo.canonicalize()?;

    println!("== build ==");
    let report = pipeline::build(&repo, None)?;
    pipeline::print_report(&report);

    println!("\n== git hooks ==");
    if repo.join(".git").exists() {
        hooks::install(&repo)?;
    } else {
        println!("not a git repository — skipping hooks");
    }

    println!("\n== agent integration ==");
    // claude is global and idempotent — including it makes init complete
    // on a fresh machine instead of ending with a doctor FIX.
    let mut targets = vec!["claude".to_string(), "agents".to_string()];
    if cursor {
        targets.push("cursor".to_string());
    }
    install::run_targets(&targets, None, true, &repo)?;

    println!("\n== doctor ==");
    doctor::run(&repo)
}
