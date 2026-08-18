//! Workspaces acceptance (docs/design-workspace.md): federation across
//! member repos, boundary links by import evidence only, declared links
//! carrying their own evidence kind, deterministic output.

use std::path::Path;
use std::process::Command;

fn sinter(cwd: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .current_dir(cwd)
        .output()
        .expect("run sinter");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Three-member distributed system: auth and billing both import the
/// shared lib `common` by Go module path; billing also consumes a queue
/// topic that auth publishes (declared link).
fn build_workspace(root: &Path) -> std::path::PathBuf {
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write(
        "common/pkg/retry/retry.go",
        "package retry\n\n// Backoff retries an operation with backoff.\nfunc Backoff(attempts int) int {\n\treturn attempts\n}\n",
    );
    write(
        "common/go.mod",
        "module example.com/org/common\n\ngo 1.22\n",
    );
    write(
        "auth/main.go",
        "package main\n\nimport \"example.com/org/common/pkg/retry\"\n\n// Login authenticates with retries.\nfunc Login(user string) int {\n\treturn retry.Backoff(3)\n}\n\n// PublishSettled emits the settled event.\nfunc PublishSettled() {}\n",
    );
    write("auth/go.mod", "module example.com/org/auth\n\ngo 1.22\n");
    write(
        "billing/main.go",
        "package main\n\nimport \"example.com/org/common/pkg/retry\"\n\n// Charge bills a customer with retries.\nfunc Charge(amount int) int {\n\treturn retry.Backoff(amount)\n}\n\n// ConsumeSettled handles the settled event.\nfunc ConsumeSettled() {}\n",
    );
    write(
        "billing/go.mod",
        "module example.com/org/billing\n\ngo 1.22\n",
    );
    write(
        "workspace.toml",
        r#"[workspace]
name = "shop"

[members]
auth = "auth"
billing = "billing"
common = "common"

[[links]]
from_member = "billing"
from_symbol = "ConsumeSettled"
to_member = "auth"
to_symbol = "PublishSettled"
via = "topic payments.settled"
"#,
    );
    root.join("workspace.toml")
}

#[test]
fn workspace_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let manifest = build_workspace(root);
    let m = manifest.to_str().unwrap();

    // Build members + refresh links in one verb.
    let (ok, out) = sinter(root, &["workspace", m]);
    assert!(ok, "{out}");
    assert!(out.contains("boundary links:"), "{out}");
    // At least: auth->Backoff call, billing->Backoff call, two import
    // edges to the common file/module, one declared link.
    let count: usize = out
        .lines()
        .find_map(|l| l.strip_prefix("boundary links: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        count >= 4,
        "expected >=4 boundary links, got {count}:\n{out}"
    );

    // Cross-repo blast radius: who depends on common's Backoff?
    let (ok, out) = sinter(root, &["affected", "Backoff", "--workspace", m]);
    assert!(ok, "{out}");
    assert!(out.contains("auth:Login"), "{out}");
    assert!(out.contains("billing:Charge"), "{out}");
    assert!(out.contains("import"), "{out}");

    // Declared link: PublishSettled's dependents include billing's
    // consumer, tagged with declared evidence; filtering to import-only
    // evidence excludes it.
    let (ok, out) = sinter(root, &["affected", "PublishSettled", "--workspace", m]);
    assert!(ok, "{out}");
    assert!(out.contains("billing:ConsumeSettled"), "{out}");
    assert!(out.contains("declared"), "{out}");
    let (ok, out) = sinter(
        root,
        &[
            "affected",
            "PublishSettled",
            "--workspace",
            m,
            "--evidence",
            "import",
        ],
    );
    assert!(ok, "{out}");
    assert!(
        !out.contains("ConsumeSettled"),
        "declared link not filterable:\n{out}"
    );

    // Cross-repo path: billing's Charge reaches common's Backoff.
    let (ok, out) = sinter(
        root,
        &["path", "billing:Charge", "common:Backoff", "--workspace", m],
    );
    assert!(ok, "{out}");
    assert!(out.contains("-[calls/import]->"), "{out}");
    assert!(out.contains("common:Backoff"), "{out}");

    // Fan-out ask finds the shared symbol with member attribution.
    let (ok, out) = sinter(root, &["ask", "retry backoff", "--workspace", m]);
    assert!(ok, "{out}");
    let first = out.lines().find(|l| l.starts_with("1. ")).unwrap();
    assert!(first.contains("common:"), "{out}");
    assert!(first.contains("Backoff"), "{out}");

    // Determinism: byte-identical across runs.
    let (_, again) = sinter(root, &["affected", "Backoff", "--workspace", m]);
    let (_, first_run) = sinter(root, &["affected", "Backoff", "--workspace", m]);
    assert_eq!(again, first_run, "workspace traversal not deterministic");
}

#[test]
fn workspace_impact_crosses_members() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let manifest = build_workspace(root);
    let m = manifest.to_str().unwrap();
    let common = root.join("common");

    // common is a git repo with one committed baseline, then a change to
    // Backoff.
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&common)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap()
                .status
                .success()
        );
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    std::fs::write(
        common.join("pkg/retry/retry.go"),
        "package retry\n\n// Backoff retries an operation with backoff.\nfunc Backoff(attempts int) int {\n\treturn attempts * 2\n}\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "change backoff"]);

    let (ok, out) = sinter(root, &["workspace", m]);
    assert!(ok, "{out}");
    let (ok, out) = sinter(
        root,
        &[
            "impact",
            "HEAD~1..HEAD",
            "--repo",
            common.to_str().unwrap(),
            "--workspace",
            m,
        ],
    );
    assert!(ok, "{out}");
    assert!(out.contains("Backoff"), "{out}");
    assert!(
        out.contains("auth:"),
        "cross-member radius missing auth:\n{out}"
    );
    assert!(
        out.contains("billing:"),
        "cross-member radius missing billing:\n{out}"
    );
}

#[test]
fn init_workspace_scaffolds_without_clobber() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (ok, out) = sinter(root, &["init", "--workspace", "--name", "shop"]);
    assert!(ok, "{out}");
    let manifest = std::fs::read_to_string(root.join("ws.toml")).unwrap();
    assert!(manifest.contains("name = \"shop\""), "{manifest}");
    // Template must parse as a valid (empty) workspace as written.
    let (ok, out) = sinter(root, &["workspace", "ws.toml"]);
    assert!(ok, "{out}");
    // Second run refuses to overwrite.
    let (ok, out) = sinter(root, &["init", "--workspace"]);
    assert!(!ok, "{out}");
    assert!(out.contains("refusing to overwrite"), "{out}");
    assert!(manifest == std::fs::read_to_string(root.join("ws.toml")).unwrap());
}

#[test]
fn workspace_stale_and_declared_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let manifest = build_workspace(root);
    let m = manifest.to_str().unwrap();
    let (ok, out) = sinter(root, &["workspace", m]);
    assert!(ok, "{out}");

    // A bogus declared symbol fails loudly, never guesses.
    let bad = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("PublishSettled", "NoSuchSymbol");
    std::fs::write(root.join("workspace.toml"), bad).unwrap();
    let (ok, out) = sinter(root, &["workspace", m]);
    assert!(!ok, "{out}");
    assert!(out.contains("NoSuchSymbol"), "{out}");
}

/// Parallel workspace queries share the link store; opens must ride out
/// contention instead of failing with DatabaseAlreadyOpen.
#[test]
fn parallel_workspace_queries_all_succeed() {
    let root = tempfile::tempdir().unwrap();
    let manifest = build_workspace(root.path());
    let (ok, out) = sinter(root.path(), &["workspace", manifest.to_str().unwrap()]);
    assert!(ok, "{out}");
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let manifest = manifest.clone();
            std::thread::spawn(move || {
                Command::new(env!("CARGO_BIN_EXE_sinter"))
                    .args([
                        "affected",
                        "common:Backoff",
                        "--workspace",
                        manifest.to_str().unwrap(),
                    ])
                    .output()
                    .expect("run sinter")
            })
        })
        .collect();
    for t in threads {
        let out = t.join().unwrap();
        assert!(
            out.status.success(),
            "workspace query failed under contention: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Workspace scope over MCP: one server spans the members, tools/list is
/// the honest cross-repo surface, and affected crosses repositories.
#[test]
fn serve_workspace_answers_across_members() {
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let manifest = build_workspace(root.path());
    let (ok, out) = sinter(root.path(), &["workspace", manifest.to_str().unwrap()]);
    assert!(ok, "{out}");

    let mut child = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["serve", "--workspace", manifest.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn serve");
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list"}}"#).unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"affected","arguments":{{"symbol":"common:Backoff"}}}}}}"#
        )
        .unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();

    let tools: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["query", "affected", "path"], "honest ws surface");

    let affected: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let body = affected["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(parsed["symbol"]["member"], "common", "{body}");
    assert!(parsed["total"].as_u64().unwrap() >= 1, "{body}");
    let deps = parsed["dependents"].as_array().unwrap();
    assert!(
        deps.iter().any(|d| {
            let s = d["s"].as_str().unwrap();
            s.starts_with("auth:") || s.starts_with("billing:")
        }),
        "terse dependents must cross into other members: {body}"
    );
}
