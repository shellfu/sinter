# Real repository evaluation

This harness measures Sinter against hand-labeled navigation tasks in a pinned public repository. It covers exact symbol lookup, natural-language ranking, direct callers, and structural paths. The generated scorecard describes this corpus and configuration; it is not a claim about all repositories.

The suite currently pins ripgrep 14.1.1 (Rust), Cobra 1.8.1 (Go), Flask 3.0.3
(Python), Hono 4.4.0 (TypeScript), and Gson 2.11.0 (Java) at the exact commits
recorded in `cases.json`. It contains 249 hand-labeled tasks: 3 exact lookups,
166 natural-language `ask` questions, 36 direct-caller checks, and 44 path
checks. Each run builds syntax-only graphs, so the score represents Sinter's
zero-config behavior without SCIP.

Caller labels list every call site found by reading the source, including
tests and receiver-typed calls (`cmd.Traverse(...)`) that a syntax-only graph
cannot bind; recall therefore measures what a compiler index would add, and
precision measures whether what the graph does return is real. Path cases
cover positive and negative (`not_proven`) answers, Rust trait dispatch,
Java interface factories, cross-crate calls, and two `dirty: true` cases
that run with an untracked scratch file in the working tree and require the
coverage envelope to report `snapshot.dirty`. Not yet covered: C# interfaces
(no C# corpus), cross-repository workspace paths, and stale or present SCIP
indexes (the harness never runs an indexer).

Every `ask` case carries an `intent` (`construction`, `registration`,
`dispatch`, `lifecycle`, `error_handling`, `output`, `lookup`). The split is
owned by the repository: ripgrep, Cobra, and Flask are tuning repositories;
Hono and Gson are holdout repositories. A ranking change may be motivated by
tuning results, but the holdout repositories only measure it. This prevents a
repository's vocabulary and structure from appearing on both sides of the
split. The scorecard reports ask metrics per split, repository, and intent,
reports confidence-bucket precision only on holdout repositories, and lists
every wrong top result beside the first relevant rank.

## Prerequisites

- A Rust toolchain that can build the [Sinter workspace](../../README.md#install).
- `git` and network access to clone the pinned public repository.

## Run the evaluation

The `test-eval` target builds the release binary, clones the pinned commit into a temporary directory, runs every labeled case, and checks the metric floors in `cases.json`.

```bash
make test-eval
```

The command writes `target/sinter-eval/scorecard.json` for automation and `target/sinter-eval/scorecard.md` for review. Set `SINTER_EVAL_OUT` to write them elsewhere.

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

`cases.json` is the source of truth. Each repository entry uses a human-readable Git ref, the exact commit it must resolve to, and one repository-level ask split. A moved or retagged ref fails before scoring. Moving a repository from holdout to tuning is an evaluation-policy change and must not be used to conceal a regression.

Labels come from source inspection. Do not change an expected symbol because Sinter returned a different answer. Ambiguous endpoints use `name@file` (`parse@crates/core/flags/parse.rs`). Add a case when a user report exposes a concrete navigation task, then record every relevant result within the case's limit; questions that legitimately resolve to several symbols (overloads, lifecycle stages) list all of them. Raise a metric floor only after an implementation improves the tuning repositories without regressing the holdout repositories.

Normal `cargo test` runs compile this harness but skip the network test.
