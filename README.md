<p align="center">
  <img src="assets/sinter_logo.png" alt="sinter logo" width="360">
</p>

# sinter

A local code knowledge graph for coding agents, shipped as one static binary.
sinter builds a typed, directed graph of a repository's symbols with
tree-sitter and keeps it fresh incrementally. Start in an unfamiliar repository
with `sinter map`: it returns a one-screen module tree, the most depended-on
symbols, and documentation entry points. Then use the graph for focused work:

- **Reverse blast radius** — what transitively depends on a symbol,
  cross-file and cross-language (`sinter affected`).
- **Paths** — how one symbol reaches another (`sinter path`).
- **Diff impact** — which symbols and tests a changeset can affect
  (`sinter impact`).
- **Polyglot, zero setup** — syntax-only graphs work without an LSP. When the
  syntax provides the necessary type information, sinter resolves self
  receivers, locally typed receivers, typed fields, and explicit static/class
  calls. Complete receiver-call binding often requires a fresh SCIP index;
  zero setup is not compiler-complete resolution.
- **Calibrated lexical navigation** — `sinter ask` ranks content-bearing
  starting points from names, docs, paths, signatures, and limited call
  evidence. It reports measured top-result calibration and explicit
  verify-or-abstain guidance; it is a navigator, not a semantic answer engine.
- **Symbol orientation** — `sinter show` turns a selected symbol into a compact
  card with its definition and graph neighborhood.
- **Cross-repo workspaces** — federated graphs over many repos for
  distributed systems: blast radius, paths, and PR impact across service
  boundaries (`--workspace`).

The design rule underneath everything: **evidence or nothing.** An edge
exists only when structural, scope, import, or compiler (SCIP) evidence
binds a reference to a definition (workspace manifests may add
operator-declared cross-repo links, and Rust trait impls add labeled
`dynamic` dispatch fan-out so blast radius survives `dyn Trait`).
Ambiguity resolves to nothing — "unresolved" is a first-class, counted
outcome, never a guess. Every edge carries its evidence kind, and every
query can filter on it.

## Install

Package managers:

```
cargo install sinter-io        # or: cargo binstall sinter-io (prebuilt)
uv tool install sinter-io      # or: pipx install sinter-io
brew install shellfu/tap/sinter
```

All of them put a binary named `sinter` on PATH. Or the one-liner, no
dependencies (Linux and macOS; verifies the release checksum, installs
to `~/.local/bin`):

```
curl -fsSL https://raw.githubusercontent.com/shellfu/sinter/main/scripts/install.sh | sh
```

Windows, in PowerShell (new and less battle-tested; verifies the release
checksum, installs to `%LOCALAPPDATA%\sinter\bin`):

```
irm https://raw.githubusercontent.com/shellfu/sinter/main/scripts/install.ps1 | iex
```

To verify a manually downloaded release asset against its GitHub build
provenance attestation (requires the `gh` CLI):

```
gh attestation verify sinter-<target>.tar.gz --owner shellfu
```

PyPI wheels carry sigstore provenance via Trusted Publishing. Once
installed, `sinter update` self-updates from GitHub releases.

Or build from source (requires a Rust toolchain):

```
cargo build --release
```

## Quickstart

Agents that only need a usable local graph should create derived state without
installing hooks or editing client configuration:

```
sinter ensure /path/to/repo
```

This command only builds or refreshes `.sinter/`. It is safe to run within a
read-oriented coding flow.

Onboard a repository — builds the graph, installs git hooks, registers
agent integration (AGENTS.md block, MCP, Claude skill), and finishes with
a doctor report:

```
./target/release/sinter init /path/to/repo
```

On a terminal, init asks before running compiler indexers (`sinter scip`)
because those toolchains execute repository build scripts; pass `--scip`
or `--no-scip` to answer up front (non-interactive init skips them).

After that, every query self-syncs at the query boundary — when nothing
changed, the sync is a stat-only walk (no file reads, no write
transactions) — and the git hooks refresh eagerly on commit;
`sinter build` stays available for CI and scripting. Commands also work
from any subdirectory, discovering the graph root the way git does. The
build report distinguishes the heuristic's anchored miss rate from
compiler-relative accuracy. The anchored rate is useful without SCIP but
is not recall: the heuristic can classify a compiler-resolvable reference
as external.

```
resolution (this pass): ... resolved (scip 0, import 118, scope 189), ... unresolved (105 internal, 3193 external)
anchored miss rate (this pass): 25.5% (heuristic classification, not compiler-relative recall)
```

When a SCIP index is present, the report also prints the compiler
cross-check (what share of internally-bound refs agree with the
compiler) and internal recall vs the compiler (how many compiler-bound
refs sinter found without SCIP).

Orient before searching so the next query uses the repository's own module and
symbol vocabulary:

```
sinter map /path/to/repo
```

The map is the first-pass inventory. Use `ask` next when the target is still a
concept rather than a known symbol.

Ask a question against the graph (output shown for this repository):

```
$ sinter ask "where is the trigram search"

1. function Store::search    [doc+name+path+sig 2/2 terms]
   crates/sinter-store/src/search.rs:148
   /// Fuzzy candidates: nodes sharing the most trigrams with the query,
   pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Node>, StoreError>
```

`ask` is a calibrated lexical navigator, so treat its hits as places to inspect
rather than generated answers. Every hit shows its match provenance. Agent JSON
groups results by topic under one strict result budget and reports
`ranking_margin`, query-term
coverage, the named holdout calibration, `verify_required`, and topic-level
advice. Weak singletons, low term coverage, and undersampled confidence
buckets abstain. CLI JSON and MCP `structuredContent.data` use the same
`sinter.agent.v1` payload.

Every `affected`, `deps`, and `path` response is deliberately bounded. Positive
and negative answers include a `coverage` object with the graph snapshot,
requested filters, available evidence sources, certain and possible result
counts, unresolved-reference counts, compiler-index status, and explicit
gaps. `completeness` describes only the indexed snapshot and `conclusive`
remains false, so agents do not turn a non-empty syntax-only result into an
exhaustive claim.

Agent-facing node `id` values are stable symbol keys and survive unrelated
offset shifts. `snapshot_id` retains the byte-exact locator for the reported
snapshot. Handle-consuming operations accept `if_snapshot` and return typed
stale-snapshot, relocated-handle, or ambiguous-candidate outcomes instead of
silently rebinding.

## Commands

| Command | Purpose |
|---|---|
| `sinter ensure [repo]` | Build or refresh only the derived graph; does not install hooks or edit agent/client configuration |
| `sinter map [repo]` | First action in an unfamiliar repo: one-screen module tree, hub symbols, and doc entry points (`--json`) |
| `sinter init [repo]` | Onboard a repo: build + hooks + agent integration + doctor (`--scip`/`--no-scip` answer the indexer consent up front; `-g` also installs enforcement hooks globally) |
| `sinter uninit [repo]` | Offboard completely: remove the graph and every sinter-managed artifact (`-g` also removes global skill + hooks) |
| `sinter build [repo]` | Build or incrementally refresh the graph |
| `sinter watch [repo]` | Keep the graph fresh from filesystem events |
| `sinter hooks install` | Git hooks that refresh after commit/checkout/merge |
| `sinter ask "<question>"` | Calibrated lexical starting points for a vague question, with verify/abstain guidance |
| `sinter show <symbol>` | One-screen orientation card for a symbol or file |
| `sinter query <symbol>` | Exact + fuzzy symbol search |
| `sinter affected <symbol>` | Reverse blast radius, evidence-filterable |
| `sinter deps <symbol>` | Forward blast radius: everything a symbol transitively depends on |
| `sinter unresolved` | List unresolved references — the graph's honest gaps (`--file`, `--name`) |
| `sinter path <from> <to>` | Shortest dependency path with per-step evidence |
| `sinter impact <rev-range>` | Changed symbols → blast radius → affected tests |
| `sinter serve` | MCP server over stdio (`--repo` for one repo, `--workspace <manifest>` for a cross-repo scope) |
| `sinter overlap <range>...` | Map open PRs onto the graph; rank pairwise merge risk (direct/radius/file) |
| `sinter workspace <manifest>` | Build all members of a cross-repo workspace + refresh boundary links |
| `sinter init --workspace` | Write a starter workspace manifest (never overwrites) |
| `sinter install [targets]` | Write agent cards (claude, cursor, agents/AGENTS.md, enforce (`--strict` available), all); `--mcp` registers the server for Claude Code, Cursor, and Codex |
| `sinter scip [repo]` | Run every matching compiler indexer, merge into `.sinter/index.scip`, rebuild; no-op when fresh (`--force` reindexes); `scip check` is the CI freshness guard |
| `sinter doctor [repo]` | Diagnose installation + graph (including an MCP handshake and lock-held reporting); every finding names its fix; `--fix` applies the safe ones |
| `sinter update` | Self-update to the latest release, checksum-verified (`--dry-run` reports only) |
| `sinter completion <shell>` | Shell completions |
| `sinter version` | Version, graph schema, language packs |

MCP registrations use the portable `sinter` command and start non-required so
a missing binary cannot prevent the client from starting. `sinter doctor`
checks that the command resolves to an executable on `PATH` and performs an MCP
handshake.

`affected`, `deps`, and `path` accept `--evidence scip,import,scope,dynamic`
and `--certain` to restrict traversal to stronger evidence tiers, and
`--relations calls,uses,imports,implements,extends` to restrict which edge
relations are followed (e.g. drop file-level import edges from a blast
radius); their MCP counterparts take the same filters as `evidence` (array),
`min_confidence: "certain"`, and `relations` (array) parameters.

Discovery commands default to the `production,docs` corpus. Use `--scope` or
the MCP `scope` array to include tests, fixtures, examples, generated files, or
vendor code. Exact `show` remains unfiltered. Repositories can exclude paths in
`.sinterignore` and apply ordered classification overrides in `.sinter.toml`
with `[[scope.override]]` entries.

## Languages

Rust, Go, Python, TypeScript, JavaScript (ESM, CJS, JSX), Java, C#, C,
C++ (including Unreal Engine macro conventions), SQL (DDL/DML), Bash,
Protobuf, and Markdown (headings become nested section nodes with the
opening paragraph as doc, so `sinter ask` finds prose docs with file:line
provenance; `[text](target)` links become `uses` edges from the linking
section to the target file — or, with `#fragment`, to the section whose
heading slugifies to it — when and only when the target resolves to a
file in the corpus: dead links stay unresolved, external URLs produce
nothing). A language is pure data — a tree-sitter grammar, one `.scm`
capture query (plus an optional secondary inline grammar, spec-declared),
and a spec row — consumed by a single engine that never
branches on language. Adding a language requires no engine code; if it
ever does, the capture contract is wrong, not the language.

Top-level `graphify-out/`, `memory/`, and `.memory/` are excluded from the
semantic corpus and SCIP freshness inventory. These are derived analysis
products; indexing them would feed generated summaries back into `ask` and
dependency answers.

If a compiler-produced SCIP index (`index.scip`) is present at the repo
root or at `.sinter/index.scip`, sinter ingests it as the highest
evidence tier. `sinter scip` discovers configured project roots from build
markers and runs only matching indexers that are available on `PATH`
(rust-analyzer, scip-go, scip-typescript for TS and JS,
scip-python, scip-clang for C/C++, scip-java, scip-dotnet), merges the
results into `.sinter/index.scip`, and rebuilds. Isolated source files and
fixtures without a project marker do not trigger repository-level indexer
recommendations. Bash, proto, SQL, and Markdown have no SCIP indexers.

## Teams

Graphs are per-machine and rebuild in seconds; the SCIP index is the
expensive shared artifact. Build it once in CI (`sinter scip`),
distribute the file, and each teammate's next build ingests it
automatically. Recipe, cache-key guidance, and a copy-paste workflow:
[`docs/team.md`](docs/team.md).

## Accuracy and performance are measured, not asserted

- **Golden corpus**: hand-verified fixtures (82 at time of writing) across all thirteen languages,
  mined from real-world extraction idioms. Extraction and resolution both
  gate CI at precision/recall 1.0 for the enumerated contract facts; any change that moves the metric fails
  with the exact missing/extra tuples printed. Expectations derive from
  language semantics, never from engine output (`harness/golden/`).
- **Real-repository evaluation**: 249 hand-labeled, syntax-only navigation
  tasks run against pinned releases of ripgrep (Rust), Cobra (Go), Flask
  (Python), Hono (TypeScript), and Gson (Java): 3 exact lookups, 166
  natural-language `ask` questions labeled by intent with a tuning/holdout
  split, 36 direct-caller checks, and 44 path checks. The current scorecard
  reports `ask` top-1 accuracy 0.705 (holdout 0.696), MRR 0.810, recall@5
  0.945, recall@10 0.986, and p95 latency 55 ms. Callers score precision
  1.000 and recall 0.523, paths 0.727: syntax-only graphs miss receiver-typed
  and Java static-class calls that a compiler index binds, and the harness
  labels those sites anyway. The weekly and manually dispatched workflow
  uploads the full scorecard and syntax-only build timings (`harness/eval/`).
- **Agent-flow evaluation**: nine deterministic, network-free coding flows
  exercise orientation, dependency and blast-radius analysis, test selection,
  ambiguity, diff impact, stable-handle reuse, dirty edits, and CLI/MCP parity.
  The current scorecard passes 9/9 flows and 21/21 steps with one correct
  abstention and zero unsafe-confidence failures. These are observational
  scenarios, not a claim of general end-to-end coding accuracy.
- **Budgets** (measured on a ~2M-LOC Go repository, 271k nodes, before
  the stat-gated scan landed): full build 18s, one-file edit under 1s
  typical, cold point query under 100ms, `ask` 66ms end-to-end. A clean
  sync is now a stat-only walk — no file reads, no write transactions —
  measured 46→16ms on this repository and 55→10ms on an 80MB/400-file
  synthetic corpus; the old 73ms 2M-LOC no-op figure predates the stat
  gate. Cold open and incremental-edit budgets are CI-enforced
  (`crates/sinter-store/tests/cold_start.rs`,
  `crates/sinter-cli/tests/incremental_build.rs`); the full-build, warm-query,
  and memory budgets are measured manually per `harness/perf/README.md`.

## Workspace layout

| Crate | Responsibility |
|---|---|
| `sinter-core` | Typed graph model; invariants enforced at construction |
| `sinter-store` | Embedded redb store: adjacency, search indexes, incremental derivation |
| `sinter-extract` | Language-agnostic tree-sitter extraction; languages as data |
| `sinter-resolve` | Evidence-based reference resolution + SCIP ingest |
| `sinter-cli` | The `sinter` binary: pipeline, verbs, MCP server |
