# Agent routing benchmark: do agents pick sinter unprompted?

Measures whether a clean agent session, asked natural questions that
never mention sinter, chooses sinter before grep or git archaeology.
Run with `docs/bench-routing.sh` — it clones/copies a repo, onboards it
with plain `sinter init`, runs headless Claude Code sessions, and
classifies each session's first search-shaped tool action.

Gates: sinter-first on graph-eligible questions >= 90%; sinter invoked
on ineligible questions (function-body/content reading) < 10%.

## Results

Run 2026-08-17, target repo helve (Go, ~100 files), Claude Code
headless, model claude-haiku-4-5 (deliberately the weakest
instruction-follower — a floor, not a ceiling), 2 reps x
(5 eligible + 3 ineligible) = 16 trials.

| Arm | Result | Gate |
|---|---|---|
| Eligible: sinter-first | **10/10 = 100%** | >= 90% ✓ |
| Ineligible: sinter invoked | **0/6 = 0%** | < 10% ✓ |

Total cost: $0.80.

An earlier run on the same setup, before two routing fixes, scored
8/10 / 0/6. The two misses drove the fixes:

- One session answered "what does the most recent commit affect?" with
  `git show`/`git log` archaeology and never searched. Fixed by an
  impact-specific hook nudge on `git show|diff|diff-tree|log` in graph
  repos, plus card wording that matches how the question is asked.
- One session grepped first, hit the grep nudge, and self-corrected to
  sinter mid-task — a save under the strict first-action metric, and
  evidence the nudge converts rather than blocks.

## Honest scope

- Phase 1: Claude Code only. Codex and Cursor need their own headless
  harness before their rates exist; nothing here speaks for them.
- One repo, n=16, one model. A rate, not a distribution.
- The ineligible arm also matched the field: a live session reported
  the hooks "didn't nag on legitimate content greps."
- Repo-scope `.claude` hooks may be unloaded in headless clones
  (project-trust consent); the measured stack is cards + MCP + any
  machine-global hooks. That is what a fresh teammate gets.

## Reproducing

```
docs/bench-routing.sh <repo-path-or-url> [reps-per-question]
```

Edit the question arrays in the script for the target repo — eligible
questions only measure routing if the symbols they name exist. Model is
pinned to Haiku; `BENCH_ALLOW_EXPENSIVE=1` unlocks comparison runs.
Traces are kept in the printed workdir for per-session inspection.
