use std::collections::HashSet;
use std::path::{Component, Path};

use anyhow::{Result, bail};

use super::super::model::{
    AgentCapability, AgentDecision, AgentFlowStepSpec, AgentFlowSuite, FileEditOperation,
    JsonCapture, JsonExpectation, JsonPredicate, JsonReference,
};

const AGENT_FLOW_SUITE_SCHEMA: u32 = 1;

pub fn validate(suite: &AgentFlowSuite) -> Result<()> {
    if suite.schema != AGENT_FLOW_SUITE_SCHEMA {
        bail!(
            "unsupported agent-flow suite schema {}, expected {}",
            suite.schema,
            AGENT_FLOW_SUITE_SCHEMA
        );
    }
    validate_relative_path(&suite.fixture.base)?;
    validate_relative_path(&suite.fixture.committed_overlay)?;
    if suite.cases.is_empty() {
        bail!("agent-flow suite needs at least one case");
    }
    let mut case_ids = HashSet::new();
    let mut capabilities = HashSet::new();
    for case in &suite.cases {
        if case.id.trim().is_empty() || !case_ids.insert(case.id.as_str()) {
            bail!("agent-flow case ids must be non-empty and unique");
        }
        if case.steps.len() < 2 {
            bail!("agent-flow case {} needs at least two steps", case.id);
        }
        capabilities.insert(case.capability);
        let mut step_ids = HashSet::new();
        let mut available_results = HashSet::new();
        let mut available_captures = HashSet::new();
        for step in &case.steps {
            if step.id().trim().is_empty() || !step_ids.insert(step.id()) {
                bail!("case {} has an empty or duplicate step id", case.id);
            }
            match step {
                AgentFlowStepSpec::Cli {
                    args,
                    expected_exit,
                    decision,
                    expect,
                    capture,
                    ..
                } => {
                    if args.is_empty() {
                        bail!("case {} step {} has no CLI arguments", case.id, step.id());
                    }
                    if *decision == AgentDecision::Abstain && *expected_exit == 0 {
                        bail!(
                            "case {} step {} expects abstention but a zero exit",
                            case.id,
                            step.id()
                        );
                    }
                    validate_expectations(&case.id, step.id(), expect)?;
                    validate_captures(&case.id, step.id(), capture, &mut available_captures)?;
                    available_results.insert(step.id());
                }
                AgentFlowStepSpec::Mcp {
                    tool,
                    arguments,
                    expect,
                    capture,
                    ..
                } => {
                    if tool.trim().is_empty() {
                        bail!("case {} step {} has no MCP tool", case.id, step.id());
                    }
                    if !arguments.is_object() {
                        bail!(
                            "case {} step {} MCP arguments must be an object",
                            case.id,
                            step.id()
                        );
                    }
                    validate_expectations(&case.id, step.id(), expect)?;
                    validate_captures(&case.id, step.id(), capture, &mut available_captures)?;
                    available_results.insert(step.id());
                }
                AgentFlowStepSpec::Edit {
                    path,
                    operation,
                    find,
                    ..
                } => {
                    validate_relative_path(path)?;
                    if matches!(operation, FileEditOperation::Replace) && find.is_none() {
                        bail!(
                            "case {} step {} replace edit needs find text",
                            case.id,
                            step.id()
                        );
                    }
                }
                AgentFlowStepSpec::Compare { left, right, .. } => {
                    validate_reference(&case.id, step.id(), left, &available_results)?;
                    validate_reference(&case.id, step.id(), right, &available_results)?;
                }
            }
        }
    }
    for required in [
        AgentCapability::Orientation,
        AgentCapability::DependencyAnalysis,
        AgentCapability::BlastRadius,
        AgentCapability::TestSelection,
        AgentCapability::UnresolvedAmbiguity,
        AgentCapability::DiffImpact,
        AgentCapability::StableHandleReuse,
        AgentCapability::DirtyEdit,
        AgentCapability::McpCliParity,
    ] {
        if !capabilities.contains(&required) {
            bail!(
                "agent-flow suite does not cover capability {}",
                required.as_str()
            );
        }
    }
    Ok(())
}

fn validate_captures<'a>(
    case_id: &str,
    step_id: &str,
    captures: &'a [JsonCapture],
    available: &mut HashSet<&'a str>,
) -> Result<()> {
    for capture in captures {
        if capture.name.trim().is_empty() || !available.insert(capture.name.as_str()) {
            bail!("case {case_id} step {step_id} has a duplicate or empty capture");
        }
        validate_json_pointer(case_id, step_id, &capture.pointer)?;
    }
    Ok(())
}

fn validate_expectations(
    case_id: &str,
    step_id: &str,
    expectations: &[JsonExpectation],
) -> Result<()> {
    for expectation in expectations {
        validate_json_pointer(case_id, step_id, &expectation.pointer)?;
        if matches!(
            expectation.predicate,
            JsonPredicate::Equals | JsonPredicate::Contains
        ) && expectation.value.is_none()
        {
            bail!(
                "case {case_id} step {step_id} {:?} expectation needs a value",
                expectation.predicate
            );
        }
    }
    Ok(())
}

fn validate_json_pointer(case_id: &str, step_id: &str, pointer: &str) -> Result<()> {
    if !pointer.is_empty() && !pointer.starts_with('/') {
        bail!("case {case_id} step {step_id} has invalid JSON pointer {pointer:?}");
    }
    Ok(())
}

fn validate_reference(
    case_id: &str,
    step_id: &str,
    reference: &JsonReference,
    available: &HashSet<&str>,
) -> Result<()> {
    validate_json_pointer(case_id, step_id, &reference.pointer)?;
    if !available.contains(reference.step.as_str()) {
        bail!(
            "case {case_id} step {step_id} compares unavailable step {}",
            reference.step
        );
    }
    Ok(())
}

pub fn validate_relative_path(relative: &str) -> Result<()> {
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        bail!("path must stay within the evaluation root: {relative:?}");
    }
    Ok(())
}
