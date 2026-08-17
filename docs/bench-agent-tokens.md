# Agent token benchmark: sinter vs grep

Measures what an agent session actually spends answering code-structure
questions with and without sinter. Run 2026-08-16 on this repository
(161 files, SCIP-backed graph) with Claude Code 2.1.233 in headless mode,
one session per task per arm, correctness judged against ground truth
established independently (graph output cross-checked with grep).

## Method

- Clone the repo to a scratch directory; remove `CLAUDE.md`, `AGENTS.md`,
  `.mcp.json` so neither arm gets steering; `sinter build` + `sinter scip`.
- Five structure questions, each asked once per arm:
  - **sinter arm** — system prompt names the sinter CLI and tells the
    agent to prefer it for structure questions.
  - **grep arm** — system prompt forbids sinter and `.sinter/`; grep,
    glob, and file reads only.
- Token counts, turns, cost, and duration from the CLI's usage JSON.

## Results

All ten sessions answered correctly.

| Task | Tokens in (grep → sinter) | Turns | Wall time |
|---|---|---|---|
| Direct callers of `open_store` | 100k → 100k | 3 → 3 | 16s → 13s |
| Direct callers of `Store::dependents` | 101k → 99k | 4 → 3 | 19s → 10s |
| Call chain serve → `shortest_path` | 70k → 140k | 3 → 4 | 14s → 16s |
| Transitive blast radius of `EdgeFilter::admits` (depth 3) | **264k → 66k** | **11 → 2** | **40s → 10s** |
| Where SCIP loads, what generates it | 66k → 103k | 2 → 3 | 7s → 9s |
| **Total** | 602k → 477k ($4.38 → $3.92) | 23 → 15 | 96s → 58s |

## Reading the numbers honestly

- **Simple lookups are a wash.** Roughly 66–100k tokens of every session
  is fixed overhead; on "who calls X" in a repo this size, grep ties or
  wins. Two tasks cost *more* with sinter (an extra turn spent on tool
  syntax).
- **The durable win is transitive blast radius**: 4× fewer tokens, 5×
  fewer turns, 4× faster. Grep's cost grows with every dependency level;
  a graph point-query's does not. Expect the gap to widen with repo size
  and depth.
- **Correctness parity here is a property of this repo**, small and
  conventionally organized. It is not evidence that grep stays correct at
  scale, nor that sinter does — see the `unresolved_refs` signal on
  `affected`, which exists because a graph must say when it may be
  incomplete.
- One cell per condition, one repo, one model. Bounds, not medians.

## Reproducing

```bash
#!/bin/bash
# Requires: claude CLI, sinter on PATH, a scratch dir in $S.
REPO=$S/repo
git clone <repo-under-test> $REPO
(cd $REPO && rm -f CLAUDE.md AGENTS.md .mcp.json && sinter build && sinter scip)

declare -A TASKS
TASKS[t1]='List every function that directly calls open_store (defined in crates/sinter-cli/src/lookup.rs). Answer as a file:function list only.'
TASKS[t2]='List every direct caller of Store::dependents (crates/sinter-store/src/traverse.rs). Answer as a file:function list only.'
TASKS[t3]='Trace how the MCP serve command (crates/sinter-cli/src/serve.rs) reaches Store::shortest_path. Give the function call chain.'
TASKS[t4]='If EdgeFilter::admits (crates/sinter-store/src/traverse.rs) changes behavior, which functions are transitively affected within 3 call levels? List them.'
TASKS[t5]='Where does the build pipeline look for a SCIP index file, and which CLI command generates it? Answer in one sentence.'

SINTER_ARM='This repo has a sinter code graph (.sinter/). The sinter binary is on PATH and answers code-structure questions precisely: sinter show <sym>, sinter affected <sym> [--max-depth N], sinter path <A> <B>, sinter query <sym>, sinter ask "<question>". For any structure question use sinter first instead of grep.'
GREP_ARM='Do not use any tool named sinter and do not read the .sinter directory. Answer using grep, glob, and file reads only.'

for t in t1 t2 t3 t4 t5; do
  for arm in sinter grep; do
    [ "$arm" = sinter ] && SYS="$SINTER_ARM" || SYS="$GREP_ARM"
    (cd $REPO && claude -p "${TASKS[$t]}" --append-system-prompt "$SYS" \
      --allowedTools Bash Read Grep Glob --output-format json \
      > $S/$t-$arm.json 2>$S/$t-$arm.err) &
  done
done
wait
```

Usage fields per result file: `num_turns`, `total_cost_usd`,
`usage.input_tokens + cache_creation_input_tokens +
cache_read_input_tokens`, `usage.output_tokens`, `duration_ms`. Judge
each `result` field against ground truth you establish before reading
either arm's answers.
