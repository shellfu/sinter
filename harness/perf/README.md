# Perf harness — skeleton, grows with each phase

Budgets (R4/R5/R8), tracked as hard assertions, not vibes:

| Budget | Where enforced | Last measured (skaffold, ~9.2k Go files) |
|---|---|---|
| Cold open + point query < 100ms, any size | `sinter-store/tests/cold_start.rs` (CI) | passes at 20k nodes; passes on 271k-node db |
| Full build of 1M-LOC repo < 5 min | manual real-repo run | 18.1s full, 271k nodes |
| One-file edit update < 1s | `sinter-cli/tests/incremental_build.rs` (CI) + manual | 498ms (4 files re-resolved); no-op 73ms pre-stat-gate (a clean sync is now a stat-only walk, ~16ms measured on this repo) |
| Warm query < 50ms | `sinter-store/tests/scale.rs` (nightly release) + manual | point reads and depth-4 traversal at 500k nodes; query/affected sub-ms to low-ms on 271k nodes |
| Resolver index < 3s at 200k nodes | `sinter-resolve/tests/scale.rs` (nightly release) | synthetic index build plus 20k same-file resolutions; absolute 1s resolve budget |
| Peak RSS < 2GB | manual `/usr/bin/time -v` | 1.5GB full build; incremental far below |

Manual benchmark procedure: `sinter build <big-repo>` twice (full, no-op),
append a function to one file, build again, revert. Synthetic CI corpus lives
inside `incremental_build.rs` (300 files). The ignored release scale gates use
500k stored nodes and 1.5m edges for query traversal, plus 200k extracted nodes
for resolver indexing. Their absolute budgets cover cold reads, warm reads,
real traversal, resolver index construction, and same-file binding.
