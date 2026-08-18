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

- For codebase questions, query sinter first whenever `.sinter/` exists:
  `sinter ask` for concepts, `sinter path` for how two symbols relate,
  `sinter affected` for blast radius. Results are scoped and ranked —
  usually much smaller than raw grep output, and content-bearing enough
  to answer without opening files.
- Queries self-sync: every command refreshes the graph incrementally
  before answering, so results reflect uncommitted edits with no manual
  step. `sinter build <repo>` remains for CI, scripting, and hooks.
- `.sinter/` is derived local state: never commit it, never edit it, and
  never treat it as stale-proof — `sinter doctor` reports freshness and
  names the fix for anything wrong.
- Only skip sinter when the task is about the graph itself being stale or
  wrong, or the user explicitly says not to use it.

## Setup check

If `.sinter/graph.redb` does not exist in the target repo, onboard it —
one command builds the graph, installs git hooks, and registers agent
integration, ending with a doctor report:

    sinter init <repo>

(`sinter build <repo>` alone refreshes an existing graph.)

`sinter install enforce --strict` opts into strict enforcement: the first
raw recursive search (grep/rg or the Grep tool) of a Claude Code session
is blocked with a redirect to `sinter ask/show/affected/path/impact`;
running the same search again passes with an advisory nudge, as does
every later one — sinter-first, grep-second, never grep-never. Strict mode only
ever denies (it never auto-approves anything); default installs remain
advisory context injection only.

## Routing

| Question shape | Command |
|---|---|
| Vague/conceptual: "where is the X", "how does Y work" | `sinter ask "<question>" --repo <repo>` |
| Orient on a found symbol or file | `sinter show <symbol> --repo <repo>` |
| Exact/fuzzy symbol lookup | `sinter query <symbol> --repo <repo>` |
| What depends on X / blast radius | `sinter affected <symbol> --repo <repo>` |
| How does A reach B | `sinter path <A> <B> --repo <repo>` |
| What does this commit/diff/PR affect downstream ("what changed recently and what does it touch") | `sinter impact <rev-range> --repo <repo>` (e.g. `HEAD~1..HEAD`) — prefer over `git show`/`git log` archaeology |
| Where do open PRs collide / merge risk | `sinter overlap <base...prA> <base...prB> ... --repo <repo>` |
| Cross-repo (distributed system) versions of the above | add `--workspace <manifest.toml>`; symbols may be `member:Symbol` |

Ask one topic per `ask` call, phrased with the words you expect in an
identifier or doc comment — a multi-topic question ("what documentation
describes X, Y, or Z?") dilutes ranking and earns a weak-match warning.
Add `--json` to `ask` for structured output. `affected`/`path` accept
`--evidence scip,import,scope,dynamic` and `--certain` to restrict to stronger
evidence tiers.

## Reading results

- Every `ask` hit shows match provenance (`[name+doc 2/2 terms]`) and
  file:line with signature/doc — usually no file read is needed to answer.
- Edges carry evidence; "unresolved" is a real answer meaning no evidence
  binds the reference — say so rather than guessing.
- Symbol ambiguity returns a candidate list; pick the qualified name or
  node id and rerun rather than assuming.

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
