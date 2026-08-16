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
exists only when import, scope, receiver, typed-local, or compiler (SCIP)
evidence binds a reference to a definition. Ambiguity resolves to nothing —
"unresolved" is a first-class, counted outcome, never a guess. Every edge
carries its evidence kind, and every query can filter on it.

## Quickstart

Build the binary from the workspace root (requires a Rust toolchain):

```
cargo build --release
```

Onboard a repository — builds the graph, installs git hooks, registers
agent integration (AGENTS.md block, MCP, Claude skill), and finishes with
a doctor report:

```
./target/release/sinter init /path/to/repo
```

After that, `sinter build` refreshes the graph — an incremental hash-diff
that is a fast no-op when nothing changed; the git hooks run it
automatically. The build report ends with the honesty line — how much of the graph is
evidence-bound, and how much would need a dependency index:

```
resolution (this pass): ... resolved (scip 0, import 118, scope 189), ... unresolved (105 internal, 3193 external)
accuracy gauge: 3.1% internal-unresolved (external refs need dependency indexes, not resolver fixes)
```

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
| `sinter init [repo]` | Onboard a repo: build + hooks + agent integration + doctor |
| `sinter build [repo]` | Build or incrementally refresh the graph |
| `sinter watch [repo]` | Keep the graph fresh from filesystem events |
| `sinter hooks install` | Git hooks that refresh after commit/checkout/merge |
| `sinter ask "<question>"` | Ranked, content-bearing answers to vague questions |
| `sinter show <symbol>` | One-screen orientation card for a symbol or file |
| `sinter query <symbol>` | Exact + fuzzy symbol search |
| `sinter affected <symbol>` | Reverse blast radius, evidence-filterable |
| `sinter path <from> <to>` | Shortest dependency path with per-step evidence |
| `sinter impact <rev-range>` | Changed symbols → blast radius → affected tests |
| `sinter serve` | MCP server over stdio for agent use |
| `sinter workspace <manifest>` | Build all members of a cross-repo workspace + refresh boundary links |
| `sinter init --workspace` | Write a starter workspace manifest (never overwrites) |
| `sinter install --for <targets>` | Write agent cards (claude, cursor, agents/AGENTS.md); `--mcp` registers the server |
| `sinter doctor [repo]` | Diagnose installation + graph; every finding names its fix |
| `sinter version` | Version, graph schema, language packs |

`affected`, `path`, and every MCP tool accept `--evidence
scip,import,scope` and `--certain` to restrict traversal to stronger
evidence tiers.

## Languages

Rust, Go, Python, TypeScript, Bash, C++ (including Unreal Engine macro
conventions). A language is pure data — a tree-sitter grammar, one `.scm`
capture query, and a spec row — consumed by a single engine that never
branches on language. Adding a language requires no engine code; if it
ever does, the capture contract is wrong, not the language
(`DECISIONS.md`, D13).

If a compiler-produced SCIP index (`index.scip`) is present at the repo
root, sinter ingests it as the highest evidence tier.

## Accuracy and performance are measured, not asserted

- **Golden corpus**: 50 hand-verified fixtures across all six languages,
  mined from real-world extraction idioms. Extraction and resolution both
  gate CI at precision/recall 1.0; any change that moves the metric fails
  with the exact missing/extra tuples printed. Expectations derive from
  language semantics, never from engine output (`harness/golden/`).
- **Budgets** (measured on a ~2M-LOC Go repository, 271k nodes): full
  build 18s, no-op rebuild 73ms, one-file edit under 1s typical, cold
  point query under 100ms, `ask` 66ms end-to-end. Enforced by tests in
  `harness/perf/` and the CI suites.

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
