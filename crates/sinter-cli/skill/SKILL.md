---
name: sinter
description: Answer any question about a codebase's structure — where something is, what depends on it, how symbols connect, what a change affects — by querying the sinter code graph. Use for "where is X", "what calls/uses X", "what breaks if I change X", "how does A reach B", and PR/diff impact. If .sinter/ exists in the repo, treat the question as a sinter query first.
---

# sinter

`.sinter/` holds a derived code graph: never commit or edit it, and
never treat it as stale-proof (`sinter doctor` names the fix). When
`.sinter/graph.redb` exists, query sinter before any filesystem search; if
missing, `sinter ensure <repo>` creates it (writes only `.sinter/`); `sinter
init` only for full onboarding. Queries self-sync; `sinter build` stays for
CI. Schema v16: an older graph rebuilds once on the first query.

## Routing (full table: the sinter block in AGENTS.md)

| Need | Verb |
|---|---|
| Orient in an unfamiliar repo | `sinter map` |
| Start a coding task | `sinter context "<task>"` (name real symbols; gives literals, mirrors, tests, next commands) |
| Vague discovery | `sinter ask "<question>"` (`--explain` = ranking diagnostics) |
| Exact/fuzzy symbol, members | `sinter query <sym>`, `'Type::*'`, `'*::method'` |
| Inspect a symbol | `sinter show <sym>`: `--body` (whole source when short), `--outline` (huge spans, auto over 8 KB), `--impls`, `--callers`, `X@file:line` |
| Where exactly is it called | rows list every site: `f.rs:12, :48 (+4 more)`; JSON adds `sites`/`sites_total` |
| Dependents / deps / path | `sinter affected <sym>...` (`--include-tests`, `--through-hubs`), `sinter deps <sym>` (depth 1), `sinter path <A> <B>` (`-k N`) |
| Text search | `sinter grep '<re>'` (unbounded); `--within 'affected(SYM)'`/`deps(SYM)`/`file(PATH)` narrows |
| Can I delete this | `sinter assert deletable <sym>`: `has_dependents`/`none_observed`, all scopes |
| No-production-caller proof | `sinter assert no-callers <sym> --json`: accept only `holds_for_indexed_snapshot` |
| Nothing-depends-on / nothing-writes-table proof | `sinter assert no-dependents <sym> --json`; `sinter assert no-writers <table> --json` |
| User graph gaps | `sinter unresolved [--file f] [--name n]` (`--all` adds external/resolver gaps) |
| Citations | `sinter cite <sym>`; gate: `sinter verify-doc <f.md> --json` |
| Diff impact; unfinished refactor | `sinter impact [rev-range]` (`--limit 0` = all); `sinter impact --expect <sym>` (base-rev names ok; `--full`) |
| PR collisions | `sinter overlap <rangeA> <rangeB>` |
| Cross-repo | `sinter workspace <manifest.toml>`, then `--workspace <manifest.toml>`; `member:Symbol` |
| Setup, repair, SCIP evidence | `sinter ensure`, `sinter doctor`, `sinter scip` |

## Reading results

- `--json` (`sinter.agent.v1`) when branching. Read verbs exit 0 results, 1
  none, 2 error; gates 0 pass, 1 fail, 2 error. Branch on the code, then
  read `status`: exit 1 conflates `not_found` with `not_proven`.
- `not_proven`, `partial`, `conclusive: false`, and unresolved references
  forbid negative claims. Run `sinter unresolved`, a `not_proven` path's
  `suggested_retries`, or `sinter scip` first. `reason: filter_excluded`
  means your filter emptied the result, not the graph.
- JSON carries a coverage summary; `--coverage` adds the full block.
  `coverage.universe` names what was searched; anything absent was not.
- Ambiguous name: rerun as `Name@file` or `Name@file:line`
  (`run@doctor.rs:175`); lookup otherwise prefers `production`.
- Edges carry `site` (`file:line`): jump there. Any `--relations` filter
  drops file-level import noise.
- Scope defaults: traversal `production,test,docs`, `ask` `production,docs`;
  MCP matches.

## MCP

`mcp__sinter__*` tools (ask, show, query, context, affected, deps, path, grep,
unresolved, impact, overlap, map; `--workspace` servers omit grep, overlap,
map) mirror the flags: `grep{pattern, within[]}`, `show{body, context_lines}`,
`impact{expect[]}`; batch via `symbols[]`/`pairs[]`. Read `structuredContent`
and `outcome.status`/`outcome.reason` before acting; fixable errors are
`isError` results with `Name@file` candidates. Guide: `sinter://guide`.

## Hooks and subagents

`sinter init` installs strict Claude Code hooks: the first raw recursive search
(grep/rg or the Grep tool) of a session is denied with a redirect, the retry
gets one nudge, later ones are silent. Advisory installs nudge each
class at most once per session. Calls without a session ID remain nudge-only.
Subagent prompts must mandate sinter for structure claims and `sinter grep`
for text; rg only for content sinter did not index.
