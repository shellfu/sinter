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

/// Refresh the cache when it is older than 24h. One `curl -sI` HEAD to
/// the stable /releases/latest URL; the redirect Location names the tag.
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
    let Ok(out) = Command::new("curl")
        .args([
            "-sI",
            "--max-time",
            "4",
            "https://github.com/shellfu/sinter/releases/latest",
        ])
        .output()
    else {
        return;
    };
    let headers = String::from_utf8_lossy(&out.stdout);
    let Some(tag) = headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("location:"))
        .and_then(|l| l.rsplit('/').next())
        .map(str::trim)
        .filter(|t| parse_semver(t).is_some())
    else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, format!("{tag}\n"));
}

#[cfg(test)]
mod tests {
    use super::parse_semver;

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
