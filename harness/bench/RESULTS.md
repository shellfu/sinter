# Benchmark: sinter vs graphify

Run: 2026-08-15, single machine, local binaries only (no network).
Method: harness/bench/bench.sh + questions.tsv (hand-labeled expected
symbols). Raw per-question outputs retained under harness/bench/logs/
(untracked). Rank metrics are asymmetric in graphify's favor: sinter's
rank counts ranked hits; graphify's counts ANY output line containing
the expected symbol.

| repo | tool | index build | reindex (no-op) | artifact size |
|---|---|---|---|---|
| bl | sinter | 9685ms | 136ms | 87M |
| bl | graphify | 211598ms | 190767ms | 32M |
| sinter | sinter | 608ms | 17ms | 6.1M |
| sinter | graphify | 1191ms | 919ms | 3.0M |
| proto | sinter | 2395ms | 18ms | 88M |
| proto | graphify | 9222ms | 6874ms | 29M |
| skaffold | sinter | 45918ms | 110ms | 2.2G |
| skaffold | graphify | 152753ms | 98936ms | 462M |

| repo | question | tool | latency | rank of expected | output chars |
|---|---|---|---|---|---|
| bl | where is the character controller? | sinter | 42ms | 6 | 2932 |
| bl | where is the character controller? | graphify | 225ms | line 43 | 6167 |
| bl | where is the control mode state machin | sinter | 41ms | 1 | 2949 |
| bl | where is the control mode state machin | graphify | 232ms | line miss | 1849 |
| bl | where is the mechanics gym controller? | sinter | 22ms | 1 | 2832 |
| bl | where is the mechanics gym controller? | graphify | 242ms | line miss | 4172 |
| sinter | where is the trigram search? | sinter | 16ms | 1 | 1998 |
| sinter | where is the trigram search? | graphify | 133ms | line 1 | 5882 |
| sinter | how are stale files detected? | sinter | 17ms | miss | 2345 |
| sinter | how are stale files detected? | graphify | 138ms | line miss | 6166 |
| sinter | where is the evidence based resolver? | sinter | 18ms | 1 | 1994 |
| sinter | where is the evidence based resolver? | graphify | 142ms | line 2 | 5905 |
| proto | where is the incremental cache invalid | sinter | 30ms | 1 | 2881 |
| proto | where is the incremental cache invalid | graphify | 314ms | line 1 | 6102 |
| proto | where is the graph builder? | sinter | 34ms | 2 | 2639 |
| proto | where is the graph builder? | graphify | 318ms | line miss | 6204 |
| proto | where is community detection? | sinter | 24ms | 1 | 2831 |
| proto | where is community detection? | graphify | 308ms | line 4 | 6249 |
| skaffold | where is the docker image builder? | sinter | 104ms | 2 | 2817 |
| skaffold | where is the docker image builder? | graphify | 3062ms | line 1 | 6208 |
| skaffold | how does file sync to the cluster work | sinter | 136ms | 1 | 1883 |
| skaffold | how does file sync to the cluster work | graphify | 3053ms | line 1 | 1825 |

## Summary

- Quality (hit in top 5): sinter 9/11, graphify 5/11 (by its generous
  line metric). sinter's two non-top-5: the Black Lantern controller
  class (#6 — its doc never contains "controller"; evidence ceiling) and
  one true miss ("stale files detected" — no matching words in any
  doc/signature; same ceiling).
- Query latency: sinter 16-136ms; graphify 133ms-3.1s, growing with graph
  size (whole-JSON parse per query).
- Freshness (no-op reindex): sinter 17-136ms across all repos; graphify
  0.9s-190s — it re-pays near-full build cost on every update.
- Full index: sinter 4-22x faster on every repo.
- Artifact size: graphify smaller everywhere (2.7x on BL, 4.8x on
  skaffold: 462M vs sinter's 2.2G). sinter trades disk for the keyed
  reads above; the skaffold 2.2G (larger than the 356M checkout, grown
  by the TOKENS index) upgrades the db-size watch item to the top of the
  fix-on-demand list.

## Size optimization pass (post-benchmark, same day)

The size gap was the benchmark's one adverse finding. Per-table
measurement showed 58% of stored bytes were node-id strings repeated in
the token/trigram indexes, 19% uncompressed FileFacts blobs, and nearly
half the file was page slack / sparse apparent size. Fixes (schema v4):
interned u32 ids in all index tables, zstd(1) on FileFacts, iterative
compaction after bulk builds only, and size reporting switched to real
allocated blocks.

| repo | before (real) | after (real) | graphify |
|---|---|---|---|
| Black Lantern | 87M | 32.6M | 32M |
| skaffold | ~2.1G apparent / not measured real | 663M | 462M |

Stored bytes on skaffold: 1161MB -> 310MB (tokens 405->19.5MB, trigrams
271->13.3MB, file_facts 217->28.4MB). Speed guardrails all improved or
held: full build 46s -> 35s, no-op 63ms, one-file edit 850ms, ask 81ms.
Remaining lever in reserve if ever needed: posting-list token index
(per-word blobs instead of per-entry multimap rows).
