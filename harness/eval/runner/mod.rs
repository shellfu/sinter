mod agent_flow;
mod command;
mod model;
mod report;
mod scoring;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use self::model::{
    CaseResult, CaseSpec, EvaluatedBinary, EvaluationScope, RepositoryResult, Scorecard, Split,
    SuiteSpec,
};

const SCORECARD_SCHEMA: u32 = 5;
const SUITE_SCHEMA: u32 = 3;
const ASK_CANDIDATE_LIMIT: usize = 200;
const INTENTS: &[&str] = &[
    "construction",
    "registration",
    "dispatch",
    "lifecycle",
    "error_handling",
    "output",
    "lookup",
];

pub fn run() -> Result<()> {
    let workspace = workspace_root();
    let suite_path = workspace.join("harness/eval/cases.json");
    let suite: SuiteSpec = serde_json::from_slice(
        &fs::read(&suite_path)
            .with_context(|| format!("failed to read {}", suite_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", suite_path.display()))?;
    validate_suite(&suite)?;
    let scope = evaluation_scope()?;

    let scratch = tempfile::tempdir().context("failed to create evaluation directory")?;
    let sinter = sinter_binary()?;
    let evaluated_binary = EvaluatedBinary {
        path: sinter.display().to_string(),
        version: command::version(&sinter)?,
    };
    let mut repositories = HashMap::new();
    let mut repository_results = Vec::new();

    for spec in &suite.repositories {
        let path = scratch.path().join(&spec.name);
        eprintln!("eval: cloning {} at {}", spec.name, spec.git_ref);
        command::clone_repository(spec, &path)?;
        eprintln!("eval: building syntax-only graph for {}", spec.name);
        let build_duration = command::build_graph(&sinter, &path)?;
        repositories.insert(spec.name.clone(), (path, spec.ask_split));
        repository_results.push(RepositoryResult {
            name: spec.name.clone(),
            url: spec.url.clone(),
            git_ref: spec.git_ref.clone(),
            commit: spec.commit.clone(),
            ask_split: spec.ask_split,
            compiler_index: "not_requested",
            build_duration_ms: build_duration.as_millis(),
        });
    }

    let selected_cases = suite.cases.iter().filter(|case| scope.includes(case));
    let mut cases = Vec::new();
    for case in selected_cases {
        let (repository, ask_split) = repositories
            .get(case.repository())
            .with_context(|| format!("case {} names an unknown repository", case.id()))?;
        eprintln!("eval: running {}", case.id());
        cases.push(evaluate_case(&sinter, repository, *ask_split, case)?);
    }

    let agent_flows = if scope == EvaluationScope::All {
        let suite = agent_flow::load_suite(&workspace)?;
        eprintln!("eval: running {} local agent flows", suite.cases.len());
        agent_flow::run_suite(&sinter, &workspace, &suite)?
    } else {
        Vec::new()
    };
    let mut metrics = scoring::aggregate_metrics(&cases);
    metrics.agent_flows = agent_flow::aggregate_metrics(&agent_flows);
    let regressions = scoring::compare_minimums(&metrics, &suite.minimums, scope);
    let scorecard = Scorecard {
        schema: SCORECARD_SCHEMA,
        suite_schema: suite.schema,
        generated_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_secs(),
        scope,
        evaluated_binary,
        repositories: repository_results,
        metrics,
        minimums: suite.minimums,
        regressions,
        cases,
        agent_flows,
    };
    let output_dir = report::output_directory(&workspace);
    report::write_scorecards(&output_dir, &scorecard)?;

    println!("real-repository evaluation: {}", output_dir.display());
    if scope == EvaluationScope::All {
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
    } else {
        println!(
            "ask top-1/MRR/R@5 {:.3}/{:.3}/{:.3}",
            scorecard.metrics.ask.top_1_accuracy,
            scorecard.metrics.ask.mean_reciprocal_rank,
            scorecard.metrics.ask.mean_recall_at_5,
        );
    }
    if !scorecard.regressions.is_empty() {
        bail!(
            "real-repository evaluation regressed:\n{}",
            scorecard.regressions.join("\n")
        );
    }
    Ok(())
}

/// Exercise the synthetic agent-flow corpus without cloning repositories or
/// contacting an indexer. Individual behavioral misses remain scorecard data;
/// this contract fails only when the suite or runner itself is not executable.
pub fn run_agent_flow_contract() -> Result<()> {
    let workspace = workspace_root();
    let suite = agent_flow::load_suite(&workspace)?;
    let expected = suite.cases.len();
    let results = agent_flow::run_suite(&sinter_binary()?, &workspace, &suite)?;
    let metrics = agent_flow::aggregate_metrics(&results);
    if results.len() != expected || metrics.cases != expected {
        bail!(
            "agent-flow runner returned {} of {expected} cases",
            results.len()
        );
    }
    if metrics.tool_calls == 0 {
        bail!("agent-flow suite executed no Sinter tool calls");
    }
    let output_path = std::env::var_os("SINTER_AGENT_FLOW_OUT").map_or_else(
        || workspace.join("target/sinter-agent-flow/scorecard.json"),
        PathBuf::from,
    );
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(
        &output_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1,
            "suite_schema": suite.schema,
            "metrics": &metrics,
            "cases": &results,
        }))?,
    )
    .with_context(|| format!("failed to write {}", output_path.display()))?;
    println!("agent-flow evaluation: {}", output_path.display());
    Ok(())
}

fn evaluate_case(
    sinter: &Path,
    repository: &Path,
    ask_split: Split,
    case: &CaseSpec,
) -> Result<CaseResult> {
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
                scoring::score_ranking(input, *limit, relevant, results, results, None)?,
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
            let topic = value
                .get("topics")
                .and_then(serde_json::Value::as_array)
                .and_then(|topics| topics.first())
                .context("ask JSON is missing its topic result")?;
            let results = topic
                .get("hits")
                .and_then(serde_json::Value::as_array)
                .context("ask topic JSON is missing hits")?;
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
                    .pointer("/topics/0/hits")
                    .and_then(serde_json::Value::as_array)
                    .context("diagnostic ask JSON is missing topic hits")
            })?;
            (
                "ask",
                scoring::score_ranking(
                    input,
                    *limit,
                    relevant,
                    results,
                    candidate_results,
                    Some(topic),
                )?,
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
            // An "external" answer (symbol not found as a definition) has
            // no dependents array: score it as returning nothing.
            let returned = value
                .get("dependents")
                .and_then(serde_json::Value::as_array)
                .map(|dependents| {
                    dependents
                        .iter()
                        .map(|node| scoring::symbol_key(node, "s", "f"))
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
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
            dirty,
            ..
        } => {
            let scratch = dirty.then(|| repository.join("sinter-eval-dirty.txt"));
            if let Some(scratch) = &scratch {
                fs::write(scratch, "untracked scratch file for the dirty-tree check\n")
                    .with_context(|| format!("failed to write {}", scratch.display()))?;
            }
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
            );
            if let Some(scratch) = &scratch {
                let _ = fs::remove_file(scratch);
            }
            (
                "path",
                scoring::score_path(from, to, *expect, *dirty, &value?)?,
                started.elapsed().as_millis(),
            )
        }
    };
    let (intent, split) = match case {
        CaseSpec::Ask { intent, .. } => (Some(intent.clone()), Some(ask_split)),
        _ => (None, None),
    };
    Ok(CaseResult {
        id: case.id().to_owned(),
        repository: case.repository().to_owned(),
        kind,
        intent,
        split,
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
    let repository_splits = suite
        .repositories
        .iter()
        .map(|repository| (repository.name.as_str(), repository.ask_split))
        .collect::<HashMap<_, _>>();
    let holdout = suite
        .cases
        .iter()
        .filter(|case| {
            matches!(case, CaseSpec::Ask { .. })
                && repository_splits.get(case.repository()) == Some(&Split::Holdout)
        })
        .count();
    let asks = suite
        .cases
        .iter()
        .filter(|case| matches!(case, CaseSpec::Ask { .. }))
        .count();
    if asks > 0 && holdout * 4 < asks {
        bail!("at least a quarter of ask cases must be holdout ({holdout}/{asks})");
    }
    let tuning_repositories = suite
        .repositories
        .iter()
        .filter(|repository| repository.ask_split == Split::Tuning)
        .count();
    let holdout_repositories = suite
        .repositories
        .iter()
        .filter(|repository| repository.ask_split == Split::Holdout)
        .count();
    if tuning_repositories == 0 || holdout_repositories == 0 {
        bail!("evaluation needs both tuning and holdout repositories");
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
            } => {
                if *limit == 0 || relevant.is_empty() {
                    bail!("case {} needs a positive limit and labels", case.id());
                }
            }
            CaseSpec::Ask {
                limit,
                relevant,
                intent,
                ..
            } => {
                if *limit == 0 || relevant.is_empty() {
                    bail!("case {} needs a positive limit and labels", case.id());
                }
                if !INTENTS.contains(&intent.as_str()) {
                    bail!("case {} has unknown intent {intent}", case.id());
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

fn sinter_binary() -> Result<PathBuf> {
    let configured = std::env::var_os("SINTER_EVAL_BIN").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_sinter")),
        PathBuf::from,
    );
    configured.canonicalize().with_context(|| {
        format!(
            "failed to resolve evaluation binary {}",
            configured.display()
        )
    })
}

fn evaluation_scope() -> Result<EvaluationScope> {
    match std::env::var("SINTER_EVAL_SCOPE") {
        Ok(value) if value == "ask" => Ok(EvaluationScope::Ask),
        Ok(value) if value == "all" => Ok(EvaluationScope::All),
        Ok(value) => bail!("unsupported SINTER_EVAL_SCOPE {value:?}; expected all or ask"),
        Err(std::env::VarError::NotPresent) => Ok(EvaluationScope::All),
        Err(error) => Err(error).context("SINTER_EVAL_SCOPE is not valid UTF-8"),
    }
}
