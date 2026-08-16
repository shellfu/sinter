//! `sinter install`: write the Claude Code skill card. The card ships
//! embedded in the binary so integration text can never drift from the
//! tool's actual verbs — rerun after upgrading to refresh it.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

const SKILL: &str = include_str!("../skill/SKILL.md");

pub fn run(dir: Option<PathBuf>) -> Result<()> {
    let target = match dir {
        Some(dir) => dir,
        None => {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from);
            match home {
                Some(home) => home.join(".claude").join("skills").join("sinter"),
                None => bail!("cannot locate home directory; pass --dir"),
            }
        }
    };
    std::fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
    let path = target.join("SKILL.md");
    std::fs::write(&path, SKILL).with_context(|| format!("write {}", path.display()))?;
    println!("installed {}", path.display());
    println!("rerun `sinter install` after upgrading sinter to refresh the card");
    Ok(())
}
