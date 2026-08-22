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
    stage_overlay(destination)?;
    git(destination, &["commit", "-qm", "agent-flow fixture change"])?;
    Ok(())
}

fn stage_overlay(repository: &Path) -> Result<()> {
    // Some copy implementations preserve an overlay file's size and mtime.
    // Git may then trust its index stat cache and skip hashing changed content.
    // Renormalizing forces tracked files through the clean/hash path; the
    // ordinary add that follows still picks up files newly added by an overlay.
    git(repository, &["add", "--renormalize", "."])?;
    git(repository, &["add", "."])
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

#[cfg(test)]
mod tests {
    use std::fs::{FileTimes, OpenOptions};
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    #[test]
    fn stage_overlay_rehashes_same_size_content_with_unchanged_mtime() -> Result<()> {
        let scratch = tempfile::tempdir()?;
        let repository = scratch.path();
        let tracked = repository.join("tracked.rs");
        let fixed_time = UNIX_EPOCH + Duration::from_secs(946_684_800);

        fs::write(&tracked, "pub fn value() -> u8 { 1 }\n")?;
        set_modified(&tracked, fixed_time)?;
        git(repository, &["init", "-q"])?;
        git(repository, &["add", "."])?;
        git(repository, &["commit", "-qm", "base"])?;
        git(repository, &["config", "core.trustctime", "false"])?;
        git(repository, &["config", "core.checkstat", "minimal"])?;
        git(repository, &["config", "core.ignorecase", "true"])?;

        fs::write(&tracked, "pub fn value() -> u8 { 2 }\n")?;
        set_modified(&tracked, fixed_time)?;
        git(repository, &["add", "."])?;
        assert!(git_exit_success(
            repository,
            &["diff", "--cached", "--quiet"]
        ));

        stage_overlay(repository)?;

        assert!(!git_exit_success(
            repository,
            &["diff", "--cached", "--quiet"]
        ));
        git(repository, &["commit", "-qm", "same-size overlay"])?;
        Ok(())
    }

    fn set_modified(path: &Path, modified: std::time::SystemTime) -> Result<()> {
        OpenOptions::new()
            .write(true)
            .open(path)?
            .set_times(FileTimes::new().set_modified(modified))?;
        Ok(())
    }

    fn git_exit_success(repository: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(repository)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .is_ok_and(|status| status.success())
    }
}
