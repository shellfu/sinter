//! `sinter install`: write the Claude Code skill card. The card ships
//! embedded in the binary so integration text can never drift from the
//! tool's actual verbs — rerun after upgrading to refresh it.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

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
