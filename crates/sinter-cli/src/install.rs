//! `sinter install`: write the Claude Code skill card. The card ships
//! embedded in the binary so integration text can never drift from the
//! tool's actual verbs — rerun after upgrading to refresh it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

pub const SKILL: &str = include_str!("../skill/SKILL.md");
/// Claude Code enforcement hook script, embedded so it can never drift
/// from the modes the settings entries invoke.
pub const ENFORCE_HOOK: &str = include_str!("../skill/sinter-first.sh");
/// PowerShell variant of the enforcement hook, installed on Windows where
/// implicit bash execution is not available.
pub const ENFORCE_HOOK_PS1: &str = include_str!("../skill/sinter-first.ps1");

/// Enforcement hook the LOCAL platform installs: (file name, embedded
/// content). Windows gets the PowerShell port; everything else the
/// original bash script.
pub const PLATFORM_HOOK: (&str, &str) = if cfg!(windows) {
    ("sinter-first.ps1", ENFORCE_HOOK_PS1)
} else {
    ("sinter-first.sh", ENFORCE_HOOK)
};

/// Card body without the Claude-specific YAML frontmatter — the single
/// source every assistant adapter wraps. One content, many writers:
/// forked per-assistant content is the failure mode this design forbids.
pub fn card_body() -> &'static str {
    SKILL
        .strip_prefix("---")
        .and_then(|rest| rest.split_once("---"))
        .map(|(_, body)| body.trim_start_matches('\n'))
        .unwrap_or(SKILL)
}

/// Always-in-context block for AGENTS.md: the full routing table plus the
/// behavior rules an agent must not skim past. Keep it in sync with the
/// skill card when verbs change — `agents_block_routes_match_card`
/// enforces the command surface.
const AGENTS_CARD: &str = r#"## sinter

This repo has a code knowledge graph at `.sinter/` (derived state — never
commit or edit it). When `.sinter/graph.redb` exists, query sinter BEFORE
any broad filesystem search for symbol location, callers, dependency
impact, structural paths, or diff impact. `sinter grep` is the text search
(unbounded by default, `--within` narrows it to a blast radius); rg only for
content sinter did not index. Schema v16: an older graph rebuilds once on
the first query.

| Question | Command |
|---|---|
| First look in an unfamiliar repo (modules, dependency hubs, docs, graph health) | `sinter map` |
| Evidence packet before starting a task | `sinter context "<task>"` — name real symbols; gives edit candidates, literals, mirrors, tests, and next commands; `--workspace <manifest>` for one packet across members |
| Vague/conceptual discovery (calibrated lexical search) | `sinter ask "<question>"` (`--explain` adds ranking diagnostics) |
| Exact or fuzzy symbol lookup | `sinter query <symbol>` (`'Type::*'`, `'*::method'`) |
| Inspect one symbol | `sinter show <symbol>` — `--body` is a real read (whole source when ≤ 60 lines); `--outline` maps a span too big to read (automatic over 8 KB); `show X@file:line` for the enclosing symbol; `--impls` for a type's impl blocks; `--callers` for the used-by files |
| Where exactly is a symbol called from | edge rows carry every site (up to 8) — `file.rs:12, :48 (+4 more)`; `--json` adds `sites` and `sites_total`. `affected <Trait>::<method>` reaches implementing methods in Rust, Java, C#, TypeScript and Go |
| Who depends on X (direct dependents) | `sinter affected <symbol>...` — test rows hidden (`--include-tests`), stops at hubs and names them (`--through-hubs`); seeds repeatable and unioned |
| What does X depend on (forward) | `sinter deps <symbol>` — depth 1 by default; `--max-depth N` widens |
| How does A reach B | `sinter path <A> <B>` (`-k N` for N node-disjoint routes) |
| Find text | `sinter grep '<regex>'` — production scope; `--within 'affected(<sym>)'`/`deps(SYM)`/`file(PATH)` narrows (repeatable, unioned) |
| Can I delete this | `sinter assert deletable <symbol>` — all scopes, grouped; `has_dependents` (exit 1) or `none_observed` (exit 0) |
| Prove no production callers in this indexed snapshot | `sinter assert no-callers <symbol> --json` — accept only `holds_for_indexed_snapshot` (completeness judged within `--scope`, production by default); retain scope, snapshot, universe, limitations |
| Prove nothing depends on a const/type/trait | `sinter assert no-dependents <symbol> --json` — all non-containment relations (`no-callers` counts `calls` only) |
| Who reads/writes a table; prove nothing writes it | `sinter affected <table> --relations reads,writes,creates,alters,drops`; `sinter assert no-writers <table> --json` |
| Check user gaps before a negative proof | `sinter unresolved [--file <f>] [--name <n>]` — user gaps only; `--all` adds external and resolver gaps |
| Emit a durable source citation | `sinter cite <symbol>` — paste the whole Markdown line and metadata comment |
| Gate citations in a document | `sinter verify-doc <file.md> --json` — bare path/line references remain `not_proven` |
| What does this commit/diff/PR affect downstream | `sinter impact <rev-range>` (default is capped; `--limit 0` returns all) |
| Did the refactor finish (unfinished-refactor check) | `sinter impact --expect <symbol>` — direct dependents the diff did NOT touch; names resolve at the base rev too; a body-only change reports callers unaffected; `--full` for the whole radius |
| Where do proposed changes overlap | `sinter overlap <rangeA> <rangeB> ...` |
| Build a cross-repo graph | `sinter workspace <manifest.toml>`; then add `--workspace <manifest.toml>` to reads |
| Create missing derived graph state | `sinter ensure <repo>` |
| Diagnose graph or integration problems | `sinter doctor <repo>` |
| Add compiler-grade call/type evidence | `sinter scip <repo>` |

- Every read verb takes `--json` and exits grep-style (0 results,
  1 none, 2 error). Assertion and document gates use 0 pass, 1 fail,
  2 error. Branch on the code, then read the status. Results carry call
  sites (`file:line`).
- JSON carries a coverage summary by default; `--coverage` adds the full
  block. `coverage.universe` names the canonical repository root or every
  declared workspace member searched; anything absent was not searched.
- `not_proven`, unresolved references, and candidate lists are real answers —
  refine and rerun, never report zero or guess a binding. `not_proven` with
  `reason: filter_excluded` means your filter emptied the result, not the
  graph. Receiver-typed call coverage may require `sinter scip`.
  Ambiguous symbol? Rerun as `name@file-suffix` or `name@file:line`
  (e.g. `run@init.rs`, `run@doctor.rs:175`).
- Any `--relations` filter (`calls,uses`, `reads,writes`, ...) on
  affected/deps/path drops file-level import noise.
- A `not_proven` `path` carries `closest_frontier`, `excluded_edges`, and
  `suggested_retries` — rerun a suggestion before claiming no path exists.
- Queries self-sync before answering (`sinter build` remains for CI/scripts;
  git hooks refresh on commit).
- Spawning subagents? Their prompts must mandate sinter for structure
  claims (callers, dependencies, blast radius, "no usages" proofs) and
  `sinter grep` for text; rg only for content sinter did not index.
- Cross-repo symbols may be `member:Symbol`.
- `sinter ensure <repo>` creates only derived `.sinter/` state; `sinter init`
  only when full hook and client integration was explicitly requested.
- MCP registered? `mcp__sinter__*` tools (ask/show/query/context/affected/deps/
  path/grep/unresolved/impact/overlap/map) answer the same questions.
  Arguments mirror the flags: `grep{pattern, within[]}`,
  `show{body, context_lines}`, `impact{expect[]}`; batch via `symbols[]` and
  `pairs[]`. Read `outcome.status` and `outcome.reason`; fixable errors are
  `isError` results with `Name@file` candidates. MCP `scope` matches the CLI
  default; a `--workspace` server has no `grep`.
- Anything else: `sinter --help`; graph problems: `sinter doctor`.
"#;

pub(crate) const AGENTS_BEGIN: &str =
    "<!-- BEGIN sinter (managed by `sinter install`; edits inside are overwritten) -->";
pub(crate) const AGENTS_END: &str = "<!-- END sinter -->";

/// Write the Cursor project rule (own file, native .mdc format).
pub fn cursor(repo: &Path) -> Result<PathBuf> {
    let dir = repo.join(".cursor").join("rules");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("sinter.mdc");
    let content = format!(
        "---
description: Query the sinter code graph for any codebase-structure question
alwaysApply: false
---

{}",
        card_body()
    );
    std::fs::write(&path, content)?;
    Ok(path)
}

/// Merge a managed sinter block into the repo's AGENTS.md (the convention
/// Codex, Gemini, and most non-Claude agents read). Existing content is
/// preserved; an existing sinter block is replaced in place — idempotent.
pub fn agents(repo: &Path) -> Result<PathBuf> {
    let path = repo.join("AGENTS.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let block = format!(
        "{AGENTS_BEGIN}

{}
{AGENTS_END}",
        AGENTS_CARD.trim_end()
    );
    let merged = match (existing.find(AGENTS_BEGIN), existing.find(AGENTS_END)) {
        (Some(start), Some(end)) if end > start => {
            let after = existing[end + AGENTS_END.len()..].to_string();
            format!("{}{}{}", &existing[..start], block, after)
        }
        _ if existing.trim().is_empty() => format!(
            "{block}
"
        ),
        _ => format!(
            "{}

{block}
",
            existing.trim_end()
        ),
    };
    std::fs::write(&path, merged)?;
    // Claude Code reads CLAUDE.md, not AGENTS.md. When the repo has a
    // CLAUDE.md, make it import the block once (`@AGENTS.md`); the
    // per-prompt hook already routes Claude when CLAUDE.md is absent.
    let claude_md = repo.join("CLAUDE.md");
    if let Ok(existing) = std::fs::read_to_string(&claude_md)
        && !existing.lines().any(|line| line.trim() == "@AGENTS.md")
    {
        std::fs::write(
            &claude_md,
            format!("{}\n\n@AGENTS.md\n", existing.trim_end()),
        )?;
    }
    Ok(path)
}

/// True when this content is current with the embedded card (drift check).
/// AGENTS.md carries the compact block, everything else the full card.
pub fn block_current(content: &str) -> bool {
    content.contains(card_body().trim_end()) || content.contains(AGENTS_CARD.trim_end())
}

/// Default install location for the skill card.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| {
            PathBuf::from(home)
                .join(".claude")
                .join("skills")
                .join("sinter")
        })
}

/// Register the sinter server in every client's project-scope MCP config:
/// `.mcp.json` (Claude Code), `.cursor/mcp.json` (Cursor), and a managed
/// block in `.codex/config.toml`. Registration uses the portable `sinter`
/// command name and initially leaves the Codex server non-required: a broken
/// PATH must not prevent an agent session from starting before the server has
/// completed a successful handshake. Other entries are preserved; only the
/// sinter entry is written. Global client configs belong to their applications
/// and are never edited here.
pub fn mcp(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;
    for path in [repo.join(".mcp.json"), repo.join(".cursor/mcp.json")] {
        std::fs::create_dir_all(path.parent().unwrap())?;
        let mut root: Value = match std::fs::read_to_string(&path) {
            Ok(existing) => serde_json::from_str(&existing)
                .with_context(|| format!("{} exists but is not valid JSON", path.display()))?,
            Err(_) => json!({}),
        };
        root.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("{} top level is not an object", path.display()))?
            .entry("mcpServers")
            .or_insert(json!({}));
        root["mcpServers"]["sinter"] = mcp_entry();
        std::fs::write(
            &path,
            format!(
                "{}
",
                serde_json::to_string_pretty(&root)?
            ),
        )?;
        println!("registered sinter MCP server in {}", path.display());
    }
    codex_mcp(&repo)?;
    Ok(())
}

/// The `mcpServers.sinter` entry this binary writes. Doctor compares an
/// installed entry against it: a block that no longer matches was written
/// by an older sinter or hand-edited, and the old `required = true` Codex
/// default is exactly the drift that makes a slow server fatal to a
/// session.
pub fn mcp_entry() -> Value {
    json!({"command": mcp_command(), "args": ["serve", "--repo", "."]})
}

/// Project-scoped MCP configuration is commonly shared across machines and
/// checkout locations, so it must not capture the installer's absolute path.
fn mcp_command() -> &'static str {
    "sinter"
}

pub(crate) const CODEX_BEGIN: &str =
    "# BEGIN sinter (managed by `sinter install`; edits inside are overwritten)";
pub(crate) const CODEX_END: &str = "# END sinter";

/// The managed `.codex/config.toml` block this binary writes, markers
/// included. `required = false`: a broken PATH must not stop a Codex
/// session from starting.
pub fn codex_block() -> Result<String> {
    #[derive(serde::Serialize)]
    struct Server {
        command: &'static str,
        args: [&'static str; 3],
        required: bool,
    }
    let server = toml::to_string(&Server {
        command: mcp_command(),
        args: ["serve", "--repo", "."],
        required: false,
    })
    .context("serialize the Codex MCP registration")?;
    Ok(format!(
        "{CODEX_BEGIN}\n[mcp_servers.sinter]\n{server}{CODEX_END}"
    ))
}

/// The managed block as it is currently installed in `content`, markers
/// included. `None` when no managed block is present — including a
/// hand-written `[mcp_servers.sinter]` that sinter never wrote.
pub fn codex_installed_block(content: &str) -> Option<&str> {
    let start = content.find(CODEX_BEGIN)?;
    let end = content[start..].find(CODEX_END)? + start + CODEX_END.len();
    Some(&content[start..end])
}

/// Merge a managed sinter server block into `.codex/config.toml` (marker
/// replacement, same convention as the AGENTS.md block — no TOML parser
/// needed for an append-or-replace of our own block).
fn codex_mcp(repo: &Path) -> Result<()> {
    let dir = repo.join(".codex");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let block = codex_block()?;
    let merged = match (existing.find(CODEX_BEGIN), existing.find(CODEX_END)) {
        (Some(start), Some(end)) if end > start => {
            let after = existing[end + CODEX_END.len()..].to_string();
            format!("{}{}{}", &existing[..start], block, after)
        }
        _ if existing.trim().is_empty() => format!("{block}\n"),
        _ => format!("{}\n\n{block}\n", existing.trim_end()),
    };
    std::fs::write(&path, merged)?;
    println!("registered sinter MCP server in {}", path.display());
    Ok(())
}

/// Installed-but-stale artifacts: each entry is one warning line naming
/// the artifact and its fix. Only artifacts that exist and differ from
/// this binary's embedded copies count — a user who never installed one
/// is never nagged about it. Missing files and unreadable paths are not
/// findings here; `sinter doctor` owns the full diagnosis.
pub fn stale_artifacts(repo: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(dir) = default_dir()
        && let Ok(card) = std::fs::read_to_string(dir.join("SKILL.md"))
        && card != SKILL
    {
        out.push("skill card is stale — run `sinter install`".to_string());
    }
    let (hook_file, hook_body) = PLATFORM_HOOK;
    for (claude, fix) in [
        (Some(repo.join(".claude")), "run `sinter install enforce`"),
        (claude_home(), "run `sinter install enforce -g`"),
    ] {
        if let Some(claude) = claude
            && let Ok(script) = std::fs::read_to_string(claude.join("hooks").join(hook_file))
            && script != hook_body
        {
            out.push(format!(
                "enforcement hook {} is stale — {fix}",
                claude.join("hooks").join(hook_file).display()
            ));
        }
    }
    if let Ok(agents) = std::fs::read_to_string(repo.join("AGENTS.md"))
        && agents.contains(AGENTS_BEGIN)
        && !block_current(&agents)
    {
        out.push("AGENTS.md sinter block is stale — run `sinter install agents`".to_string());
    }
    if let Ok(rule) = std::fs::read_to_string(repo.join(".cursor/rules/sinter.mdc"))
        && !block_current(&rule)
    {
        out.push("Cursor rule is stale — run `sinter install cursor`".to_string());
    }
    out
}

/// Whether one Claude settings file selects strict search redirection.
pub(crate) fn enforcement_is_strict(claude: &Path) -> bool {
    std::fs::read_to_string(claude.join("settings.json")).is_ok_and(|settings| {
        settings.contains(" grep-strict\"") && settings.contains(" greptool-strict\"")
    })
}

/// Validate the installed hook body and all three settings entries. Repo
/// onboarding requires strict search modes; machine-global installation may
/// remain advisory.
pub(crate) fn enforcement_current_at(claude: &Path, require_strict: bool) -> bool {
    let (hook_file, hook_body) = PLATFORM_HOOK;
    std::fs::read_to_string(claude.join("hooks").join(hook_file))
        .is_ok_and(|script| script == hook_body)
        && std::fs::read_to_string(claude.join("settings.json")).is_ok_and(|settings| {
            settings.contains(hook_file)
                && settings.contains(" prompt\"")
                && if require_strict {
                    settings.contains(" grep-strict\"") && settings.contains(" greptool-strict\"")
                } else {
                    (settings.contains(" grep\"") || settings.contains(" grep-strict\""))
                        && (settings.contains(" greptool\"")
                            || settings.contains(" greptool-strict\""))
                }
        })
}

/// Claude Code home (`~/.claude`), shared with the skill install.
pub(crate) fn claude_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".claude"))
}

/// Install Claude Code enforcement: the sinter-first hook script plus the
/// three settings entries that fire it (once-per-session prompt router,
/// Bash grep nudge, Grep-tool nudge). The script gates on `.sinter/graph.redb`
/// existing, so hooks stay silent in graph-less repos. Merging is
/// idempotent and preserves every other setting and hook.
///
/// `repo` Some = project scope: <repo>/.claude with a relative command,
/// so the settings file is committable and works for every teammate and
/// checkout path. None = global scope: ~/.claude, absolute command.
///
/// `strict` opts the two grep entries into the script's `-strict` modes
/// (first search of a session is denied with a sinter redirect; its retry
/// gets the session's one search nudge). Search, git-archaeology, and
/// prompt nudges are otherwise emitted at most once per session. Switching
/// strictness is idempotent: the same settings slot is replaced either way.
/// Strict uses only permissionDecision "deny" — the hooks never emit
/// "allow".
pub fn enforce(repo: Option<&Path>, strict: bool) -> Result<()> {
    let claude = match repo {
        Some(repo) => repo.canonicalize()?.join(".claude"),
        None => claude_home().ok_or_else(|| anyhow::anyhow!("cannot locate home directory"))?,
    };
    let hooks_dir = claude.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let (hook_file, hook_body) = PLATFORM_HOOK;
    let script = hooks_dir.join(hook_file);
    std::fs::write(&script, hook_body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    }
    println!("installed {}", script.display());

    let settings_path = claude.join("settings.json");
    let mut root: Value = match std::fs::read_to_string(&settings_path) {
        Ok(existing) => serde_json::from_str(&existing)
            .with_context(|| format!("{} exists but is not valid JSON", settings_path.display()))?,
        Err(_) => json!({}),
    };
    let script_str = match repo {
        Some(_) => format!(".claude/hooks/{hook_file}"),
        None => script.display().to_string(),
    };
    // Windows entries carry "shell": "powershell" so Claude Code runs the
    // script with PowerShell instead of implicit bash; the `&` call
    // operator tolerates spaces in the (quoted) global-scope path.
    let entry = |mode: &str| {
        if cfg!(windows) {
            json!({"type": "command", "shell": "powershell",
                   "command": format!("& '{script_str}' {mode}")})
        } else {
            json!({"type": "command", "command": format!("bash {script_str} {mode}")})
        }
    };
    let hooks = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} top level is not an object", settings_path.display()))?
        .entry("hooks")
        .or_insert(json!({}));
    // (event, matcher, base mode): matcher None = event-level hook (no
    // matcher key). Only the two grep modes have strict variants.
    let (grep_mode, greptool_mode) = if strict {
        ("grep-strict", "greptool-strict")
    } else {
        ("grep", "greptool")
    };
    for (event, matcher, mode) in [
        ("PreToolUse", Some("Bash"), grep_mode),
        ("PreToolUse", Some("Grep"), greptool_mode),
        ("UserPromptSubmit", None, "prompt"),
    ] {
        let groups = hooks
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("settings `hooks` is not an object"))?
            .entry(event)
            .or_insert(json!([]));
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("settings hooks.{event} is not an array"))?;
        let group = groups
            .iter_mut()
            .find(|g| g.get("matcher").and_then(Value::as_str) == matcher);
        let group = match group {
            Some(g) => g,
            None => {
                groups.push(match matcher {
                    Some(m) => json!({"matcher": m, "hooks": []}),
                    None => json!({"hooks": []}),
                });
                groups.last_mut().expect("just pushed")
            }
        };
        let list = group
            .as_object_mut()
            .and_then(|g| g.get_mut("hooks"))
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow::anyhow!("settings hooks.{event} group has no hooks array"))?;
        // Idempotent: refresh a stale entry in place (either platform's
        // variant — a repo settings.json may have been written on the
        // other OS — and either strictness: strict and non-strict share
        // one slot, so switching modes replaces rather than duplicates),
        // append when absent.
        let base = mode.strip_suffix("-strict").unwrap_or(mode);
        let ours = |c: &str| {
            c.contains("sinter-first.")
                && (c.ends_with(&format!(" {base}")) || c.ends_with(&format!(" {base}-strict")))
        };
        match list
            .iter_mut()
            .find(|h| h.get("command").and_then(Value::as_str).is_some_and(&ours))
        {
            Some(existing) => *existing = entry(mode),
            None => list.push(entry(mode)),
        }
    }
    std::fs::write(
        &settings_path,
        format!("{}\n", serde_json::to_string_pretty(&root)?),
    )?;
    println!(
        "registered enforcement hooks in {}",
        settings_path.display()
    );
    Ok(())
}

/// Dispatch install targets. Unknown names fail loudly with the list.
pub fn run_targets(
    targets: &[String],
    dir: Option<PathBuf>,
    mcp_flag: bool,
    repo: &Path,
    global: bool,
    strict: bool,
) -> Result<()> {
    let expanded: Vec<&str> = if targets.iter().any(|t| t == "all") {
        vec!["claude", "cursor", "agents", "enforce"]
    } else {
        targets.iter().map(String::as_str).collect()
    };
    for target in expanded {
        match target {
            "claude" => run(dir.clone())?,
            "cursor" => {
                let path = cursor(&repo.canonicalize()?)?;
                println!("installed {}", path.display());
            }
            "agents" => {
                let path = agents(&repo.canonicalize()?)?;
                println!("merged managed sinter block into {}", path.display());
            }
            "enforce" => enforce((!global).then_some(repo), strict)?,
            other => {
                bail!("unknown install target `{other}` (claude, cursor, agents, enforce, all)")
            }
        }
    }
    if mcp_flag {
        mcp(repo)?;
    }
    Ok(())
}

pub fn run(dir: Option<PathBuf>) -> Result<()> {
    let target = match dir.or_else(default_dir) {
        Some(dir) => dir,
        None => bail!("cannot locate home directory; pass --dir"),
    };
    std::fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
    let path = target.join("SKILL.md");
    std::fs::write(&path, SKILL).with_context(|| format!("write {}", path.display()))?;
    println!("installed {}", path.display());
    println!("rerun `sinter install` after upgrading sinter to refresh the card");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compact AGENTS block and the full skill card are separate
    /// constants; this is the seam that keeps them from drifting: every
    /// verb the compact block routes to must appear in the full card,
    /// and both must state the load-bearing behavior rules.
    #[test]
    fn agents_block_routes_match_card() {
        let card = card_body();
        let durable = [AGENTS_CARD, card];
        for command in [
            "sinter map",
            "sinter ask",
            "sinter query",
            "sinter show",
            "sinter affected",
            "sinter deps",
            "sinter path",
            "sinter unresolved",
            "sinter assert",
            "sinter cite",
            "sinter verify-doc",
            "sinter impact",
            "sinter overlap",
            "sinter workspace",
            "sinter ensure",
            "sinter doctor",
            "sinter scip",
        ] {
            for text in durable {
                assert!(text.contains(command), "durable card lost `{command}`");
            }
        }
        for text in durable {
            assert!(
                text.find("sinter map") < text.find("sinter ask"),
                "orientation must route to map before ask"
            );
            for contract in ["--explain", "--limit 0", "not_proven"] {
                assert!(text.contains(contract), "durable card lost `{contract}`");
            }
        }
        for chunk in AGENTS_CARD.split("`sinter ").skip(1) {
            let verb = chunk.split([' ', '`', '\n']).next().unwrap();
            if verb.starts_with('-') {
                continue; // flag, not a verb
            }
            assert!(
                card.contains(&format!("sinter {verb}")),
                "compact block routes `sinter {verb}` but the full card never mentions it"
            );
        }
        for rule in ["never", "unresolved", "sinter build", "--workspace"] {
            assert!(
                AGENTS_CARD.contains(rule),
                "compact block lost rule: {rule}"
            );
            assert!(card.contains(rule), "full card lost rule: {rule}");
        }
    }

    #[test]
    fn mcp_registration_is_portable_and_non_required() {
        let dir = tempfile::tempdir().unwrap();

        mcp(dir.path()).unwrap();

        let json: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(json["mcpServers"]["sinter"]["command"], "sinter");

        let codex: toml::Value = toml::from_str(
            &std::fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            codex["mcp_servers"]["sinter"]["command"].as_str(),
            Some("sinter")
        );
        assert_eq!(
            codex["mcp_servers"]["sinter"]["required"].as_bool(),
            Some(false)
        );
    }

    /// Doctor's drift check compares an installed block against these;
    /// they must describe exactly what `mcp` just wrote.
    #[test]
    fn installed_mcp_blocks_match_what_this_binary_writes() {
        let dir = tempfile::tempdir().unwrap();

        mcp(dir.path()).unwrap();

        for rel in [".mcp.json", ".cursor/mcp.json"] {
            let json: Value =
                serde_json::from_str(&std::fs::read_to_string(dir.path().join(rel)).unwrap())
                    .unwrap();
            assert_eq!(json["mcpServers"]["sinter"], mcp_entry(), "{rel}");
        }
        let codex = std::fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap();
        assert_eq!(
            codex_installed_block(&codex),
            Some(codex_block().unwrap().as_str())
        );

        // An older install's `required = true` is drift, not a match.
        let stale = codex.replace("required = false", "required = true");
        assert_ne!(
            codex_installed_block(&stale),
            Some(codex_block().unwrap().as_str())
        );
    }

    #[test]
    fn enforcement_install_writes_session_deduplicating_platform_hook() {
        let repo = tempfile::tempdir().unwrap();

        enforce(Some(repo.path()), false).unwrap();

        let (hook_name, hook_body) = PLATFORM_HOOK;
        let installed =
            std::fs::read_to_string(repo.path().join(".claude").join("hooks").join(hook_name))
                .unwrap();
        assert_eq!(installed, hook_body);
        assert!(ENFORCE_HOOK.contains("mark_session_once"));
        assert!(ENFORCE_HOOK_PS1.contains("New-SessionMarker"));
        assert!(!ENFORCE_HOOK.contains("permissionDecision\":\"allow"));
        assert!(!ENFORCE_HOOK_PS1.contains("permissionDecision\":\"allow"));
        assert!(enforcement_current_at(&repo.path().join(".claude"), false));
        assert!(!enforcement_current_at(&repo.path().join(".claude"), true));
        enforce(Some(repo.path()), true).unwrap();
        assert!(enforcement_current_at(&repo.path().join(".claude"), true));
    }

    #[test]
    fn enforcement_hooks_route_agent_first_workflows() {
        for hook in [ENFORCE_HOOK, ENFORCE_HOOK_PS1] {
            for contract in [
                "context",
                "assert no-callers",
                "holds_for_indexed_snapshot",
                "universe/limitations",
                "cite/verify-doc",
                "unresolved for graph gaps",
            ] {
                assert!(
                    hook.contains(contract),
                    "enforcement hook lost `{contract}`"
                );
            }
            assert!(
                !hook.contains("unresolved for negative proofs"),
                "enforcement hook must not route production-caller proofs to unresolved"
            );
        }
    }

    #[test]
    fn agents_install_refreshes_the_card_and_connects_claude() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("AGENTS.md"),
            format!("before\n\n{AGENTS_BEGIN}\n\nstale\n{AGENTS_END}\n"),
        )
        .unwrap();
        std::fs::write(
            repo.path().join("CLAUDE.md"),
            "Follow `.rustkit/AGENTS.md` for Rust engineering work.\n",
        )
        .unwrap();

        agents(repo.path()).unwrap();

        let installed = std::fs::read_to_string(repo.path().join("AGENTS.md")).unwrap();
        assert!(installed.starts_with("before\n\n"));
        assert!(block_current(&installed));
        assert!(!installed.contains("\nstale\n"));

        let claude = std::fs::read_to_string(repo.path().join("CLAUDE.md")).unwrap();
        assert_eq!(
            claude,
            "Follow `.rustkit/AGENTS.md` for Rust engineering work.\n\n@AGENTS.md\n"
        );

        agents(repo.path()).unwrap();
        let claude = std::fs::read_to_string(repo.path().join("CLAUDE.md")).unwrap();
        assert_eq!(claude.matches("@AGENTS.md").count(), 1);
    }
}
