use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::model::{CaseOutcome, LabeledRanking, Scorecard};

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
        "| ask | {} | top-1 accuracy | {:.3} | {:.3} |",
        scorecard.metrics.ask.cases,
        scorecard.metrics.ask.top_1_accuracy,
        scorecard.minimums.ask_top_1_accuracy,
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
        "| ask | {} | recall@5 | {:.3} | {:.3} |",
        scorecard.metrics.ask.cases,
        scorecard.metrics.ask.mean_recall_at_5,
        scorecard.minimums.ask_recall_at_5,
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
        "\nAsk candidate misses: **{} / {}**. Primary-query latency: **p50 {} ms**, **p95 {} ms**.\n",
        scorecard.metrics.ask.candidate_miss_cases,
        scorecard.metrics.ask.cases,
        scorecard.metrics.ask.p50_duration_ms,
        scorecard.metrics.ask.p95_duration_ms,
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
    write_breakdown(&mut output, "Ask by split", &scorecard.metrics.ask_by_split);
    write_breakdown(
        &mut output,
        "Ask by repository",
        &scorecard.metrics.ask_by_repository,
    );
    write_breakdown(
        &mut output,
        "Ask by intent",
        &scorecard.metrics.ask_by_intent,
    );
    writeln!(output, "\n## Cases\n").unwrap();
    writeln!(
        output,
        "| Case | Surface | Result | Duration |\n|---|---|---|---:|"
    )
    .unwrap();
    for case in &scorecard.cases {
        let result = match &case.outcome {
            CaseOutcome::Ranking {
                top_1_correct,
                reciprocal_rank,
                recall_at_5,
                recall_at_limit,
                candidate_miss,
                top_incorrect,
                ..
            } => {
                let mut line = format!(
                    "top-1 {top_1_correct}; MRR {reciprocal_rank:.3}; R@5 {recall_at_5:.3}; R@limit {recall_at_limit:.3}; candidate miss {candidate_miss}"
                );
                if let Some(wrong) = top_incorrect {
                    write!(line, "; got `{}` ({})", wrong.qualified, wrong.file).unwrap();
                }
                line
            }
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
    write_misses(&mut output, scorecard);
    if !scorecard.regressions.is_empty() {
        writeln!(output, "\n## Regressions\n").unwrap();
        for regression in &scorecard.regressions {
            writeln!(output, "- {regression}").unwrap();
        }
    }
    output
}

fn write_breakdown(output: &mut String, title: &str, groups: &[LabeledRanking]) {
    if groups.is_empty() {
        return;
    }
    writeln!(output, "\n## {title}\n").unwrap();
    writeln!(
        output,
        "| Group | Cases | Top-1 | MRR | R@5 | R@limit | Candidate misses | p95 |\n|---|---:|---:|---:|---:|---:|---:|---:|"
    )
    .unwrap();
    for group in groups {
        let m = &group.metrics;
        writeln!(
            output,
            "| {} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {} | {} ms |",
            group.label,
            m.cases,
            m.top_1_accuracy,
            m.mean_reciprocal_rank,
            m.mean_recall_at_5,
            m.mean_recall_at_limit,
            m.candidate_miss_cases,
            m.p95_duration_ms,
        )
        .unwrap();
    }
}

/// Every ask case whose top result is wrong, with the wrong answer beside
/// the labels, so a ranking regression can be read without the JSON.
fn write_misses(output: &mut String, scorecard: &Scorecard) {
    let misses = scorecard
        .cases
        .iter()
        .filter_map(|case| match &case.outcome {
            CaseOutcome::Ranking {
                input,
                top_incorrect: Some(wrong),
                first_relevant_rank,
                ..
            } if case.kind == "ask" => Some((case, input, wrong, first_relevant_rank)),
            _ => None,
        });
    let mut wrote_header = false;
    for (case, input, wrong, first_relevant_rank) in misses {
        if !wrote_header {
            writeln!(output, "\n## Ask misses\n").unwrap();
            writeln!(
                output,
                "| Case | Intent | Question | Got | Expected rank |\n|---|---|---|---|---:|"
            )
            .unwrap();
            wrote_header = true;
        }
        writeln!(
            output,
            "| `{}` | {} | {} | `{}` ({}) | {} |",
            case.id,
            case.intent.as_deref().unwrap_or("-"),
            input,
            wrong.qualified,
            wrong.file,
            first_relevant_rank.map_or("miss".to_owned(), |rank| rank.to_string()),
        )
        .unwrap();
    }
}
