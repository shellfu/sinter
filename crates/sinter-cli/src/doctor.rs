//! `sinter doctor`: diagnose the installation and (optionally) a repo's
//! graph. Every finding names its fix. Findings in the `graph` section are
//! problems (exit 1) or completeness warnings (`warn`: evidence the graph
//! admits it lacks, never fails); findings in the `integration` section are
//! notes (drifted cards/hooks/registrations) and never fail the exit code.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Result;
use sinter_core::{Edge, Node, NodeId, Relation, SymbolKind, UnresolvedReference};
use sinter_extract::LANGUAGES;
use sinter_store::Store;

use crate::{install, pipeline};

struct Report {
    problems: usize,
    warnings: usize,
    notes: usize,
    fix: bool,
    fixed: usize,
    /// Findings after `integration()` are notes, not problems.
    integration: bool,
    /// Text mode buffers integration rows: when every one is `ok` the
    /// section collapses to a single rollup line at `summary()`.
    integration_rows: Vec<String>,
    integration_flagged: bool,
    /// `--json`: findings are collected per section and emitted once by
    /// `summary()` instead of being printed as they arrive.
    json: Option<HashMap<&'static str, Vec<serde_json::Value>>>,
}

impl Report {
    fn new(fix: bool, json: bool) -> Self {
        Self {
            problems: 0,
            warnings: 0,
            notes: 0,
            fix,
            fixed: 0,
            integration: false,
            integration_rows: Vec::new(),
            integration_flagged: false,
            json: json.then(HashMap::new),
        }
    }
    fn emit(&mut self, status: &str, msg: &str, fix: Option<&str>) {
        if let Some(sections) = &mut self.json {
            let section = if self.integration {
                "integration"
            } else {
                "graph"
            };
            let mut finding = serde_json::json!({ "status": status, "message": msg });
            if let Some(fix) = fix {
                finding["fix"] = fix.into();
            }
            sections.entry(section).or_default().push(finding);
        } else if self.integration {
            if status != "ok" {
                self.integration_flagged = true;
            }
            self.integration_rows.push(format!("  {status:<5} {msg}"));
            if let Some(fix) = fix {
                self.integration_rows.push(format!("        -> {fix}"));
            }
        } else {
            println!("  {status:<5} {msg}");
            if let Some(fix) = fix {
                println!("        -> {fix}");
            }
        }
    }
    fn ok(&mut self, msg: &str) {
        self.emit("ok", msg, None);
    }
    /// Graph-section completeness warning: the graph is honest about a
    /// gap (`map` calls it `partial`); nothing to fix by hand, exit code
    /// unchanged.
    fn completeness(&mut self, msg: &str) {
        self.warnings += 1;
        self.emit("warn", msg, None);
    }
    fn section(&mut self, name: &str) {
        self.integration = name == "integration";
        if self.json.is_none() && !self.integration {
            println!("{name}");
        }
    }
    /// Text mode: an all-ok integration section is one line; anything
    /// flagged prints every row so the note sits in context.
    fn flush_integration(&mut self) {
        if self.json.is_some() || self.integration_rows.is_empty() {
            return;
        }
        if self.integration_flagged {
            println!("integration");
            for row in self.integration_rows.drain(..) {
                println!("{row}");
            }
        } else {
            println!("integration: all {} checks ok", self.integration_rows.len());
            self.integration_rows.clear();
        }
    }
    fn warn(&mut self, msg: &str, fix: &str) {
        if self.integration {
            self.notes += 1;
            self.emit("note", msg, Some(fix));
        } else {
            self.problems += 1;
            self.emit("FIX", msg, Some(fix));
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
                self.emit("FIXED", msg, None);
            }
            Err(e) => self.warn(&format!("{msg} (auto-fix failed: {e:#})"), cmd),
        }
    }
    fn summary(&mut self) {
        self.flush_integration();
        if let Some(mut sections) = self.json.take() {
            let out = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "graph": sections.remove("graph").unwrap_or_default(),
                "integration": sections.remove("integration").unwrap_or_default(),
                "summary": { "fixed": self.fixed, "problems": self.problems, "warnings": self.warnings, "notes": self.notes },
            });
            println!("{out}");
            return;
        }
        if self.fix {
            println!(
                "{} fixed, {} graph problem(s), {} completeness warning(s), {} integration note(s) remaining",
                self.fixed, self.problems, self.warnings, self.notes
            );
        } else {
            println!(
                "{} graph problem(s), {} completeness warning(s), {} integration note(s)",
                self.problems, self.warnings, self.notes
            );
        }
    }
}

/// Workspace health: member freshness and boundary-link staleness.
pub fn run_workspace(manifest: &Path, fix: bool, json: bool) -> Result<bool> {
    // The one fix for every workspace finding is the build itself, and it
    // is stat-gated (cheap when fresh) — so `--fix` runs it up front and
    // the diagnosis below reports the post-fix state.
    if fix {
        crate::workspace::run(manifest)?;
    }
    let ws = crate::workspace::load(manifest)?;
    let mut r = Report::new(fix, json);
    r.section(&format!(
        "workspace `{}` ({} members)",
        ws.manifest.workspace.name,
        ws.members.len()
    ));
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

pub fn run(repo: &Path, fix: bool, json: bool, verbose: bool) -> Result<bool> {
    let mut r = Report::new(fix, json);

    if !json {
        println!("sinter {}", env!("CARGO_PKG_VERSION"));
    }
    r.section("graph");
    let names: Vec<&str> = LANGUAGES.iter().map(|l| l.name).collect();
    r.ok(&format!("languages: {}", names.join(", ")));

    // Repo checks. Subdirectory invocation resolves to the graph root,
    // matching every query command.
    let repo = pipeline::discover_root(repo);
    let repo = repo.canonicalize()?;

    graph_checks(&mut r, &repo, verbose)?;

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

    // Repo onboarding installs bounded strict redirection. A direct
    // `install enforce` call may intentionally select advisory mode, so
    // doctor validates either current mode instead of rewriting user policy.
    let (hook_file, _) = install::PLATFORM_HOOK;
    let global_claude =
        install::default_dir().and_then(|d| d.parent()?.parent().map(Path::to_path_buf));
    let repo_claude = repo.join(".claude");
    let repo_scope = repo_claude.join("hooks").join(hook_file).exists();
    if install::enforcement_current_at(&repo_claude, false) {
        r.ok("enforcement hooks installed and current (repo .claude)");
    } else if global_claude
        .as_deref()
        .is_some_and(|claude| install::enforcement_current_at(claude, false))
    {
        r.ok("enforcement hooks installed and current (global ~/.claude)");
    } else {
        // Refresh-only: a scope counts for auto-fix when its script file
        // exists at all — first-time enforcement stays an opt-in.
        let global_scope = global_claude
            .as_ref()
            .is_some_and(|c| c.join("hooks").join(hook_file).exists());
        if repo_scope || global_scope {
            r.fixable(
                "enforcement hooks stale (agents may grep instead of querying)",
                "run `sinter install enforce` (or --global)",
                || {
                    if repo_scope {
                        install::enforce(
                            Some(&repo),
                            install::enforcement_is_strict(&repo_claude),
                        )?;
                    }
                    if global_scope {
                        let strict = global_claude
                            .as_deref()
                            .is_some_and(install::enforcement_is_strict);
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
    // Leaked MCP servers from finished agent sessions keep answering with
    // the binary they were started with. An older one rewrites the graph
    // in its own format on every self-sync, so two versions ping-pong full
    // rebuilds and hold the store lock for the duration.
    let stale = stale_servers(&repo);
    if !stale.is_empty() {
        let pids: Vec<String> = stale.iter().map(|(pid, _, _)| pid.to_string()).collect();
        let detail: Vec<String> = stale
            .iter()
            .map(|(pid, version, cwd)| format!("{pid} ({version}) in {cwd}"))
            .collect();
        r.warn(
            &format!(
                "{} `sinter serve` process(es) run a different sinter than this binary: {}",
                stale.len(),
                detail.join("; ")
            ),
            &format!(
                "restart those agent sessions, or `kill {}`; every version rewrites the graph in its own format and holds the lock while doing so",
                pids.join(" ")
            ),
        );
    }
    r.summary();
    Ok(r.problems == 0)
}

/// Running `sinter serve` processes for this repository (cwd or `--repo`
/// inside it) whose binary version differs from this one: (pid, version,
/// working directory). Servers of other repositories never touch this
/// graph, so they are not this doctor's business. Linux only (reads
/// /proc); other platforms report nothing.
#[cfg(target_os = "linux")]
fn stale_servers(repo: &Path) -> Vec<(u32, String, String)> {
    use std::os::unix::ffi::OsStrExt;
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in dir.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
        let is_serve = args
            .first()
            .is_some_and(|a| a.rsplit(|b| *b == b'/').next() == Some(b"sinter"))
            && args.get(1) == Some(&&b"serve"[..]);
        if !is_serve {
            continue;
        }
        let cwd = std::fs::read_link(entry.path().join("cwd")).unwrap_or_default();
        // A relative `--repo .` is relative to the server's cwd, not ours.
        let served_here = cwd.starts_with(repo)
            || args.iter().skip(2).any(|arg| {
                cwd.join(OsStr::from_bytes(arg))
                    .canonicalize()
                    .is_ok_and(|p| p.starts_with(repo))
            });
        if !served_here {
            continue;
        }
        let version = std::process::Command::new(entry.path().join("exe"))
            .arg("--version")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "unknown version".to_string());
        if version == format!("sinter {}", env!("CARGO_PKG_VERSION")) {
            continue;
        }
        out.push((pid, version, cwd.display().to_string()));
    }
    out
}

#[cfg(not(target_os = "linux"))]
fn stale_servers(_repo: &Path) -> Vec<(u32, String, String)> {
    Vec::new()
}

/// The auto-fix rebuild, with the same phase reporter `sinter build` uses:
/// a large repository takes tens of seconds and a silent `--fix` reads as
/// a hang.
fn rebuild(repo: &Path) -> Result<()> {
    let progress = crate::progress::Progress::stderr();
    pipeline::build_with(repo, None, &mut |phase| {
        crate::progress::render(&progress, phase)
    })
    .map(drop)
}

/// Graph-section findings. Returns early when there is no readable graph
/// so the integration section still runs and the summary stays whole.
fn graph_checks(r: &mut Report, repo: &Path, verbose: bool) -> Result<()> {
    let db = pipeline::db_path(repo);
    if !db.exists() {
        r.fixable(
            &format!("no graph at {}", db.display()),
            "run `sinter build`",
            || rebuild(repo),
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
            || rebuild(repo),
        ),
        None => r.fixable("graph has no schema stamp", "run `sinter build`", || {
            rebuild(repo)
        }),
    }
    // Re-read: `--fix` may have just rebuilt. Values under an old schema
    // only decode with the codecs of that schema, so every read below
    // would die mid-report with a codec error; the mismatch row above
    // already names the one fix.
    if Store::schema_of(&db)? != Some(Store::CURRENT_SCHEMA) {
        return Ok(());
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
            || rebuild(repo),
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
    let coverage = crate::coverage::repository_coverage(repo, &store)?;
    completeness_warnings(r, &coverage, verbose);
    let gap = schema_consistency(r, repo, &store, &coverage)?;
    let total_sql = stored.keys().filter(|f| f.ends_with(".sql")).count();
    sql_parse_gap(r, repo, &coverage, total_sql, gap);
    Ok(())
}

/// Migration-sequence fold and schema-consistency lints, rendered as
/// completeness warnings: lexical filename order is the migration
/// convention, not proof of execution order, so a finding never fails
/// the exit code.
fn schema_consistency(
    r: &mut Report,
    repo: &Path,
    store: &Store,
    coverage: &serde_json::Value,
) -> Result<SqlGap> {
    let objects: Vec<Node> = store
        .all_nodes()?
        .into_iter()
        .filter(|n| matches!(n.kind, SymbolKind::Table | SymbolKind::View))
        .collect();
    let ids: Vec<NodeId> = objects.iter().map(|n| n.id.clone()).collect();
    let mut in_edges = store.in_edges_many(&ids)?;
    let tables: Vec<(Node, Vec<Edge>)> = objects
        .into_iter()
        .map(|n| {
            let edges = in_edges.remove(&n.id).unwrap_or_default();
            (n, edges)
        })
        .collect();
    let unresolved = store.all_unresolved_details()?;
    let findings = schema_findings(&tables, &unresolved);
    for dir in &findings.skipped {
        let dir = if dir.is_empty() { "." } else { dir };
        r.completeness(&format!(
            "schema fold skipped for {dir}/: migration filenames lack sortable prefixes"
        ));
    }
    for d in &findings.dropped {
        r.completeness(&format!(
            "{} `{}` is dropped at head ({}) but still referenced: {}",
            d.kind,
            d.name,
            d.dropped_in,
            sites_text(repo, &d.sites),
        ));
    }
    // A partially parsed file may hold the CREATE in a statement the
    // grammar dropped, so a reference seen only there proves nothing.
    let partial: std::collections::BTreeSet<&str> = coverage["graph"]["syntax_error_files"]
        .as_array()
        .map(|files| files.iter().filter_map(|f| f.as_str()).collect())
        .unwrap_or_default();
    let partial_sql: Vec<String> = partial
        .iter()
        .filter(|f| f.ends_with(".sql"))
        .filter_map(|f| std::fs::read_to_string(repo.join(f)).ok())
        .collect();
    let mut withheld = 0usize;
    let mut hidden_creates = 0usize;
    let mut shown = 0usize;
    const NEVER_CREATED_SHOWN: usize = 10;
    for (name, sites) in &findings.never_created {
        if !plausible_table_name(name) || defined_as_cte(repo, name, sites) {
            continue;
        }
        if created_in_text(&partial_sql, name) {
            hidden_creates += 1;
            continue;
        }
        if sites
            .iter()
            .all(|(file, _)| partial.contains(file.as_str()))
        {
            withheld += 1;
            continue;
        }
        shown += 1;
        if shown <= NEVER_CREATED_SHOWN || r.json.is_some() {
            r.completeness(&format!(
                "table `{name}` is referenced in SQL but never created by any migration: {}",
                sites_text(repo, sites),
            ));
        }
    }
    if shown > NEVER_CREATED_SHOWN && r.json.is_none() {
        r.completeness(&format!(
            "{} more table(s) referenced in SQL but never created (`sinter doctor --json` lists all)",
            shown - NEVER_CREATED_SHOWN
        ));
    }
    Ok(SqlGap {
        hidden_creates,
        withheld,
    })
}

/// What the SQL grammar gap cost the schema lints; folded into the one
/// SQL completeness line by [`sql_parse_gap`].
#[derive(Default)]
struct SqlGap {
    hidden_creates: usize,
    withheld: usize,
}

/// Names the never-created lint should not judge: system catalogs, and
/// tokens the grammar's error recovery mistakes for relation names.
fn plausible_table_name(name: &str) -> bool {
    const SQL_KEYWORDS: &[&str] = &[
        "all",
        "and",
        "as",
        "by",
        "case",
        "columns",
        "constraint",
        "cross",
        "delete",
        "distinct",
        "else",
        "end",
        "exists",
        "for",
        "from",
        "group",
        "having",
        "in",
        "index",
        "inner",
        "insert",
        "into",
        "is",
        "join",
        "left",
        "limit",
        "not",
        "null",
        "offset",
        "on",
        "only",
        "or",
        "order",
        "outer",
        "over",
        "partition",
        "returning",
        "right",
        "row",
        "select",
        "set",
        "table",
        "then",
        "union",
        "update",
        "using",
        "values",
        "view",
        "when",
        "where",
        "with",
    ];
    !(name.starts_with("pg_")
        || name.starts_with("information_schema")
        || SQL_KEYWORDS.contains(&name))
}

/// `CREATE TABLE|VIEW name` as text in a partially parsed file: the object
/// exists, the grammar dropped its statement.
fn created_in_text(texts: &[String], name: &str) -> bool {
    let Ok(re) = regex::Regex::new(&format!(
        r#"(?i)create\s+(table|(materialized\s+)?view)\s+(if\s+not\s+exists\s+)?("?\w+"?\.)?"?{}"?\b"#,
        regex::escape(name)
    )) else {
        return false;
    };
    texts.iter().any(|t| re.is_match(t))
}

/// `WITH name AS (` in a referencing file makes the name a CTE, not a
/// table. ponytail: text heuristic; capturing `cte` nodes in sql.scm as
/// file-local definitions would make resolution itself bind these.
fn defined_as_cte(repo: &Path, name: &str, sites: &[(String, u64)]) -> bool {
    let Ok(re) = regex::Regex::new(&format!(r"(?i)\b{}\s+as\s*\(", regex::escape(name))) else {
        return false;
    };
    let mut seen = std::collections::BTreeSet::new();
    sites
        .iter()
        .filter(|(file, _)| seen.insert(file.clone()))
        .any(|(file, _)| {
            std::fs::read_to_string(repo.join(file)).is_ok_and(|text| re.is_match(&text))
        })
}

/// Up to three `file:line` evidence sites (file-only when the line is
/// unavailable), then a remainder count.
fn sites_text(repo: &Path, sites: &[(String, u64)]) -> String {
    let shown: Vec<String> = sites
        .iter()
        .take(3)
        .map(
            |(file, byte)| match crate::render::line_of(repo, file, *byte) {
                Some(line) => format!("{file}:{line}"),
                None => file.clone(),
            },
        )
        .collect();
    let more = sites.len().saturating_sub(3);
    if more > 0 {
        format!("{}, +{more} more", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

/// Results of the migration fold, separated from rendering so tests can
/// drive it with an in-memory graph.
#[derive(Default)]
struct SchemaFindings {
    /// Directories whose fold was skipped: they drop objects but at least
    /// one schema-changing filename has no sortable (leading-digit) prefix.
    skipped: Vec<String>,
    dropped: Vec<DroppedButReferenced>,
    /// (name, reference sites) for tables read/written/used in SQL that no
    /// table or view node anywhere defines — distinct from plain unresolved
    /// because a create provably does not exist in the corpus.
    never_created: Vec<(String, Vec<(String, u64)>)>,
}

/// A table/view whose lexically-last migration event is a drop, yet the
/// graph still holds reads/writes/uses edges into it from outside the
/// migration sequence.
struct DroppedButReferenced {
    kind: &'static str,
    name: String,
    dropped_in: String,
    /// (file, byte offset) of each surviving reference site.
    sites: Vec<(String, u64)>,
}

/// Lexical fold over each directory's schema-changing `.sql` files.
/// Creates and drops move an object's head state; alters (including
/// renames) never change liveness — conservative, so a renamed table
/// reads as still alive under its original node. Directories whose
/// schema-changing filenames are not sortable (no leading digit) are
/// skipped and named instead of guessed at.
fn schema_findings(
    tables: &[(Node, Vec<Edge>)],
    unresolved: &[UnresolvedReference],
) -> SchemaFindings {
    use std::collections::{BTreeMap, BTreeSet};
    // File encoded in a node id: symbol ids are `file#qualified@off`,
    // file-node ids are the path itself (same derivation as edge sites).
    let file_of = |id: &NodeId| {
        id.as_str()
            .split_once('#')
            .map_or(id.as_str(), |(f, _)| f)
            .to_string()
    };
    let dir_of = |file: &str| file.rsplit_once('/').map_or("", |(d, _)| d).to_string();
    // Schema events grouped dir -> file -> (position, relation, object).
    #[allow(clippy::type_complexity)]
    let mut dirs: BTreeMap<String, BTreeMap<String, Vec<(u64, Relation, usize)>>> = BTreeMap::new();
    for (i, (node, in_edges)) in tables.iter().enumerate() {
        for e in in_edges {
            if !matches!(e.relation, Relation::Creates | Relation::Drops) {
                continue;
            }
            let src_file = file_of(&e.src);
            if !src_file.ends_with(".sql") {
                continue;
            }
            // Position of the event inside its file: the reference site, or
            // the definition span for a create edge that carries none.
            let pos = e
                .site
                .map(|s| s.start)
                .or_else(|| (node.file == src_file).then_some(node.span.start))
                .unwrap_or(0);
            dirs.entry(dir_of(&src_file))
                .or_default()
                .entry(src_file)
                .or_default()
                .push((pos, e.relation, i));
        }
    }
    let mut out = SchemaFindings::default();
    for (dir, files) in &dirs {
        if !files
            .values()
            .flatten()
            .any(|(_, rel, _)| *rel == Relation::Drops)
        {
            continue; // nothing can die: no fold needed
        }
        let sortable = files.keys().all(|f| {
            f.rsplit_once('/')
                .map_or(f.as_str(), |(_, base)| base)
                .starts_with(|c: char| c.is_ascii_digit())
        });
        if !sortable {
            out.skipped.push(dir.clone());
            continue;
        }
        // Fold in lexical file order, site order within a file: the last
        // create/drop wins the object's head state.
        let mut last: BTreeMap<usize, (Relation, &str)> = BTreeMap::new();
        for (file, events) in files {
            let mut events = events.clone();
            events.sort_unstable_by_key(|(pos, ..)| *pos);
            for (_, rel, i) in events {
                last.insert(i, (rel, file));
            }
        }
        let migration_files: BTreeSet<&str> = files.keys().map(String::as_str).collect();
        for (i, (rel, dropped_in)) in last {
            if rel != Relation::Drops {
                continue;
            }
            let (node, in_edges) = &tables[i];
            // Objects defined in another directory belong to that
            // directory's fold; a cross-directory drop proves nothing here.
            if dir_of(&node.file) != *dir {
                continue;
            }
            let mut sites: Vec<(String, u64)> = in_edges
                .iter()
                .filter(|e| {
                    matches!(
                        e.relation,
                        Relation::Reads | Relation::Writes | Relation::Uses
                    )
                })
                .filter_map(|e| {
                    let file = file_of(&e.src);
                    // Reads/writes inside the migration sequence itself
                    // (backfills) were valid at their point in history.
                    (!migration_files.contains(file.as_str()))
                        .then(|| (file, e.site.map_or(0, |s| s.start)))
                })
                .collect();
            sites.sort_unstable();
            sites.dedup();
            if !sites.is_empty() {
                out.dropped.push(DroppedButReferenced {
                    kind: node.kind.as_str(),
                    name: node.name.clone(),
                    dropped_in: dropped_in.to_string(),
                    sites,
                });
            }
        }
    }
    // Never created: a SQL read/write/use that resolution left dangling and
    // whose name matches no table/view definition anywhere in the corpus.
    let defined: BTreeSet<String> = tables
        .iter()
        .map(|(n, _)| n.name.to_ascii_lowercase())
        .collect();
    let mut missing: BTreeMap<String, Vec<(String, u64)>> = BTreeMap::new();
    for u in unresolved {
        let r = &u.reference;
        if !r.file.ends_with(".sql")
            || !matches!(
                r.relation,
                Relation::Reads | Relation::Writes | Relation::Uses
            )
        {
            continue;
        }
        let name = r
            .name
            .rsplit('.')
            .next()
            .unwrap_or(&r.name)
            .to_ascii_lowercase();
        if defined.contains(&name) {
            continue; // defined somewhere: a resolution gap, not a missing create
        }
        missing
            .entry(name)
            .or_default()
            .push((r.file.clone(), r.span.start));
    }
    out.never_created = missing.into_iter().collect();
    out
}

/// One row for the whole SQL grammar gap. tree-sitter-sequel drops the
/// statements it cannot parse (Postgres DDL: functions, triggers, policies,
/// DO blocks), so the affected files are indexed from partial trees and
/// every downstream lint inherits the hole. Nothing in the repository fixes
/// it; the row says so and names what is missing.
fn sql_parse_gap(
    r: &mut Report,
    repo: &Path,
    coverage: &serde_json::Value,
    total_sql: usize,
    gap: SqlGap,
) {
    let sql_files: Vec<&str> = coverage["graph"]["syntax_error_files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|f| f.as_str())
                .filter(|f| f.ends_with(".sql"))
                .collect()
        })
        .unwrap_or_default();
    if sql_files.is_empty() {
        return;
    }
    // ponytail: text heuristic over the partial files — which statement
    // kinds are present, not which ones the parser actually dropped. A real
    // answer needs extraction to record ERROR-node spans.
    const CONSTRUCTS: &[(&str, &str)] = &[
        (
            "CREATE PROCEDURE",
            r"(?i)\bcreate\s+(or\s+replace\s+)?procedure\b",
        ),
        (
            "CREATE FUNCTION",
            r"(?i)\bcreate\s+(or\s+replace\s+)?function\b",
        ),
        (
            "CREATE TRIGGER",
            r"(?i)\bcreate\s+(or\s+replace\s+)?trigger\b",
        ),
        ("CREATE POLICY", r"(?i)\bcreate\s+policy\b"),
        ("CREATE TYPE", r"(?i)\bcreate\s+type\b"),
        ("CREATE EXTENSION", r"(?i)\bcreate\s+extension\b"),
        ("DO block", r"(?i)\bdo\s+\$"),
        ("ROW LEVEL SECURITY", r"(?i)\brow\s+level\s+security\b"),
        ("GRANT/REVOKE", r"(?i)^\s*(grant|revoke)\b"),
    ];
    let mut counts: Vec<(&str, usize)> = CONSTRUCTS.iter().map(|(n, _)| (*n, 0)).collect();
    for file in &sql_files {
        let Ok(content) = std::fs::read_to_string(repo.join(file)) else {
            continue;
        };
        for (i, (_, pattern)) in CONSTRUCTS.iter().enumerate() {
            if let Ok(re) = regex::RegexBuilder::new(pattern).multi_line(true).build() {
                counts[i].1 += re.find_iter(&content).count();
            }
        }
    }
    counts.retain(|(_, n)| *n > 0);
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let likely = if counts.is_empty() {
        String::new()
    } else {
        let top: Vec<String> = counts
            .iter()
            .take(3)
            .map(|(name, n)| format!("{name} x{n}"))
            .collect();
        format!(" (likely: {})", top.join(", "))
    };
    let mut missing = vec!["the unparsed statements".to_string()];
    if gap.hidden_creates > 0 {
        missing.insert(0, format!("{} table CREATEs", gap.hidden_creates));
    }
    let withheld = if gap.withheld > 0 {
        format!(
            "; {} unverifiable table reference(s) withheld",
            gap.withheld
        )
    } else {
        String::new()
    };
    r.completeness(&format!(
        "SQL: {} of {total_sql} .sql files parsed only partially — the SQL grammar lacks Postgres constructs{likely}; absent from the graph: {}; reads/writes on those tables may be incomplete{withheld}. Not fixable in this repository",
        sql_files.len(),
        missing.join(" and "),
    ));
}

/// Gaps `map` already reports as `partial`, surfaced here so `0 problems`
/// never reads as "complete". Same `repository_coverage` document, one
/// line per gap.
fn completeness_warnings(r: &mut Report, coverage: &serde_json::Value, verbose: bool) {
    let graph = &coverage["graph"];
    let count = |field: &str| graph[field].as_u64().unwrap_or(0);
    // .sql files have their own line (`sql_parse_gap`): one grammar gap, one row.
    let partial: Vec<&str> = graph["syntax_error_files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|f| f.as_str())
                .filter(|f| !f.ends_with(".sql"))
                .collect()
        })
        .unwrap_or_default();
    if !partial.is_empty() {
        // Tree-sitter recovers past an error but the extractor cannot
        // see what it skipped, so no count of missing symbols exists;
        // only the shape of the loss does.
        r.completeness(&format!(
            "{} file(s) indexed from partial syntax trees; statements after the first syntax error are absent{}",
            partial.len(),
            if verbose {
                format!(": {}", partial.join(", "))
            } else {
                " (`sinter doctor --verbose` lists them)".to_string()
            }
        ));
    }
    let scip_state = coverage["compiler_index"]["state"]
        .as_str()
        .unwrap_or("missing");
    let waiting = count("missing_compiler_index");
    if scip_state != "fresh" && waiting > 0 {
        r.completeness(&format!(
            "compiler index {scip_state}: {waiting} unresolved refs waiting on `sinter scip`"
        ));
    }
    let actionable = count("actionable_unresolved");
    if actionable > 0 {
        r.completeness(&format!(
            "{actionable} actionable unresolved refs point inside this repo (`sinter unresolved`)"
        ));
    }
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

    mod schema_fold {
        use super::super::schema_findings;
        use sinter_core::{
            Confidence, Edge, Evidence, Node, NodeId, Reference, Relation, Span, SymbolKind,
            UnresolvedReason, UnresolvedReference,
        };

        fn table(name: &str, file: &str, start: u64) -> Node {
            Node {
                id: NodeId::new(format!("{file}#{name}@{start}")),
                kind: SymbolKind::Table,
                name: name.to_string(),
                file: file.to_string(),
                span: Span {
                    start,
                    end: start + 1,
                },
                signature: String::new(),
                doc: None,
            }
        }

        /// In-edge from a file node (id == path) into the table under test.
        fn edge(src_file: &str, dst: &Node, relation: Relation, site: u64) -> Edge {
            Edge {
                src: NodeId::new(src_file),
                dst: dst.id.clone(),
                relation,
                evidence: Evidence::Scope,
                confidence: Confidence::Inferred,
                site: Some(Span {
                    start: site,
                    end: site + 1,
                }),
            }
        }

        fn unresolved_read(file: &str, name: &str, start: u64) -> UnresolvedReference {
            UnresolvedReference {
                reference: Reference {
                    file: file.to_string(),
                    name: name.to_string(),
                    path: None,
                    relation: Relation::Reads,
                    span: Span {
                        start,
                        end: start + 1,
                    },
                    enclosing: None,
                    alias: None,
                },
                reason: UnresolvedReason::SyntaxAnchoredMiss,
            }
        }

        #[test]
        fn dropped_table_with_surviving_readers_is_found() {
            let users = table("users", "db/001_users.sql", 10);
            let in_edges = vec![
                edge("db/001_users.sql", &users, Relation::Creates, 10),
                edge("db/002_drop.sql", &users, Relation::Drops, 0),
                edge("db/queries.sql", &users, Relation::Reads, 5),
                // A backfill write from inside the migration sequence is
                // history, not a live reference.
                edge("db/002_drop.sql", &users, Relation::Writes, 3),
            ];

            let f = schema_findings(&[(users, in_edges)], &[]);
            assert!(f.skipped.is_empty());
            assert_eq!(f.dropped.len(), 1);
            let d = &f.dropped[0];
            assert_eq!((d.kind, d.name.as_str()), ("table", "users"));
            assert_eq!(d.dropped_in, "db/002_drop.sql");
            assert_eq!(d.sites, vec![("db/queries.sql".to_string(), 5)]);
        }

        #[test]
        fn drop_then_recreate_in_order_stays_alive() {
            let users = table("users", "db/001_users.sql", 10);
            let in_edges = vec![
                edge("db/001_users.sql", &users, Relation::Creates, 10),
                // 002 drops at byte 0 and recreates at byte 40: create wins.
                edge("db/002_redo.sql", &users, Relation::Drops, 0),
                edge("db/002_redo.sql", &users, Relation::Creates, 40),
                edge("db/queries.sql", &users, Relation::Reads, 5),
            ];

            let f = schema_findings(&[(users, in_edges)], &[]);
            assert!(f.dropped.is_empty() && f.skipped.is_empty());
        }

        #[test]
        fn unsortable_migration_names_skip_the_fold() {
            let events = table("events", "db/setup.sql", 10);
            let in_edges = vec![
                edge("db/setup.sql", &events, Relation::Creates, 10),
                edge("db/teardown.sql", &events, Relation::Drops, 0),
                edge("db/queries.sql", &events, Relation::Reads, 5),
            ];

            let f = schema_findings(&[(events, in_edges)], &[]);
            assert_eq!(f.skipped, vec!["db".to_string()]);
            assert!(f.dropped.is_empty());
        }

        #[test]
        fn never_created_tables_are_distinct_from_resolution_gaps() {
            let users = table("users", "db/001_users.sql", 10);
            let unresolved = [
                unresolved_read("db/queries.sql", "ghosts", 7),
                // Defined somewhere: a resolution gap, not a missing create.
                unresolved_read("db/queries.sql", "users", 20),
                // Non-SQL files never participate.
                unresolved_read("src/db.rs", "phantoms", 3),
            ];
            let f = schema_findings(&[(users, vec![])], &unresolved);
            assert_eq!(
                f.never_created,
                vec![(
                    "ghosts".to_string(),
                    vec![("db/queries.sql".to_string(), 7)]
                )]
            );
        }
    }

    #[test]
    fn completeness_warnings_count_but_never_fail() {
        let mut r = Report::new(false, true);
        r.section("graph");
        let coverage = serde_json::json!({
            "compiler_index": {"state": "missing"},
            "graph": {
                "syntax_error_files": ["a.rs", "b.rs", "c.rs", "d.rs"],
                "missing_compiler_index": 7,
                "actionable_unresolved": 2,
            },
        });
        completeness_warnings(&mut r, &coverage, false);
        assert_eq!((r.problems, r.warnings, r.notes), (0, 3, 0));
        let graph = &r.json.as_ref().unwrap()["graph"];
        assert!(graph.iter().all(|f| f["status"] == "warn"));
        let msg = graph[0]["message"].as_str().unwrap();
        assert!(msg.starts_with("4 file(s)") && msg.contains("--verbose") && !msg.contains("d.rs"));

        let mut r = Report::new(false, true);
        completeness_warnings(&mut r, &coverage, true);
        let msg = r.json.as_ref().unwrap()["graph"][0]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(msg.contains("a.rs, b.rs, c.rs, d.rs"), "{msg}");

        let mut r = Report::new(false, true);
        completeness_warnings(
            &mut r,
            &serde_json::json!({
                "compiler_index": {"state": "fresh"},
                "graph": {"syntax_error_files": [], "missing_compiler_index": 0, "actionable_unresolved": 0},
            }),
            false,
        );
        assert_eq!(r.warnings, 0);
    }

    #[test]
    fn all_ok_integration_collapses_to_one_line_and_notes_expand_it() {
        let mut r = Report::new(false, false);
        r.section("integration");
        r.ok("a");
        r.ok("b");
        assert_eq!(r.integration_rows.len(), 2);
        assert!(!r.integration_flagged);
        r.warn("c", "fix c");
        assert!(r.integration_flagged);
        assert_eq!(r.integration_rows.len(), 4);
        assert_eq!(r.notes, 1);
    }

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

#[cfg(test)]
mod never_created_filters {
    use super::{created_in_text, plausible_table_name};

    #[test]
    fn catalog_tables_and_keywords_are_not_judged() {
        assert!(!plausible_table_name("pg_roles"));
        assert!(!plausible_table_name("information_schema"));
        assert!(!plausible_table_name("on"));
        assert!(!plausible_table_name("row"));
        assert!(plausible_table_name("trajectories"));
    }

    #[test]
    fn creates_hidden_in_partial_files_are_found_by_text() {
        let texts = vec![
            "CREATE TABLE IF NOT EXISTS public.\"trajectories\" (id int);".to_string(),
            "create materialized view agg as select 1;".to_string(),
        ];
        assert!(created_in_text(&texts, "trajectories"));
        assert!(created_in_text(&texts, "agg"));
        assert!(!created_in_text(&texts, "trajectory"));
    }
}
