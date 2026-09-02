//! Repository onboarding operations. `ensure` creates only derived graph
//! state; `init` is the explicit full installation that also writes hooks and
//! agent integrations.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{doctor, hooks, install, pipeline};

/// Resolve an onboarding target independently of every ancestor graph. The
/// path printed in the plan is the path `init` and `ensure` establish.
fn onboarding_root(repo: &Path) -> Result<PathBuf> {
    Ok(repo.canonicalize()?)
}

/// Establish the selected root before the first build so later read commands
/// can discover this repository's graph without consulting any ancestor.
fn establish_graph_root(repo: &Path) -> Result<()> {
    std::fs::create_dir_all(repo.join(".sinter"))?;
    gitignore_derived_state(repo)?;
    Ok(())
}

/// `.sinter/` is derived state and must not show up as untracked. Appends
/// one line to the root `.gitignore` (creating it) when the directory is
/// inside a git worktree and no existing line already covers it. Idempotent.
fn gitignore_derived_state(repo: &Path) -> Result<()> {
    if !repo.ancestors().any(|dir| dir.join(".git").exists()) {
        return Ok(());
    }
    let path = repo.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let covered = existing.lines().any(|line| {
        matches!(
            line.trim().trim_end_matches('/'),
            ".sinter" | "/.sinter" | "**/.sinter"
        )
    });
    if covered {
        return Ok(());
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(".sinter/\n");
    std::fs::write(&path, text)?;
    println!("added .sinter/ to {}", path.display());
    Ok(())
}

/// Make the repository graph available without changing repository policy or
/// client configuration. This is the safe setup operation for agents: the
/// only persistent writes belong under `.sinter/`.
pub fn ensure(repo: &Path) -> Result<()> {
    let repo = onboarding_root(repo)?;
    establish_graph_root(&repo)?;
    crate::build::run(&repo)
}

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

/// Everything `init` is about to write, grouped by the authority it
/// needs. Printed before anything happens: the repo half is committable
/// and reversible with `sinter uninit`; the machine half is opt-in.
fn plan(repo: &Path, cursor: bool, global: bool, scip: bool) {
    println!("sinter init — {}", repo.display());
    println!("\n  this repo");
    println!("    .sinter/                     code graph (derived state, gitignored)");
    if repo.join(".git").exists() {
        println!("    .git/hooks/post-*            refresh the graph after commit/checkout/merge");
    } else {
        println!("    (no .git — git hooks skipped)");
    }
    println!("    AGENTS.md                    managed sinter block");
    println!("    .mcp.json, .cursor/mcp.json  MCP server registration");
    println!("    .codex/config.toml           managed MCP block");
    println!("    .claude/                     strict sinter-first hooks, this repo only");
    println!("                                 first broad search redirects; retry is allowed");
    if cursor {
        println!("    .cursor/rules/sinter.mdc     Cursor rule");
    }

    if global {
        println!("\n  your machine (--global)");
        println!("    ~/.claude/skills/sinter/     skill card, every repo on this machine");
        println!("    ~/.claude/settings.json      enforcement hooks, every repo");
    } else {
        println!("\n  your machine");
        println!("    untouched — pass --global to install the skill card and machine-wide hooks");
    }

    println!("\n  compiler indexers (SCIP)");
    if scip {
        println!("    will run — they build the project and can execute repository build scripts");
    } else {
        println!("    not run — pass --scip to enable (they execute repository build scripts)");
    }
}

/// Ask once, default yes. Non-interactive callers never reach here.
fn confirm() -> bool {
    use std::io::Write;
    print!("\nproceed? [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim(), "" | "y" | "Y" | "yes")
}

pub fn run(
    repo: &Path,
    cursor: bool,
    scip: Option<bool>,
    global: bool,
    assume_yes: bool,
) -> Result<bool> {
    use std::io::IsTerminal;
    let repo = onboarding_root(repo)?;
    // Cursor is auto-configured when the repo already carries Cursor state;
    // --cursor still forces it on a fresh checkout.
    let cursor = cursor || repo.join(".cursor").exists();
    let interactive = std::io::stdin().is_terminal() && !assume_yes;

    // Compiler evidence needs consent: indexers are language toolchains
    // that can execute repository build scripts (build.rs, procmacros), so
    // init must never launch them on an untrusted repo without being told.
    // Asked before the plan so the plan states the answer; non-interactive
    // runs skip them unless --scip was passed.
    let scip = match scip {
        Some(explicit) => explicit,
        None if interactive => {
            use std::io::Write;
            print!(
                "run compiler indexers? They build the project and can execute \
                 repository build scripts [y/N] "
            );
            let _ = std::io::stdout().flush();
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            matches!(answer.trim(), "y" | "Y" | "yes")
        }
        None => false,
    };

    plan(&repo, cursor, global, scip);
    if interactive && !confirm() {
        println!("aborted — nothing written");
        return Ok(false);
    }
    println!();

    establish_graph_root(&repo)?;
    let progress = crate::progress::Progress::stderr();
    let report = pipeline::build_with(&repo, None, &mut |phase| {
        crate::progress::render(&progress, phase)
    })?;
    pipeline::print_summary(&repo, &report);

    if scip {
        println!("\n== compiler evidence ==");
        if let Err(e) = crate::scip::run(&repo) {
            println!("skipped: {e:#}");
            println!("(optional — rerun `sinter scip` once an indexer is installed)");
        }
    }

    println!("\n== git hooks ==");
    if repo.join(".git").exists() {
        hooks::install(&repo)?;
    } else {
        println!("not a git repository — skipping hooks");
    }

    println!("\n== agent integration ==");
    // Project scope by default: everything here is committable, so
    // teammates and every checkout inherit it.
    install_project_integration(&repo, cursor)?;
    // --global reaches past the repo: the skill card and the enforcement
    // hooks that every repo on this machine then inherits.
    // Without it nothing outside the repo is touched, even a stale
    // machine-wide card: doctor names that and `sinter install` fixes it.
    if global {
        install::run(None)?;
        install::enforce(None, false)?;
    }

    // Agent integration writes indexable files into the repo (AGENTS.md is
    // markdown); refresh incrementally so doctor verifies a current graph.
    // Silent: the phases above already reported the build that matters.
    pipeline::build(&repo, None)?;

    println!("\ntip: `sinter completion <shell>` prints shell completions (bash/zsh/fish)");
    if !global {
        println!(
            "tip: `sinter install` puts the skill card on this machine for every repo (`sinter init --global` also adds machine-wide hooks)"
        );
    }

    println!("\n== doctor ==");
    doctor::run(&repo, false, false, false)
}

/// Install the committable repository integration. Full onboarding makes the
/// first broad search of each Claude session take the graph route; the hook's
/// existing retry escape hatch keeps fallback search available.
fn install_project_integration(repo: &Path, cursor: bool) -> Result<()> {
    let mut targets = vec!["agents".to_string(), "enforce".to_string()];
    if cursor {
        targets.push("cursor".to_string());
    }
    install::run_targets(&targets, None, true, repo, false, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_writes_only_derived_graph_state() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::write(repo.join("a.rs"), "pub fn ready() {}\n").unwrap();
        std::fs::create_dir_all(repo.join(".git/hooks")).unwrap();

        ensure(repo).unwrap();

        assert!(repo.join(".sinter/graph.redb").exists());
        assert_eq!(
            std::fs::read_to_string(repo.join(".gitignore")).unwrap(),
            ".sinter/\n"
        );
        for absent in [
            "AGENTS.md",
            "CLAUDE.md",
            ".mcp.json",
            ".cursor",
            ".codex",
            ".claude",
            ".git/hooks/post-commit",
            ".git/hooks/post-checkout",
            ".git/hooks/post-merge",
        ] {
            assert!(
                !repo.join(absent).exists(),
                "ensure must not create {absent}"
            );
        }
    }

    #[test]
    fn ensure_pins_the_requested_root_beneath_an_ancestor_graph() {
        let parent = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(parent.path().join(".sinter")).unwrap();
        let repo = parent.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("a.rs"), "pub fn local() {}\n").unwrap();

        ensure(&repo).unwrap();

        assert!(repo.join(".sinter/graph.redb").exists());
        assert!(
            !parent.path().join(".sinter/graph.redb").exists(),
            "ensure wrote the ancestor graph instead of the requested repository"
        );
    }

    #[test]
    fn ensure_uses_the_exact_requested_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let nested = repo.join("src/deep");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(repo.join("a.rs"), "pub fn root() {}\n").unwrap();
        std::fs::write(nested.join("local.rs"), "pub fn local() {}\n").unwrap();

        ensure(&nested).unwrap();

        assert!(nested.join(".sinter/graph.redb").exists());
        assert!(!repo.join(".sinter").exists());
    }

    #[test]
    fn gitignore_entry_is_appended_once_and_only_inside_git_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        gitignore_derived_state(repo).unwrap();
        assert!(!repo.join(".gitignore").exists(), "no .git, no .gitignore");

        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".gitignore"), "target").unwrap();
        gitignore_derived_state(repo).unwrap();
        gitignore_derived_state(repo).unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join(".gitignore")).unwrap(),
            "target\n.sinter/\n"
        );

        std::fs::write(repo.join(".gitignore"), "/.sinter\n").unwrap();
        gitignore_derived_state(repo).unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join(".gitignore")).unwrap(),
            "/.sinter\n"
        );
    }

    #[test]
    fn full_onboarding_installs_strict_repo_local_enforcement() {
        let repo = tempfile::tempdir().unwrap();

        install_project_integration(repo.path(), false).unwrap();

        let settings = std::fs::read_to_string(repo.path().join(".claude/settings.json")).unwrap();
        assert!(settings.contains(" grep-strict"));
        assert!(settings.contains(" greptool-strict"));
    }
}
