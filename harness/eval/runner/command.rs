use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::model::RepositorySpec;

pub fn clone_repository(spec: &RepositorySpec, destination: &Path) -> Result<()> {
    let output = Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--branch",
            &spec.git_ref,
            "--single-branch",
            &spec.url,
        ])
        .arg(destination)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .with_context(|| format!("failed to start git clone for {}", spec.name))?;
    require_success("git clone", &output)?;

    let output = Command::new("git")
        .arg("-C")
        .arg(destination)
        .args(["rev-parse", "HEAD"])
        .output()
        .with_context(|| format!("failed to inspect cloned repository {}", spec.name))?;
    require_success("git rev-parse HEAD", &output)?;
    let actual = String::from_utf8(output.stdout)
        .context("git rev-parse returned non-UTF-8 output")?
        .trim()
        .to_owned();
    if actual != spec.commit {
        bail!(
            "repository {} resolved {} to {}, expected {}",
            spec.name,
            spec.git_ref,
            actual,
            spec.commit
        );
    }
    Ok(())
}

pub fn build_graph(sinter: &Path, repository: &Path) -> Result<Duration> {
    let started = Instant::now();
    let output = Command::new(sinter)
        .arg("build")
        .arg(repository)
        .output()
        .context("failed to start sinter build")?;
    require_success("sinter build", &output)?;
    Ok(started.elapsed())
}

pub fn run_json(sinter: &Path, args: &[String]) -> Result<serde_json::Value> {
    let output = Command::new(sinter)
        .args(args)
        .output()
        .with_context(|| format!("failed to start sinter {}", args.join(" ")))?;
    let code = output.status.code().unwrap_or(-1);
    if code != 0 && code != 1 {
        bail!(
            "sinter {} exited {code}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "sinter {} returned invalid JSON\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn require_success(label: &str, output: &Output) -> Result<()> {
    if !output.status.success() {
        bail!(
            "{label} failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
