//! Event-driven freshness for the long-lived MCP server. One-shot CLI
//! commands still scan at their query boundary; a server reuses a clean
//! generation until notify reports a repository change.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};

use crate::pipeline;

/// Server-lifetime owner of the filesystem watcher. The callback owns no
/// graph state and performs no I/O beyond flipping `dirty`; the request
/// loop remains the sole build owner. Dropping this value stops the watcher.
pub struct RepoFreshness {
    repo: PathBuf,
    dirty: Arc<AtomicBool>,
    // None is the safe degradation mode: scan every request. The watcher
    // arrives from a background thread, so a repository whose recursive
    // watch takes minutes to install (a 100k-file tree) still answers the
    // MCP handshake immediately and scans until the watch is live.
    // Held so the watch outlives installation; never read directly.
    _watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
    watching: Arc<AtomicBool>,
    _installer: Option<std::thread::JoinHandle<()>>,
}

impl RepoFreshness {
    pub fn new(repo: &Path) -> Result<Self> {
        let repo = repo.canonicalize()?;
        let dirty = Arc::new(AtomicBool::new(true));
        let watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>> = Arc::new(Mutex::new(None));
        let watching = Arc::new(AtomicBool::new(false));
        // Installing a recursive watch walks the whole tree. That is fast on
        // a normal repository and minutes on a huge one, and the MCP
        // handshake must not wait for it: hand back a scanning server now
        // and upgrade it in place when the watch is ready.
        let installer = {
            let (callback_dirty, callback_repo) = (Arc::clone(&dirty), repo.clone());
            let (slot, live) = (Arc::clone(&watcher), Arc::clone(&watching));
            let watch_root = repo.clone();
            std::thread::Builder::new()
                .name("sinter-watch".into())
                .spawn(move || {
                    let built =
                        notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
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
                    let Ok(mut built) = built else { return };
                    if built.watch(&watch_root, RecursiveMode::Recursive).is_err() {
                        return;
                    }
                    if let Ok(mut guard) = slot.lock() {
                        *guard = Some(built);
                        live.store(true, Ordering::Release);
                    }
                })
                .ok()
        };
        Ok(Self {
            repo,
            dirty,
            _watcher: watcher,
            watching,
            _installer: installer,
        })
    }

    /// Synchronize once for each observed dirty generation. An event that
    /// arrives during the build sets the bit again and is handled by the
    /// next call. Failed builds also re-arm it before returning the error.
    pub fn sync(&self) -> Result<()> {
        let must_scan =
            !self.watching.load(Ordering::Acquire) || self.dirty.swap(false, Ordering::AcqRel);
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
    if first.starts_with('.')
        && !matches!(
            first.as_ref(),
            ".gitignore" | ".ignore" | ".sinterignore" | ".sinter.toml"
        )
    {
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
        assert!(affects_graph(repo, &repo.join(".sinterignore")));
        assert!(affects_graph(repo, &repo.join(".sinter.toml")));
    }
}
