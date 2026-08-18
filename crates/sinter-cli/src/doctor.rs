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

/// Workspace health: member freshness and boundary-link staleness.
pub fn run_workspace(manifest: &Path) -> Result<bool> {
    let ws = crate::workspace::load(manifest)?;
    let mut r = Report { problems: 0 };
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
    println!("{} problem(s)", r.problems);
    Ok(r.problems == 0)
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

    // Repo checks. Subdirectory invocation resolves to the graph root,
    // matching every query command.
    let repo = pipeline::discover_root(repo);
    let repo = repo.canonicalize()?;

    // Enforcement hooks: script current with this binary and the three
    // settings entries present (per-prompt router, Bash + Grep nudges),
    // satisfied by either the repo's .claude or the global ~/.claude.
    let enforced_at = |claude: &Path| {
        std::fs::read_to_string(claude.join("hooks/sinter-first.sh"))
            .is_ok_and(|s| s == install::ENFORCE_HOOK)
            && std::fs::read_to_string(claude.join("settings.json")).is_ok_and(|s| {
                [
                    "sinter-first.sh prompt",
                    "sinter-first.sh grep\"",
                    "sinter-first.sh greptool",
                ]
                .iter()
                .all(|m| s.contains(m))
            })
    };
    let global_claude =
        install::default_dir().and_then(|d| d.parent()?.parent().map(Path::to_path_buf));
    if enforced_at(&repo.join(".claude")) {
        r.ok("enforcement hooks installed and current (repo .claude)");
    } else if global_claude.as_deref().is_some_and(enforced_at) {
        r.ok("enforcement hooks installed and current (global ~/.claude)");
    } else {
        r.warn(
            "enforcement hooks missing or stale (agents may grep instead of querying)",
            "run `sinter install --for enforce` (or --global)",
        );
    }
    let db = pipeline::db_path(&repo);
    if !db.exists() {
        r.warn(
            &format!("no graph at {}", db.display()),
            "run `sinter build`",
        );
        println!("{} problem(s)", r.problems);
        return Ok(r.problems == 0);
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
            println!("{} problem(s)", r.problems);
            return Ok(false);
        }
    };
    match schema {
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
    let stored: HashMap<String, sinter_store::FileStamp> =
        store.file_hashes()?.into_iter().collect();
    let current = pipeline::scan_hashes(&repo, &stored)?;
    let stale = current
        .iter()
        .filter(|(f, h)| stored.get(f).map(|s| &s.hash) != Some(h))
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
        "{} nodes, {} edges, {} unresolved refs, {} on disk",
        store.node_count()?,
        store.edge_count()?,
        store.unresolved_count()?,
        pipeline::db_size(&repo),
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
                r.warn(
                    &format!("{label} is stale (differs from this binary's embedded card)"),
                    "rerun `sinter install --for cursor,agents`",
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
        r.warn(
            &format!("MCP registered for {} only", registered.join(", ")),
            "run `sinter install --mcp` to register every client",
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
    match crate::pipeline::scip_index_path(&repo) {
        Some(index) => match stale_since_index(&repo, &index) {
            0 => r.ok("SCIP index present and fresh (compiler-grade evidence tier active)"),
            n => r.warn(
                &format!("SCIP index stale ({n} source files newer than the index)"),
                "run `sinter scip` (newer files fall back to import/scope evidence until then)",
            ),
        },
        None => r.ok("no SCIP index (optional; `sinter scip` would bind external/method refs)"),
    }

    println!("{} problem(s)", r.problems);
    Ok(r.problems == 0)
}

/// How many language files were modified after the SCIP index was written.
fn stale_since_index(repo: &Path, index: &Path) -> usize {
    let Ok(index_mtime) = std::fs::metadata(index).and_then(|m| m.modified()) else {
        return 0;
    };
    let mut newer = 0;
    for entry in ignore::WalkBuilder::new(repo).build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = sinter_core::rel_display(entry.path().strip_prefix(repo).unwrap_or(entry.path()));
        if rel.starts_with(".sinter/") || sinter_extract::spec_for_path(&rel).is_none() {
            continue;
        }
        if entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .is_some_and(|m| m > index_mtime)
        {
            newer += 1;
        }
    }
    newer
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
