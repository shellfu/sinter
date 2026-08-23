use std::collections::HashMap;

use anyhow::{Context, Result};

use super::super::model::{
    AgentAssertionResult, AgentDecision, AgentEvidence, JsonExpectation, JsonPredicate,
};

pub fn evaluate_expectation(
    root: &serde_json::Value,
    expectation: &JsonExpectation,
) -> AgentAssertionResult {
    let actual = json_pointer(root, &expectation.pointer);
    let passed = match expectation.predicate {
        JsonPredicate::Exists => actual.is_some(),
        JsonPredicate::NonEmpty => actual.is_some_and(non_empty),
        JsonPredicate::Equals => actual
            .zip(expectation.value.as_ref())
            .is_some_and(|(a, b)| a == b),
        JsonPredicate::Contains => actual
            .zip(expectation.value.as_ref())
            .is_some_and(|(haystack, needle)| contains_json(haystack, needle)),
    };
    AgentAssertionResult {
        description: format!(
            "{} {:?} {}",
            if expectation.pointer.is_empty() {
                "/"
            } else {
                &expectation.pointer
            },
            expectation.predicate,
            expectation
                .value
                .as_ref()
                .map_or(String::new(), serde_json::Value::to_string)
        ),
        passed,
    }
}

pub fn json_pointer<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
) -> Option<&'a serde_json::Value> {
    if pointer.is_empty() {
        Some(value)
    } else {
        value.pointer(pointer)
    }
}

fn non_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
        serde_json::Value::String(value) => !value.is_empty(),
        serde_json::Value::Array(value) => !value.is_empty(),
        serde_json::Value::Object(value) => !value.is_empty(),
    }
}

fn contains_json(haystack: &serde_json::Value, needle: &serde_json::Value) -> bool {
    match (haystack, needle) {
        (serde_json::Value::String(haystack), serde_json::Value::String(needle)) => {
            haystack.contains(needle)
        }
        (serde_json::Value::Array(haystack), needle) => {
            haystack.iter().any(|value| contains_json(value, needle))
        }
        (serde_json::Value::Object(haystack), serde_json::Value::Object(needle)) => {
            needle.iter().all(|(key, value)| {
                haystack
                    .get(key)
                    .is_some_and(|item| contains_json(item, value))
            })
        }
        (haystack, needle) => haystack == needle,
    }
}

pub fn interpolate_args(
    args: &[String],
    captures: &HashMap<String, serde_json::Value>,
) -> Result<Vec<String>> {
    args.iter()
        .map(|argument| interpolate_string(argument, captures))
        .collect()
}

pub fn interpolate_json(
    value: &serde_json::Value,
    captures: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    match value {
        serde_json::Value::String(value) => {
            if let Some(name) = exact_variable(value) {
                return captures
                    .get(name)
                    .cloned()
                    .with_context(|| format!("unknown capture {name:?}"));
            }
            Ok(serde_json::Value::String(interpolate_string(
                value, captures,
            )?))
        }
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| interpolate_json(value, captures))
            .collect(),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), interpolate_json(value, captures)?)))
            .collect(),
        value => Ok(value.clone()),
    }
}

fn interpolate_string(
    template: &str,
    captures: &HashMap<String, serde_json::Value>,
) -> Result<String> {
    let mut output = template.to_owned();
    while let Some(start) = output.find("${") {
        let end = output[start + 2..]
            .find('}')
            .map(|offset| start + 2 + offset)
            .context("unterminated capture interpolation")?;
        let name = &output[start + 2..end];
        let value = captures
            .get(name)
            .with_context(|| format!("unknown capture {name:?}"))?;
        let replacement = match value {
            serde_json::Value::String(value) => value.clone(),
            value => value.to_string(),
        };
        output.replace_range(start..=end, &replacement);
    }
    Ok(output)
}

fn exact_variable(value: &str) -> Option<&str> {
    value
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
        .filter(|value| !value.contains("${"))
}

pub fn observe_evidence(value: &serde_json::Value) -> AgentEvidence {
    let coverage = value
        .get("coverage")
        .or_else(|| value.get("health"))
        .unwrap_or(value);
    let coverage_status = coverage
        .get("status")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let compiler_index_state = coverage
        .pointer("/compiler_index/state")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let dirty_snapshot = coverage
        .pointer("/snapshot/dirty")
        .and_then(serde_json::Value::as_bool);
    let stale = compiler_index_state.as_deref() == Some("stale")
        || coverage
            .get("stale")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
    let partial = matches!(coverage_status.as_deref(), Some("not_proven" | "partial"))
        || coverage
            .get("conclusive")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        || coverage
            .get("partial")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
    AgentEvidence {
        coverage_status,
        compiler_index_state,
        dirty_snapshot,
        stale,
        partial,
    }
}

pub fn confidence(value: &serde_json::Value) -> Option<&str> {
    value
        .pointer("/topics/0/confidence/level")
        .or_else(|| value.pointer("/results/0/confidence"))
        .or_else(|| value.pointer("/hits/0/confidence"))
        .or_else(|| value.pointer("/0/confidence"))
        .or_else(|| value.get("confidence"))
        .and_then(serde_json::Value::as_str)
}

pub fn is_abstention_response(value: &serde_json::Value) -> bool {
    value.get("error").is_some()
        || value
            .pointer("/topics/0/status")
            .and_then(serde_json::Value::as_str)
            == Some("abstain")
        || value
            .get("code")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|code| {
                matches!(
                    code,
                    "ambiguous_symbol" | "no_match" | "invalid_arguments" | "execution_error"
                )
            })
        || value
            .pointer("/outcome/status")
            .and_then(serde_json::Value::as_str)
            == Some("error")
}

pub const fn decision_label(decision: AgentDecision) -> &'static str {
    match decision {
        AgentDecision::Answer => "answer",
        AgentDecision::Abstain => "abstain",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::{contains_json, interpolate_json, observe_evidence};

    #[test]
    fn interpolation_preserves_whole_json_values_and_embeds_strings() {
        let captures = HashMap::from([
            ("handle".to_owned(), json!("src/lib.rs#leaf@10")),
            ("limit".to_owned(), json!(3)),
        ]);
        let value = interpolate_json(
            &json!({"symbol": "${handle}", "limit": "${limit}", "label": "id=${handle}"}),
            &captures,
        )
        .unwrap();
        assert_eq!(value["symbol"], "src/lib.rs#leaf@10");
        assert_eq!(value["limit"], 3);
        assert_eq!(value["label"], "id=src/lib.rs#leaf@10");
    }

    #[test]
    fn contains_matches_object_subsets_inside_arrays() {
        let value = json!([{"s": "leaf", "f": "src/lib.rs", "extra": true}]);
        assert!(contains_json(&value, &json!({"s": "leaf"})));
        assert!(!contains_json(&value, &json!({"s": "other"})));
    }

    #[test]
    fn coverage_observation_marks_not_proven_and_stale() {
        let evidence = observe_evidence(&json!({
            "coverage": {
                "status": "not_proven",
                "conclusive": false,
                "snapshot": {"dirty": true},
                "compiler_index": {"state": "stale"}
            }
        }));
        assert!(evidence.partial);
        assert!(evidence.stale);
        assert_eq!(evidence.dirty_snapshot, Some(true));
    }

    #[test]
    fn map_health_observation_marks_partial_inventory() {
        let evidence = observe_evidence(&json!({
            "health": {
                "status": "partial",
                "snapshot": {"dirty": false},
                "compiler_index": {"state": "missing"}
            }
        }));
        assert!(evidence.partial);
        assert!(!evidence.stale);
        assert_eq!(evidence.coverage_status.as_deref(), Some("partial"));
        assert_eq!(evidence.compiler_index_state.as_deref(), Some("missing"));
    }
}
