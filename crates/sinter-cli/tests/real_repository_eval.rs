#[path = "../../../harness/eval/runner/mod.rs"]
mod eval;

#[test]
#[ignore = "clones a pinned public repository; run with make test-eval"]
fn real_repository_eval() -> anyhow::Result<()> {
    eval::run()
}
