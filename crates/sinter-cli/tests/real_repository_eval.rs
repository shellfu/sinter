#[path = "../../../harness/eval/runner/mod.rs"]
mod eval;

#[test]
fn agent_flow_contract_is_network_free_and_executable() -> anyhow::Result<()> {
    eval::run_agent_flow_contract()
}

#[test]
#[ignore = "clones a pinned public repository; run with make test-eval"]
fn real_repository_eval() -> anyhow::Result<()> {
    eval::run()
}
