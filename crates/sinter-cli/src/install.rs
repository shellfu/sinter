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

/// Compact always-in-context block for AGENTS.md. Deliberately smaller
/// than the skill card (which loads on demand): an always-on block gets
/// skimmed, so it carries only the behavior rules and routing. Keep the
/// two in sync when verbs change — `agents_block_routes_match_card`
/// enforces the command surface.
const AGENTS_CARD: &str = r#"## sinter

This repo has a code knowledge graph at `.sinter/` (derived state — never
commit or edit it). When `.sinter/graph.redb` exists, query sinter BEFORE
any broad filesystem search for symbol location, callers, dependency
impact, structural paths, or diff impact. Fall back to grep only when
sinter returns no usable evidence; read source directly for
function-body behavior.

| Question | Command |
|---|---|
| Vague/conceptual: "where is X handled" | `sinter ask "<question>"` |
| Orient on a symbol (signature, docs, callers) | `sinter show <symbol>` |
| What depends on X / blast radius | `sinter affected <symbol>` |
| How does A reach B | `sinter path <A> <B>` |
| What does this commit/diff/PR affect downstream | `sinter impact <rev-range>` (e.g. `HEAD~1..HEAD`) |

- Queries self-sync before answering — no manual refresh needed
  (`sinter build` remains for CI/scripts; git hooks refresh on commit).
- "unresolved" and candidate lists are real answers — refine and rerun,
  never guess a binding.
- Cross-repo workspace? Add `--workspace <manifest.toml>`; symbols may
  be `member:Symbol`.
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
/// block in `.codex/config.toml` (Codex, `required = true` so sessions
/// start with a working server). Other entries are preserved; only the
/// sinter entry is written. Global client configs belong to their
/// applications and are never edited here.
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
        root["mcpServers"]["sinter"] = json!({
            "command": "sinter",
            "args": ["serve", "--repo", "."],
        });
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

pub(crate) const CODEX_BEGIN: &str =
    "# BEGIN sinter (managed by `sinter install`; edits inside are overwritten)";
pub(crate) const CODEX_END: &str = "# END sinter";

/// Merge a managed sinter server block into `.codex/config.toml` (marker
/// replacement, same convention as the AGENTS.md block — no TOML parser
/// needed for an append-or-replace of our own block).
fn codex_mcp(repo: &Path) -> Result<()> {
    let dir = repo.join(".codex");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let block = format!(
        "{CODEX_BEGIN}
[mcp_servers.sinter]
command = \"sinter\"
args = [\"serve\", \"--repo\", \".\"]
required = true
{CODEX_END}"
    );
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

/// Claude Code home (`~/.claude`), shared with the skill install.
pub(crate) fn claude_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".claude"))
}

/// Install Claude Code enforcement: the sinter-first hook script plus the
/// three settings entries that fire it (per-prompt router, Bash grep
/// nudge, Grep-tool nudge). The script gates on `.sinter/graph.redb`
/// existing, so hooks stay silent in graph-less repos. Merging is
/// idempotent and preserves every other setting and hook.
///
/// `repo` Some = project scope: <repo>/.claude with a relative command,
/// so the settings file is committable and works for every teammate and
/// checkout path. None = global scope: ~/.claude, absolute command.
pub fn enforce(repo: Option<&Path>) -> Result<()> {
    let claude = match repo {
        Some(repo) => repo.canonicalize()?.join(".claude"),
        None => claude_home().ok_or_else(|| anyhow::anyhow!("cannot locate home directory"))?,
    };
    let hooks_dir = claude.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let script = hooks_dir.join("sinter-first.sh");
    std::fs::write(&script, ENFORCE_HOOK)?;
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
        Some(_) => ".claude/hooks/sinter-first.sh".to_string(),
        None => script.display().to_string(),
    };
    let entry =
        |mode: &str| json!({"type": "command", "command": format!("bash {script_str} {mode}")});
    let hooks = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} top level is not an object", settings_path.display()))?
        .entry("hooks")
        .or_insert(json!({}));
    // (event, matcher, mode): matcher None = event-level hook (no matcher key).
    for (event, matcher, mode) in [
        ("PreToolUse", Some("Bash"), "grep"),
        ("PreToolUse", Some("Grep"), "greptool"),
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
        let marker = format!("sinter-first.sh {mode}");
        // Idempotent: refresh a stale entry in place, append when absent.
        match list.iter_mut().find(|h| {
            h.get("command")
                .and_then(Value::as_str)
                .is_some_and(|c| c.contains(&marker))
        }) {
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
            "enforce" => enforce((!global).then_some(repo))?,
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
}
