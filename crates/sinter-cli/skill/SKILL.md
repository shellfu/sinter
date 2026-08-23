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
  raw grep output. Read source for behavior inside a function body.
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
compiler-grade bindings. A negative result with incomplete coverage is
`not_proven`, never proof of zero callers or dependencies.

`sinter install enforce --strict` opts into strict enforcement: the first
raw recursive search (grep/rg or the Grep tool) of a Claude Code session
is blocked with a redirect to `sinter ask/show/affected/deps/path/impact`;
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
| Starting a coding task ("add X", "fix Y", "cap Z") | `sinter context "<task>" --repo <repo>` first; then the specialized verbs below on the handles it returns |
| Vague/conceptual: "where is the X", "how does Y work" | `sinter ask "<question>" --repo <repo>` |
| Orient on a found symbol or file | `sinter show <symbol> --repo <repo>` |
| Exact/fuzzy symbol lookup | `sinter query <symbol> --repo <repo>` |
| What depends on X / blast radius (reverse) | `sinter affected <symbol> --repo <repo>` |
| What does X depend on (forward, before touching X) | `sinter deps <symbol> --repo <repo>` |
| Where is the graph blind (honesty check, negative proofs) | `sinter unresolved [--file <f>] [--name <n>] [--all] --repo <repo>` (default prints actionable gaps only; `--all` lists external/unsupported rows) |
| List a type's members or every impl of a method | `sinter query 'Type::*'` · `sinter query '*::method'` |
| How does A reach B | `sinter path <A> <B> --repo <repo>` |
| What does this commit/diff/PR affect downstream ("what changed recently and what does it touch") | `sinter impact [rev-range] --repo <repo>` (no range while editing = uncommitted working tree incl. untracked files; `--staged` = index only; e.g. `HEAD~1..HEAD`; each symbol collection returns 20 entries by default with full totals/truncation metadata; use `--limit 0` for all entries; a single rev such as `HEAD` also reports staged, unstaged, deleted, renamed, and untracked working-tree entries) — prefer over `git show`/`git log` archaeology |
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
2 error — branch on the exit code instead of parsing. In `--json` mode,
errors are structured `sinter.agent.v1` outcomes; MCP exposes the identical
failure under JSON-RPC `error.data`. `affected`/`path`/`deps` accept
`--evidence scip,import,scope,dynamic` and `--certain` (stronger evidence
tiers) plus `--relations calls,uses,imports,implements,extends` —
`--relations calls,uses` cuts file-level import noise from a blast
radius; implements/extends follows interface fan-out.

Text footers are one `coverage:` line plus query-specific gaps; set `SINTER_VERBOSE_COVERAGE=1` for filters and every repository-wide limitation.

Every verb defaults to `production,test,docs`. Pass `--scope` (or MCP `scope`)
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
- `affected`/`deps` cap output at `--limit` (default 200) and print a
  footer with the exact `--limit` rerun that widens it.
- Agent-facing `id` is a stable symbol key; `snapshot_id` is the byte-offset
  locator for one graph snapshot. Reuse `id` across harmless offset shifts.
  Use `if_snapshot` when consistency matters, and handle typed stale,
  relocated, or ambiguous outcomes; never guess a replacement binding.

## MCP

When the sinter MCP server is registered (`.mcp.json`, `.cursor/mcp.json`,
`.codex/config.toml` — full `sinter init` does this), the same verbs are
available as `mcp__sinter__*` tools (`ask`, `show`, `query`, `affected`,
`deps`, `path`, `unresolved`, `impact`, `overlap`, `map`). Every tool has a
closed input schema. Read `structuredContent.data` as the CLI `--json`
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
searches. A subagent told to use rg will not discover sinter on its own.

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
inside a single function body — read the file for those. For agent clients that cannot run shell commands, `sinter serve`
exposes the same graph over MCP stdio (`--workspace <manifest>` serves a
cross-repo scope with the same tool names).
