//! Event-driven freshness for the long-lived MCP server. One-shot CLI
//! commands still scan at their query boundary; a server reuses a clean
//! generation until notify reports a repository change.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};

use crate::pipeline;

/// Server-lifetime owner of the filesystem watcher. The callback owns no
/// graph state and performs no I/O beyond flipping `dirty`; the request
/// loop remains the sole build owner. Dropping this value stops the watcher.
pub struct RepoFreshness {
    repo: PathBuf,
    dirty: Arc<AtomicBool>,
    // None is the safe degradation mode: scan every request.
    _watcher: Option<notify::RecommendedWatcher>,
}

impl RepoFreshness {
    pub fn new(repo: &Path) -> Result<Self> {
        let repo = repo.canonicalize()?;
        let dirty = Arc::new(AtomicBool::new(true));
        let callback_dirty = Arc::clone(&dirty);
        let callback_repo = repo.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let changed = match event {
                Ok(event) => {
                    event.need_rescan()
                        || event
                            .paths
                            .iter()
                            .any(|path| affects_graph(&callback_repo, path))
                }
                // Lost events make the clean generation untrustworthy.
                Err(_) => true,
            };
            if changed {
                callback_dirty.store(true, Ordering::Release);
            }
        });
        let mut watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                eprintln!(
                    "sinter serve: filesystem watcher unavailable ({error}); scanning every tool call"
                );
                return Ok(Self {
                    repo,
                    dirty,
                    _watcher: None,
                });
            }
        };
        if let Err(error) = watcher.watch(&repo, RecursiveMode::Recursive) {
            eprintln!(
                "sinter serve: cannot watch {} ({error}); scanning every tool call",
                repo.display()
            );
            return Ok(Self {
                repo,
                dirty,
                _watcher: None,
            });
        }
        Ok(Self {
            repo,
            dirty,
            _watcher: Some(watcher),
        })
    }

    /// Synchronize once for each observed dirty generation. An event that
    /// arrives during the build sets the bit again and is handled by the
    /// next call. Failed builds also re-arm it before returning the error.
    pub fn sync(&self) -> Result<()> {
        let must_scan = self._watcher.is_none() || self.dirty.swap(false, Ordering::AcqRel);
        if !must_scan {
            return Ok(());
        }
        pipeline::build(&self.repo, None)
            .map(drop)
            .inspect_err(|_| self.dirty.store(true, Ordering::Release))
            .context("synchronize graph")
    }
}

fn affects_graph(repo: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(repo).unwrap_or(path);
    let mut components = rel.components();
    let first = components
        .next()
        .map(|c| c.as_os_str().to_string_lossy())
        .unwrap_or_default();
    // Graph writes are consequences of a build, not new source changes.
    if first == ".sinter" {
        return rel == Path::new(".sinter/index.scip");
    }
    if matches!(first.as_ref(), ".git" | "target" | "node_modules") {
        return false;
    }
    // Hidden source trees are ignored by the corpus walker. Root ignore
    // files are policy inputs and therefore do invalidate the generation.
    if first.starts_with('.') && !matches!(first.as_ref(), ".gitignore" | ".ignore") {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::affects_graph;

    #[test]
    fn graph_outputs_and_build_trees_do_not_dirty_source_generation() {
        let repo = std::path::Path::new("/repo");
        assert!(!affects_graph(repo, &repo.join(".sinter/graph.redb")));
        assert!(!affects_graph(repo, &repo.join("target/debug/app")));
        assert!(affects_graph(repo, &repo.join(".sinter/index.scip")));
        assert!(affects_graph(repo, &repo.join("src/lib.rs")));
        assert!(affects_graph(repo, &repo.join(".gitignore")));
    }
}
