use std::path::Path;

use anyhow::Result;

use crate::pipeline;

/// `sinter build`: one incremental pass over the whole corpus.
pub fn run(repo: &Path) -> Result<()> {
    let report = pipeline::build(repo, None)?;
    pipeline::print_report(&report);
    let repo = repo.canonicalize()?;
    println!(
        "  -> {} ({} on disk)",
        pipeline::db_path(&repo).display(),
        pipeline::db_size(&repo)
    );
    Ok(())
}
