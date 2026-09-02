<p align="center">
  <img src="assets/sinter_logo.png" alt="sinter logo" width="360">
</p>

# sinter

A local code knowledge graph for coding agents, shipped as one static binary.
sinter builds a typed, directed graph of a repository's symbols with
tree-sitter and keeps it fresh incrementally. Start in an unfamiliar repository
with `sinter map`: it returns a bounded structural inventory with module
node/file counts, dependency hubs measured by graph in-degree, documentation
entry points, and graph-health limitations. It does not infer runtime entry
points or domain ownership. Then use the graph for focused work:

- **Reverse blast radius** — what transitively depends on a symbol,
  cross-file and cross-language (`sinter affected`). Traversal stops at
  hubs and names them; test dependents are counted, not listed, unless
  asked for.
- **Paths** — how one symbol reaches another (`sinter path`).
- **Diff impact** — which symbols and tests a changeset can affect
  (`sinter impact`), plus `--expect <symbol>` for the unfinished-refactor
  check: direct dependents of a symbol the diff did not touch.
- **Bounded text search** — a regex search whose corpus is a graph traversal
  (`sinter grep '<regex>' --within 'affected(<sym>)'`), replacing the
  run-affected-then-grep-the-files pipeline.
- **Polyglot, zero setup** — syntax-only graphs work without an LSP. When the
  syntax provides the necessary type information, sinter resolves self
  receivers, locally typed receivers, typed fields, and explicit static/class
  calls. Complete receiver-call binding often requires a fresh SCIP index;
  zero setup is not compiler-complete resolution.
- **Calibrated lexical navigation** — `sinter ask` ranks content-bearing
  starting points from names, docs, paths, signatures, string literals, and
  limited call evidence. Its confidence line reflects the evidence class of
  the top hit; `--explain` adds the ranking-margin bucket and that bucket's
  holdout count with a 95% interval (the holdout is 46 cases, so the
  interval is wide). The bucket is not a per-result probability; `ask` is a
  navigator, not a semantic answer engine.
- **Symbol orientation** — `sinter show` turns a selected symbol into a compact
  card with its definition (attributes included) and a one-line used-by
  tally; `--body` adds the source (whole when it fits in 60 lines, else up
  to the byte budget), `--impls` the type's impl blocks, and
  `show @file:line` names the enclosing symbol. A span over 8 KB or 200
  lines is a black hole no body dump can show: the card outlines it
  instead, listing the nested definitions, literal branches and command
  literals inside it with their line numbers (`--outline` forces it).
- **Task evidence packet** — `sinter context "<task>"` resolves identifiers in
  the task against real node names (fuzzy when needed, shown as
  `term ~> symbol`) and seeds from them, returning edit candidates, string
  literals and hand-maintained mirrors that mention the task's terms, and
  affected tests as runnable commands with the symbol that reached each.
  It never abstains while it has hits. Add `--workspace <manifest>` to rank
  candidates across declared member repositories in one packet.
- **Snapshot-scoped assertions** — `sinter assert no-callers <symbol>` checks
  depth-one call edges in an explicit corpus scope. Its decision is one of
  `violated`, `holds_for_indexed_snapshot`, or `not_proven`; it never claims
  runtime exhaustiveness. `sinter assert deletable <symbol>` tallies every
  depth-one dependent across all scopes, grouped by scope.
- **Citation maintenance** — `sinter cite <symbol>` emits a Markdown location
  with a stable symbol key. `sinter verify-doc <file.md>` re-resolves managed
  citations and fails on moved, missing, invalid, or identity-free references.
- **[Cross-repo workspaces](docs/workspaces.md)** — federated graphs over many repos for
  distributed systems: blast radius, paths, and PR impact across service
  boundaries (`--workspace`).

The design rule underneath everything: **evidence or nothing.** An edge
exists only when structural, scope, import, or compiler (SCIP) evidence
binds a reference to a definition (workspace manifests may add
operator-declared cross-repo links, and Rust trait impls add labeled
`dynamic` dispatch fan-out so blast radius survives `dyn Trait`).
Ambiguity resolves to nothing — "unresolved" is a first-class, counted
outcome, never a guess. Every edge carries its evidence kind, and every
query can filter on it. Be clear about what that buys without a compiler:
syntax-only evidence binds names it can prove by scope and imports and
leaves the rest (receiver calls, re-exports, macros) unresolved, so the
zero-setup graph is a precise subset, not the full graph. `sinter scip`
closes that gap where a compiler index is available.

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
agent integration (AGENTS.md block, MCP), and finishes with a doctor
report:

```
./target/release/sinter init /path/to/repo
```

Init prints everything it is about to write, grouped by scope, and asks
once before writing any of it (`-y` skips the prompt; a non-interactive
run prints the plan and proceeds). Every write lands inside the repo —
nothing under `~/.claude` is touched unless `--global` is passed, which
adds the machine-wide skill card and enforcement hooks. Both `init` and
`ensure` append `.sinter/` to the root `.gitignore` when the repository is
a git worktree and no existing line covers it.

Repo-local Claude hooks use bounded strict enforcement: the first broad
recursive search in a session is redirected to Sinter, while a retry is
allowed with a fallback-search reminder. Machine-wide hooks installed by
`--global` remain advisory.

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
refs sinter found without SCIP). Files edited since the index was built
get no SCIP evidence at all — their references fall back to import and
scope resolution rather than being rebound by byte position — so a stale
index never attributes a call to a name that no longer sits on that line.
Files the grammar could only parse partially are counted in one build
line (`N parsed partially (M symbols in them; …)`); `sinter doctor
--verbose` lists them.

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
rather than generated answers. Every hit shows its match provenance and the
text card carries one confidence line (`confidence: high — verify top hit`)
rated from the top hit's evidence; a weak top hit is `unrated`. String
literals that match the question are listed after the symbol hits, and
fixture, example, and test-local definitions rank below production ones
with the same evidence.
Agent JSON groups results by topic under one strict result budget and keeps
each hit lean: rank, name, kind, file, line, signature, doc, matched terms,
channels, and a one-word confidence. `--explain` (text or JSON) adds the
ranking margin, query-term coverage, the named holdout calibration, and
per-hit scores. Query terms expand through a small synonym table (cap, limit,
budget; size, bytes; caller, dependent) at reduced weight, so a literal match
always outranks a synonym. Weak singletons, low term coverage, and undersampled
ranking-margin buckets abstain. CLI JSON and MCP `structuredContent.data` use
the same payload.

Every `affected`, `deps`, and `path` response is deliberately bounded.
`deps` defaults to depth 1 (`--max-depth` widens). `affected` stops at hubs
(fan-in over 100, or a seed with more than 50 direct callers) and says so
(`stopped at hub Store (fan-in 114); --through-hubs to continue`). Both
count test-scope rows instead of listing them (`--include-tests`), and
`--relations calls,uses` drops file-level import rows. `path -k N` returns
up to N node-disjoint routes; a miss prints the closest frontier, the edges
the filter excluded, `reason: filter_excluded` when a filter caused it, and
a retry to run. In `--json`, positive and negative answers carry a
`coverage` summary (completeness, conclusive, snapshot, compiler-index
state); `--coverage` restores the repository-wide block with evidence
sources, certain and possible result counts, unresolved-reference counts,
and explicit gaps. The `universe` field names the repository root or every
declared workspace member searched. `completeness` describes only the
indexed snapshot and `conclusive` remains false, so agents do not turn a
non-empty syntax-only result into an exhaustive claim.

Agent-facing node `id` values are stable symbol keys and survive unrelated
offset shifts. `snapshot_id` retains the byte-exact locator for the reported
snapshot. Handle-consuming operations accept `if_snapshot` and return typed
stale-snapshot, relocated-handle, or ambiguous-candidate outcomes instead of
silently rebinding. A symbol whose bare name is ambiguous can be addressed as
`Name@file` or `Name@file:line` (`run@doctor.rs:175`).

## Test Sinter on a real repository

Sinter is recruiting ten design partners who use coding agents on repositories
where dependency or blast-radius investigations are expensive. Bring one real
task. The evaluation will compare the investigation with and without Sinter and
record where the graph helped or failed.

[Open a design-partner issue](https://github.com/shellfu/sinter/issues/new?template=design-partner.yml)
to propose a public repository or describe a private repository without naming
it. Do not include private source code, credentials, or customer information in
the issue.

## Commands

| Command | Purpose |
|---|---|
| `sinter ensure [repo]` | Build or refresh only the derived graph; does not install hooks or edit agent/client configuration |
| `sinter map [repo]` | First action in an unfamiliar repo: structural module inventory, explicitly measured dependency hubs, doc entry points, and one graph-health line (partial-syntax files, user gaps, compiler-index state) (`--json`) |
| `sinter init [repo]` | Onboard a repo: build + hooks + agent integration + doctor. Shows its plan and confirms first (`-y` skips). Repo-scoped by default; `-g` also installs the skill card and enforcement hooks machine-wide (`--scip`/`--no-scip` answer the indexer consent up front) |
| `sinter uninit [repo]` | Offboard completely: remove the graph and every sinter-managed artifact (`-g` also removes global skill + hooks) |
| `sinter build [repo]` | Build or incrementally refresh the graph |
| `sinter watch [repo]` | Keep the graph fresh from filesystem events |
| `sinter hooks install` | Git hooks that refresh after commit/checkout/merge |
| `sinter ask "<question>"` | Calibrated lexical starting points for a vague question, with verify/abstain guidance |
| `sinter show <symbol>` | One-screen orientation card for a symbol or file: signature with attributes, a one-line `used by: N files, M edges` tally (`--callers` lists the files), `impls (N)` for types (`--impls` prints their bodies). `--body` prints the whole source when it is 60 lines or fewer, else as much as fits `--budget-bytes`; `--context-lines 0` forces the whole span. A span over 8 KB or 200 lines prints `outline (N)` instead — its nested definitions, literal-discriminating branches and command/flag literals, by line (`--outline` forces the outline on any symbol; `--body` the source). `show @file:line` names the symbol enclosing that line, and `Name@file:line --body` excerpts around the line with a `>` marker. A tie-broken name leads with `resolved: Name@file` and lists `also_see` same-stem symbols |
| `sinter query <symbol>` | Exact + fuzzy symbol search, production copies first; exits 1 when only fuzzy neighbors match; Markdown section bodies are capped at 200 characters |
| `sinter affected <symbol>...` | Reverse blast radius, evidence-filterable; multiple seeds are unioned and deduplicated, each row naming the seeds that reached it. Stops at hubs (`--through-hubs` continues) and counts test rows (`--include-tests` lists them) |
| `sinter deps <symbol>` | Forward blast radius: what a symbol depends on, direct only by default (`--max-depth N` widens) |
| `sinter unresolved` | List unresolved references — the graph's honest gaps. Rows are user gaps (a name the corpus should define, a dangling `crate::x::gone` path); external, resolver-gap, and unsupported-syntax refs are counted and hidden (`--all` lists them). 50 rows per page, `--cursor N` for the next; `--file`, `--name` filter |
| `sinter path <from> <to>` | Shortest dependency path with per-step evidence; `-k N` returns up to N node-disjoint routes. An unproven answer reports the closest frontier, excluded edges, a `reason` (`filter_excluded`), and suggested retries |
| `sinter grep <regex> [--within <traversal>]` | Regex over the indexed corpus. Unbounded by default (every file in `--scope`; `--no-tests` drops test files); `--within 'affected(SYM)'`, `deps(SYM)`, `file(PATH)`, or `file(DIR)` (every indexed file under it) bounds it, repeatable and unioned, the seed's own file always in the bound. A bound that matches nothing is a warning, not a silent empty search |
| `sinter context "<task>"` | Evidence packet for a coding task: edit candidates, deps/dependents, matching literals and mirrors, relevant tests as runnable commands, gaps, next sinter commands (`--workspace <manifest>` federates member packets) |
| `sinter assert no-callers <symbol>` | Check for production callers by default; exits 0 only for `holds_for_indexed_snapshot`, with `--scope`, `--workspace`, `--certain`, and `--json` controls. Refuses to pick silently among same-stem symbols in other files (`also_see`); an unknown name exits 2. JSON is compact (`ignored_out_of_scope` inline); `--verbose` keeps the repository-wide `coverage.graph` block |
| `sinter assert no-dependents <symbol>` | Same contract over every non-containment relation (uses, reads, writes, implements, …) for constants, types, and traits; `no-callers` counts `calls` edges only |
| `sinter assert deletable <symbol>` | Every depth-one dependent across all scopes, grouped by scope; `has_dependents` exits 1, `none_observed` exits 0 |
| `sinter cite <symbol>` | Emit a repository-root-relative Markdown `file#Lline` citation carrying a stable symbol key |
| `sinter verify-doc <file.md>` | Re-resolve managed citations; bare `path:line` references return `not_proven` even when the location exists |
| `sinter impact <rev-range>` | Changed symbols → blast radius → affected tests. Validation commands come first; tests are ordered by distance from the changed symbols and printed as runnable commands per language (`cargo test`, `go test -run`, `pytest`, `npx vitest run`); production files precede test harness files in the radius. `--expect <symbol>` reports direct dependents the diff did not touch; `--full` restores the whole radius beside it |
| `sinter serve` | MCP server over stdio (`--repo` for one repo, `--workspace <manifest>` for a cross-repo scope) |
| `sinter overlap <range>...` | Map open PRs onto the graph; rank pairwise merge risk (direct/radius/file; `--relations` picks what the radius tier follows). Ranges where one contains the other's endpoint are reported as `sequential`, not scored |
| [`sinter workspace <manifest>`](docs/workspaces.md) | Build all members of a cross-repo workspace + refresh boundary links |
| [`sinter init --workspace`](docs/workspaces.md) | Write a starter workspace manifest (never overwrites) |
| `sinter install [targets]` | Write agent cards (claude, cursor, agents/AGENTS.md, enforce (`--strict` available), all); `--mcp` registers the server for Claude Code, Cursor, and Codex |
| `sinter scip [repo]` | Run every matching compiler indexer, merge into `.sinter/index.scip`, rebuild; no-op when fresh (`--force` reindexes); `scip check` is the CI freshness guard. Indexer output lands in `.sinter/scip-<lang>-<n>.log`, one summary line per indexer on the terminal |
| `sinter doctor [repo]` | Diagnose installation + graph: one `integration: all N checks ok` rollup (or the failing checks), `sinter serve` processes for this repo running a different version (Linux), partial-syntax file count (`--verbose` lists them), the SQL grammar gap, schema lints; every finding names its fix; `--fix` applies the safe ones and shows rebuild progress |
| `sinter update` | Self-update to the latest release, checksum-verified (`--dry-run` reports only) |
| `sinter completion <shell>` | Shell completions |
| `sinter version` | Version, graph schema, language packs |

MCP registrations use the portable `sinter` command and start non-required so
a missing binary cannot prevent the client from starting. `sinter doctor`
checks that the command resolves to an executable on `PATH` and performs an MCP
handshake.

Reads open the graph with a shared lock; a rebuild holds it exclusively. A
query that arrives during a rebuild queues for up to two minutes and prints
one `waiting for another sinter process` notice after a second, so parallel
agents see a delay rather than a `Database already open` error. Leaked MCP
servers from finished sessions keep their original binary; when versions
differ each one rewrites the graph in its own format, so `sinter doctor` lists
the ones serving this repository with their pids.

`affected`, `deps`, and `path` accept `--evidence scip,import,scope,dynamic`
and `--certain` to restrict traversal to stronger evidence tiers, and
`--relations calls,uses,imports,implements,extends,reads,writes,creates,alters,drops`
to restrict which edge
relations are followed (e.g. drop file-level import edges from a blast
radius); `sinter grep` accepts the same traversal filters for its `--within`
bound; their MCP counterparts take the same filters as `evidence` (array),
`min_confidence: "certain"`, and `relations` (array) parameters.

MCP tools share the CLI defaults: `scope` is the CLI corpus
(`production,test,docs`; `ask` uses `production,docs`), `deps` is depth 1,
`affected` stops at hubs. Arguments are validated against the advertised
schema (`tools/list` carries enums and one-line descriptions), so
`max_depth: "two"` or `relations: "calls"` is an `invalid_arguments` result,
not a silently ignored filter. Every user-fixable failure — unknown symbol,
ambiguous name, bad argument — is an `isError` tool result whose
`structuredContent.error` carries `code`, `message`, and `Name@file`
candidates; `outcome.status` (`complete`, `partial`, `not_proven`,
`not_found`, `error`) and `outcome.reason` (`limit_reached`,
`filter_excluded`, …) are the one place to branch. Paging covers the whole
result: `limit: 0` is unlimited, `next_cursor` is set whenever rows remain,
`cursor` resumes. `symbol` echoes are trimmed to key, name, file, line; the
`coverage` block is omitted unless `include_coverage: true`. `show`, `deps`,
and `affected` take `symbols: [...]` and `path` takes `pairs: [[from, to]]`
for one result per entry. `budget_bytes` below the smallest answer returns
that answer flagged, not an error; `next_actions` are tool calls.

`ask` defaults to the `production,docs` corpus; every other verb to
`production,test,docs`. Use `--scope` or the MCP `scope` array to include
fixtures, examples, generated files, or vendor code. Exact `show` remains
unfiltered. Repositories can exclude paths in `.sinterignore`; classify
fixture corpora and apply ordered overrides in `.sinter.toml` (committed) or
`.sinter/config.toml` (local, wins) with gitignore-syntax patterns:

```toml
[scope]
fixture = ["worked/**", "tools/*/expected/**"]

[[scope.override]]
pattern = "tools/golden-production/**"
scope = "production"
```

Without configuration, any path segment named `fixture(s)`, `golden`,
`testdata`, `expected`, `worked`, or `snapshot(s)` classifies as a fixture
and `example(s)`, `sample(s)`, or `demo(s)` as an example.

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

Syntax-only binding covers, per language: Python `import x as y` aliases,
function-local `from m import f` calls, and calls inside nested `def`s;
TypeScript `new X()` constructor calls, default exports, and `export *`
barrel re-exports; Go interface implementations matched by method set
across packages (an `affected` on an interface method that could not
traverse implementations says `gap: implementations not traversed`). Graph
schema v14 carries these facts; an existing graph rebuilds once on the
first query after upgrading.

### SQL graph

For `.sql` files, Sinter emits `table`, `view`, `column`, and `index` nodes.
It also records the direction and purpose of object references:

- `reads`: a `SELECT`, `FROM`, or `JOIN` source
- `writes`: an `INSERT`, `UPDATE`, or `DELETE` target
- `creates`, `alters`, `drops`: schema changes owned by the SQL file
- `uses`: foreign-key and index dependencies

A view owns the reads in its defining query. Top-level statements belong to
their file, including migration files.

SQL embedded in Rust is extracted too, at known query sinks and for literal
strings only: the `sqlx::query!`/`query_as!`/`query_scalar!` macros, the
`sqlx::query*` and diesel `sql_query` functions, and the `query*`, `execute*`,
`batch_execute`, and `prepare*` methods of tokio-postgres, rusqlite, and
connection pools. The enclosing Rust function gains the `reads`/`writes`
edges, so a table's blast radius crosses the language boundary. Table names
resolve within a database root first (a directory holding migrations and
queries), then fall back to the unique corpus-wide definition.

To find every query, migration, and Rust function that touches a table:

```sh
sinter affected users --relations reads,writes,creates,alters,drops
```

To prove nothing in the indexed snapshot writes, alters, or drops a table:

```sh
sinter assert no-writers users --json
```

To inspect the data and schema dependencies of one migration:

```sh
sinter deps migrations/20260901_users.sql \
  --relations reads,writes,uses,creates,alters,drops
```

`sinter doctor` folds migrations in filename order and warns when a table is
dropped at head but still referenced, or referenced but never created.

Limits. The SQL grammar (tree-sitter-sequel) does not parse several
PostgreSQL constructs — `CREATE FUNCTION`/`PROCEDURE`/`TRIGGER`/`POLICY`,
`DO` blocks, row-level security. Files containing them are indexed from
partial trees: the statements the grammar dropped are absent from the graph,
and `doctor` reports the gap as one row naming the likely constructs. SQL
strings built at runtime, and SQL embedded in Go, Python, or TypeScript, are
not extracted. Sinter does not infer column-level lineage, transaction scope,
indexes required by a query, or planner behavior; use `EXPLAIN` against the
target database for planner evidence.

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
- **Agent-flow evaluation**: eighteen deterministic, network-free coding flows
  cover multi-step graph use, bounded agent responses, task context, and a
  labeled design-similarity question that must surface both existing mapping
  tables before inspection.
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
