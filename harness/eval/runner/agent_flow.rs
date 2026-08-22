mod fixture;
mod invocation;
mod json_contract;
mod schema;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use self::invocation::{Invocation, run_cli, run_mcp};
use self::json_contract::{
    confidence, decision_label, evaluate_expectation, interpolate_args, interpolate_json,
    is_abstention_response, json_pointer, observe_evidence,
};
use super::model::{
    AgentAssertionResult, AgentDecision, AgentEvidence, AgentFlowMetrics, AgentFlowResult,
    AgentFlowSpec, AgentFlowStepResult, AgentFlowStepSpec, AgentFlowSuite, FileEditOperation,
    JsonCapture, JsonExpectation, JsonReference,
};

pub fn load_suite(workspace: &Path) -> Result<AgentFlowSuite> {
    let path = workspace.join("harness/eval/agent-flows.json");
    let suite = serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", path.display()))?;
    schema::validate(&suite)?;
    Ok(suite)
}

pub fn run_suite(
    sinter: &Path,
    workspace: &Path,
    suite: &AgentFlowSuite,
) -> Result<Vec<AgentFlowResult>> {
    let scratch =
        tempfile::tempdir().context("failed to create agent-flow evaluation directory")?;
    suite
        .cases
        .iter()
        .enumerate()
        .map(|(index, case)| {
            let repository = scratch.path().join(format!("{index:02}-{}", case.id));
            fixture::prepare(workspace, &suite.fixture, &repository)?;
            super::command::build_graph(sinter, &repository)?;
            evaluate_flow(sinter, &repository, case)
        })
        .collect()
}

pub fn aggregate_metrics(results: &[AgentFlowResult]) -> AgentFlowMetrics {
    let mut metrics = AgentFlowMetrics {
        cases: results.len(),
        correct: results.iter().filter(|result| result.correct).count(),
        ..AgentFlowMetrics::default()
    };
    for result in results {
        metrics.steps += result.steps.len();
        metrics.correct_steps += result.steps.iter().filter(|step| step.correct).count();
        metrics.tool_calls += result.tool_calls;
        metrics.output_bytes += result.output_bytes;
        for step in &result.steps {
            if step.expected_decision == AgentDecision::Abstain {
                metrics.abstention_cases += 1;
                metrics.correct_abstentions += usize::from(step.abstained && step.correct);
            }
            metrics.unsafe_confidence_failures += usize::from(step.unsafe_confidence_failure);
            metrics.stale_evidence_steps += usize::from(step.evidence.stale);
            metrics.partial_evidence_steps += usize::from(step.evidence.partial);
        }
    }
    metrics.accuracy = ratio(metrics.correct, metrics.cases);
    metrics
}

fn evaluate_flow(
    sinter: &Path,
    repository: &Path,
    case: &AgentFlowSpec,
) -> Result<AgentFlowResult> {
    let mut captures = HashMap::<String, serde_json::Value>::new();
    let mut outputs = HashMap::<String, serde_json::Value>::new();
    let mut steps = Vec::new();
    for step in &case.steps {
        let result = match step {
            AgentFlowStepSpec::Cli {
                id,
                args,
                expected_exit,
                decision,
                expect,
                capture,
            } => match interpolate_args(args, &captures) {
                Ok(args) => evaluate_invocation(
                    id,
                    "cli",
                    *decision,
                    *expected_exit,
                    expect,
                    capture,
                    run_cli(sinter, repository, &args),
                    &mut captures,
                    &mut outputs,
                ),
                Err(error) => failed_step(id, "cli", *decision, error),
            },
            AgentFlowStepSpec::Mcp {
                id,
                tool,
                arguments,
                decision,
                expect,
                capture,
            } => match interpolate_json(arguments, &captures) {
                Ok(arguments) => evaluate_invocation(
                    id,
                    "mcp",
                    *decision,
                    i32::from(*decision == AgentDecision::Abstain),
                    expect,
                    capture,
                    run_mcp(sinter, repository, tool, arguments),
                    &mut captures,
                    &mut outputs,
                ),
                Err(error) => failed_step(id, "mcp", *decision, error),
            },
            AgentFlowStepSpec::Edit {
                id,
                path,
                operation,
                value,
                find,
            } => evaluate_edit(repository, id, path, *operation, value, find.as_deref()),
            AgentFlowStepSpec::Compare { id, left, right } => {
                evaluate_compare(id, left, right, &outputs)
            }
        };
        steps.push(result);
    }
    let tool_calls = steps.iter().map(|step| step.tool_calls).sum();
    let output_bytes = steps.iter().map(|step| step.output_bytes).sum();
    Ok(AgentFlowResult {
        id: case.id.clone(),
        capability: case.capability,
        correct: steps.iter().all(|step| step.correct),
        tool_calls,
        output_bytes,
        steps,
    })
}

#[allow(clippy::too_many_arguments)]
fn evaluate_invocation(
    id: &str,
    kind: &'static str,
    expected_decision: AgentDecision,
    expected_exit: i32,
    expectations: &[JsonExpectation],
    capture_specs: &[JsonCapture],
    invocation: Result<Invocation>,
    captures: &mut HashMap<String, serde_json::Value>,
    outputs: &mut HashMap<String, serde_json::Value>,
) -> AgentFlowStepResult {
    let invocation = match invocation {
        Ok(invocation) => invocation,
        Err(error) => return failed_step(id, kind, expected_decision, error),
    };
    let abstained = invocation
        .value
        .as_ref()
        .is_some_and(is_abstention_response)
        || (invocation.exit_code != 0 && invocation.value.is_none());
    let mut assertions = vec![AgentAssertionResult {
        description: format!(
            "exit code is {} (expected {expected_exit})",
            invocation.exit_code
        ),
        passed: invocation.exit_code == expected_exit,
    }];
    assertions.push(AgentAssertionResult {
        description: format!("decision is {}", decision_label(expected_decision)),
        passed: abstained == (expected_decision == AgentDecision::Abstain),
    });
    let mut error = invocation.parse_error;
    if let Some(value) = invocation.value.as_ref() {
        assertions.extend(
            expectations
                .iter()
                .map(|expectation| evaluate_expectation(value, expectation)),
        );
        for capture in capture_specs {
            match json_pointer(value, &capture.pointer) {
                Some(captured) => {
                    captures.insert(capture.name.clone(), captured.clone());
                }
                None => {
                    assertions.push(AgentAssertionResult {
                        description: format!("capture {} at {}", capture.name, capture.pointer),
                        passed: false,
                    });
                }
            }
        }
        outputs.insert(id.to_owned(), value.clone());
    } else {
        assertions.push(AgentAssertionResult {
            description: "machine-readable JSON output".to_owned(),
            passed: false,
        });
        if error.is_none() {
            error = Some("command returned no JSON output".to_owned());
        }
    }
    let assertions_passed = assertions.iter().all(|assertion| assertion.passed);
    let unsafe_confidence_failure = (expected_decision == AgentDecision::Abstain && !abstained)
        || (!assertions_passed
            && invocation
                .value
                .as_ref()
                .and_then(confidence)
                .is_some_and(|level| matches!(level, "high" | "medium")));
    let observed = invocation.value.clone();
    AgentFlowStepResult {
        id: id.to_owned(),
        kind,
        correct: assertions_passed && error.is_none(),
        tool_calls: 1,
        output_bytes: invocation.output_bytes,
        exit_code: Some(invocation.exit_code),
        expected_decision,
        abstained,
        unsafe_confidence_failure,
        evidence: invocation
            .value
            .as_ref()
            .map_or_else(AgentEvidence::default, observe_evidence),
        assertions,
        observed,
        error,
    }
}

fn evaluate_edit(
    repository: &Path,
    id: &str,
    relative: &str,
    operation: FileEditOperation,
    value: &str,
    find: Option<&str>,
) -> AgentFlowStepResult {
    let result = (|| -> Result<()> {
        let path = safe_join(repository, relative)?;
        let current = fs::read_to_string(&path)
            .with_context(|| format!("failed to read edit target {}", path.display()))?;
        let next = match operation {
            FileEditOperation::Prepend => format!("{value}{current}"),
            FileEditOperation::Append => format!("{current}{value}"),
            FileEditOperation::Replace => {
                let find = find.context("replace edit is missing find text")?;
                if !current.contains(find) {
                    bail!("edit target does not contain replacement text {find:?}");
                }
                current.replacen(find, value, 1)
            }
        };
        fs::write(&path, next).with_context(|| format!("failed to write {}", path.display()))
    })();
    let (correct, error) = match result {
        Ok(()) => (true, None),
        Err(error) => (false, Some(format!("{error:#}"))),
    };
    AgentFlowStepResult {
        id: id.to_owned(),
        kind: "edit",
        correct,
        tool_calls: 0,
        output_bytes: 0,
        exit_code: None,
        expected_decision: AgentDecision::Answer,
        abstained: false,
        unsafe_confidence_failure: false,
        evidence: AgentEvidence::default(),
        assertions: vec![AgentAssertionResult {
            description: "fixture edit applied".to_owned(),
            passed: correct,
        }],
        observed: None,
        error,
    }
}

fn evaluate_compare(
    id: &str,
    left: &JsonReference,
    right: &JsonReference,
    outputs: &HashMap<String, serde_json::Value>,
) -> AgentFlowStepResult {
    let result = (|| -> Result<bool> {
        let left_value = outputs
            .get(&left.step)
            .with_context(|| format!("step {} has no JSON output", left.step))?;
        let right_value = outputs
            .get(&right.step)
            .with_context(|| format!("step {} has no JSON output", right.step))?;
        let left_value = json_pointer(left_value, &left.pointer)
            .with_context(|| format!("{} has no value at {}", left.step, left.pointer))?;
        let right_value = json_pointer(right_value, &right.pointer)
            .with_context(|| format!("{} has no value at {}", right.step, right.pointer))?;
        Ok(left_value == right_value)
    })();
    let (correct, error) = match result {
        Ok(matches) => (matches, None),
        Err(error) => (false, Some(format!("{error:#}"))),
    };
    AgentFlowStepResult {
        id: id.to_owned(),
        kind: "compare",
        correct,
        tool_calls: 0,
        output_bytes: 0,
        exit_code: None,
        expected_decision: AgentDecision::Answer,
        abstained: false,
        unsafe_confidence_failure: false,
        evidence: AgentEvidence::default(),
        assertions: vec![AgentAssertionResult {
            description: format!(
                "{}.{} equals {}.{}",
                left.step, left.pointer, right.step, right.pointer
            ),
            passed: correct,
        }],
        observed: None,
        error,
    }
}

fn failed_step(
    id: &str,
    kind: &'static str,
    expected_decision: AgentDecision,
    error: impl std::fmt::Display,
) -> AgentFlowStepResult {
    AgentFlowStepResult {
        id: id.to_owned(),
        kind,
        correct: false,
        tool_calls: usize::from(matches!(kind, "cli" | "mcp")),
        output_bytes: 0,
        exit_code: None,
        expected_decision,
        abstained: false,
        unsafe_confidence_failure: expected_decision == AgentDecision::Abstain,
        evidence: AgentEvidence::default(),
        assertions: Vec::new(),
        observed: None,
        error: Some(error.to_string()),
    }
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    schema::validate_relative_path(relative)?;
    Ok(root.join(relative))
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}
