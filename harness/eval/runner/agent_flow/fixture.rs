use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::super::model::AgentFixtureSpec;

pub fn prepare(workspace: &Path, fixture: &AgentFixtureSpec, destination: &Path) -> Result<()> {
    let base = fixture_source(workspace, &fixture.base)?;
    let overlay = fixture_source(workspace, &fixture.committed_overlay)?;
    copy_tree(&base, destination)?;
    git(destination, &["init", "-q"])?;
    git(destination, &["add", "."])?;
    git(destination, &["commit", "-qm", "agent-flow base"])?;
    copy_tree(&overlay, destination)?;
    git(destination, &["add", "."])?;
    git(destination, &["commit", "-qm", "agent-flow fixture change"])?;
    Ok(())
}

fn fixture_source(workspace: &Path, relative: &str) -> Result<PathBuf> {
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("failed to resolve workspace {}", workspace.display()))?;
    let source = safe_join(&workspace, &format!("harness/eval/{relative}"))?
        .canonicalize()
        .with_context(|| format!("failed to resolve fixture source {relative}"))?;
    if !source.starts_with(&workspace) {
        bail!("fixture source escapes the workspace: {relative:?}");
    }
    Ok(source)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    for entry in fs::read_dir(source)
        .with_context(|| format!("failed to read fixture directory {}", source.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_symlink() {
            bail!(
                "fixture {} must not be a symbolic link",
                entry.path().display()
            );
        } else if file_type.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "failed to copy fixture {} to {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        } else {
            bail!("fixture {} is not a regular file", entry.path().display());
        }
    }
    Ok(())
}

fn git(repository: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository)
        .env("GIT_AUTHOR_NAME", "Sinter Evaluation")
        .env("GIT_AUTHOR_EMAIL", "eval@sinter.invalid")
        .env("GIT_COMMITTER_NAME", "Sinter Evaluation")
        .env("GIT_COMMITTER_EMAIL", "eval@sinter.invalid")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative_path(relative)?;
    Ok(root.join(relative))
}

fn validate_relative_path(relative: &str) -> Result<()> {
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        bail!("path must stay within the evaluation root: {relative:?}");
    }
    Ok(())
}
