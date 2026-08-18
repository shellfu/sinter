<p align="center">
  <img src="assets/sinter_logo.png" alt="sinter logo" width="360">
</p>

# sinter

A code knowledge-graph engine in one static binary. sinter builds a typed,
directed graph of a repository's symbols with tree-sitter, keeps it fresh
incrementally, and answers the structural questions that grep, an LSP, or an
agent reading files cannot:

- **Reverse blast radius** — what transitively depends on a symbol,
  cross-file and cross-language (`sinter affected`).
- **Paths** — how one symbol reaches another (`sinter path`).
- **Diff impact** — which symbols and tests a changeset can affect
  (`sinter impact`).
- **Polyglot, zero setup** — works on repositories with no LSP configured,
  across every supported language at once.
- **Human entry point** — a vague question returns a ranked, content-bearing
  starting point in milliseconds (`sinter ask`, `sinter show`).
- **Cross-repo workspaces** — federated graphs over many repos for
  distributed systems: blast radius, paths, and PR impact across service
  boundaries (`--workspace`, `docs/design-workspace.md`).

The design rule underneath everything: **evidence or nothing.** An edge
exists only when structural, scope, import, or compiler (SCIP) evidence
binds a reference to a definition (workspace manifests may add
operator-declared cross-repo links, and Rust trait impls add labeled
`dynamic` dispatch fan-out so blast radius survives `dyn Trait`).
Ambiguity resolves to nothing — "unresolved" is a first-class, counted
outcome, never a guess. Every edge carries its evidence kind, and every
query can filter on it.

## Install

One line, no dependencies (Linux and macOS; verifies the release
checksum, installs to `~/.local/bin`):

```
curl -fsSL https://raw.githubusercontent.com/shellfu/sinter/main/install.sh | sh
```

Windows, in PowerShell (new and less battle-tested; verifies the release
checksum, installs to `%LOCALAPPDATA%\sinter\bin`):

```
irm https://raw.githubusercontent.com/shellfu/sinter/main/install.ps1 | iex
```

Or build from source (requires a Rust toolchain):

```
cargo build --release
```

## Quickstart

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
build report ends with the honesty line — how much of the graph is
evidence-bound, and how much would need a dependency index:

```
resolution (this pass): ... resolved (scip 0, import 118, scope 189), ... unresolved (105 internal, 3193 external)
accuracy gauge: 3.1% internal-unresolved (external refs need dependency indexes, not resolver fixes)
```

When a SCIP index is present, the report also prints the compiler
cross-check (what share of internally-bound refs agree with the
compiler) and internal recall vs the compiler (how many compiler-bound
refs sinter found without SCIP).

Ask a question against the graph (output shown for this repository):

```
$ sinter ask "where is the trigram search"

1. function Store::search    [doc+name+path+sig 2/2 terms]
   crates/sinter-store/src/search.rs:148
   /// Fuzzy candidates: nodes sharing the most trigrams with the query,
   pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Node>, StoreError>
```

Every hit shows its match provenance (`[doc+name 2/2 terms]`) — the ranking
is auditable, in the same spirit as the edges.

## Commands

| Command | Purpose |
|---|---|
| `sinter init [repo]` | Onboard a repo: build + hooks + agent integration + doctor (`--scip`/`--no-scip` answer the indexer consent up front; `-g` also installs enforcement hooks globally) |
| `sinter uninit [repo]` | Offboard completely: remove the graph and every sinter-managed artifact (`-g` also removes global skill + hooks) |
| `sinter build [repo]` | Build or incrementally refresh the graph |
| `sinter watch [repo]` | Keep the graph fresh from filesystem events |
| `sinter hooks install` | Git hooks that refresh after commit/checkout/merge |
| `sinter ask "<question>"` | Ranked, content-bearing answers to vague questions |
| `sinter show <symbol>` | One-screen orientation card for a symbol or file |
| `sinter query <symbol>` | Exact + fuzzy symbol search |
| `sinter affected <symbol>` | Reverse blast radius, evidence-filterable |
| `sinter path <from> <to>` | Shortest dependency path with per-step evidence |
| `sinter impact <rev-range>` | Changed symbols → blast radius → affected tests |
| `sinter serve` | MCP server over stdio (`--repo` for one repo, `--workspace <manifest>` for a cross-repo scope) |
| `sinter overlap <range>...` | Map open PRs onto the graph; rank pairwise merge risk (direct/radius/file) |
| `sinter workspace <manifest>` | Build all members of a cross-repo workspace + refresh boundary links |
| `sinter init --workspace` | Write a starter workspace manifest (never overwrites) |
| `sinter install [targets]` | Write agent cards (claude, cursor, agents/AGENTS.md, enforce, all); `--mcp` registers the server for Claude Code, Cursor, and Codex |
| `sinter scip [repo]` | Run every matching compiler indexer, merge into `.sinter/index.scip`, rebuild |
| `sinter doctor [repo]` | Diagnose installation + graph (including an MCP handshake and lock-held reporting); every finding names its fix |
| `sinter completion <shell>` | Shell completions |
| `sinter version` | Version, graph schema, language packs |

`affected`, `path`, and every MCP tool accept `--evidence
scip,import,scope,dynamic` and `--certain` to restrict traversal to stronger
evidence tiers.

## Languages

Rust, Go, Python, TypeScript, JavaScript (ESM, CJS, JSX), Java, C#, C,
C++ (including Unreal Engine macro conventions), SQL (DDL/DML), Bash,
and Protobuf. A language is pure data — a tree-sitter grammar, one `.scm`
capture query, and a spec row — consumed by a single engine that never
branches on language. Adding a language requires no engine code; if it
ever does, the capture contract is wrong, not the language
(`DECISIONS.md`, D13).

If a compiler-produced SCIP index (`index.scip`) is present at the repo
root or at `.sinter/index.scip`, sinter ingests it as the highest
evidence tier. `sinter scip` runs the matching indexer for every language
present (rust-analyzer, scip-go, scip-typescript for TS and JS,
scip-python, scip-clang for C/C++, scip-java, scip-dotnet), merges the
results into `.sinter/index.scip`, and rebuilds; a missing indexer
prints its install command and is skipped. Bash, proto, and SQL have no
SCIP indexers.

## Teams

Graphs are per-machine and rebuild in seconds; the SCIP index is the
expensive shared artifact. Build it once in CI (`sinter scip --if-stale`),
distribute the file, and each teammate's next build ingests it
automatically. Recipe, cache-key guidance, and a copy-paste workflow:
[`docs/team.md`](docs/team.md).

## Accuracy and performance are measured, not asserted

- **Golden corpus**: 69 hand-verified fixtures across all twelve languages,
  mined from real-world extraction idioms. Extraction and resolution both
  gate CI at precision/recall 1.0; any change that moves the metric fails
  with the exact missing/extra tuples printed. Expectations derive from
  language semantics, never from engine output (`harness/golden/`).
- **Agent token benchmark** (`docs/bench-agent-tokens.md`): blast-radius
  questions cost an agent ~4× fewer tokens and 5× fewer turns than
  grepping; simple lookups are parity. Measured, honest, N=1 per cell.
- **Agent routing benchmark** (`docs/bench-routing.md`): unprompted
  Claude Code sessions chose sinter first on 10/10 structure questions
  and never invoked it on content questions (0/6) — measured on Haiku,
  the floor model, on a machine with the enforcement hooks installed
  (the `sinter init` default). One repo, n=16; script checked in.
- **Budgets** (measured on a ~2M-LOC Go repository, 271k nodes, before
  the stat-gated scan landed): full build 18s, one-file edit under 1s
  typical, cold point query under 100ms, `ask` 66ms end-to-end. A clean
  sync is now a stat-only walk — no file reads, no write transactions —
  measured 46→16ms on this repository and 55→10ms on an 80MB/400-file
  synthetic corpus; the old 73ms 2M-LOC no-op figure predates the stat
  gate. Enforced by tests in `harness/perf/` and the CI suites.

## Workspace layout

| Crate | Responsibility |
|---|---|
| `sinter-core` | Typed graph model; invariants enforced at construction |
| `sinter-store` | Embedded redb store: adjacency, search indexes, incremental derivation |
| `sinter-extract` | Language-agnostic tree-sitter extraction; languages as data |
| `sinter-resolve` | Evidence-based reference resolution + SCIP ingest |
| `sinter-cli` | The `sinter` binary: pipeline, verbs, MCP server |

Design history lives in `DECISIONS.md` (one paragraph per decision,
alternatives named). The human-query layer's full design is
`docs/design-human-query.md`.
