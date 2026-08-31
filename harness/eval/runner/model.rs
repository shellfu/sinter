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
    pub ask_split: Split,
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

/// Deterministic, repository-local scenarios that exercise several Sinter
/// invocations as one agent decision. These cases complement the pinned
/// retrieval corpus; they do not replace its hand-labeled ranking metrics.
#[derive(Debug, Deserialize)]
pub struct AgentFlowSuite {
    pub schema: u32,
    pub fixture: AgentFixtureSpec,
    pub cases: Vec<AgentFlowSpec>,
}

#[derive(Debug, Deserialize)]
pub struct AgentFixtureSpec {
    pub base: String,
    pub committed_overlay: String,
}

#[derive(Debug, Deserialize)]
pub struct AgentFlowSpec {
    pub id: String,
    pub capability: AgentCapability,
    pub steps: Vec<AgentFlowStepSpec>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapability {
    Orientation,
    DependencyAnalysis,
    BlastRadius,
    TestSelection,
    UnresolvedAmbiguity,
    DiffImpact,
    StableHandleReuse,
    DirtyEdit,
    McpCliParity,
    BoundedContentSearch,
    RefactorCompleteness,
    SymbolBody,
    MultiSeedBlastRadius,
    TaskContext,
    DesignSimilarity,
}

impl AgentCapability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Orientation => "orientation",
            Self::DependencyAnalysis => "dependency_analysis",
            Self::BlastRadius => "blast_radius",
            Self::TestSelection => "test_selection",
            Self::UnresolvedAmbiguity => "unresolved_ambiguity",
            Self::DiffImpact => "diff_impact",
            Self::StableHandleReuse => "stable_handle_reuse",
            Self::DirtyEdit => "dirty_edit",
            Self::McpCliParity => "mcp_cli_parity",
            Self::BoundedContentSearch => "bounded_content_search",
            Self::RefactorCompleteness => "refactor_completeness",
            Self::SymbolBody => "symbol_body",
            Self::MultiSeedBlastRadius => "multi_seed_blast_radius",
            Self::TaskContext => "task_context",
            Self::DesignSimilarity => "design_similarity",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentFlowStepSpec {
    Cli {
        id: String,
        args: Vec<String>,
        #[serde(default)]
        expected_exit: i32,
        #[serde(default)]
        decision: AgentDecision,
        #[serde(default)]
        expect: Vec<JsonExpectation>,
        #[serde(default)]
        capture: Vec<JsonCapture>,
    },
    Mcp {
        id: String,
        tool: String,
        #[serde(default)]
        arguments: serde_json::Value,
        #[serde(default)]
        decision: AgentDecision,
        #[serde(default)]
        expect: Vec<JsonExpectation>,
        #[serde(default)]
        capture: Vec<JsonCapture>,
    },
    Edit {
        id: String,
        path: String,
        operation: FileEditOperation,
        value: String,
        #[serde(default)]
        find: Option<String>,
    },
    Compare {
        id: String,
        left: JsonReference,
        right: JsonReference,
    },
}

impl AgentFlowStepSpec {
    pub fn id(&self) -> &str {
        match self {
            Self::Cli { id, .. }
            | Self::Mcp { id, .. }
            | Self::Edit { id, .. }
            | Self::Compare { id, .. } => id,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDecision {
    #[default]
    Answer,
    Abstain,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEditOperation {
    Prepend,
    Append,
    Replace,
}

#[derive(Debug, Deserialize)]
pub struct JsonExpectation {
    #[serde(default)]
    pub pointer: String,
    pub predicate: JsonPredicate,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonPredicate {
    Exists,
    NonEmpty,
    Equals,
    Contains,
}

#[derive(Debug, Deserialize)]
pub struct JsonCapture {
    pub name: String,
    pub pointer: String,
}

#[derive(Debug, Deserialize)]
pub struct JsonReference {
    pub step: String,
    #[serde(default)]
    pub pointer: String,
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
    pub scope: EvaluationScope,
    pub evaluated_binary: EvaluatedBinary,
    pub repositories: Vec<RepositoryResult>,
    pub metrics: Metrics,
    pub minimums: Minimums,
    pub regressions: Vec<String>,
    pub cases: Vec<CaseResult>,
    pub agent_flows: Vec<AgentFlowResult>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationScope {
    All,
    Ask,
}

impl EvaluationScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Ask => "ask",
        }
    }

    pub fn includes(self, case: &CaseSpec) -> bool {
        self == Self::All || matches!(case, CaseSpec::Ask { .. })
    }
}

#[derive(Debug, Serialize)]
pub struct EvaluatedBinary {
    pub path: String,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct RepositoryResult {
    pub name: String,
    pub url: String,
    pub git_ref: String,
    pub commit: String,
    pub ask_split: Split,
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
    pub ask_holdout_confidence: ConfidenceCalibration,
    pub callers: CallerMetrics,
    pub paths: PathMetrics,
    pub agent_flows: AgentFlowMetrics,
}

#[derive(Debug, Default, Serialize)]
pub struct ConfidenceCalibration {
    pub cases: usize,
    pub rated_cases: usize,
    pub buckets: Vec<ConfidenceBucket>,
}

#[derive(Debug, Serialize)]
pub struct ConfidenceBucket {
    pub level: &'static str,
    pub cases: usize,
    pub correct: usize,
    pub precision: f64,
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

#[derive(Debug, Default, Serialize)]
pub struct AgentFlowMetrics {
    pub cases: usize,
    pub correct: usize,
    pub accuracy: f64,
    pub steps: usize,
    pub correct_steps: usize,
    pub tool_calls: usize,
    pub output_bytes: usize,
    pub abstention_cases: usize,
    pub correct_abstentions: usize,
    pub unsafe_confidence_failures: usize,
    pub stale_evidence_steps: usize,
    pub partial_evidence_steps: usize,
}

#[derive(Debug, Serialize)]
pub struct AgentFlowResult {
    pub id: String,
    pub capability: AgentCapability,
    pub correct: bool,
    pub tool_calls: usize,
    pub output_bytes: usize,
    pub steps: Vec<AgentFlowStepResult>,
}

#[derive(Debug, Serialize)]
pub struct AgentFlowStepResult {
    pub id: String,
    pub kind: &'static str,
    pub correct: bool,
    pub tool_calls: usize,
    pub output_bytes: usize,
    pub exit_code: Option<i32>,
    pub expected_decision: AgentDecision,
    pub abstained: bool,
    pub unsafe_confidence_failure: bool,
    pub evidence: AgentEvidence,
    pub assertions: Vec<AgentAssertionResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct AgentEvidence {
    pub coverage_status: Option<String>,
    pub compiler_index_state: Option<String>,
    pub dirty_snapshot: Option<bool>,
    pub stale: bool,
    pub partial: bool,
}

#[derive(Debug, Serialize)]
pub struct AgentAssertionResult {
    pub description: String,
    pub passed: bool,
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
        /// Confidence emitted by `ask`; absent for exact `query` cases.
        top_confidence: Option<String>,
        top_ranking_margin_permille: Option<i64>,
        top_calibration_version: Option<String>,
        top_calibration_sample_size: Option<usize>,
        top_calibration_measured_precision: Option<f64>,
        top_term_coverage_permille: Option<u16>,
        top_verify_required: Option<bool>,
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
