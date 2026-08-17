//! `sinter init`: onboard a repository in one command. Pure composition of
//! existing verbs — build, git hooks, agent integration, MCP registration —
//! finished by a doctor pass so the outcome is verified, not assumed.

use std::path::Path;

use anyhow::Result;

use crate::{doctor, hooks, install, pipeline};

/// `sinter init --workspace`: write a starter manifest. Never clobbers —
/// members and runtime links are facts only the operator knows.
pub fn run_workspace(path: &Path, name: &str) -> Result<bool> {
    if path.exists() {
        anyhow::bail!("{} already exists — refusing to overwrite", path.display());
    }
    let template = format!(
        r#"# sinter workspace manifest — see `sinter workspace --help`
[workspace]
name = "{name}"

# Repos forming the system. Key = member name (used as the symbol
# prefix, e.g. `auth:Login`), value = repo path (~ expands).
[members]
# auth    = "~/src/auth"
# billing = "~/src/billing"

# Optional: runtime coupling no parser can see (queue topics, RPC by
# config). Each entry becomes an edge with `declared` evidence; a
# missing or ambiguous symbol fails `sinter workspace` loudly.
# [[links]]
# from_member = "billing"
# from_symbol = "ConsumeSettled"
# to_member   = "auth"
# to_symbol   = "PublishSettled"
# via         = "topic payments.settled"
"#
    );
    std::fs::write(path, template)?;
    println!("wrote {}", path.display());
    println!(
        "next: fill in [members], then run `sinter workspace {}`",
        path.display()
    );
    Ok(true)
}

pub fn run(repo: &Path, cursor: bool, scip: Option<bool>) -> Result<bool> {
    let repo = repo.canonicalize()?;
    // Cursor is auto-configured when the repo already carries Cursor state;
    // --cursor still forces it on a fresh checkout.
    let cursor = cursor || repo.join(".cursor").exists();

    println!("== build ==");
    let report = pipeline::build(&repo, None)?;
    pipeline::print_report(&report);

    // Compiler evidence needs consent: indexers are language toolchains
    // that can execute repository build scripts (build.rs, procmacros), so
    // init must never launch them on an untrusted repo without being told.
    // TTY: ask once. Non-interactive: skip unless --scip was passed.
    println!("\n== scip (compiler evidence) ==");
    let consent = scip.unwrap_or_else(|| {
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            print!(
                "run compiler indexers? They build the project and can execute \
                 repository build scripts [y/N] "
            );
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            matches!(answer.trim(), "y" | "Y" | "yes")
        } else {
            println!("skipped: non-interactive and no --scip (indexers execute build scripts)");
            false
        }
    });
    if consent {
        if let Err(e) = crate::scip::run(&repo) {
            println!("skipped: {e:#}");
            println!("(optional — rerun `sinter scip` once an indexer is installed)");
        }
    } else {
        println!("(optional — run `sinter scip` when you trust this repository)");
    }

    println!("\n== git hooks ==");
    if repo.join(".git").exists() {
        hooks::install(&repo)?;
    } else {
        println!("not a git repository — skipping hooks");
    }

    println!("\n== agent integration ==");
    // claude is global and idempotent — including it makes init complete
    // on a fresh machine instead of ending with a doctor FIX.
    let mut targets = vec![
        "claude".to_string(),
        "agents".to_string(),
        "enforce".to_string(),
    ];
    if cursor {
        targets.push("cursor".to_string());
    }
    install::run_targets(&targets, None, true, &repo)?;

    println!("\n== doctor ==");
    doctor::run(&repo)
}
