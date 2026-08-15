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
    for hook in ["post-commit", "post-checkout", "post-merge"] {
        let path = hooks_dir.join(hook);
        let script = "#!/bin/sh\n# installed by `sinter hooks install`\nsinter build . >/dev/null 2>&1 || true\n";
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
