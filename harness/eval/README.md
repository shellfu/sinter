# Real repository evaluation

This harness measures Sinter against hand-labeled navigation tasks in a pinned public repository. It covers exact symbol lookup, natural-language ranking, direct callers, and structural paths. The generated scorecard describes this corpus and configuration; it is not a claim about all repositories.

The suite currently pins ripgrep 14.1.1, Cobra 1.8.1, and Flask 3.0.3 at
the exact commits recorded in `cases.json`. It contains 59 hand-labeled tasks:
3 exact lookups, 50 natural-language `ask` questions, 3 direct-caller checks,
and 3 path checks. Each run builds syntax-only graphs, so the score represents
Sinter's zero-config behavior without SCIP. The small caller and path samples
are regression gates, not estimates of accuracy across all repositories.

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
