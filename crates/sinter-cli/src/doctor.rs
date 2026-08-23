//! `sinter doctor`: diagnose the installation and (optionally) a repo's
//! graph. Every finding names its fix. Findings in the `graph` section are
//! problems (exit 1); findings in the `integration` section are notes
//! (drifted cards/hooks/registrations) and never fail the exit code.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Result;
use sinter_extract::LANGUAGES;
use sinter_store::Store;

use crate::{install, pipeline};

struct Report {
    problems: usize,
    notes: usize,
    fix: bool,
    fixed: usize,
    /// Findings after `integration()` are notes, not problems.
    integration: bool,
}

impl Report {
    fn new(fix: bool) -> Self {
        Self {
            problems: 0,
            notes: 0,
            fix,
            fixed: 0,
            integration: false,
        }
    }
    fn ok(&mut self, msg: &str) {
        println!("  ok    {msg}");
    }
    fn section(&mut self, name: &str) {
        self.integration = name == "integration";
        println!("{name}");
    }
    fn warn(&mut self, msg: &str, fix: &str) {
        if self.integration {
            self.notes += 1;
            println!("  note  {msg}\n        -> {fix}");
        } else {
            self.problems += 1;
            println!("  FIX   {msg}\n        -> {fix}");
        }
    }
    /// A finding doctor can repair itself. Under `--fix` the action runs
    /// (falling back to a warning naming the failure); otherwise it
    /// warns with the manual command. Auto-fix only ever refreshes what
    /// is already installed or rebuilds derived state — it never makes a
    /// new opt-in decision on the user's behalf.
    fn fixable(&mut self, msg: &str, cmd: &str, action: impl FnOnce() -> Result<()>) {
        if !self.fix {
            self.warn(msg, cmd);
            return;
        }
        match action() {
            Ok(()) => {
                self.fixed += 1;
                println!("  FIXED {msg}");
            }
            Err(e) => self.warn(&format!("{msg} (auto-fix failed: {e:#})"), cmd),
        }
    }
    fn summary(&self) {
        if self.fix {
            println!(
                "{} fixed, {} graph problem(s), {} integration note(s) remaining",
                self.fixed, self.problems, self.notes
            );
        } else {
            println!(
                "{} graph problem(s), {} integration note(s)",
                self.problems, self.notes
            );
        }
    }
}

/// Workspace health: member freshness and boundary-link staleness.
pub fn run_workspace(manifest: &Path, fix: bool) -> Result<bool> {
    // The one fix for every workspace finding is the build itself, and it
    // is stat-gated (cheap when fresh) — so `--fix` runs it up front and
    // the diagnosis below reports the post-fix state.
    if fix {
        crate::workspace::run(manifest)?;
    }
    let ws = crate::workspace::load(manifest)?;
    let mut r = Report::new(fix);
    println!(
        "workspace `{}` ({} members)",
        ws.manifest.workspace.name,
        ws.members.len()
    );
    for (name, repo) in &ws.members {
        if pipeline::db_path(repo).exists() {
            r.ok(&format!(
                "member {name}: graph present ({})",
                repo.display()
            ));
        } else {
            r.warn(
                &format!("member {name}: no graph at {}", repo.display()),
                "run `sinter workspace <manifest>`",
            );
        }
    }
    match crate::workspace::stale_members(&ws) {
        Ok(stale) if stale.is_empty() => {
            let links = crate::workspace::LinkStore::open(&ws)?;
            r.ok(&format!("boundary links fresh ({} links)", links.count()?));
        }
        Ok(stale) => r.warn(
            &format!(
                "boundary links stale (changed members: {})",
                stale.join(", ")
            ),
            "run `sinter workspace <manifest>`",
        ),
        Err(_) => r.warn("no link store yet", "run `sinter workspace <manifest>`"),
    }
    r.summary();
    Ok(r.problems == 0)
}

pub fn run(repo: &Path, fix: bool) -> Result<bool> {
    let mut r = Report::new(fix);

    println!("sinter {}", env!("CARGO_PKG_VERSION"));
    r.section("graph");
    let names: Vec<&str> = LANGUAGES.iter().map(|l| l.name).collect();
    r.ok(&format!("languages: {}", names.join(", ")));

    // Repo checks. Subdirectory invocation resolves to the graph root,
    // matching every query command.
    let repo = pipeline::discover_root(repo);
    let repo = repo.canonicalize()?;

    graph_checks(&mut r, &repo)?;

    r.section("integration");
    // Release check: one HEAD request, TTY-only, 24h-cached, opt-out via
    // SINTER_NO_UPDATE_CHECK=1. Not auto-fixable — replacing the running
    // binary is the installer's job, not doctor's.
    {
        use std::io::IsTerminal;
        if std::io::stderr().is_terminal() {
            crate::update::refresh_cache();
        }
        match crate::update::cached_newer() {
            Some(latest) => r.warn(
                &format!(
                    "{latest} is available (running {})",
                    env!("CARGO_PKG_VERSION")
                ),
                "rerun the install one-liner (or your package manager)",
            ),
            None => r.ok("this is the latest known release"),
        }
    }

    // Skill card: installed and current with this binary.
    match install::default_dir() {
        Some(dir) => match std::fs::read_to_string(dir.join("SKILL.md")) {
            Ok(card) if card == install::SKILL => r.ok("skill card installed and current"),
            Ok(_) => r.fixable(
                "skill card is stale (differs from this binary's embedded copy)",
                "run `sinter install`",
                || install::run(None),
            ),
            // Not installed is a choice, not a defect: `sinter init` is
            // project-scoped by default and the machine-wide card is what
            // `--global` opts into. Reporting it as a problem is what used
            // to force init to write outside the repo unasked.
            Err(_) => r.ok("skill card not installed (optional — `sinter install` adds it)"),
        },
        None => r.warn(
            "cannot locate home directory for skill card",
            "pass --dir to `sinter install`",
        ),
    }

    // Enforcement hooks: script current with this binary and the three
    // settings entries present (per-prompt router, Bash + Grep nudges),
    // satisfied by either the repo's .claude or the global ~/.claude.
    // Only the variant THIS platform installs is demanded (sh on unix,
    // ps1 on Windows) — the other's absence is not a finding.
    let (hook_file, hook_body) = install::PLATFORM_HOOK;
    let enforced_at = |claude: &Path| {
        std::fs::read_to_string(claude.join("hooks").join(hook_file)).is_ok_and(|s| s == hook_body)
            && std::fs::read_to_string(claude.join("settings.json")).is_ok_and(|s| {
                // Commands end with their mode in both variants; the
                // closing JSON quote anchors "grep" against "greptool".
                // Strict installs use the -strict grep modes; both
                // variants are current.
                s.contains(hook_file)
                    && s.contains(" prompt\"")
                    && (s.contains(" grep\"") || s.contains(" grep-strict\""))
                    && (s.contains(" greptool\"") || s.contains(" greptool-strict\""))
            })
    };
    let global_claude =
        install::default_dir().and_then(|d| d.parent()?.parent().map(Path::to_path_buf));
    if enforced_at(&repo.join(".claude")) {
        r.ok("enforcement hooks installed and current (repo .claude)");
    } else if global_claude.as_deref().is_some_and(enforced_at) {
        r.ok("enforcement hooks installed and current (global ~/.claude)");
    } else {
        // Refresh-only: a scope counts for auto-fix when its script file
        // exists at all — first-time enforcement stays an opt-in.
        let repo_scope = repo.join(".claude/hooks").join(hook_file).exists();
        let global_scope = global_claude
            .as_ref()
            .is_some_and(|c| c.join("hooks").join(hook_file).exists());
        if repo_scope || global_scope {
            r.fixable(
                "enforcement hooks stale (agents may grep instead of querying)",
                "run `sinter install enforce` (or --global)",
                || {
                    // Preserve whichever strictness is installed per
                    // scope — a fix refreshes, it never changes modes.
                    let is_strict = |claude: &Path| {
                        std::fs::read_to_string(claude.join("settings.json"))
                            .is_ok_and(|s| s.contains(" grep-strict\""))
                    };
                    if repo_scope {
                        install::enforce(Some(&repo), is_strict(&repo.join(".claude")))?;
                    }
                    if global_scope {
                        let strict = global_claude.as_deref().is_some_and(is_strict);
                        install::enforce(None, strict)?;
                    }
                    Ok(())
                },
            );
        } else {
            r.warn(
                "enforcement hooks not installed (agents may grep instead of querying)",
                "run `sinter install enforce` (or --global)",
            );
        }
    }
    if repo.join(".git").exists() {
        let hook = repo.join(".git/hooks/post-commit");
        let installed = std::fs::read_to_string(&hook).is_ok_and(|s| s.contains("sinter build"));
        if installed {
            r.ok("git hooks installed");
        } else {
            r.fixable(
                "git hooks not installed (graph won't refresh on commit/checkout)",
                "run `sinter hooks install`",
                || crate::hooks::install(&repo),
            );
        }
    }
    for (label, path) in [
        ("cursor rule", repo.join(".cursor/rules/sinter.mdc")),
        ("AGENTS.md block", repo.join("AGENTS.md")),
    ] {
        match std::fs::read_to_string(&path) {
            Ok(content) if !content.contains("sinter") => {}
            Ok(content) if install::block_current(&content) => {
                r.ok(&format!("{label} installed and current"));
            }
            Ok(content) if content.contains("BEGIN sinter") || label == "cursor rule" => {
                let _ = content;
                r.fixable(
                    &format!("{label} is stale (differs from this binary's embedded card)"),
                    "rerun `sinter install cursor agents`",
                    || {
                        if label == "cursor rule" {
                            install::cursor(&repo).map(drop)
                        } else {
                            install::agents(&repo).map(drop)
                        }
                    },
                );
            }
            _ => {}
        }
    }
    let json_registered = |rel: &str| {
        std::fs::read_to_string(repo.join(rel))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .is_some_and(|v| v["mcpServers"]["sinter"].is_object())
    };
    // A project-portable registration uses a bare executable name. Validate
    // it through PATH; explicit absolute or relative paths are checked at
    // their configured locations.
    let json_command = |rel: &str| {
        std::fs::read_to_string(repo.join(rel))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| {
                v["mcpServers"]["sinter"]["command"]
                    .as_str()
                    .map(str::to_string)
            })
    };
    for rel in [".mcp.json", ".cursor/mcp.json"] {
        if let Some(cmd) = json_command(rel)
            && !mcp_command_resolves(&repo, &cmd, std::env::var_os("PATH").as_deref())
        {
            r.warn(
                &format!("{rel} MCP command `{cmd}` is not resolvable from PATH"),
                "put `sinter` on the MCP client's PATH, then rerun `sinter doctor`",
            );
        }
    }
    let codex_registered = std::fs::read_to_string(repo.join(".codex/config.toml"))
        .is_ok_and(|s| s.contains("[mcp_servers.sinter]"));
    let registered: Vec<&str> = [
        (".mcp.json (Claude)", json_registered(".mcp.json")),
        (
            ".cursor/mcp.json (Cursor)",
            json_registered(".cursor/mcp.json"),
        ),
        (".codex/config.toml (Codex)", codex_registered),
    ]
    .into_iter()
    .filter_map(|(name, ok)| ok.then_some(name))
    .collect();
    if registered.len() == 3 {
        r.ok("MCP server registered for Claude, Cursor, and Codex");
    } else if registered.is_empty() {
        r.ok("MCP not registered (optional; `sinter install --mcp` registers all clients)");
    } else {
        r.fixable(
            &format!("MCP registered for {} only", registered.join(", ")),
            "run `sinter install --mcp` to register every client",
            || install::mcp(&repo),
        );
    }
    // Registered is not working: handshake the server the way a client
    // would and confirm the expected tools come back.
    if !registered.is_empty() {
        match mcp_handshake(&repo) {
            Ok(tools)
                if ["ask", "affected", "path"]
                    .iter()
                    .all(|t| tools.contains(&t.to_string())) =>
            {
                r.ok(&format!("MCP handshake ok ({} tools served)", tools.len()));
            }
            Ok(tools) => r.warn(
                &format!(
                    "MCP handshake served unexpected tools: {}",
                    tools.join(", ")
                ),
                "reinstall sinter and rerun `sinter install --mcp`",
            ),
            Err(e) => r.warn(
                &format!("MCP registered but the server failed to answer: {e:#}"),
                "check `sinter` is on PATH for your MCP client; rerun `sinter install --mcp`",
            ),
        }
    }
    r.summary();
    Ok(r.problems == 0)
}

/// Graph-section findings. Returns early when there is no readable graph
/// so the integration section still runs and the summary stays whole.
fn graph_checks(r: &mut Report, repo: &Path) -> Result<()> {
    let db = pipeline::db_path(repo);
    if !db.exists() {
        r.fixable(
            &format!("no graph at {}", db.display()),
            "run `sinter build`",
            || pipeline::build(repo, None).map(drop),
        );
        if !db.exists() {
            return Ok(());
        }
    }
    // A held lock (long-lived serve/watch from another process) is a
    // finding to report, never a crash.
    let schema = match Store::schema_of(&db) {
        Ok(schema) => schema,
        Err(e) => {
            r.warn(
                &format!("graph database is not readable right now: {e}"),
                "another process holds it (serve/watch?); stop it or retry, then `sinter doctor`",
            );
            return Ok(());
        }
    };
    match schema {
        Some(v) if v == Store::CURRENT_SCHEMA => r.ok(&format!("graph schema v{v} (current)")),
        Some(v) => r.fixable(
            &format!(
                "graph schema v{v}, binary writes v{}",
                Store::CURRENT_SCHEMA
            ),
            "run `sinter build` (rebuilds automatically)",
            || pipeline::build(repo, None).map(drop),
        ),
        None => r.fixable("graph has no schema stamp", "run `sinter build`", || {
            pipeline::build(repo, None).map(drop)
        }),
    }

    let store = Store::open(&db)?;
    let stored: HashMap<String, sinter_store::FileStamp> =
        store.file_hashes()?.into_iter().collect();
    let current = pipeline::scan_hashes(repo, &stored)?;
    let stale = current
        .iter()
        .filter(|(f, h)| stored.get(f).map(|s| &s.hash) != Some(h))
        .count();
    let removed = {
        let live: std::collections::HashSet<&str> =
            current.iter().map(|(f, _)| f.as_str()).collect();
        stored.keys().filter(|f| !live.contains(f.as_str())).count()
    };
    // The rebuild (and the stats reopen below) needs this handle released.
    drop(store);
    if stale == 0 && removed == 0 {
        r.ok(&format!("graph fresh ({} files indexed)", stored.len()));
    } else {
        r.fixable(
            &format!("graph stale: {stale} changed, {removed} removed files"),
            "run `sinter build`",
            || pipeline::build(repo, None).map(drop),
        );
    }

    let store = Store::open(&db)?;
    r.ok(&format!(
        "{} nodes, {} edges, {} unresolved refs, {} on disk",
        store.node_count()?,
        store.edge_count()?,
        store.unresolved_count()?,
        pipeline::db_size(repo),
    ));

    // SCIP staleness is mtime-based in `scip::staleness`; a file whose
    // content hash still matches the stamp recorded before the index was
    // written was merely touched (checkout, restore) and is excused.
    // ponytail: once `sinter build` re-stamps a touched file the proof is
    // gone; recording a corpus fingerprint at `sinter scip` time would fix
    // that at the source.
    let excused = scip_excused(repo, &stored, &current);
    match crate::scip::staleness(repo) {
        crate::scip::Staleness::Stale(n) if n > excused => r.warn(
            &format!(
                "SCIP index stale ({} source files changed since the index)",
                n - excused
            ),
            "run `sinter scip` (newer files fall back to import/scope evidence until then)",
        ),
        crate::scip::Staleness::Fresh | crate::scip::Staleness::Stale(_) => {
            r.ok("SCIP index present and fresh (compiler-grade evidence tier active)")
        }
        crate::scip::Staleness::Missing => {
            r.ok("no SCIP index (optional; `sinter scip` would bind external/method refs)")
        }
    }
    Ok(())
}

/// Files newer than the SCIP index whose content provably predates it.
fn scip_excused(
    repo: &Path,
    stored: &HashMap<String, sinter_store::FileStamp>,
    current: &[(String, String)],
) -> usize {
    let Some(index_mtime) =
        pipeline::scip_index_path(repo).and_then(|p| std::fs::metadata(p).ok()?.modified().ok())
    else {
        return 0;
    };
    let index_nanos = index_mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    current
        .iter()
        .filter(|(file, hash)| {
            let Some(stamp) = stored.get(file) else {
                return false;
            };
            let newer = std::fs::metadata(repo.join(file))
                .and_then(|m| m.modified())
                .is_ok_and(|m| m > index_mtime);
            newer && stamp.hash == *hash && stamp_mtime_nanos(stamp) <= index_nanos
        })
        .count()
}

/// Mtime half of a stored stamp identity (unix packs mtime above ctime).
fn stamp_mtime_nanos(stamp: &sinter_store::FileStamp) -> u128 {
    #[cfg(unix)]
    {
        stamp.identity_nanos >> 64
    }
    #[cfg(not(unix))]
    {
        stamp.identity_nanos
    }
}

fn mcp_command_resolves(repo: &Path, command: &str, search_path: Option<&OsStr>) -> bool {
    let configured = Path::new(command);
    let has_directory = configured
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty());
    if configured.is_absolute() {
        return is_executable_file(configured);
    }
    if has_directory {
        return is_executable_file(&repo.join(configured));
    }

    let Some(search_path) = search_path else {
        return false;
    };
    std::env::split_paths(search_path).any(|directory| {
        let directory = if directory.as_os_str().is_empty() {
            repo.to_path_buf()
        } else {
            directory
        };
        executable_candidates(directory.join(configured))
            .into_iter()
            .any(|candidate| is_executable_file(&candidate))
    })
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn executable_candidates(path: PathBuf) -> Vec<PathBuf> {
    let mut candidates = vec![path.clone()];
    if !std::env::consts::EXE_SUFFIX.is_empty() && path.extension().is_none() {
        let mut executable = path.into_os_string();
        executable.push(std::env::consts::EXE_SUFFIX);
        candidates.push(PathBuf::from(executable));
    }
    candidates
}

/// Spawn this binary as the MCP server (registrations say `sinter`; this
/// binary IS that product, so testing current_exe tests the real path
/// without depending on the caller's PATH), run initialize + tools/list
/// over stdio, and return the served tool names.
fn mcp_handshake(repo: &Path) -> anyhow::Result<Vec<String>> {
    use std::io::Write;
    let exe = std::env::current_exe()?;
    let mut child = std::process::Command::new(exe)
        .args(["serve", "--repo"])
        .arg(repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    {
        let stdin = child.stdin.as_mut().expect("piped");
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
        )?;
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#)?;
    }
    drop(child.stdin.take());
    let output = child.wait_with_output()?;
    let mut tools = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(list) = v["result"]["tools"].as_array() {
            tools.extend(
                list.iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_string)),
            );
        }
    }
    if tools.is_empty() {
        anyhow::bail!("no tools/list response");
    }
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_mcp_command_must_resolve_from_search_path() {
        let repo = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let executable = bin
            .path()
            .join(format!("sinter{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&executable, "fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let search_path = std::env::join_paths([bin.path()]).unwrap();

        assert!(mcp_command_resolves(
            repo.path(),
            "sinter",
            Some(&search_path)
        ));
        assert!(!mcp_command_resolves(
            repo.path(),
            "missing-sinter",
            Some(&search_path)
        ));
        assert!(!mcp_command_resolves(repo.path(), "sinter", None));
    }

    #[test]
    fn configured_mcp_paths_are_resolved_at_their_owned_locations() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("tools")).unwrap();
        let executable = repo.path().join("tools/sinter");
        std::fs::write(&executable, "fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        assert!(mcp_command_resolves(repo.path(), "tools/sinter", None));
        assert!(!mcp_command_resolves(repo.path(), "tools/missing", None));
    }
}
