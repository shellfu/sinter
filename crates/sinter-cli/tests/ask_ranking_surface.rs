//! Focused acceptance tests for action-oriented `ask` ranking policy.

use std::path::Path;
use std::process::Command;

fn sinter(repo: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .env("USERPROFILE", repo)
        .output()
        .expect("run sinter");
    (
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn fixture(repo: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/json.rs"),
        r#"
pub struct JsonSink;

impl JsonSink {
    pub fn matched(&mut self, searcher: &Searcher, found: &Match) -> bool {
        true
    }

    pub fn has_match(&self) -> bool {
        true
    }
}

pub struct Json;

impl Json {
    /// Returns whether this printer has written output during a search.
    pub fn has_written(&self) -> bool {
        true
    }
}

pub struct Searcher;
pub struct Match;
"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("src/command.rs"),
        r#"
pub struct Command;

impl Command {
    /// Add a child command to this parent.
    pub fn add_command(&mut self, child: Command) {}

    /// Traverse the command tree and parse arguments for each parent.
    pub fn traverse(&self, args: &[String]) {}

    pub fn all_child_commands_have_group(&self) -> bool {
        false
    }
}
"#,
    )
    .unwrap();
}

fn ask_json(repo: &Path, question: &str) -> Vec<serde_json::Value> {
    let (ok, output) = sinter(repo, &["ask", question, "--json", "--limit", "10"]);
    assert!(ok, "{output}");
    serde_json::from_str(&output).unwrap()
}

#[test]
fn action_and_owner_names_rank_handlers_over_related_types() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let (ok, output) = sinter(directory.path(), &["build"]);
    assert!(ok, "{output}");

    let json = ask_json(
        directory.path(),
        "where is a search match written as JSON output",
    );
    let matched_rank = json
        .iter()
        .position(|hit| hit["qualified"] == "JsonSink::matched")
        .expect("JsonSink::matched should be returned");
    let type_rank = json
        .iter()
        .position(|hit| hit["qualified"] == "JsonSink")
        .unwrap_or(usize::MAX);
    assert!(matched_rank < type_rank, "{json:#?}");

    let add = ask_json(directory.path(), "where are child commands registered");
    assert_eq!(add[0]["qualified"], "Command::add_command", "{add:#?}");
}

#[test]
fn json_results_explain_the_score() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let (ok, output) = sinter(directory.path(), &["build"]);
    assert!(ok, "{output}");

    let hits = ask_json(directory.path(), "where are child commands registered");
    let top = &hits[0];
    assert!(
        top["channels"]
            .as_array()
            .is_some_and(|channels| { channels.iter().any(|channel| channel == "action-name") })
    );
    assert_eq!(top["score"], top["score_breakdown"]["final_score"]);
    assert!(top["score_breakdown"]["evidence"].as_i64().unwrap() > 0);
}
