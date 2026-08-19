use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{RecursiveMode, Watcher};

use crate::pipeline;

/// One event batch: the paths that changed, or a full-pass order (notify
/// signalled rescan/overflow — its event stream is no longer trustworthy).
enum Batch {
    Paths(Vec<PathBuf>),
    Rescan,
}

/// Gitignore rules for event filtering, built once. The pipeline scan
/// walks with these same rules (`ignore::WalkBuilder` defaults), so a
/// build storm in target/ or node_modules/ never triggers rebuild passes.
// ponytail: root-level ignore files only; nested .gitignores fall through
// to the pipeline's own no-op scan — add WalkBuilder-style nesting if a
// deep monorepo's storms ever get through.
fn ignore_matcher(repo: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(repo);
    for name in [".gitignore", ".ignore", ".git/info/exclude"] {
        builder.add(repo.join(name));
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// Should this event path trigger a rebuild pass? Hidden components are
/// out (same as the scan's walker default — covers .git and .sinter), then
/// gitignore rules apply to the path and its parents.
fn triggers_rebuild(repo: &Path, matcher: &Gitignore, path: &Path) -> bool {
    let rel = path.strip_prefix(repo).unwrap_or(path);
    if rel
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
    {
        return false;
    }
    !matcher
        .matched_path_or_any_parents(rel, path.is_dir())
        .is_ignore()
}

/// `sinter watch`: keep the graph fresh. Events batch up under a debounce
/// window, then the changed set — never the corpus — goes through the
/// incremental pipeline.
pub fn run(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;
    // Baseline pass so the watcher starts from a consistent graph.
    let report = pipeline::build(&repo, None)?;
    pipeline::print_report(&report);

    let matcher = ignore_matcher(&repo);
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let _ = match event {
            Ok(event) if event.need_rescan() => tx.send(Batch::Rescan),
            Ok(event) => tx.send(Batch::Paths(event.paths)),
            // Watcher errors (inotify queue overflow surfaces here on some
            // backends) mean events were lost — full pass, never a silent drop.
            Err(_) => tx.send(Batch::Rescan),
        };
    })
    .context("create file watcher")?;
    watcher.watch(&repo, RecursiveMode::Recursive)?;
    println!("watching {} (ctrl-c to stop)", repo.display());

    loop {
        let Ok(first) = rx.recv() else {
            return Ok(());
        };
        let mut full = false;
        let mut paths: Vec<PathBuf> = Vec::new();
        // Debounce: swallow the burst, then run once.
        let mut batch = first;
        loop {
            match batch {
                Batch::Rescan => full = true,
                Batch::Paths(more) => paths.extend(more),
            }
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(more) => batch = more,
                Err(_) => break,
            }
        }
        paths.sort();
        paths.dedup();
        paths.retain(|p| triggers_rebuild(&repo, &matcher, p));
        if !full && paths.is_empty() {
            continue;
        }
        let changed = if full { None } else { Some(paths.as_slice()) };
        match pipeline::build(&repo, changed) {
            Ok(report) if report.changed > 0 || report.removed > 0 => {
                pipeline::print_report(&report)
            }
            Ok(_) => {}
            Err(e) => eprintln!("sinter watch: update failed: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ignore_matcher, triggers_rebuild};

    #[test]
    fn event_filtering_respects_gitignore_and_hidden_paths() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::write(repo.join(".gitignore"), "target/\nnode_modules/\n*.log\n").unwrap();
        let matcher = ignore_matcher(repo);

        // Source files trigger.
        assert!(triggers_rebuild(repo, &matcher, &repo.join("src/main.rs")));
        // Gitignored build storms do not (files nor their deleted parents).
        assert!(!triggers_rebuild(
            repo,
            &matcher,
            &repo.join("target/debug/deps/foo.d")
        ));
        assert!(!triggers_rebuild(
            repo,
            &matcher,
            &repo.join("node_modules/left-pad/index.js")
        ));
        assert!(!triggers_rebuild(repo, &matcher, &repo.join("build.log")));
        // Hidden trees (the scan's walker skips them too).
        assert!(!triggers_rebuild(
            repo,
            &matcher,
            &repo.join(".git/index.lock")
        ));
        assert!(!triggers_rebuild(
            repo,
            &matcher,
            &repo.join(".sinter/graph.redb")
        ));
    }
}
