# Real repository evaluation

This harness measures Sinter against hand-labeled navigation tasks in pinned
public repositories and deterministic agent-flow contracts in a local
synthetic repository. It covers exact symbol lookup, natural-language
ranking, direct callers, structural paths, and multi-step tool use. The
generated scorecard describes this corpus and configuration; it is not a
claim about all repositories or end-to-end coding success.

The suite currently pins ripgrep 14.1.1 (Rust), Cobra 1.8.1 (Go), Flask 3.0.3
(Python), Hono 4.4.0 (TypeScript), and Gson 2.11.0 (Java) at the exact commits
recorded in `cases.json`. It contains 249 hand-labeled tasks: 3 exact lookups,
166 natural-language `ask` questions, 36 direct-caller checks, and 44 path
checks. Each run builds syntax-only graphs, so the score represents Sinter's
zero-config behavior without SCIP.

The local corpus in `agent-flows.json` adds eighteen multi-step scenarios. Its
orientation case checks labeled repository facts from Map: the expected module,
dependency hub, documentation entry point, hub metric, and graph-health state.
The remaining scenarios cover forward dependency analysis, reverse blast radius, test
selection, ambiguity and unresolved-reference handling, diff impact,
stable-handle reuse after a harmless edit, dirty-working-tree refresh, and
MCP/CLI parity. Eight more cover the 0.51/0.52 batched verbs: bounded content
search (`grep --within`), refactor completeness (`impact --expect`), a bounded
source excerpt (`show --body`), multi-seed blast radius, and a `context` task
whose identifiers name real nodes. Three of those eight are `-old-way`
counterparts that answer the same question with pre-0.51 verbs only, so a
release-to-release run can separate flows both builds complete from flows only
the new build can serve. Each flow starts from a fresh two-commit fixture under
`fixtures/agent-flow/`; it never clones a repository or invokes an indexer.
The final labeled design-similarity flow asks for a role-to-scope mapping by
shape, expects both existing mapping tables, and verifies one before any new
semantic-overlap verb is considered.
The runner records flow and step correctness, abstention failures,
unsafe-confidence failures, tool-call count, output bytes, and stale or
partial evidence. These agent-flow measurements are observational and do not
have release floors yet. The Map assertions validate its structural inventory;
they do not prove that seeing Map improves an autonomous agent's task outcome.
A failing assertion appears in the scorecard instead of making the retrieval
gates easier to pass.

Caller labels list every call site found by reading the source, including
tests and receiver-typed calls (`cmd.Traverse(...)`) that a syntax-only graph
cannot bind; recall therefore measures what a compiler index would add, and
precision measures whether what the graph does return is real. Path cases
cover positive and negative (`not_proven`) answers, Rust trait dispatch,
Java interface factories, cross-crate calls, and two `dirty: true` cases
that run with an untracked scratch file in the working tree and require the
coverage envelope to report `snapshot.dirty`. Not yet covered: C# interfaces
(no C# corpus), cross-repository workspace paths, stale or present SCIP
indexes, concurrent agents, model tokenization, whether an agent chose the
right Sinter verb without prompting, or whether a coding task ultimately
compiled and passed its own tests. Output bytes are a stable token-cost proxy,
not a tokenizer measurement.

Every `ask` case carries an `intent` (`construction`, `registration`,
`dispatch`, `lifecycle`, `error_handling`, `output`, `lookup`). The split is
owned by the repository: ripgrep, Cobra, and Flask are tuning repositories;
Hono and Gson are holdout repositories. A ranking change may be motivated by
tuning results, but the holdout repositories only measure it. This prevents a
repository's vocabulary and structure from appearing on both sides of the
split. The scorecard reports ask metrics per split, repository, and intent,
reports ranking-margin-bucket precision only on holdout repositories, and lists
every wrong top result beside the first relevant rank.

The agent contract names the current frozen observation
`ask-holdout-2026-08-23.v2`: high-margin results were correct in 22/25
holdout cases, medium in 9/15, and low in 2/6. These are descriptive bucket
measurements, not per-result probabilities, and 46 holdout cases is a small
sample: the high bucket's Wilson 95% interval is about 70-96%. Runtime output
therefore exposes the score gap as `ranking_margin`, reports the calibration
version, counts, measured precision, and `precision_interval_95` separately,
and requires verification below 95% measured precision. The 2026-08-23
IDF-weighted field scoring, rarest-term penalty, and relational topic
planner were re-run on the holdout (top-1 0.717 before and after; tuning
0.700 to 0.708) and moved cases between the medium and low buckets, hence
version v2. A result abstains when there is no runner-up, query-term
coverage is below 50%, or its calibration bucket has fewer than 10 cases.
Changing these constants requires a new full holdout run and calibration
version; tuning-only results cannot be substituted.

## Prerequisites

- A Rust toolchain that can build the [Sinter workspace](../../README.md#install).
- `git` for the local fixture and network access to clone the pinned public
  repositories when running the complete evaluation.

## Run the evaluation

The `test-eval` target builds the release binary, clones the pinned commit into a temporary directory, runs every labeled case, and checks the metric floors in `cases.json`.

```bash
make test-eval
```

The command writes `target/sinter-eval/scorecard.json` for automation and `target/sinter-eval/scorecard.md` for review. Set `SINTER_EVAL_OUT` to write them elsewhere.

Normal `cargo test` runs the network-free agent-flow contract. This verifies
that the case schema, fixture setup, CLI driver, MCP driver, capture reuse,
and score aggregation remain executable. It intentionally does not require
every flow assertion to pass: product misses are scorecard evidence, while a
broken harness is a test failure.

To run only the local agent-flow evaluation, use the focused integration
test:

```bash
cargo test -p sinter-io --test real_repository_eval agent_flow_contract
```

It writes `target/sinter-agent-flow/scorecard.json`. Set
`SINTER_AGENT_FLOW_OUT` to choose another output path.

Set `SINTER_EVAL_BIN` to evaluate an already-built Sinter executable instead
of the executable built by the test. This makes release-to-HEAD comparisons
use the identical corpus and scorer:

```bash
SINTER_EVAL_BIN=/path/to/sinter-0.43.0 \
  SINTER_EVAL_SCOPE=ask \
  SINTER_EVAL_OUT=target/sinter-eval-0.43.0 \
  make test-eval
SINTER_EVAL_SCOPE=ask \
  SINTER_EVAL_OUT=target/sinter-eval-head \
  make test-eval
```

`SINTER_EVAL_SCOPE=ask` runs only the natural-language corpus and applies
only its gates. Use it when an older binary cannot execute structural cases
added after that release. The default scope remains the complete suite.

The `real-repository-eval` GitHub Actions workflow runs every Monday and accepts manual dispatches. It uploads both scorecards even when a metric floor fails.

## Maintain the labels

`cases.json` is the source of truth for retrieval labels. Each repository
entry uses a human-readable Git ref, the exact commit it must resolve to, and
one repository-level ask split. A moved or retagged ref fails before scoring.
Moving a repository from holdout to tuning is an evaluation-policy change and
must not be used to conceal a regression.

`agent-flows.json` is the source of truth for agent-flow contracts. Every
case has a capability label and at least two steps. CLI and MCP steps declare
machine-readable assertions; edit steps mutate only the temporary fixture;
capture values can be interpolated into later steps; compare steps check
selected JSON subtrees. Add a flow when an agent failure crosses tool calls or
depends on snapshot state. Keep single-call retrieval relevance labels in
`cases.json`.

Labels come from source inspection. Do not change an expected symbol because Sinter returned a different answer. Ambiguous endpoints use `name@file` (`parse@crates/core/flags/parse.rs`). Add a case when a user report exposes a concrete navigation task, then record every relevant result within the case's limit; questions that legitimately resolve to several symbols (overloads, lifecycle stages) list all of them. Raise a metric floor only after an implementation improves the tuning repositories without regressing the holdout repositories.

The network-backed retrieval evaluation remains ignored during normal
`cargo test` and runs through `make test-eval`.

## Coding-agent evaluation (optional-use)

The retrieval suite above forces verb sequences. `agent_eval.py` instead
hands a real change request to an unmodified coding agent and asks whether
Sinter, merely being available, changes the outcome. This is the measurement
that decides adoption. **No results have been produced yet**; the harness is
scaffolded and verified only with a fake agent.

Corpus: `agent_tasks/tasks.json` pins 30 change tasks across the five
retrieval repositories (ripgrep, Cobra, Flask, Hono, Gson; five each) and this
repository at a fixed commit (five). Each task has a natural-language request,
`expected_files` (the files a correct change edits), optional
`forbidden_files`, a `validate` command, and `hidden_files`: a test the agent
never sees, copied into the clone after the agent finishes and before
`validate` runs. Hidden tests live under `agent_tasks/hidden/<task-id>/`.
Labels come from reading the pinned source; do not change a task to match an
agent's answer.

Arms, same model and prompt:

- `baseline`: fresh clone, Sinter removed from `PATH`, no skill card.
- `sinter-optional`: `sinter build` run in the clone, the skill card copied to
  `.claude/skills/sinter/SKILL.md` and `AGENTS.md`; the agent chooses.
- `sinter-context`: as above plus a prompt hint to use `sinter context`. The
  arm is marked `skipped` when `sinter context --help` does not exit 0.

Agent CLIs are configured in `agent_tasks/agents.json` (`claude -p` and
`codex exec` included; `{prompt}` is substituted). Per (task, arm, run) the
runner records one JSONL row: validate success, edited files and wrong-file
edits (edited but not in `expected_files`), wall time, discovery tool calls
(`rg`/`grep`/`find`/`cat`/`head`/`sed`/`ls`/Read/Grep/Glob and `sinter`
invocations parsed from the transcript), bytes returned by those calls,
fallback searches after the first `sinter` call, and total and maximum `sinter`
response bytes. `scorecard.md` reports per-arm medians and the adoption gates:
success holds or improves; median discovery calls and bytes fall at least 25%;
wrong-file edits fall; every `sinter` response is within
`SINTER_BUDGET_BYTES` (8192). Transcript metrics depend on the agent emitting
JSONL (`--output-format stream-json` / `--json`); plain-text transcripts fall
back to a regex over lines and report 0 bytes.

```bash
# prove the pipeline: fake agent, one task, one arm (builds sinter-core once)
python3 harness/eval/agent_eval.py --dry-run

# full evaluation (30 tasks x 3 arms x N runs; every run is a fresh clone
# and an agent session — expect hours and real API spend)
python3 harness/eval/agent_eval.py --agent claude --runs 3
python3 harness/eval/agent_eval.py --agent codex --runs 3 --out target/sinter-agent-eval-codex
```

Outputs go to `target/sinter-agent-eval/` (`results.jsonl`, `scorecard.json`,
`scorecard.md`, `transcripts/`). Toolchains the validate commands need: cargo,
go, python3 with venv, node plus yarn, and maven with a JDK for Gson.
