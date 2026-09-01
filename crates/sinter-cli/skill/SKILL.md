---
name: sinter
description: Answer any question about a codebase's structure — where something is, what depends on it, how symbols connect, what a change affects — by querying the sinter code graph. Use for "where is X", "what calls/uses X", "what breaks if I change X", "how does A reach B", and PR/diff impact. If .sinter/ exists in the repo, treat the question as a sinter query first.
---

# sinter

`.sinter/` holds a derived code graph: never commit or edit it, and
never treat it as stale-proof (`sinter doctor` names the fix). When
`.sinter/graph.redb` exists, query sinter before any filesystem search. If it
is missing, `sinter ensure <repo>` creates it (writes only `.sinter/`); run
`sinter init` only when full onboarding is requested. Queries self-sync;
`sinter build` stays for CI.

## Routing (full table: the sinter block in AGENTS.md)

| Need | Verb |
|---|---|
| Orient in an unfamiliar repo | `sinter map` (first) |
| Start a coding task | `sinter context "<task>"` (name real symbols) |
| Vague discovery | `sinter ask "<question>"` (`--explain` adds ranking diagnostics) |
| Exact/fuzzy symbol; members | `sinter query <sym>`, `'Type::*'`, `'*::method'` |
| Inspect a symbol / its body | `sinter show <sym> [--body [--context-lines N]]` |
| Blast radius / forward deps / route | `sinter affected <sym>...`, `sinter deps <sym>`, `sinter path <A> <B>` |
| Text inside a blast radius | `sinter grep '<re>' --within 'affected(SYM)'` (also `deps(SYM)`, `file(PATH)`; repeatable) |
| Graph gaps | `sinter unresolved [--file f] [--name n]` |
| No-production-caller proof | `sinter assert no-callers <sym> --json`: accept only `holds_for_indexed_snapshot` |
| Nothing-depends-on proof (const/type/trait) | `sinter assert no-dependents <sym> --json` (`no-callers` counts `calls` only) |
| Citations | `sinter cite <sym>`; gate with `sinter verify-doc <f.md> --json` |
| Diff/PR impact; unfinished refactor | `sinter impact [rev-range]` (`--limit 0` = all); `sinter impact --expect <sym>` |
| PR collisions | `sinter overlap <rangeA> <rangeB>` |
| Cross-repo | `sinter workspace <manifest.toml>`, then `--workspace <manifest.toml>`; symbols `member:Symbol` |
| Setup, repair, compiler evidence | `sinter ensure`, `sinter doctor`, `sinter scip` |

## Reading results

- Always pass `--json` (`sinter.agent.v1`). Read verbs exit 0 results, 1
  none, 2 error; gates exit 0 pass, 1 fail, 2 error. Branch on the code, then
  read `status`: exit 1 conflates `not_found` with `not_proven`.
- `not_proven`, `partial`, `conclusive: false`, and unresolved references
  forbid negative claims. Run `sinter unresolved`, a `not_proven` path's
  `suggested_retries`, or `sinter scip` before saying "no callers"/"no path".
- Ambiguous name: rerun as `Name@file` or `Name@file:line`
  (`run@doctor.rs:175`); lookup otherwise prefers `production`.
- Edges carry `site` (`file:line`): jump there instead of re-searching.
  `coverage.universe` names what was searched; anything absent was not.
- `--relations calls,uses,reads,writes,creates,alters,drops` on
  affected/deps/path drops import noise.
- Traversal scope defaults to `production,test,docs`, `ask` to
  `production,docs`; MCP `scope` defaults to `all`.

## MCP

`mcp__sinter__*` tools (ask, show, query, context, affected, deps, path, grep,
unresolved, impact, overlap, map; `--workspace` servers omit grep, overlap,
map) mirror the flags: `grep{pattern, within[]}`, `show{body, context_lines}`,
`impact{expect[]}`. Read `structuredContent` (`{protocol, operation, outcome,
data}`; `data` is the CLI `--json` payload), not `content[0].text`, and check
`outcome.status` before acting. Long-form guidance: resource `sinter://guide`.

## Hooks and subagents

`sinter init` installs strict Claude Code hooks: the first raw recursive search
(grep/rg or the Grep tool) of a session is denied with a redirect, the retry
gets one nudge, later ones are silent. Advisory installs nudge each
class at most once per session. Calls without a session ID remain nudge-only.
Subagent prompts must mandate sinter for structure claims and reserve rg
for unbounded text.
