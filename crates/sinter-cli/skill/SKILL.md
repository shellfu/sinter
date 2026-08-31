---
name: sinter
description: Answer any question about a codebase's structure — where something is, what depends on it, how symbols connect, what a change affects — by querying the sinter code graph. Use for "where is X", "what calls/uses X", "what breaks if I change X", "how does A reach B", and PR/diff impact. If .sinter/ exists in the repo, treat the question as a sinter query first.
---

# sinter

This project uses sinter: a code knowledge graph with typed, evidence-backed
cross-file relationships, stored at `.sinter/` in the repository. All logic
lives in the binary — this card only routes questions to the right verb and
reads the output. Never re-implement chunking, retries, or graph traversal
in prose. If the user types `/sinter`, use this card before anything else.

## Rules

- In an unfamiliar repository, run `sinter map` first. It returns a structural
  inventory: module node/file counts, documentation entry points, dependency
  hubs ranked by non-containment in-degree, and graph health. Treat hubs as
  dependency-central symbols, not runtime entry points or domain ownership.
- For later codebase questions, query sinter first whenever `.sinter/` exists:
  use `sinter ask` as a calibrated lexical navigator for vague discovery,
  `sinter query`/`show` for exact symbols, and the traversal verbs for graph
  relationships. Results are scoped and ranked — usually much smaller than
  raw grep output. To find text *inside* a blast radius, use
  `sinter grep '<pat>' --within 'affected(SYM)'` rather than running
  `affected` and then grepping its files by hand. For a function body,
  `sinter show <symbol> --body` returns the excerpt without a file read.
- Queries self-sync: every command refreshes the graph incrementally
  before answering, so results reflect uncommitted edits with no manual
  step. `sinter build <repo>` remains for CI, scripting, and hooks.
- `.sinter/` is derived local state: never commit it, never edit it, and
  never treat it as stale-proof — `sinter doctor` reports freshness and
  names the fix for anything wrong.
- Only skip sinter when the task is about the graph itself being stale or
  wrong, or the user explicitly says not to use it.

## Setup check

If `.sinter/graph.redb` does not exist in the target repo, create the derived
graph state without changing hooks, repository instructions, or client
configuration:

    sinter ensure <repo>

`sinter ensure` is safe for an agent to run while answering a read-only code
question. It writes only under `.sinter/`. Run `sinter init <repo>` only when
the user explicitly asks for full onboarding (git hooks, agent integration,
MCP registration, and a doctor pass). `sinter build <repo>` remains the
explicit build/refresh command for CI and scripts.

If a graph exists but its health is uncertain, run `sinter doctor <repo>` and
follow the named repair. When traversal reports missing compiler evidence or
cannot prove a receiver-typed call, run `sinter scip <repo>` to add
compiler-grade bindings. A negative result is `not_proven`, never proof of
zero callers or dependencies: `coverage.conclusive` is always `false`, and
`completeness: complete_for_indexed_snapshot` describes the index, not the
running program.

For the narrower question "does this symbol have production callers?", run
`sinter assert no-callers <symbol> --json`. Exit 0 requires
`holds_for_indexed_snapshot`: zero observed depth-one `calls` edges, complete
indexed-snapshot coverage, and no unresolved reference matching the symbol
name. `violated` lists observed callers. `not_proven` names the remaining gap.
The assertion keeps `runtime_exhaustive: false` and
`coverage.conclusive: false`.

`sinter install enforce --strict` opts into strict enforcement: the first
raw recursive search (grep/rg or the Grep tool) of a Claude Code session
is blocked with a redirect to
`sinter context/ask/show/affected/deps/path/assert no-callers/cite/verify-doc/grep/impact`;
the retry passes with a one-time advisory nudge, and later searches in that
class are silent for the session — sinter-first, grep-second, never
grep-never. Strict mode only ever denies (it never auto-approves anything).
Default installs are quiet: hooks fire only on plain structure searches
(rg/ag/grep -r/git grep/find -name, `git log -S`) and the Grep tool, each
class at most once per session, plus one router line on the session's first
prompt. Everyday commands (git status, cargo, ls, cat) and subagent spawns
never nudge. Calls without a session ID remain nudge-only.

## Routing

| Question shape | Command |
|---|---|
| Orient in an unfamiliar repo (module inventory, dependency hubs, docs, graph health) | `sinter map --repo <repo>` |
| Starting a coding task ("add X", "fix Y", "cap Z") | `sinter context "<task>" --repo <repo>` first; then the specialized verbs below on the handles it returns. Identifiers in the task are resolved against real node names and seed the packet — naming a real symbol materially improves the answer. It returns anchors (task term -> resolved node), unresolved intents (terms that matched nothing), and affected tests already shaped as runnable commands |
| Starting a task across declared repositories | `sinter context "<task>" --workspace <manifest.toml>`; read `coverage.universe.members` before making a cross-repo claim |
| Vague/conceptual: "where is the X", "how does Y work" | `sinter ask "<question>" --repo <repo>` |
| Orient on a found symbol or file | `sinter show <symbol> --repo <repo>` (add `--body [--context-lines N]` for a bounded source excerpt — no follow-up file read) |
| Exact/fuzzy symbol lookup | `sinter query <symbol> --repo <repo>` |
| What depends on X / blast radius (reverse) | `sinter affected <symbol>... --repo <repo>` (seeds are repeatable; results are unioned and deduplicated, each row naming the seeds that reached it) |
| What does X depend on (forward, before touching X) | `sinter deps <symbol> --repo <repo>` |
| Where is the graph blind (honesty check, negative proofs) | `sinter unresolved [--file <f>] [--name <n>] [--all] --repo <repo>` (default prints actionable gaps only; `--all` lists external/unsupported rows) |
| Does X have production callers in this indexed snapshot? | `sinter assert no-callers <symbol> --json [--workspace <manifest.toml>]`; accept only `status: holds_for_indexed_snapshot`, and quote the returned scope, snapshot, universe, and limitations |
| Emit a durable source citation | `sinter cite <symbol> --repo <repo>`; paste the entire Markdown line, including its `sinter-cite:v1` metadata comment. Link targets are repository-root relative |
| Gate a document's source citations | `sinter verify-doc <file.md> --repo <repo> --json`; `current` passes, `stale` identifies moved or missing citations, and `not_proven` identifies bare path/line references without symbol identity |
| List a type's members or every impl of a method | `sinter query 'Type::*'` · `sinter query '*::method'` |
| How does A reach B | `sinter path <A> <B> --repo <repo>` |
| Find text, but only inside a blast radius ("which of these callers still say X") | `sinter grep '<regex>' --within 'affected(<sym>)' --repo <repo>` — `--within` takes `affected(SYM)`, `deps(SYM)`, or `file(PATH)`, is repeatable, and unions the bounded file sets. Full regex. This replaces running `affected` and grepping the result by hand |
| What does this commit/diff/PR affect downstream ("what changed recently and what does it touch") | `sinter impact [rev-range] --repo <repo>` (no range while editing = uncommitted working tree incl. untracked files; `--staged` = index only; e.g. `HEAD~1..HEAD`; each symbol collection returns 20 entries by default with full totals/truncation metadata; use `--limit 0` for all entries; a single rev such as `HEAD` also reports staged, unstaged, deleted, renamed, and untracked working-tree entries) — prefer over `git show`/`git log` archaeology |
| Did the refactor finish? | `sinter impact --expect <symbol> --repo <repo>` — direct dependents of the symbol the diff did NOT touch (repeatable) |
| Where do open PRs collide / merge risk | `sinter overlap <base...prA> <base...prB> ... --repo <repo>` |
| Build or refresh a cross-repo graph | `sinter workspace <manifest.toml>` |
| Cross-repo (distributed system) versions of the above | add `--workspace <manifest.toml>`; symbols may be `member:Symbol` |
| Create missing derived graph state | `sinter ensure <repo>` |
| Diagnose graph or integration problems | `sinter doctor <repo>` |
| Add compiler-grade call/type evidence | `sinter scip <repo>` |

Focused `ask` questions minimize output, but multi-topic questions are safe:
the agent payload returns explicit `topics[]`, applies one global hit budget,
and gives every topic independent calibration, advice, and abstention state.
Phrase topics with words expected in identifiers or doc comments.
Use `sinter ask "<question>" --explain` only when ranking diagnostics are
needed; default agent JSON omits the score breakdown to conserve context.

Agents should always pass `--json`: it emits the compact data payload from
the versioned `sinter.agent.v1` contract. Omitting `--json` selects the
human-oriented renderer. MCP `structuredContent` wraps that same payload as
`{protocol, operation, outcome, data}`; the CLI value is exactly `data`.
MCP's `content[0].text` is a one-line summary; read `structuredContent`. Repo-scope
read verbs exit grep-style: 0 results found, 1 valid query with no results,
2 error — the exit code only decides whether to read further, since 1
conflates `not_found` (the symbol was not matched) with `not_proven` (the
graph could not see). Any negative claim must read `status` (CLI `--json`)
or `outcome.status` (MCP). In `--json` mode, errors are structured
`sinter.agent.v1` outcomes; MCP exposes the identical failure under
JSON-RPC `error.data`. CLI assertion and document gates use 0 pass, 1 fail,
and 2 error; inspect their top-level `status` after branching on the code.
`affected`/`path`/`deps` accept
`--evidence scip,import,scope,dynamic` and `--certain` (stronger evidence
tiers) plus `--relations calls,uses,imports,implements,extends` —
`--relations calls,uses` cuts file-level import noise from a blast
radius; implements/extends follows interface fan-out.

Every traversal coverage envelope carries `universe`. Repository mode names
the canonical root. Workspace mode names the manifest workspace and every
canonical member root. Treat repositories absent from that field as
unsearched.

Text footers are one `coverage:` line plus query-specific gaps; set `SINTER_VERBOSE_COVERAGE=1` for filters and every repository-wide limitation.

Traversal verbs default to `production,test,docs`; `ask` defaults to
`production,docs`; `--scope` overrides. The MCP `scope` argument defaults to
`all` instead, so an MCP call sees fixtures and vendor code a bare CLI call
would not — pass `scope` explicitly when that matters. Pass it
when fixtures, examples, generated files, or vendor code are relevant.
Exact `show` remains unfiltered. Result nodes carry their persisted `scope`;
do not infer production ownership from a path string.

## Reading results

- Every `ask` topic reports query-term coverage, `ranking_margin`,
  `confidence.ranking_bucket`, calibration version/sample/precision,
  `verify_required`, and advice. The bucket measures score separation; it is
  not a per-result probability. `confidence.level` is the v1 compatibility
  alias. Honor `abstain` and verification decisions before using a hit to
  mutate code.
- Edges carry the call site: dependents and path steps print the
  `file:line` where the reference occurs (`site` in JSON) — jump straight
  there instead of re-searching.
- Edges carry evidence; "unresolved" is a real answer meaning no evidence
  binds the reference — say so rather than guessing. Every positive or
  negative `affected`/`deps`/`path` result includes `coverage` with evidence
  sources, filters, certain/possible/unresolved counts, gaps, and bounded
  completeness. A `not_proven` outcome or `conclusive: false` forbids
  exhaustive negative claims; `sinter unresolved` lists the gaps themselves.
- A `not_proven` `path` carries `closest_frontier` (how far the forward
  search got, ranked by file proximity to the target), `excluded_edges`
  (incoming edges refused, by reason) and `suggested_retries` (the exact
  reruns that would lift each refusal). Rerun one of those before
  concluding no path exists.
- A bare name that matches several definitions can be addressed as
  `Name@file` or `Name@file:line` (`run@doctor.rs:175`); lookup otherwise
  prefers `production`. `sinter context` already emits handles in this form.
- `affected`/`deps` cap output at `--limit` (default 200) and print a
  footer with the exact `--limit` rerun that widens it.
- Agent-facing `id` is a stable symbol key; `snapshot_id` is the byte-offset
  locator for one graph snapshot. Reuse `id` across harmless offset shifts.
  Use `if_snapshot` when consistency matters, and handle typed stale,
  relocated, or ambiguous outcomes; never guess a replacement binding.

## MCP

When the sinter MCP server is registered (`.mcp.json`, `.cursor/mcp.json`,
`.codex/config.toml` — full `sinter init` does this), the same verbs are
available as `mcp__sinter__*` tools (`ask`, `show`, `query`, `context`, `affected`,
`deps`, `path`, `grep`, `unresolved`, `impact`, `overlap`, `map`). Every tool
has a closed input schema. `grep` takes `pattern` and `within` (array, at
least one bound); `show` takes `body` and `context_lines`; `impact` takes
`expect` (array) and answers it under `expect[].untouched`. A `--workspace`
server serves a smaller set — `ask`, `show`, `query`, `context`, `affected`, `deps`, `path`,
`unresolved`, `impact` — so `grep` is repository-scope only. Read `structuredContent.data` as the CLI `--json`
payload (`content[0].text` is only a one-line summary) and inspect `outcome`
before acting: `not_proven` is explicitly non-conclusive, and neither
`partial` nor `not_found` may be silently upgraded into a negative proof.
Terse `affected`/`deps` rows use short keys; a `legend` field decodes them on
the first page. Long-form guidance (coverage semantics, batching, budget
paging) is the MCP resource `sinter://guide`.

## Orchestrating subagents

When writing prompts for subagents, mandate sinter for every structure
claim — callers, dependencies, blast radius, and especially negative
proofs ("no production caller") — and scope grep/rg to content-only
searches. When a content search is bounded by structure, name
`sinter grep --within` explicitly. A subagent told to use rg will not
discover sinter on its own.

## Workspaces (cross-repo)

If a workspace manifest exists (TOML with [members] mapping names to repo
paths), `sinter workspace <manifest>` builds every member and refreshes
boundary links; `affected`/`path`/`ask`/`impact`/`doctor` accept
`--workspace <manifest>` to traverse across repos. Cross-repo edges carry
import evidence (or `declared` for manifest-declared runtime coupling) and
are filterable like any other.

## Boundaries

Symbol-level structure only — plus markdown structure: headings index as
section nodes (13 languages total), so `ask` finds where something is
documented with file:line. Not for summarization or content questions
inside a single function body — use `show --body` for the excerpt, or
`grep --within` when the text is what you are looking for, and read the
file when you need the whole thing. For agent clients that cannot run shell commands, `sinter serve`
exposes the same graph over MCP stdio (`--workspace <manifest>` serves a
cross-repo scope with the same tool names).
