//! Release-availability check. Network policy: exactly one HEAD request
//! to github.com, made only by `sinter doctor`, only on a terminal, at
//! most once per 24h, disabled by SINTER_NO_UPDATE_CHECK=1. Every other
//! command reads the cached answer and never touches the network.

use std::path::PathBuf;
use std::process::Command;

/// `<cache>/sinter/latest-release` holds the last-seen tag; its mtime is
/// the check timestamp.
fn cache_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".cache"))
        })?;
    Some(base.join("sinter").join("latest-release"))
}

fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().trim_start_matches('v').splitn(3, '.');
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

/// The cached latest version when it is strictly newer than this binary.
/// Cache read only — no network, safe on every command.
pub fn cached_newer() -> Option<String> {
    let cached = std::fs::read_to_string(cache_file()?).ok()?;
    let latest = parse_semver(&cached)?;
    let running = parse_semver(env!("CARGO_PKG_VERSION"))?;
    (latest > running).then(|| cached.trim().to_string())
}

/// One `curl -sI` HEAD to the stable /releases/latest URL; the redirect
/// Location names the tag.
fn fetch_latest_tag() -> Option<String> {
    let out = Command::new("curl")
        .args([
            "-sI",
            "--max-time",
            "4",
            "https://github.com/shellfu/sinter/releases/latest",
        ])
        .output()
        .ok()?;
    let headers = String::from_utf8_lossy(&out.stdout);
    headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("location:"))
        .and_then(|l| l.rsplit('/').next())
        .map(str::trim)
        .filter(|t| parse_semver(t).is_some())
        .map(str::to_string)
}

fn write_cache(tag: &str) {
    let Some(path) = cache_file() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, format!("{tag}\n"));
}

/// Refresh the cache when it is older than 24h.
/// Silent on any failure — an update check must never break a command.
pub fn refresh_cache() {
    if std::env::var_os("SINTER_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let Some(path) = cache_file() else { return };
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < 24 * 3600);
    if fresh {
        return;
    }
    if let Some(tag) = fetch_latest_tag() {
        write_cache(&tag);
    }
}

/// Release target triple for this platform, matching the published asset
/// matrix. None off the matrix (build from source there).
fn target_for(os: &str, arch: &str) -> Option<String> {
    let suffix = match os {
        "linux" => "unknown-linux-musl",
        "macos" => "apple-darwin",
        "windows" => "pc-windows-msvc",
        _ => return None,
    };
    matches!(arch, "x86_64" | "aarch64").then(|| format!("{arch}-{suffix}"))
}

/// Expected hash from a `<sha256>  <name>` checksum file, verified to
/// name the asset it covers.
fn parse_checksum_line(line: &str, asset: &str) -> Option<String> {
    let mut it = line.split_whitespace();
    let hash = it.next()?;
    let name = it.next()?;
    (name == asset && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| hash.to_ascii_lowercase())
}

/// Hex sha256 of a file via the platform's tool (no hash dependency —
/// matches this module's curl approach).
fn sha256_of(path: &std::path::Path) -> anyhow::Result<String> {
    let attempts: &[(&str, &[&str])] = if cfg!(windows) {
        // certutil prints the hash on its second output line.
        &[("certutil", &["-hashfile"])]
    } else {
        &[("sha256sum", &[]), ("shasum", &["-a", "256"])]
    };
    for (cmd, args) in attempts {
        let mut c = Command::new(cmd);
        c.args(*args).arg(path);
        if cfg!(windows) {
            c.arg("SHA256");
        }
        let Ok(out) = c.output() else { continue };
        if !out.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let hash = text
            .split_whitespace()
            .find(|w| w.len() == 64 && w.chars().all(|c| c.is_ascii_hexdigit()));
        if let Some(h) = hash {
            return Ok(h.to_ascii_lowercase());
        }
    }
    anyhow::bail!(
        "cannot verify the downloaded release: no sha256 tool found (need sha256sum, shasum, or certutil)"
    )
}

const INSTALL_HINT: &str = if cfg!(windows) {
    "irm https://raw.githubusercontent.com/shellfu/sinter/main/scripts/install.ps1 | iex"
} else {
    "curl -fsSL https://raw.githubusercontent.com/shellfu/sinter/main/scripts/install.sh | sh"
};

fn download(url: &str, dest: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context;
    let status = Command::new("curl")
        .arg("-fsSL")
        .arg("--proto")
        .arg("=https")
        .arg("-o")
        .arg(dest)
        .arg(url)
        .status()
        .context("run curl")?;
    anyhow::ensure!(status.success(), "download failed: {url}");
    Ok(())
}

/// Swap the new binary in for the running one. The final step is always a
/// same-directory rename, so a crash mid-update can never leave a
/// half-written `sinter` on PATH.
fn replace_exe(new_bin: &std::path::Path, exe: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context;
    let dir = exe.parent().context("current_exe has no parent")?;
    let staged = dir.join(format!(".sinter-update-{}", std::process::id()));
    let writable_hint = || {
        format!(
            "{} is not writable — reinstall with: {INSTALL_HINT}",
            dir.display()
        )
    };
    std::fs::copy(new_bin, &staged).with_context(writable_hint)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    let result = (|| -> anyhow::Result<()> {
        #[cfg(windows)]
        {
            // Windows cannot overwrite a running exe but CAN rename it: the
            // standard self-update dance is rename-aside then rename-in.
            // The .old leftover is unlinked on the next `sinter update`
            // (it stays locked until this process exits).
            let old = dir.join("sinter.exe.old");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(exe, &old).with_context(writable_hint)?;
        }
        std::fs::rename(&staged, exe).with_context(writable_hint)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// `sinter update`: refresh the release cache unconditionally, then
/// download, verify, and atomically install the newer binary over
/// `current_exe`. `--dry-run` reports from the cache and downloads
/// nothing (offline by contract, so it is testable without a network).
pub fn run(dry_run: bool) -> anyhow::Result<()> {
    use anyhow::Context;
    if std::env::var_os("SINTER_NO_UPDATE_CHECK").is_some() {
        anyhow::bail!(
            "update check is disabled (SINTER_NO_UPDATE_CHECK is set) — unset it to use `sinter update`"
        );
    }
    // Best-effort cleanup of a previous Windows update's renamed-aside exe.
    if cfg!(windows)
        && let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let _ = std::fs::remove_file(dir.join("sinter.exe.old"));
    }
    let latest = if dry_run {
        cache_file()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.trim().to_string())
            .filter(|t| parse_semver(t).is_some())
            .context("no cached release info — run `sinter update` without --dry-run")?
    } else {
        let tag = fetch_latest_tag()
            .context("could not determine the latest release (is github.com reachable?)")?;
        write_cache(&tag);
        tag
    };
    let running = env!("CARGO_PKG_VERSION");
    if parse_semver(&latest) <= parse_semver(running) {
        println!("sinter {running} is current");
        return Ok(());
    }
    let target = target_for(std::env::consts::OS, std::env::consts::ARCH)
        .with_context(|| {
            format!(
                "no prebuilt release for {}-{} — build from source: cargo install --git https://github.com/shellfu/sinter sinter-cli",
                std::env::consts::ARCH,
                std::env::consts::OS
            )
        })?;
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    let asset = format!("sinter-{target}.{ext}");
    let url = format!("https://github.com/shellfu/sinter/releases/latest/download/{asset}");
    let exe = std::env::current_exe().context("locate current executable")?;
    if dry_run {
        println!("sinter {running} → {latest}");
        println!("would download {url}");
        println!("would verify {asset}.sha256 and replace {}", exe.display());
        return Ok(());
    }

    let tmp = std::env::temp_dir().join(format!("sinter-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let result = (|| -> anyhow::Result<()> {
        let archive = tmp.join(&asset);
        println!("downloading {asset} ...");
        download(&url, &archive)?;
        download(&format!("{url}.sha256"), &tmp.join("expected.sha256"))?;
        let expected_line = std::fs::read_to_string(tmp.join("expected.sha256"))?;
        let expected = parse_checksum_line(&expected_line, &asset)
            .with_context(|| format!("malformed checksum file: {expected_line:?}"))?;
        anyhow::ensure!(
            sha256_of(&archive)? == expected,
            "checksum mismatch for {asset} — refusing to install"
        );
        // bsdtar (`tar` on Windows 10+) extracts zip too, so one command
        // covers both archive formats.
        let status = Command::new("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&tmp)
            .status()
            .context("run tar")?;
        anyhow::ensure!(status.success(), "could not extract {asset}");
        let bin = tmp.join(if cfg!(windows) {
            "sinter.exe"
        } else {
            "sinter"
        });
        anyhow::ensure!(bin.is_file(), "archive did not contain a sinter binary");
        replace_exe(&bin, &exe)?;
        println!("sinter {running} → {latest}");
        println!("updated {}", exe.display());
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::{parse_checksum_line, parse_semver, target_for};

    #[test]
    fn target_selection_covers_the_release_matrix() {
        assert_eq!(
            target_for("linux", "x86_64").as_deref(),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            target_for("linux", "aarch64").as_deref(),
            Some("aarch64-unknown-linux-musl")
        );
        assert_eq!(
            target_for("macos", "x86_64").as_deref(),
            Some("x86_64-apple-darwin")
        );
        assert_eq!(
            target_for("macos", "aarch64").as_deref(),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(
            target_for("windows", "x86_64").as_deref(),
            Some("x86_64-pc-windows-msvc")
        );
        assert_eq!(
            target_for("windows", "aarch64").as_deref(),
            Some("aarch64-pc-windows-msvc")
        );
        assert_eq!(target_for("freebsd", "x86_64"), None);
        assert_eq!(target_for("linux", "riscv64"), None);
    }

    #[test]
    fn checksum_line_parses_and_rejects() {
        let hash = "a".repeat(64);
        let asset = "sinter-x86_64-unknown-linux-musl.tar.gz";
        assert_eq!(
            parse_checksum_line(&format!("{hash}  {asset}\n"), asset).as_deref(),
            Some(hash.as_str())
        );
        // Uppercase hashes normalize; single-space separators parse.
        assert_eq!(
            parse_checksum_line(&format!("{} {asset}", hash.to_uppercase()), asset).as_deref(),
            Some(hash.as_str())
        );
        // Wrong asset name, short hash, non-hex, empty: all refused.
        assert_eq!(
            parse_checksum_line(&format!("{hash}  other.tar.gz"), asset),
            None
        );
        assert_eq!(
            parse_checksum_line(&format!("abc123  {asset}"), asset),
            None
        );
        assert_eq!(
            parse_checksum_line(&format!("{}  {asset}", "z".repeat(64)), asset),
            None
        );
        assert_eq!(parse_checksum_line("", asset), None);
    }

    #[test]
    fn semver_parses_and_orders() {
        assert_eq!(parse_semver("v0.36.0"), Some((0, 36, 0)));
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
        assert!(parse_semver("v0.36.0") < parse_semver("v0.36.1"));
        assert!(parse_semver("v0.9.0") < parse_semver("v0.36.0"));
        assert_eq!(parse_semver("latest"), None);
        assert_eq!(parse_semver(""), None);
    }
}
