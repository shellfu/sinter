use std::path::Path;

use anyhow::Result;

use crate::pipeline;

/// `sinter build`: one incremental pass over the whole corpus.
pub fn run(repo: &Path) -> Result<()> {
    // Phases go to stderr, the report to stdout: a redirected build log
    // keeps the numbers and drops the spinner.
    let progress = crate::progress::Progress::stderr();
    let report = pipeline::build_with(repo, None, &mut |phase| {
        crate::progress::render(&progress, phase)
    })?;
    drop(progress);
    pipeline::print_report(&report);
    let repo = repo.canonicalize()?;
    if report.scanned == 0 {
        eprintln!(
            "warning: 0 source files found under {} — wrong directory?",
            repo.display()
        );
    }
    println!(
        "  -> {} ({} on disk)",
        pipeline::db_path(&repo).display(),
        pipeline::db_size(&repo)
    );
    Ok(())
}
