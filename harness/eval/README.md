# Real repository evaluation

This harness measures Sinter against hand-labeled navigation tasks in a pinned public repository. It covers exact symbol lookup, natural-language ranking, direct callers, and structural paths. The generated scorecard describes this corpus and configuration; it is not a claim about all repositories.

The suite currently pins ripgrep 14.1.1 (Rust), Cobra 1.8.1 (Go), Flask 3.0.3
(Python), Hono 4.4.0 (TypeScript), and Gson 2.11.0 (Java) at the exact commits
recorded in `cases.json`. It contains 175 hand-labeled tasks: 3 exact lookups,
166 natural-language `ask` questions, 3 direct-caller checks, and 3 path
checks. Each run builds syntax-only graphs, so the score represents Sinter's
zero-config behavior without SCIP. The small caller and path samples are
regression gates, not estimates of accuracy across all repositories.

Every `ask` case carries an `intent` (`construction`, `registration`,
`dispatch`, `lifecycle`, `error_handling`, `output`, `lookup`) and a `split`.
`tuning` cases may motivate ranking changes; `holdout` cases (at least a
quarter of the ask set, every third case per repository) only measure them.
A ranking change that lifts tuning scores but not holdout scores is
overfitting and must not raise a floor. The scorecard reports ask metrics
per split, per repository, and per intent, and lists every ask case whose
top result is wrong beside the rank of the first relevant answer.

## Prerequisites

- A Rust toolchain that can build the [Sinter workspace](../../README.md#install).
- `git` and network access to clone the pinned public repository.

## Run the evaluation

The `test-eval` target builds the release binary, clones the pinned commit into a temporary directory, runs every labeled case, and checks the metric floors in `cases.json`.

```bash
make test-eval
```

The command writes `target/sinter-eval/scorecard.json` for automation and `target/sinter-eval/scorecard.md` for review. Set `SINTER_EVAL_OUT` to write them elsewhere.

The `real-repository-eval` GitHub Actions workflow runs every Monday and accepts manual dispatches. It uploads both scorecards even when a metric floor fails.

## Maintain the labels

`cases.json` is the source of truth. Each repository entry uses a human-readable Git ref and the exact commit it must resolve to. A moved or retagged ref fails before scoring.

Labels come from source inspection. Do not change an expected symbol because Sinter returned a different answer. Add a case when a user report exposes a concrete navigation task, then record every relevant result within the case's limit; questions that legitimately resolve to several symbols (overloads, lifecycle stages) list all of them. Raise a metric floor after an implementation improves the measured score on both splits.

Normal `cargo test` runs compile this harness but skip the network test.
