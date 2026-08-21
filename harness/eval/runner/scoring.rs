use std::collections::BTreeSet;

use anyhow::{Context, Result};

use super::model::{
    CallerMetrics, CaseOutcome, CaseResult, LabeledRanking, Metrics, Minimums, PathExpectation,
    PathMetrics, RankedSymbol, RankingMetrics, Split, SymbolKey,
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
        top_incorrect: returned
            .first()
            .filter(|top| !relevant_set.contains(&top.symbol))
            .map(|top| top.symbol.clone()),
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
    let mut callers = CallerMetrics::default();
    let mut paths = PathMetrics::default();
    for case in cases {
        match &case.outcome {
            CaseOutcome::Ranking { .. } => {}
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
    callers.precision = ratio(callers.true_positives, callers.returned);
    callers.recall = ratio(callers.true_positives, callers.expected);
    paths.accuracy = ratio(paths.correct, paths.cases);
    let ask = |case: &&CaseResult| case.kind == "ask";
    Metrics {
        query: ranking_metrics(cases.iter().filter(|case| case.kind == "query")),
        ask: ranking_metrics(cases.iter().filter(ask)),
        ask_by_split: grouped(cases, |case| {
            case.split.map(|split| split.as_str().to_owned())
        }),
        ask_by_repository: grouped(cases, |case| ask(&case).then(|| case.repository.clone())),
        ask_by_intent: grouped(cases, |case| case.intent.clone()),
        callers,
        paths,
    }
}

/// Ask metrics for every distinct label `key` assigns, in first-seen order.
fn grouped(
    cases: &[CaseResult],
    key: impl Fn(&CaseResult) -> Option<String>,
) -> Vec<LabeledRanking> {
    let mut labels: Vec<String> = Vec::new();
    for case in cases {
        if let Some(label) = key(case)
            && !labels.contains(&label)
        {
            labels.push(label);
        }
    }
    labels
        .into_iter()
        .map(|label| LabeledRanking {
            metrics: ranking_metrics(
                cases
                    .iter()
                    .filter(|case| key(case).as_deref() == Some(label.as_str())),
            ),
            label,
        })
        .collect()
}

fn ranking_metrics<'a>(cases: impl Iterator<Item = &'a CaseResult>) -> RankingMetrics {
    let mut metrics = RankingMetrics::default();
    let mut durations = Vec::new();
    for case in cases {
        let CaseOutcome::Ranking {
            top_1_correct,
            reciprocal_rank,
            recall_at_5,
            recall_at_limit,
            candidate_miss,
            ..
        } = &case.outcome
        else {
            continue;
        };
        metrics.cases += 1;
        metrics.top_1_accuracy += f64::from(*top_1_correct);
        metrics.mean_reciprocal_rank += reciprocal_rank;
        metrics.mean_recall_at_5 += recall_at_5;
        metrics.mean_recall_at_limit += recall_at_limit;
        metrics.candidate_miss_cases += usize::from(*candidate_miss);
        durations.push(case.duration_ms);
    }
    if metrics.cases > 0 {
        metrics.top_1_accuracy /= metrics.cases as f64;
        metrics.mean_reciprocal_rank /= metrics.cases as f64;
        metrics.mean_recall_at_5 /= metrics.cases as f64;
        metrics.mean_recall_at_limit /= metrics.cases as f64;
    }
    durations.sort_unstable();
    metrics.p50_duration_ms = percentile(&durations, 50);
    metrics.p95_duration_ms = percentile(&durations, 95);
    metrics
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
            "ask holdout top-1 accuracy",
            holdout_top_1(metrics),
            minimums.ask_holdout_top_1_accuracy,
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

fn holdout_top_1(metrics: &Metrics) -> f64 {
    metrics
        .ask_by_split
        .iter()
        .find(|group| group.label == Split::Holdout.as_str())
        .map_or(1.0, |group| group.metrics.top_1_accuracy)
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
            top_incorrect,
            ..
        } = outcome
        else {
            panic!("expected ranking outcome");
        };
        assert_eq!(top_incorrect, Some(label("noise")));
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
