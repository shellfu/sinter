use std::path::Path;

use anyhow::{Context, Result, bail};

/// `sinter hooks install`: git hooks that run an incremental build after
/// history-changing operations. The build itself diffs content hashes, so
/// the hook body stays a one-liner and branch switches never full-rebuild.
pub fn install(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;
    let hooks_dir = repo.join(".git").join("hooks");
    if !hooks_dir.parent().is_some_and(|g| g.exists()) {
        bail!("{} is not a git repository", repo.display());
    }
    std::fs::create_dir_all(&hooks_dir)?;
    const MARKER: &str = "# managed by `sinter hooks install`";
    let line = format!("{MARKER}\nsinter build . >/dev/null 2>&1 || true\n");
    for hook in ["post-commit", "post-checkout", "post-merge", "post-rewrite"] {
        let path = hooks_dir.join(hook);
        // Never clobber a user's existing hook: append the managed line to
        // it instead; rerunning is a no-op once the marker is present.
        let script = match std::fs::read_to_string(&path) {
            Ok(existing) if existing.contains(MARKER) => {
                println!("already installed {}", path.display());
                continue;
            }
            Ok(existing) => format!("{}\n{line}", existing.trim_end()),
            Err(_) => format!("#!/bin/sh\n{line}"),
        };
        std::fs::write(&path, script).with_context(|| format!("write {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
        println!("installed {}", path.display());
    }
    Ok(())
}
