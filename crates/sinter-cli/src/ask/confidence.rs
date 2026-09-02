//! Empirical safety metadata for a ranked `ask` result.
//!
//! The confidence level follows the top hit's own evidence: a name match
//! is high, a doc/signature-only match is medium, a synonym- or body-only
//! match is low. A score gap is a ranking fact, not a probability; the
//! ranking-margin bucket and its named holdout calibration are a secondary
//! input that can only demote a field-level match with a thin lead.

use serde::Serialize;

pub(crate) const CALIBRATION_VERSION: &str = "ask-holdout-2026-08-23.v2";

pub(crate) const HIGH_MARGIN_PERMILLE: i64 = 200;
const MEDIUM_MARGIN_PERMILLE: i64 = 50;
const MIN_CALIBRATION_SAMPLE: usize = 10;
const AUTO_ACCEPT_PRECISION_PERMILLE: u16 = 950;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RankingBucket {
    High,
    Medium,
    Low,
    Unrated,
}

impl RankingBucket {
    pub(crate) fn from_label(label: &str) -> Option<Self> {
        match label {
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "unrated" => Some(Self::Unrated),
            _ => None,
        }
    }
}

/// What the top hit itself matched on, strongest first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Evidence {
    /// A query term is (or is close to) the symbol name.
    Name,
    /// Terms matched only in doc, signature, owner, path, or a callee.
    Field,
    /// Synonym-only or body-only matches; nothing the user literally said
    /// appears on the symbol.
    Weak,
}

impl Evidence {
    pub(crate) fn from_label(label: &str) -> Option<Self> {
        match label {
            "name" => Some(Self::Name),
            "field" => Some(Self::Field),
            "weak" => Some(Self::Weak),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct RankingMargin {
    /// Score lead over the runner-up. `None` means no comparison exists.
    pub(crate) absolute: Option<i64>,
    /// Relative lead in thousandths of the top score.
    pub(crate) permille: Option<i64>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Calibration {
    pub(crate) version: &'static str,
    pub(crate) sample_size: usize,
    /// Holdout cases whose top-1 was correct in this bucket.
    pub(crate) correct: usize,
    pub(crate) measured_precision: f64,
    /// Wilson 95% interval for `measured_precision`; wide when the sample
    /// is small, which is the point of showing it.
    pub(crate) precision_interval_95: [f64; 2],
    pub(crate) in_calibration: bool,
}

/// Wilson score interval (z = 1.96). A bare percent from n = 25 reads as
/// authority; the interval reads as what it is.
pub(crate) fn wilson_95(correct: usize, total: usize) -> [f64; 2] {
    if total == 0 {
        return [0.0, 0.0];
    }
    let (z, n, p) = (1.96_f64, total as f64, correct as f64 / total as f64);
    let denominator = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denominator;
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / denominator;
    [(center - half).max(0.0), (center + half).min(1.0)]
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct TermCoverage {
    pub(crate) matched: usize,
    pub(crate) total: usize,
    pub(crate) permille: u16,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Assessment {
    /// The confidence level: evidence first, margin second.
    pub(crate) level: RankingBucket,
    /// The score-gap bucket the calibration is keyed by.
    pub(crate) ranking_bucket: RankingBucket,
    pub(crate) ranking_margin: RankingMargin,
    pub(crate) calibration: Calibration,
    pub(crate) term_coverage: TermCoverage,
    pub(crate) verify_required: bool,
    pub(crate) abstain: bool,
    pub(crate) reason: &'static str,
}

impl TermCoverage {
    fn new(matched: usize, total: usize) -> Self {
        let permille = matched
            .saturating_mul(1000)
            .checked_div(total)
            .unwrap_or(0)
            .min(1000) as u16;
        Self {
            matched,
            total,
            permille,
        }
    }
}

/// Assess only the first ranked result. Later ranks have never been
/// calibrated and must not inherit the top result's label.
pub(crate) fn assess_top(
    scores: &[i64],
    matched_terms: usize,
    total_terms: usize,
    evidence: Evidence,
) -> Assessment {
    let coverage = TermCoverage::new(matched_terms, total_terms);
    let Some(&score) = scores.first() else {
        return unrated(coverage, "no_match");
    };
    let Some(&runner_up) = scores.get(1) else {
        return unrated(coverage, "no_runner_up");
    };
    if score <= 0 {
        return unrated(coverage, "non_positive_score");
    }

    let absolute = (score - runner_up).max(0);
    let permille = absolute * 1000 / score;
    let level = if permille >= HIGH_MARGIN_PERMILLE {
        RankingBucket::High
    } else if permille >= MEDIUM_MARGIN_PERMILLE {
        RankingBucket::Medium
    } else {
        RankingBucket::Low
    };
    let (sample_size, correct) = calibration_bucket(level);
    let precision_permille = (correct * 1000 / sample_size.max(1)) as u16;
    let in_calibration = sample_size >= MIN_CALIBRATION_SAMPLE;
    let confidence = match evidence {
        Evidence::Name => RankingBucket::High,
        Evidence::Field if level == RankingBucket::Low => RankingBucket::Low,
        Evidence::Field => RankingBucket::Medium,
        Evidence::Weak => RankingBucket::Low,
    };
    let weak_coverage = coverage.permille < 500;
    let abstain = weak_coverage || confidence == RankingBucket::Low;
    let reason = if weak_coverage {
        "weak_term_coverage"
    } else if confidence == RankingBucket::Low {
        "weak_evidence"
    } else {
        "calibrated_ranking"
    };
    Assessment {
        level: confidence,
        ranking_bucket: level,
        ranking_margin: RankingMargin {
            absolute: Some(absolute),
            permille: Some(permille),
        },
        calibration: Calibration {
            version: CALIBRATION_VERSION,
            sample_size,
            correct,
            measured_precision: correct as f64 / sample_size.max(1) as f64,
            precision_interval_95: wilson_95(correct, sample_size),
            in_calibration,
        },
        term_coverage: coverage,
        verify_required: abstain || precision_permille < AUTO_ACCEPT_PRECISION_PERMILLE,
        abstain,
        reason,
    }
}

fn unrated(coverage: TermCoverage, reason: &'static str) -> Assessment {
    Assessment {
        level: RankingBucket::Unrated,
        ranking_bucket: RankingBucket::Unrated,
        ranking_margin: RankingMargin {
            absolute: None,
            permille: None,
        },
        calibration: Calibration {
            version: CALIBRATION_VERSION,
            sample_size: 0,
            correct: 0,
            measured_precision: 0.0,
            precision_interval_95: [0.0, 0.0],
            in_calibration: false,
        },
        term_coverage: coverage,
        verify_required: true,
        abstain: true,
        reason,
    }
}

/// `(cases, correct top-1)` from the named repository holdout run. These
/// are descriptive counts, not promises about an individual result.
const fn calibration_bucket(level: RankingBucket) -> (usize, usize) {
    match level {
        RankingBucket::High => (25, 22),
        RankingBucket::Medium => (15, 9),
        RankingBucket::Low => (6, 2),
        RankingBucket::Unrated => (0, 0),
    }
}

impl RankingBucket {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Unrated => "unrated",
        }
    }
}

/// One-line confidence caveat for text output, or None when the top hit
/// stands clear. Abstain means "candidates listed, no confidence bucket
/// claimed", never "no answer": the ranked list still follows this line.
pub(crate) fn advice(assessment: Assessment, family_size: usize) -> Option<String> {
    let family = if family_size > 1 {
        format!("; {family_size} returned symbols share the top name")
    } else {
        String::new()
    };
    let line = match (assessment.abstain, assessment.reason) {
        (true, "weak_term_coverage") => "confidence: unrated (weak term coverage)".to_owned(),
        // Thin evidence keeps its level; the list still follows.
        (true, "weak_evidence") => format!("confidence: {}", assessment.level.label()),
        (true, _) => "confidence: unrated".to_owned(),
        (false, _) if assessment.verify_required => {
            format!("confidence: {} — verify top hit", assessment.level.label())
        }
        (false, _) => return None,
    };
    Some(format!("{line}{family}"))
}

/// Calibration detail behind `advice`, for `--explain` text output.
pub(crate) fn explain_line(assessment: Assessment) -> String {
    let calibration = assessment.calibration;
    let [low, high] = calibration.precision_interval_95;
    format!(
        "calibration: {} evidence level; {} ranking-margin bucket (margin {}‰, term coverage {}/{}); holdout top-1 {}/{} correct (95% interval {:.0}-{:.0}%); {} · {}",
        assessment.level.label(),
        assessment.ranking_bucket.label(),
        assessment
            .ranking_margin
            .permille
            .map_or("n/a".to_owned(), |p| p.to_string()),
        assessment.term_coverage.matched,
        assessment.term_coverage.total,
        calibration.correct,
        calibration.sample_size,
        low * 100.0,
        high * 100.0,
        assessment.reason,
        calibration.version,
    )
}

#[cfg(test)]
mod tests {
    use super::{Evidence, RankingBucket, advice, assess_top, explain_line, wilson_95};

    #[test]
    fn wilson_interval_widens_for_small_samples() {
        let [low, high] = wilson_95(22, 25);
        assert!((low - 0.70).abs() < 0.01, "{low}");
        assert!((high - 0.96).abs() < 0.01, "{high}");
        assert_eq!(wilson_95(0, 0), [0.0, 0.0]);
        let [wide_low, wide_high] = wilson_95(2, 9);
        assert!(wide_high - wide_low > 0.4);
    }

    #[test]
    fn score_gap_is_named_separately_from_empirical_confidence() {
        let assessment = assess_top(&[400, 300], 2, 2, Evidence::Name);
        assert_eq!(assessment.level, RankingBucket::High);
        assert_eq!(assessment.ranking_bucket, RankingBucket::High);
        assert_eq!(assessment.ranking_margin.absolute, Some(100));
        assert_eq!(assessment.ranking_margin.permille, Some(250));
        assert_eq!(assessment.calibration.sample_size, 25);
        assert_eq!(assessment.calibration.correct, 22);
        assert_eq!(assessment.calibration.measured_precision, 0.88);
        assert!(assessment.verify_required);
        assert!(!assessment.abstain);
        assert_eq!(
            advice(assessment, 1).as_deref(),
            Some("confidence: high — verify top hit")
        );
        assert!(
            explain_line(assessment).contains("holdout top-1 22/25 correct (95% interval 70-96%)")
        );
    }

    #[test]
    fn singleton_and_weak_evidence_abstain() {
        let singleton = assess_top(&[500], 1, 1, Evidence::Name);
        assert_eq!(singleton.ranking_bucket, RankingBucket::Unrated);
        assert_eq!(singleton.ranking_margin.permille, None);
        assert_eq!(singleton.reason, "no_runner_up");
        assert!(singleton.abstain);

        let weak = assess_top(&[500, 300], 1, 4, Evidence::Name);
        assert_eq!(weak.reason, "weak_term_coverage");
        assert!(weak.abstain);
        assert!(advice(weak, 1).unwrap().starts_with("confidence: unrated"));
    }

    #[test]
    fn evidence_sets_the_level_and_a_thin_margin_only_demotes_field_matches() {
        // An exact name match with a near tie is still the answer.
        let named = assess_top(&[1000, 980], 2, 2, Evidence::Name);
        assert_eq!(named.ranking_bucket, RankingBucket::Low);
        assert_eq!(named.level, RankingBucket::High);
        assert!(!named.abstain);
        assert_eq!(
            advice(named, 1).as_deref(),
            Some("confidence: high — verify top hit")
        );
        // Doc-only evidence is medium with a clear lead, low with a near tie.
        let clear = assess_top(&[1000, 500], 2, 2, Evidence::Field);
        assert_eq!(clear.level, RankingBucket::Medium);
        assert!(!clear.abstain);
        let tied = assess_top(&[1000, 980], 2, 2, Evidence::Field);
        assert_eq!(tied.level, RankingBucket::Low);
        assert_eq!(tied.reason, "weak_evidence");
        assert!(tied.abstain);
        assert_eq!(advice(tied, 1).as_deref(), Some("confidence: low"));
        // Synonym-only evidence is low however wide the margin.
        let synonym = assess_top(&[1000, 100], 2, 2, Evidence::Weak);
        assert_eq!(synonym.level, RankingBucket::Low);
        assert!(synonym.abstain);
    }

    #[test]
    fn weak_coverage_names_its_reason_in_plain_words() {
        let assessment = assess_top(&[1000, 100], 1, 4, Evidence::Name);
        assert_eq!(assessment.reason, "weak_term_coverage");
        let line = advice(assessment, 1).unwrap();
        assert_eq!(line, "confidence: unrated (weak term coverage)");
    }
}
