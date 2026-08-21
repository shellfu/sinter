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
    candidate_results: &[serde_json::Value],
) -> Result<CaseOutcome> {
    let returned = results
        .iter()
        .take(limit)
        .enumerate()
        .map(|(index, node)| {
            Ok(RankedSymbol {
                rank: index + 1,
                symbol: symbol_key(node, "qualified", "file")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let relevant_set = relevant.iter().cloned().collect::<BTreeSet<_>>();
    let found = relevant_in(&returned, &relevant_set, limit);
    let found_at_5 = relevant_in(&returned, &relevant_set, 5.min(limit));
    let candidate_returned = candidate_results
        .iter()
        .enumerate()
        .map(|(index, node)| {
            Ok(RankedSymbol {
                rank: index + 1,
                symbol: symbol_key(node, "qualified", "file")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let candidate_found = relevant_in(&candidate_returned, &relevant_set, candidate_results.len());
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
        top_1_correct: first_relevant_rank == Some(1),
        relevant_found: found.len(),
        relevant_total: relevant_set.len(),
        reciprocal_rank,
        recall_at_5: ratio(found_at_5.len(), relevant_set.len()),
        recall_at_limit,
        candidate_pool_size: candidate_results.len(),
        candidate_relevant_found: candidate_found.len(),
        candidate_miss: candidate_found.len() < relevant_set.len(),
        returned,
    })
}

fn relevant_in(
    returned: &[RankedSymbol],
    relevant: &BTreeSet<SymbolKey>,
    limit: usize,
) -> BTreeSet<SymbolKey> {
    returned
        .iter()
        .take(limit)
        .filter(|result| relevant.contains(&result.symbol))
        .map(|result| result.symbol.clone())
        .collect()
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
                top_1_correct,
                reciprocal_rank,
                recall_at_5,
                recall_at_limit,
                candidate_miss,
                ..
            } => {
                let metric = if case.kind == "query" {
                    &mut query
                } else {
                    &mut ask
                };
                metric.cases += 1;
                metric.top_1_accuracy += f64::from(*top_1_correct);
                metric.mean_reciprocal_rank += reciprocal_rank;
                metric.mean_recall_at_5 += recall_at_5;
                metric.mean_recall_at_limit += recall_at_limit;
                metric.candidate_miss_cases += usize::from(*candidate_miss);
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
    set_latency_percentiles(&mut query, cases, "query");
    set_latency_percentiles(&mut ask, cases, "ask");
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
            "ask top-1 accuracy",
            metrics.ask.top_1_accuracy,
            minimums.ask_top_1_accuracy,
        ),
        (
            "ask MRR",
            metrics.ask.mean_reciprocal_rank,
            minimums.ask_mrr,
        ),
        (
            "ask recall@5",
            metrics.ask.mean_recall_at_5,
            minimums.ask_recall_at_5,
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
        metrics.top_1_accuracy /= metrics.cases as f64;
        metrics.mean_reciprocal_rank /= metrics.cases as f64;
        metrics.mean_recall_at_5 /= metrics.cases as f64;
        metrics.mean_recall_at_limit /= metrics.cases as f64;
    }
}

fn set_latency_percentiles(metrics: &mut RankingMetrics, cases: &[CaseResult], kind: &str) {
    let mut durations = cases
        .iter()
        .filter(|case| case.kind == kind)
        .map(|case| case.duration_ms)
        .collect::<Vec<_>>();
    durations.sort_unstable();
    metrics.p50_duration_ms = percentile(&durations, 50);
    metrics.p95_duration_ms = percentile(&durations, 95);
}

fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let index = (sorted.len() * percentile).div_ceil(100).saturating_sub(1);
    sorted[index]
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::model::{CaseOutcome, SymbolKey};
    use super::{percentile, score_ranking};

    fn label(qualified: &str) -> SymbolKey {
        SymbolKey {
            qualified: qualified.into(),
            file: "src/lib.rs".into(),
        }
    }

    fn result(qualified: &str) -> serde_json::Value {
        json!({"qualified": qualified, "file": "src/lib.rs"})
    }

    #[test]
    fn ranking_separates_top_k_miss_from_candidate_miss() {
        let relevant = vec![label("wanted"), label("also_wanted")];
        let top = vec![result("noise"), result("wanted")];
        let candidates = vec![result("noise"), result("wanted"), result("also_wanted")];
        let outcome = score_ranking("question", 2, &relevant, &top, &candidates).unwrap();
        let CaseOutcome::Ranking {
            first_relevant_rank,
            top_1_correct,
            recall_at_5,
            recall_at_limit,
            candidate_relevant_found,
            candidate_miss,
            ..
        } = outcome
        else {
            panic!("expected ranking outcome");
        };
        assert_eq!(first_relevant_rank, Some(2));
        assert!(!top_1_correct);
        assert_eq!(recall_at_5, 0.5);
        assert_eq!(recall_at_limit, 0.5);
        assert_eq!(candidate_relevant_found, 2);
        assert!(!candidate_miss);
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        assert_eq!(percentile(&[], 95), 0);
        assert_eq!(percentile(&[10, 20, 30, 40], 50), 20);
        assert_eq!(percentile(&[10, 20, 30, 40], 95), 40);
    }
}
