use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct SuiteSpec {
    pub schema: u32,
    pub repositories: Vec<RepositorySpec>,
    pub minimums: Minimums,
    pub cases: Vec<CaseSpec>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RepositorySpec {
    pub name: String,
    pub url: String,
    pub git_ref: String,
    pub commit: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Minimums {
    pub query_mrr: f64,
    pub query_recall_at_limit: f64,
    pub ask_top_1_accuracy: f64,
    pub ask_mrr: f64,
    pub ask_recall_at_5: f64,
    pub ask_recall_at_limit: f64,
    pub ask_holdout_top_1_accuracy: f64,
    pub caller_precision: f64,
    pub caller_recall: f64,
    pub path_accuracy: f64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaseSpec {
    Query {
        id: String,
        repository: String,
        input: String,
        limit: usize,
        relevant: Vec<SymbolKey>,
    },
    Ask {
        id: String,
        repository: String,
        input: String,
        limit: usize,
        intent: String,
        split: Split,
        relevant: Vec<SymbolKey>,
    },
    Callers {
        id: String,
        repository: String,
        symbol: String,
        expected: Vec<SymbolKey>,
    },
    Path {
        id: String,
        repository: String,
        from: String,
        to: String,
        relations: Vec<String>,
        expect: PathExpectation,
        /// Run with an untracked scratch file in the working tree; a
        /// negative answer must then report the snapshot as dirty.
        #[serde(default)]
        dirty: bool,
    },
}

impl CaseSpec {
    pub fn id(&self) -> &str {
        match self {
            Self::Query { id, .. }
            | Self::Ask { id, .. }
            | Self::Callers { id, .. }
            | Self::Path { id, .. } => id,
        }
    }

    pub fn repository(&self) -> &str {
        match self {
            Self::Query { repository, .. }
            | Self::Ask { repository, .. }
            | Self::Callers { repository, .. }
            | Self::Path { repository, .. } => repository,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SymbolKey {
    pub qualified: String,
    pub file: String,
}

/// Tuning cases may drive weight changes; holdout cases only measure them.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Tuning,
    Holdout,
}

impl Split {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tuning => "tuning",
            Self::Holdout => "holdout",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathExpectation {
    Found,
    NotProven,
}

impl PathExpectation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Found => "found",
            Self::NotProven => "not_proven",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Scorecard {
    pub schema: u32,
    pub suite_schema: u32,
    pub generated_at_unix_seconds: u64,
    pub repositories: Vec<RepositoryResult>,
    pub metrics: Metrics,
    pub minimums: Minimums,
    pub regressions: Vec<String>,
    pub cases: Vec<CaseResult>,
}

#[derive(Debug, Serialize)]
pub struct RepositoryResult {
    pub name: String,
    pub url: String,
    pub git_ref: String,
    pub commit: String,
    pub compiler_index: &'static str,
    pub build_duration_ms: u128,
}

#[derive(Debug, Serialize)]
pub struct Metrics {
    pub query: RankingMetrics,
    pub ask: RankingMetrics,
    pub ask_by_split: Vec<LabeledRanking>,
    pub ask_by_repository: Vec<LabeledRanking>,
    pub ask_by_intent: Vec<LabeledRanking>,
    pub callers: CallerMetrics,
    pub paths: PathMetrics,
}

#[derive(Debug, Serialize)]
pub struct LabeledRanking {
    pub label: String,
    #[serde(flatten)]
    pub metrics: RankingMetrics,
}

#[derive(Debug, Default, Serialize)]
pub struct RankingMetrics {
    pub cases: usize,
    pub top_1_accuracy: f64,
    pub mean_reciprocal_rank: f64,
    pub mean_recall_at_5: f64,
    pub mean_recall_at_limit: f64,
    pub candidate_miss_cases: usize,
    pub p50_duration_ms: u128,
    pub p95_duration_ms: u128,
}

#[derive(Debug, Default, Serialize)]
pub struct CallerMetrics {
    pub cases: usize,
    pub true_positives: usize,
    pub returned: usize,
    pub expected: usize,
    pub precision: f64,
    pub recall: f64,
}

#[derive(Debug, Default, Serialize)]
pub struct PathMetrics {
    pub cases: usize,
    pub correct: usize,
    pub accuracy: f64,
}

#[derive(Debug, Serialize)]
pub struct CaseResult {
    pub id: String,
    pub repository: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<Split>,
    pub duration_ms: u128,
    #[serde(flatten)]
    pub outcome: CaseOutcome,
}

#[derive(Debug, Serialize)]
#[serde(tag = "measurement", rename_all = "snake_case")]
pub enum CaseOutcome {
    Ranking {
        input: String,
        limit: usize,
        first_relevant_rank: Option<usize>,
        top_1_correct: bool,
        relevant_found: usize,
        relevant_total: usize,
        reciprocal_rank: f64,
        recall_at_5: f64,
        recall_at_limit: f64,
        candidate_pool_size: usize,
        candidate_relevant_found: usize,
        candidate_miss: bool,
        /// Top result when it is not relevant; None when top-1 is correct.
        top_incorrect: Option<SymbolKey>,
        returned: Vec<RankedSymbol>,
    },
    Callers {
        symbol: String,
        true_positives: usize,
        returned_total: usize,
        expected_total: usize,
        precision: f64,
        recall: f64,
        expected: Vec<SymbolKey>,
        returned: Vec<SymbolKey>,
    },
    Path {
        from: String,
        to: String,
        expected: &'static str,
        observed: &'static str,
        correct: bool,
        coverage_status: Option<String>,
        /// `coverage.snapshot.dirty` when a coverage envelope was returned.
        dirty_snapshot: Option<bool>,
        steps: Vec<serde_json::Value>,
    },
}

#[derive(Debug, Serialize)]
pub struct RankedSymbol {
    pub rank: usize,
    #[serde(flatten)]
    pub symbol: SymbolKey,
}
