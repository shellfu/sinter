mod command;
mod model;
mod report;
mod scoring;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use self::model::{CaseResult, CaseSpec, RepositoryResult, Scorecard, SuiteSpec};

const SCORECARD_SCHEMA: u32 = 2;
const SUITE_SCHEMA: u32 = 1;
const ASK_CANDIDATE_LIMIT: usize = 200;

pub fn run() -> Result<()> {
    let workspace = workspace_root();
    let suite_path = workspace.join("harness/eval/cases.json");
    let suite: SuiteSpec = serde_json::from_slice(
        &fs::read(&suite_path)
            .with_context(|| format!("failed to read {}", suite_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", suite_path.display()))?;
    validate_suite(&suite)?;

    let scratch = tempfile::tempdir().context("failed to create evaluation directory")?;
    let sinter = PathBuf::from(env!("CARGO_BIN_EXE_sinter"));
    let mut repositories = HashMap::new();
    let mut repository_results = Vec::new();

    for spec in &suite.repositories {
        let path = scratch.path().join(&spec.name);
        eprintln!("eval: cloning {} at {}", spec.name, spec.git_ref);
        command::clone_repository(spec, &path)?;
        eprintln!("eval: building syntax-only graph for {}", spec.name);
        let build_duration = command::build_graph(&sinter, &path)?;
        repositories.insert(spec.name.clone(), path);
        repository_results.push(RepositoryResult {
            name: spec.name.clone(),
            url: spec.url.clone(),
            git_ref: spec.git_ref.clone(),
            commit: spec.commit.clone(),
            compiler_index: "not_requested",
            build_duration_ms: build_duration.as_millis(),
        });
    }

    let mut cases = Vec::with_capacity(suite.cases.len());
    for case in &suite.cases {
        let repository = repositories
            .get(case.repository())
            .with_context(|| format!("case {} names an unknown repository", case.id()))?;
        eprintln!("eval: running {}", case.id());
        cases.push(evaluate_case(&sinter, repository, case)?);
    }

    let metrics = scoring::aggregate_metrics(&cases);
    let regressions = scoring::compare_minimums(&metrics, &suite.minimums);
    let scorecard = Scorecard {
        schema: SCORECARD_SCHEMA,
        suite_schema: suite.schema,
        generated_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_secs(),
        repositories: repository_results,
        metrics,
        minimums: suite.minimums,
        regressions,
        cases,
    };
    let output_dir = report::output_directory(&workspace);
    report::write_scorecards(&output_dir, &scorecard)?;

    println!("real-repository evaluation: {}", output_dir.display());
    println!(
        "query MRR {:.3}, ask top-1/MRR/R@5 {:.3}/{:.3}/{:.3}, caller P/R {:.3}/{:.3}, path accuracy {:.3}",
        scorecard.metrics.query.mean_reciprocal_rank,
        scorecard.metrics.ask.top_1_accuracy,
        scorecard.metrics.ask.mean_reciprocal_rank,
        scorecard.metrics.ask.mean_recall_at_5,
        scorecard.metrics.callers.precision,
        scorecard.metrics.callers.recall,
        scorecard.metrics.paths.accuracy
    );
    if !scorecard.regressions.is_empty() {
        bail!(
            "real-repository evaluation regressed:\n{}",
            scorecard.regressions.join("\n")
        );
    }
    Ok(())
}

fn evaluate_case(sinter: &Path, repository: &Path, case: &CaseSpec) -> Result<CaseResult> {
    let started = Instant::now();
    let repository_arg = repository.display().to_string();
    let (kind, outcome, duration_ms) = match case {
        CaseSpec::Query {
            input,
            limit,
            relevant,
            ..
        } => {
            let value = command::run_json(
                sinter,
                &[
                    "query".into(),
                    input.clone(),
                    "--repo".into(),
                    repository_arg,
                    "--json".into(),
                    "--limit".into(),
                    limit.to_string(),
                ],
            )?;
            let results = value
                .get("results")
                .and_then(serde_json::Value::as_array)
                .context("query JSON is missing results")?;
            (
                "query",
                scoring::score_ranking(input, *limit, relevant, results, results)?,
                started.elapsed().as_millis(),
            )
        }
        CaseSpec::Ask {
            input,
            limit,
            relevant,
            ..
        } => {
            let value = command::run_json(
                sinter,
                &[
                    "ask".into(),
                    input.clone(),
                    "--repo".into(),
                    repository_arg,
                    "--json".into(),
                    "--limit".into(),
                    limit.to_string(),
                ],
            )?;
            let results = value.as_array().context("ask JSON is not an array")?;
            let primary_duration = started.elapsed();
            let candidate_value = if ranking_has_all_labels(results, relevant)? {
                None
            } else {
                Some(command::run_json(
                    sinter,
                    &[
                        "ask".into(),
                        input.clone(),
                        "--repo".into(),
                        repository.display().to_string(),
                        "--json".into(),
                        "--limit".into(),
                        ASK_CANDIDATE_LIMIT.to_string(),
                    ],
                )?)
            };
            let candidate_results = candidate_value.as_ref().map_or(Ok(results), |value| {
                value
                    .as_array()
                    .context("diagnostic ask JSON is not an array")
            })?;
            (
                "ask",
                scoring::score_ranking(input, *limit, relevant, results, candidate_results)?,
                primary_duration.as_millis(),
            )
        }
        CaseSpec::Callers {
            symbol, expected, ..
        } => {
            let value = command::run_json(
                sinter,
                &[
                    "affected".into(),
                    symbol.clone(),
                    "--repo".into(),
                    repository_arg,
                    "--json".into(),
                    "--depth".into(),
                    "1".into(),
                    "--relations".into(),
                    "calls".into(),
                ],
            )?;
            let dependents = value
                .get("dependents")
                .and_then(serde_json::Value::as_array)
                .context("affected JSON is missing dependents")?;
            let returned = dependents
                .iter()
                .map(|node| scoring::symbol_key(node, "s", "f"))
                .collect::<Result<Vec<_>>>()?;
            (
                "callers",
                scoring::score_callers(symbol, expected, returned),
                started.elapsed().as_millis(),
            )
        }
        CaseSpec::Path {
            from,
            to,
            relations,
            expect,
            ..
        } => {
            let value = command::run_json(
                sinter,
                &[
                    "path".into(),
                    from.clone(),
                    to.clone(),
                    "--repo".into(),
                    repository_arg,
                    "--json".into(),
                    "--relations".into(),
                    relations.join(","),
                ],
            )?;
            (
                "path",
                scoring::score_path(from, to, *expect, &value)?,
                started.elapsed().as_millis(),
            )
        }
    };
    Ok(CaseResult {
        id: case.id().to_owned(),
        repository: case.repository().to_owned(),
        kind,
        duration_ms,
        outcome,
    })
}

fn ranking_has_all_labels(
    results: &[serde_json::Value],
    relevant: &[model::SymbolKey],
) -> Result<bool> {
    let returned = results
        .iter()
        .map(|node| scoring::symbol_key(node, "qualified", "file"))
        .collect::<Result<HashSet<_>>>()?;
    Ok(relevant.iter().all(|label| returned.contains(label)))
}

fn validate_suite(suite: &SuiteSpec) -> Result<()> {
    if suite.schema != SUITE_SCHEMA {
        bail!(
            "unsupported evaluation suite schema {}, expected {}",
            suite.schema,
            SUITE_SCHEMA
        );
    }
    let repository_names = suite
        .repositories
        .iter()
        .map(|repository| repository.name.as_str())
        .collect::<HashSet<_>>();
    if repository_names.len() != suite.repositories.len() {
        bail!("evaluation repository names must be unique");
    }
    let mut case_ids = HashSet::new();
    for case in &suite.cases {
        if !case_ids.insert(case.id()) {
            bail!("duplicate evaluation case id {}", case.id());
        }
        if !repository_names.contains(case.repository()) {
            bail!(
                "case {} refers to unknown repository {}",
                case.id(),
                case.repository()
            );
        }
        match case {
            CaseSpec::Query {
                limit, relevant, ..
            }
            | CaseSpec::Ask {
                limit, relevant, ..
            } => {
                if *limit == 0 || relevant.is_empty() {
                    bail!("case {} needs a positive limit and labels", case.id());
                }
            }
            CaseSpec::Callers { expected, .. } if expected.is_empty() => {
                bail!("case {} needs at least one expected caller", case.id());
            }
            CaseSpec::Path { relations, .. } if relations.is_empty() => {
                bail!("case {} needs at least one relation", case.id());
            }
            _ => {}
        }
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
