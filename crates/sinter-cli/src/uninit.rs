//! `sinter uninit`: the inverse of `sinter init`. Removes every artifact
//! sinter manages in a repo — graph, git-hook lines, managed blocks, MCP
//! entries, enforcement hooks — touching nothing else. `--global` also
//! removes the machine-level skill card and enforcement wiring.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use crate::install;

pub fn run(repo: &Path, global: bool) -> Result<bool> {
    let repo = crate::pipeline::discover_root(repo);
    let repo = repo.canonicalize()?;

    // Graph (derived state — always safe to delete).
    let sinter_dir = repo.join(".sinter");
    if sinter_dir.exists() {
        std::fs::remove_dir_all(&sinter_dir)?;
        println!("removed {}", sinter_dir.display());
    }

    // Git hooks: strip only the managed line pair, delete a hook file that
    // was ours alone (shebang + managed lines and nothing else).
    const MARKER: &str = "# managed by `sinter hooks install`";
    for hook in ["post-commit", "post-checkout", "post-merge"] {
        let path = repo.join(".git/hooks").join(hook);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !content.contains(MARKER) {
            continue;
        }
        let kept: Vec<&str> = content
            .lines()
            .filter(|l| !l.contains(MARKER) && !l.contains("sinter build"))
            .collect();
        if kept
            .iter()
            .all(|l| l.trim().is_empty() || l.starts_with("#!"))
        {
            std::fs::remove_file(&path)?;
            println!("removed {}", path.display());
        } else {
            std::fs::write(&path, format!("{}\n", kept.join("\n").trim_end()))?;
            println!("removed sinter lines from {}", path.display());
        }
    }

    // Managed marker blocks: AGENTS.md and .codex/config.toml.
    strip_block(
        &repo.join("AGENTS.md"),
        install::AGENTS_BEGIN,
        install::AGENTS_END,
    )?;
    strip_block(
        &repo.join(".codex/config.toml"),
        install::CODEX_BEGIN,
        install::CODEX_END,
    )?;

    // Cursor rule file.
    remove_file(&repo.join(".cursor/rules/sinter.mdc"))?;

    // MCP registrations: drop only the sinter server entry.
    for rel in [".mcp.json", ".cursor/mcp.json"] {
        let path = repo.join(rel);
        let Some(mut root) = read_json(&path) else {
            continue;
        };
        let Some(servers) = root.get_mut("mcpServers").and_then(Value::as_object_mut) else {
            continue;
        };
        if servers.remove("sinter").is_none() {
            continue;
        }
        let empty_servers = servers.is_empty();
        if empty_servers && root.as_object().is_some_and(|o| o.len() == 1) {
            std::fs::remove_file(&path)?;
            println!("removed {}", path.display());
        } else {
            std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&root)?))?;
            println!("removed sinter MCP entry from {}", path.display());
        }
    }

    // Enforcement: repo scope always; global scope on request.
    remove_enforcement(&repo.join(".claude"))?;
    if global {
        if let Some(dir) = install::default_dir()
            && dir.exists()
        {
            std::fs::remove_dir_all(&dir)?;
            println!("removed {}", dir.display());
        }
        if let Some(claude) = install::claude_home() {
            remove_enforcement(&claude)?;
        }
    }

    // Empty managed directories left behind.
    for rel in [
        ".cursor/rules",
        ".cursor",
        ".codex",
        ".claude/hooks",
        ".claude",
    ] {
        let _ = std::fs::remove_dir(repo.join(rel)); // fails when non-empty — correct
    }

    println!("uninit complete");
    if !global {
        println!("(global skill card and ~/.claude hooks untouched; rerun with --global)");
    }
    Ok(true)
}

/// Remove the enforcement hook script and its three settings entries from
/// one .claude directory, pruning what becomes empty and deleting nothing
/// that is not ours.
fn remove_enforcement(claude: &Path) -> Result<()> {
    remove_file(&claude.join("hooks/sinter-first.sh"))?;
    remove_file(&claude.join("hooks/sinter-first.ps1"))?;
    let settings_path = claude.join("settings.json");
    let Some(mut root) = read_json(&settings_path) else {
        return Ok(());
    };
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let mut changed = false;
    for event in ["PreToolUse", "UserPromptSubmit"] {
        let Some(groups) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = list.len();
                list.retain(|h| {
                    !h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|c| c.contains("sinter-first."))
                });
                changed |= list.len() != before;
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|l| !l.is_empty())
        });
        if groups.is_empty() {
            hooks.remove(event);
        }
    }
    if !changed {
        return Ok(());
    }
    if hooks.is_empty() {
        root.as_object_mut()
            .expect("checked object")
            .remove("hooks");
    }
    if root.as_object().is_some_and(serde_json::Map::is_empty) {
        std::fs::remove_file(&settings_path)?;
        println!("removed {}", settings_path.display());
    } else {
        std::fs::write(
            &settings_path,
            format!("{}\n", serde_json::to_string_pretty(&root)?),
        )?;
        println!("removed sinter hooks from {}", settings_path.display());
    }
    Ok(())
}

/// Strip a managed marker block; delete the file when nothing else remains.
fn strip_block(path: &Path, begin: &str, end: &str) -> Result<()> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let (Some(start), Some(stop)) = (content.find(begin), content.find(end)) else {
        return Ok(());
    };
    if stop <= start {
        return Ok(());
    }
    let remainder = format!("{}{}", &content[..start], &content[stop + end.len()..]);
    if remainder.trim().is_empty() {
        std::fs::remove_file(path)?;
        println!("removed {}", path.display());
    } else {
        std::fs::write(path, format!("{}\n", remainder.trim_end()))?;
        println!("removed sinter block from {}", path.display());
    }
    Ok(())
}

fn remove_file(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
        println!("removed {}", path.display());
    }
    Ok(())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}
