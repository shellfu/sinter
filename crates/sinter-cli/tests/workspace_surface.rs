//! Workspaces acceptance (docs/design-workspace.md): federation across
//! member repos, boundary links by import evidence only, declared links
//! carrying their own evidence kind, deterministic output.

use std::path::Path;
use std::process::Command;

fn sinter(cwd: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
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
