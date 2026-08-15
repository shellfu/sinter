use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};

use crate::pipeline;

/// `sinter watch`: keep the graph fresh. Events batch up under a debounce
/// window, then the changed set — never the corpus — goes through the
/// incremental pipeline.
pub fn run(repo: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;
    // Baseline pass so the watcher starts from a consistent graph.
    let report = pipeline::build(&repo, None)?;
    pipeline::print_report(&report);

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            let _ = tx.send(event.paths);
        }
    })
    .context("create file watcher")?;
    watcher.watch(&repo, RecursiveMode::Recursive)?;
    println!("watching {} (ctrl-c to stop)", repo.display());

    loop {
        let Ok(first) = rx.recv() else {
            return Ok(());
        };
        let mut paths: Vec<PathBuf> = first;
        // Debounce: swallow the burst, then run once.
        while let Ok(more) = rx.recv_timeout(Duration::from_millis(200)) {
            paths.extend(more);
        }
        paths.sort();
        paths.dedup();
        paths.retain(|p| {
            !p.components()
                .any(|c| c.as_os_str() == ".sinter" || c.as_os_str() == ".git")
        });
        if paths.is_empty() {
            continue;
        }
        match pipeline::build(&repo, Some(&paths)) {
            Ok(report) if report.changed > 0 || report.removed > 0 => {
                pipeline::print_report(&report)
            }
            Ok(_) => {}
            Err(e) => eprintln!("sinter watch: update failed: {e:#}"),
        }
    }
}
