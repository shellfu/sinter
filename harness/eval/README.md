# Real repository evaluation

This harness measures Sinter against hand-labeled navigation tasks in a pinned public repository. It covers exact symbol lookup, natural-language ranking, direct callers, and structural paths. The generated scorecard describes this corpus and configuration; it is not a claim about all repositories.

The first corpus is ripgrep 14.1.1 at commit `4649aa9700619f94cf9c66876e9549d83420e16c`. The run builds a syntax-only graph, so the score represents Sinter's zero-config behavior without SCIP.

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

Labels come from source inspection. Do not change an expected symbol because Sinter returned a different answer. Add a case when a user report exposes a concrete navigation task, then record every relevant result within the case's limit. Raise a metric floor after an implementation improves the measured score.

Normal `cargo test` runs compile this harness but skip the network test.
