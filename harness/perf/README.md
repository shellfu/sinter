# Perf harness — skeleton, grows with each phase

Budgets (R4/R5/R8), tracked as hard assertions, not vibes:

| Budget | Where enforced | Last measured (skaffold, ~9.2k Go files) |
|---|---|---|
| Cold open + point query < 100ms, any size | `sinter-store/tests/cold_start.rs` (CI) | passes at 20k nodes; passes on 271k-node db |
| Full build of 1M-LOC repo < 5 min | manual real-repo run | 18.1s full, 271k nodes |
| One-file edit update < 1s | `sinter-cli/tests/incremental_build.rs` (CI) + manual | 498ms (4 files re-resolved); no-op 73ms |
| Warm query < 50ms | manual | query/affected sub-ms to low-ms on 271k nodes |
| Peak RSS < 2GB | manual `/usr/bin/time -v` | 1.5GB full build; incremental far below |

Manual benchmark procedure: `sinter build <big-repo>` twice (full, no-op),
append a function to one file, build again, revert. Synthetic CI corpus lives
inside `incremental_build.rs` (300 files); scale it up if regressions slip
through.
