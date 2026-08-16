//! `sinter install`: write the Claude Code skill card. The card ships
//! embedded in the binary so integration text can never drift from the
//! tool's actual verbs — rerun after upgrading to refresh it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

pub const SKILL: &str = include_str!("../skill/SKILL.md");

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
