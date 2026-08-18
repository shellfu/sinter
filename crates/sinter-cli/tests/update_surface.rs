//! Acceptance for `sinter update --dry-run`: reads the release cache only
//! (no network, no downloads) — driven by a fake cache file under a
//! hermetic HOME/XDG_CACHE_HOME.

use std::path::Path;
use std::process::Command;

fn update(home: &Path, cache_tag: Option<&str>, extra_env: &[(&str, &str)]) -> (bool, String) {
    if let Some(tag) = cache_tag {
        let dir = home.join("cache/sinter");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("latest-release"), format!("{tag}\n")).unwrap();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sinter"));
    cmd.args(["update", "--dry-run"])
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("SINTER_NO_UPDATE_CHECK");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run sinter");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn dry_run_reports_newer_release_without_downloading() {
    let dir = tempfile::tempdir().unwrap();
    let (ok, out) = update(dir.path(), Some("v99.0.0"), &[]);
    assert!(ok, "{out}");
    assert!(
        out.contains(&format!("sinter {} → 99.0.0", env!("CARGO_PKG_VERSION")))
            || out.contains("v99.0.0"),
        "{out}"
    );
    assert!(out.contains("would download"), "{out}");
    assert!(
        out.contains("https://github.com/shellfu/sinter/releases/latest/download/sinter-"),
        "{out}"
    );
    assert!(out.contains(".sha256"), "{out}");
    assert!(out.contains("would verify"), "{out}");
    // Download nothing: the only sinter artifact under HOME is the cache file.
    let entries: Vec<_> = std::fs::read_dir(dir.path().join("cache/sinter"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries, ["latest-release"], "{out}");
}

#[test]
fn dry_run_says_current_when_cache_matches_running_version() {
    let dir = tempfile::tempdir().unwrap();
    let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    let (ok, out) = update(dir.path(), Some(&tag), &[]);
    assert!(ok, "{out}");
    assert!(
        out.contains(&format!("sinter {} is current", env!("CARGO_PKG_VERSION"))),
        "{out}"
    );
}

#[test]
fn update_refuses_when_check_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let (ok, out) = update(
        dir.path(),
        Some("v99.0.0"),
        &[("SINTER_NO_UPDATE_CHECK", "1")],
    );
    assert!(!ok, "{out}");
    assert!(out.contains("SINTER_NO_UPDATE_CHECK"), "{out}");
}

#[test]
fn dry_run_without_cache_names_the_fix() {
    let dir = tempfile::tempdir().unwrap();
    let (ok, out) = update(dir.path(), None, &[]);
    assert!(!ok, "{out}");
    assert!(out.contains("no cached release info"), "{out}");
}
