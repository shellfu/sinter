//! How much a ranked list should be trusted. Calibrated on the real-
//! repository harness: the relative margin between the first and second
//! score predicts top-1 correctness (≥0.20 → ~88%, 0.05–0.20 → ~67%,
//! <0.05 → ~38%); coverage and family size did not. Thresholds change
//! only with a new calibration run.

use serde::Serialize;

/// Relative margin (margin / score, in thousandths) at or above which the
/// top hit is usually right.
const HIGH_MARGIN_PERMILLE: i64 = 200;
/// Below this the top hit is a coin flip against the runner-up.
const MEDIUM_MARGIN_PERMILLE: i64 = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Level {
    High,
    Medium,
    Low,
}

/// Confidence facts for one ranked position.
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Confidence {
    /// Only the first position carries a calibrated level; every later
    /// position is `low` because the calibration only measured rank one.
    pub(crate) level: Level,
    /// Score lead over the next position (0 for the last).
    pub(crate) margin: i64,
    /// `margin` as a share of this score, in thousandths.
    pub(crate) margin_permille: i64,
}

/// One entry per score, in order.
pub(crate) fn assess(scores: &[i64]) -> Vec<Confidence> {
    scores
        .iter()
        .enumerate()
        .map(|(rank, &score)| {
            let next = scores.get(rank + 1).copied().unwrap_or(0);
            let margin = (score - next).max(0);
            let margin_permille = if score > 0 { margin * 1000 / score } else { 0 };
            let level = if rank != 0 {
                Level::Low
            } else if margin_permille >= HIGH_MARGIN_PERMILLE {
                Level::High
            } else if margin_permille >= MEDIUM_MARGIN_PERMILLE {
                Level::Medium
            } else {
                Level::Low
            };
            Confidence {
                level,
                margin,
                margin_permille,
            }
        })
        .collect()
}

/// Human-facing caveat for the top hit, or None when it stands clear.
pub(crate) fn advice(top: Confidence, family_size: usize) -> Option<String> {
    let family = if family_size > 1 {
        format!(" — {family_size} results share the top hit's name")
    } else {
        String::new()
    };
    match top.level {
        Level::High => None,
        Level::Medium => Some(format!(
            "medium confidence: top hit leads by {}%{family}; check the runner-up",
            top.margin_permille / 10
        )),
        Level::Low => Some(format!(
            "low confidence: top hit leads by {}%{family}; inspect the top 3 before acting",
            top.margin_permille / 10
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{Level, advice, assess};

    #[test]
    fn level_follows_relative_margin_of_the_top_hit_only() {
        let levels = assess(&[1000, 700, 690])
            .iter()
            .map(|c| c.level)
            .collect::<Vec<_>>();
        assert_eq!(levels, [Level::High, Level::Low, Level::Low]);
        assert_eq!(assess(&[1000, 900])[0].level, Level::Medium);
        assert_eq!(assess(&[1000, 980])[0].level, Level::Low);
        assert_eq!(assess(&[500])[0].level, Level::High);
    }

    #[test]
    fn margin_is_absolute_and_relative() {
        let first = assess(&[400, 300])[0];
        assert_eq!((first.margin, first.margin_permille), (100, 250));
        assert!(advice(first, 1).is_none());
        assert!(
            advice(assess(&[400, 390])[0], 3)
                .unwrap()
                .contains("3 results")
        );
    }
}
