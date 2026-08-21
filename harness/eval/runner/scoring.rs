use std::collections::BTreeSet;

use anyhow::{Context, Result};

use super::model::{
    CallerMetrics, CaseOutcome, CaseResult, Metrics, Minimums, PathExpectation, PathMetrics,
    RankedSymbol, RankingMetrics, SymbolKey,
};

pub fn score_ranking(
    input: &str,
    limit: usize,
    relevant: &[SymbolKey],
    results: &[serde_json::Value],
) -> Result<CaseOutcome> {
    let returned = results
        .iter()
        .enumerate()
        .map(|(index, node)| {
            Ok(RankedSymbol {
                rank: index + 1,
                symbol: symbol_key(node, "qualified", "file")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let relevant_set = relevant.iter().cloned().collect::<BTreeSet<_>>();
    let found = returned
        .iter()
        .filter(|result| relevant_set.contains(&result.symbol))
        .map(|result| result.symbol.clone())
        .collect::<BTreeSet<_>>();
    let first_relevant_rank = returned
        .iter()
        .find(|result| relevant_set.contains(&result.symbol))
        .map(|result| result.rank);
    let reciprocal_rank = first_relevant_rank.map_or(0.0, |rank| 1.0 / rank as f64);
    let recall_at_limit = ratio(found.len(), relevant_set.len());
    Ok(CaseOutcome::Ranking {
        input: input.to_owned(),
        limit,
        first_relevant_rank,
        relevant_found: found.len(),
        relevant_total: relevant_set.len(),
        reciprocal_rank,
        recall_at_limit,
        returned,
    })
}

pub fn score_callers(
    symbol: &str,
    expected: &[SymbolKey],
    returned: Vec<SymbolKey>,
) -> CaseOutcome {
    let expected_set = expected.iter().cloned().collect::<BTreeSet<_>>();
    let returned_set = returned.iter().cloned().collect::<BTreeSet<_>>();
    let true_positives = expected_set.intersection(&returned_set).count();
    CaseOutcome::Callers {
        symbol: symbol.to_owned(),
        true_positives,
        returned_total: returned_set.len(),
        expected_total: expected_set.len(),
        precision: ratio(true_positives, returned_set.len()),
        recall: ratio(true_positives, expected_set.len()),
        expected: expected_set.into_iter().collect(),
        returned: returned_set.into_iter().collect(),
    }
}

pub fn score_path(
    from: &str,
    to: &str,
    expected: PathExpectation,
    value: &serde_json::Value,
) -> Result<CaseOutcome> {
    let found = value
        .get("found")
        .and_then(serde_json::Value::as_bool)
        .context("path JSON is missing found")?;
    let coverage_status = value
        .pointer("/coverage/status")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let observed = if found {
        "found"
    } else if coverage_status.as_deref() == Some("not_proven") {
        "not_proven"
    } else {
        "miss"
    };
    let correct = observed == expected.as_str();
    let steps = value
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .context("path JSON is missing steps")?
        .clone();
    Ok(CaseOutcome::Path {
        from: from.to_owned(),
        to: to.to_owned(),
        expected: expected.as_str(),
        observed,
        correct,
        coverage_status,
        steps,
    })
}

pub fn symbol_key(value: &serde_json::Value, qualified: &str, file: &str) -> Result<SymbolKey> {
    Ok(SymbolKey {
        qualified: value
            .get(qualified)
            .and_then(serde_json::Value::as_str)
            .with_context(|| format!("result is missing {qualified}"))?
            .to_owned(),
        file: value
            .get(file)
            .and_then(serde_json::Value::as_str)
            .with_context(|| format!("result is missing {file}"))?
            .to_owned(),
    })
}

pub fn aggregate_metrics(cases: &[CaseResult]) -> Metrics {
    let mut query = RankingMetrics::default();
    let mut ask = RankingMetrics::default();
    let mut callers = CallerMetrics::default();
    let mut paths = PathMetrics::default();

    for case in cases {
        match &case.outcome {
            CaseOutcome::Ranking {
                reciprocal_rank,
                recall_at_limit,
                ..
            } => {
                let metric = if case.kind == "query" {
                    &mut query
                } else {
                    &mut ask
                };
                metric.cases += 1;
                metric.mean_reciprocal_rank += reciprocal_rank;
                metric.mean_recall_at_limit += recall_at_limit;
            }
            CaseOutcome::Callers {
                true_positives,
                returned_total,
                expected_total,
                ..
            } => {
                callers.cases += 1;
                callers.true_positives += true_positives;
                callers.returned += returned_total;
                callers.expected += expected_total;
            }
            CaseOutcome::Path { correct, .. } => {
                paths.cases += 1;
                paths.correct += usize::from(*correct);
            }
        }
    }
    finish_ranking(&mut query);
    finish_ranking(&mut ask);
    callers.precision = ratio(callers.true_positives, callers.returned);
    callers.recall = ratio(callers.true_positives, callers.expected);
    paths.accuracy = ratio(paths.correct, paths.cases);
    Metrics {
        query,
        ask,
        callers,
        paths,
    }
}

pub fn compare_minimums(metrics: &Metrics, minimums: &Minimums) -> Vec<String> {
    let checks = [
        (
            "query MRR",
            metrics.query.mean_reciprocal_rank,
            minimums.query_mrr,
        ),
        (
            "query recall@limit",
            metrics.query.mean_recall_at_limit,
            minimums.query_recall_at_limit,
        ),
        (
            "ask MRR",
            metrics.ask.mean_reciprocal_rank,
            minimums.ask_mrr,
        ),
        (
            "ask recall@limit",
            metrics.ask.mean_recall_at_limit,
            minimums.ask_recall_at_limit,
        ),
        (
            "caller precision",
            metrics.callers.precision,
            minimums.caller_precision,
        ),
        (
            "caller recall",
            metrics.callers.recall,
            minimums.caller_recall,
        ),
        (
            "path accuracy",
            metrics.paths.accuracy,
            minimums.path_accuracy,
        ),
    ];
    checks
        .into_iter()
        .filter(|(_, actual, minimum)| actual + f64::EPSILON < *minimum)
        .map(|(name, actual, minimum)| format!("{name}: {actual:.3} < {minimum:.3}"))
        .collect()
}

fn finish_ranking(metrics: &mut RankingMetrics) {
    if metrics.cases > 0 {
        metrics.mean_reciprocal_rank /= metrics.cases as f64;
        metrics.mean_recall_at_limit /= metrics.cases as f64;
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}
