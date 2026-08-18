//! `sinter init`: onboard a repository in one command. Pure composition of
//! existing verbs — build, git hooks, agent integration, MCP registration —
//! finished by a doctor pass so the outcome is verified, not assumed.

use std::path::Path;

use anyhow::Result;

use crate::{doctor, hooks, install, pipeline};

/// `sinter init --workspace`: write a starter manifest. Never clobbers —
/// members and runtime links are facts only the operator knows.
pub fn run_workspace(path: &Path, name: &str, members: &[String]) -> Result<bool> {
    if path.exists() {
        anyhow::bail!("{} already exists — refusing to overwrite", path.display());
    }
    // `-m [name=]path` entries become real [members] rows; every path is
    // validated up front so the manifest is runnable the moment it lands.
    let mut rows = String::new();
    let mut width = 0usize;
    let mut parsed: Vec<(String, String)> = Vec::new();
    for member in members {
        let (mname, mpath) = match member.split_once('=') {
            Some((n, p)) => (n.trim().to_string(), p.trim().to_string()),
            None => {
                let p = member.trim();
                let n = Path::new(p)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .ok_or_else(|| anyhow::anyhow!("cannot derive a member name from {p:?}"))?;
                (n, p.to_string())
            }
        };
        anyhow::ensure!(!mname.is_empty(), "empty member name in {member:?}");
        let expanded = if let Some(rest) = mpath.strip_prefix("~/") {
            std::env::var_os("HOME")
                .map(|h| Path::new(&h).join(rest))
                .ok_or_else(|| anyhow::anyhow!("cannot expand ~ in {mpath:?}"))?
        } else {
            Path::new(&mpath).to_path_buf()
        };
        anyhow::ensure!(
            expanded.is_dir(),
            "member {mname}: {mpath} is not a directory"
        );
        anyhow::ensure!(
            parsed.iter().all(|(n, _)| n != &mname),
            "duplicate member name {mname:?}"
        );
        width = width.max(mname.len());
        parsed.push((mname, mpath));
    }
    for (n, p) in &parsed {
        rows.push_str(&format!("{n:width$} = \"{p}\"\n"));
    }
    let members_block = if rows.is_empty() {
        "# auth    = \"~/src/auth\"\n# billing = \"~/src/billing\"".to_string()
    } else {
        rows.trim_end().to_string()
    };
    let template = format!(
        r#"# sinter workspace manifest — see `sinter workspace --help`
[workspace]
name = "{name}"

# Repos forming the system. Key = member name (used as the symbol
# prefix, e.g. `auth:Login`), value = repo path (~ expands).
[members]
{members_block}

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
    if members.is_empty() {
        println!(
            "next: fill in [members], then run `sinter workspace {}`",
            path.display()
        );
    } else {
        println!("next: sinter workspace {}", path.display());
    }
    Ok(true)
}

pub fn run(repo: &Path, cursor: bool, scip: Option<bool>, global: bool) -> Result<bool> {
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
    // Enforcement is per-repo by default (committable, teammates inherit
    // it); --global additionally wires ~/.claude so every repo on this
    // machine gets the hooks.
    install::run_targets(&targets, None, true, &repo, false, false)?;
    if global {
        install::enforce(None, false)?;
    }

    // Agent integration writes indexable files into the repo (AGENTS.md is
    // markdown); refresh incrementally so doctor verifies a current graph.
    pipeline::build(&repo, None)?;

    println!("\n== doctor ==");
    doctor::run(&repo, false)
}
