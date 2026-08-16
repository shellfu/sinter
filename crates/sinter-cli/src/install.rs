//! `sinter install`: write the Claude Code skill card. The card ships
//! embedded in the binary so integration text can never drift from the
//! tool's actual verbs — rerun after upgrading to refresh it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

pub const SKILL: &str = include_str!("../skill/SKILL.md");

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
commit or edit it). For any codebase-structure question, query it before
grepping; results are ranked, scoped, and content-bearing.

| Question | Command |
|---|---|
| Vague/conceptual: "where is X handled" | `sinter ask "<question>"` |
| Orient on a symbol (signature, docs, callers) | `sinter show <symbol>` |
| What depends on X / blast radius | `sinter affected <symbol>` |
| How does A reach B | `sinter path <A> <B>` |
| What does this diff/PR affect | `sinter impact <rev-range>` |

- After modifying code: `sinter build` (fast no-op when fresh; skip if
  git hooks or `sinter watch` are active).
- "unresolved" and candidate lists are real answers — refine and rerun,
  never guess a binding.
- Cross-repo workspace? Add `--workspace <manifest.toml>`; symbols may
  be `member:Symbol`.
- Anything else: `sinter --help`; graph problems: `sinter doctor`.
"#;

const AGENTS_BEGIN: &str =
    "<!-- BEGIN sinter (managed by `sinter install`; edits inside are overwritten) -->";
const AGENTS_END: &str = "<!-- END sinter -->";

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

/// Merge the sinter server into a repo's project-scope `.mcp.json` —
/// the file Claude Code reads natively. Other servers are preserved;
/// only the "sinter" entry is written. Global client configs belong to
/// their applications and are never edited here.
pub fn mcp(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;
    let path = repo.join(".mcp.json");
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
    println!("(project scope; for Claude Desktop or other clients, add the equivalent");
    println!(" command `sinter serve --repo <repo>` to that client's own MCP config)");
    Ok(())
}

/// Dispatch `--for` targets. Unknown names fail loudly with the list.
pub fn run_targets(
    targets: &[String],
    dir: Option<PathBuf>,
    mcp_flag: bool,
    repo: &Path,
) -> Result<()> {
    let expanded: Vec<&str> = if targets.iter().any(|t| t == "all") {
        vec!["claude", "cursor", "agents"]
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
            other => bail!("unknown install target `{other}` (claude, cursor, agents, all)"),
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
