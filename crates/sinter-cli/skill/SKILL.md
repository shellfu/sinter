---
name: sinter
description: Answer any question about a codebase's structure — where something is, what depends on it, how symbols connect, what a change affects — by querying the sinter code graph. Use for "where is X", "what calls/uses X", "what breaks if I change X", "how does A reach B", and PR/diff impact. If .sinter/ exists in the repo, treat the question as a sinter query first.
---

# sinter

sinter is a code knowledge-graph engine. All logic lives in the binary —
this skill only routes questions to the right verb and reads the output.
Never re-implement chunking, retries, or graph traversal in prose.

## Setup check

If `.sinter/graph.redb` does not exist in the target repo, build it first
(incremental; safe to run anytime, fast when nothing changed):

    sinter build <repo>

## Routing

| Question shape | Command |
|---|---|
| Vague/conceptual: "where is the X", "how does Y work" | `sinter ask "<question>" --repo <repo>` |
| Orient on a found symbol or file | `sinter show <symbol> --repo <repo>` |
| Exact/fuzzy symbol lookup | `sinter query <symbol> --repo <repo>` |
| What depends on X / blast radius | `sinter affected <symbol> --repo <repo>` |
| How does A reach B | `sinter path <A> <B> --repo <repo>` |
| What does this diff/PR affect | `sinter impact <rev-range> --repo <repo>` |

Add `--json` to `ask` for structured output. `affected`/`path` accept
`--evidence scip,import,scope` and `--certain` to restrict to stronger
evidence tiers.

## Reading results

- Every `ask` hit shows match provenance (`[name+doc 2/2 terms]`) and
  file:line with signature/doc — usually no file read is needed to answer.
- Edges carry evidence; "unresolved" is a real answer meaning no evidence
  binds the reference — say so rather than guessing.
- Symbol ambiguity returns a candidate list; pick the qualified name or
  node id and rerun rather than assuming.

## Boundaries

Symbol-level structure only. Not for doc/prose ingestion, summarization,
or content questions inside a single function body — read the file for
those. For agent clients that cannot run shell commands, `sinter serve`
exposes the same graph over MCP stdio.
