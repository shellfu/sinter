use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::model::{CaseOutcome, Scorecard};

pub fn output_directory(workspace: &Path) -> PathBuf {
    std::env::var_os("SINTER_EVAL_OUT")
        .map_or_else(|| workspace.join("target/sinter-eval"), PathBuf::from)
}

pub fn write_scorecards(output_dir: &Path, scorecard: &Scorecard) -> Result<()> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    let json_path = output_dir.join("scorecard.json");
    let markdown_path = output_dir.join("scorecard.md");
    fs::write(&json_path, serde_json::to_vec_pretty(scorecard)?)
        .with_context(|| format!("failed to write {}", json_path.display()))?;
    fs::write(&markdown_path, render_markdown(scorecard))
        .with_context(|| format!("failed to write {}", markdown_path.display()))?;
    Ok(())
}

fn render_markdown(scorecard: &Scorecard) -> String {
    let mut output = String::new();
    writeln!(output, "# Sinter real-repository scorecard\n").unwrap();
    writeln!(
        output,
        "Generated at Unix time `{}` from suite schema `{}`. Compiler indexes were not requested; these scores measure the zero-config syntax graph.\n",
        scorecard.generated_at_unix_seconds, scorecard.suite_schema
    )
    .unwrap();
    writeln!(output, "## Corpus\n").unwrap();
    writeln!(
        output,
        "| Repository | Git ref | Commit | Build |\n|---|---|---|---:|"
    )
    .unwrap();
    for repository in &scorecard.repositories {
        writeln!(
            output,
            "| {} | `{}` | `{}` | {} ms |",
            repository.name, repository.git_ref, repository.commit, repository.build_duration_ms
        )
        .unwrap();
    }
    writeln!(output, "\n## Accuracy\n").unwrap();
    writeln!(
        output,
        "| Surface | Cases | Metric | Score | Minimum |\n|---|---:|---|---:|---:|"
    )
    .unwrap();
    writeln!(
        output,
        "| query | {} | MRR | {:.3} | {:.3} |",
        scorecard.metrics.query.cases,
        scorecard.metrics.query.mean_reciprocal_rank,
        scorecard.minimums.query_mrr
    )
    .unwrap();
    writeln!(
        output,
        "| query | {} | recall@limit | {:.3} | {:.3} |",
        scorecard.metrics.query.cases,
        scorecard.metrics.query.mean_recall_at_limit,
        scorecard.minimums.query_recall_at_limit
    )
    .unwrap();
    writeln!(
        output,
        "| ask | {} | MRR | {:.3} | {:.3} |",
        scorecard.metrics.ask.cases,
        scorecard.metrics.ask.mean_reciprocal_rank,
        scorecard.minimums.ask_mrr
    )
    .unwrap();
    writeln!(
        output,
        "| ask | {} | recall@limit | {:.3} | {:.3} |",
        scorecard.metrics.ask.cases,
        scorecard.metrics.ask.mean_recall_at_limit,
        scorecard.minimums.ask_recall_at_limit
    )
    .unwrap();
    writeln!(
        output,
        "| callers | {} | precision | {:.3} | {:.3} |",
        scorecard.metrics.callers.cases,
        scorecard.metrics.callers.precision,
        scorecard.minimums.caller_precision
    )
    .unwrap();
    writeln!(
        output,
        "| callers | {} | recall | {:.3} | {:.3} |",
        scorecard.metrics.callers.cases,
        scorecard.metrics.callers.recall,
        scorecard.minimums.caller_recall
    )
    .unwrap();
    writeln!(
        output,
        "| paths | {} | accuracy | {:.3} | {:.3} |",
        scorecard.metrics.paths.cases,
        scorecard.metrics.paths.accuracy,
        scorecard.minimums.path_accuracy
    )
    .unwrap();
    writeln!(output, "\n## Cases\n").unwrap();
    writeln!(
        output,
        "| Case | Surface | Result | Duration |\n|---|---|---|---:|"
    )
    .unwrap();
    for case in &scorecard.cases {
        let result = match &case.outcome {
            CaseOutcome::Ranking {
                reciprocal_rank,
                recall_at_limit,
                ..
            } => format!("MRR {reciprocal_rank:.3}; recall {recall_at_limit:.3}"),
            CaseOutcome::Callers {
                precision, recall, ..
            } => format!("precision {precision:.3}; recall {recall:.3}"),
            CaseOutcome::Path {
                expected,
                observed,
                correct,
                ..
            } => format!(
                "expected `{expected}`, observed `{observed}` ({})",
                if *correct { "correct" } else { "wrong" }
            ),
        };
        writeln!(
            output,
            "| `{}` | {} | {} | {} ms |",
            case.id, case.kind, result, case.duration_ms
        )
        .unwrap();
    }
    if !scorecard.regressions.is_empty() {
        writeln!(output, "\n## Regressions\n").unwrap();
        for regression in &scorecard.regressions {
            writeln!(output, "- {regression}").unwrap();
        }
    }
    output
}
