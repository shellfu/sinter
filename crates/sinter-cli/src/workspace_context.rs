//! One bounded context packet across a declared multi-repository workspace.
//!
//! Member context packets keep their repository-local ranking and excerpts;
//! this module only federates their best candidates, tests, gaps, and trust
//! envelope. It does not invent a cross-repository relevance score.

use std::cmp::Ordering;
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};

const CANDIDATE_LIMIT: usize = 10;
const TEST_LIMIT: usize = 20;

fn annotate_candidate(mut candidate: Value, member: &str) -> Value {
    candidate["member"] = json!(member);
    candidate["member_rank"] = candidate["rank"].clone();
    if let Some(id) = candidate["id"].as_str() {
        candidate["id"] = json!(format!("{member}:{id}"));
    }
    if let Some(qualified) = candidate["qualified"].as_str() {
        candidate["qualified"] = json!(format!("{member}:{qualified}"));
    }
    candidate
}

fn candidate_order(left: &Value, right: &Value) -> Ordering {
    let priority = |value: &Value| {
        if value["anchor"].is_string() {
            0
        } else if value["focus"] == true {
            1
        } else {
            2
        }
    };
    priority(left)
        .cmp(&priority(right))
        .then_with(|| {
            left["member_rank"]
                .as_u64()
                .cmp(&right["member_rank"].as_u64())
        })
        .then_with(|| left["member"].as_str().cmp(&right["member"].as_str()))
        .then_with(|| left["qualified"].as_str().cmp(&right["qualified"].as_str()))
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Shared producer for CLI and workspace-scoped MCP.
pub(crate) fn response(workspace: &crate::workspace::Workspace, task: &str) -> Result<Value> {
    let mut candidates = Vec::new();
    let mut tests = Vec::new();
    let mut tests_total = 0u64;
    let mut anchors = Vec::new();
    let mut unresolved_intents = Vec::new();
    let mut member_summaries = Vec::new();
    let mut member_gaps = serde_json::Map::new();
    let mut certain = 0u64;
    let mut possible = 0u64;
    let mut unresolved = 0u64;

    for (member, repo) in &workspace.members {
        let store = crate::lookup::open_current(repo)?;
        let packet = crate::context::response(repo, &store, task)?;
        candidates.extend(
            packet["candidates"]
                .as_array()
                .into_iter()
                .flatten()
                .cloned()
                .map(|candidate| annotate_candidate(candidate, member)),
        );
        tests.extend(
            packet["tests"]
                .as_array()
                .into_iter()
                .flatten()
                .cloned()
                .map(|mut test| {
                    test["member"] = json!(member);
                    if let Some(qualified) = test["qualified"].as_str() {
                        test["qualified"] = json!(format!("{member}:{qualified}"));
                    }
                    test
                }),
        );
        tests_total += packet["tests_total"].as_u64().unwrap_or(0);
        anchors.extend(
            packet["anchors"]
                .as_array()
                .into_iter()
                .flatten()
                .cloned()
                .map(|mut anchor| {
                    anchor["member"] = json!(member);
                    if let Some(qualified) = anchor["qualified"].as_str() {
                        anchor["qualified"] = json!(format!("{member}:{qualified}"));
                    }
                    anchor
                }),
        );
        unresolved_intents.extend(
            packet["unresolved_intents"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|intent| json!({"member": member, "intent": intent})),
        );
        certain += packet["coverage"]["evidence"]["certain"]["results"]
            .as_u64()
            .unwrap_or(0);
        possible += packet["coverage"]["evidence"]["possible"]["results"]
            .as_u64()
            .unwrap_or(0);
        unresolved += packet["coverage"]["evidence"]["unresolved"]["matching_query"]
            .as_u64()
            .unwrap_or(0);
        member_gaps.insert(member.clone(), packet["gaps"].clone());
        member_summaries.push(json!({
            "member": member,
            "root": repo,
            "outcome": packet["outcome"],
            "candidates": packet["candidates"].as_array().map_or(0, Vec::len),
            "tests": packet["tests_total"],
        }));
    }

    candidates.sort_by(candidate_order);
    let candidates_total = candidates.len();
    candidates.truncate(CANDIDATE_LIMIT);
    for (rank, candidate) in candidates.iter_mut().enumerate() {
        candidate["rank"] = json!(rank + 1);
    }
    tests.sort_by(|left, right| {
        left["member"]
            .as_str()
            .cmp(&right["member"].as_str())
            .then_with(|| left["qualified"].as_str().cmp(&right["qualified"].as_str()))
    });
    tests.dedup_by(|left, right| {
        left["member"] == right["member"] && left["qualified"] == right["qualified"]
    });
    tests.truncate(TEST_LIMIT);

    let ranked = candidates
        .iter()
        .any(|candidate| candidate["focus"] == true);
    let filter = sinter_store::EdgeFilter::default();
    let coverage = crate::coverage::workspace_json(
        workspace,
        &filter,
        crate::coverage::TraversalEvidence {
            certain: certain as usize,
            possible: possible as usize,
            unresolved: unresolved as usize,
        },
        ranked,
    )?;
    let next_actions = candidates
        .iter()
        .filter(|candidate| candidate["focus"] == true)
        .take(3)
        .flat_map(|candidate| {
            let member = candidate["member"].as_str().unwrap_or("");
            let qualified = candidate["qualified"].as_str().unwrap_or("");
            let local = qualified
                .strip_prefix(&format!("{member}:"))
                .unwrap_or(qualified);
            let root = &workspace.members[member];
            [
                format!(
                    "sinter show {} --repo {}",
                    shell_arg(local),
                    shell_arg(&root.display().to_string())
                ),
                format!(
                    "sinter affected {} --workspace {} --max-depth 3",
                    shell_arg(qualified),
                    shell_arg(&workspace.manifest_path.display().to_string())
                ),
                format!(
                    "sinter impact HEAD --repo {} --workspace {}",
                    shell_arg(&root.display().to_string()),
                    shell_arg(&workspace.manifest_path.display().to_string())
                ),
            ]
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "task": task,
        "snapshot": crate::workspace::snapshot_token(workspace)?,
        "outcome": if ranked { "ranked" } else { "abstain" },
        "members": member_summaries,
        "anchors": anchors,
        "unresolved_intents": unresolved_intents,
        "candidates": candidates,
        "candidates_total": candidates_total,
        "tests": tests,
        "tests_total": tests_total,
        "gaps": {"members": member_gaps},
        "coverage": coverage,
        "next_actions": next_actions,
    }))
}

fn print_packet(packet: &Value) {
    println!(
        "workspace context: {}  [{}]",
        packet["task"].as_str().unwrap_or(""),
        packet["outcome"].as_str().unwrap_or("abstain")
    );
    for candidate in packet["candidates"].as_array().into_iter().flatten() {
        let marker = if candidate["focus"] == true { "*" } else { " " };
        println!(
            "{marker}{}. {}  {}:{}",
            candidate["rank"],
            candidate["qualified"].as_str().unwrap_or("?"),
            candidate["file"].as_str().unwrap_or("?"),
            candidate["line"],
        );
    }
    println!(
        "tests: {} · candidates: {}",
        packet["tests_total"], packet["candidates_total"]
    );
    crate::coverage::print_traversal_footer(&packet["coverage"], packet["snapshot"].as_str());
    println!("next:");
    for action in packet["next_actions"].as_array().into_iter().flatten() {
        println!("  {}", action.as_str().unwrap_or(""));
    }
}

pub(crate) fn run(manifest: &Path, task: &str, json_output: bool) -> Result<bool> {
    let workspace = crate::workspace::load(manifest)?;
    for repo in workspace.members.values() {
        crate::pipeline::build(repo, None)?;
    }
    if !crate::workspace::stale_members(&workspace)?.is_empty() {
        crate::workspace::refresh(&workspace)?;
    }
    let packet = response(&workspace, task)?;
    let ranked = packet["outcome"] == "ranked";
    if json_output {
        crate::agent_protocol::write_json(&packet)?;
    } else {
        print_packet(&packet);
    }
    Ok(ranked)
}
