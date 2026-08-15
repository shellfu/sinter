# Design: human interaction layer (`ask`, `show`, richer `query`)

Status: DESIGN ONLY — not implemented. Owner: CLI crate. Engine and store
contracts unchanged except one optional index table (§4).

## Goal

Match the prototype's one genuine human UX win — a vague question gives a usable
starting point — while fixing everything wrong with its answer: junk seeds,
flat unranked dump, duplicate labels, no content, mid-answer token cut.

Reference failure case (the prototype, real session): `"Where is the character
controller?"` returned 51 flat nodes; the actual answer
(`ABLPlayerCharacterV2`) was position 45, constructors named `Character()`
were the seeds, output truncated by token budget.

Target: same question against sinter returns the class as hit #1 with
signature, doc line, and a grouped one-screen map — in <50ms.

## Non-goals

No natural-language understanding, no LLM calls, no embeddings, no TUI, no
interactive picker (revisit only on demand). No changes to resolution or
graph semantics. The agent surface (MCP tools) is untouched; `ask` is
CLI-only sugar — agents keep the exact-symbol tools.

## 1. New verb: `sinter ask "<question>"`

### 1a. Question → terms (no NLP)

Lowercase, split on non-alphanumerics, drop a fixed stopword list (question
words + articles: where, is, the, how, does, what, which, who, a, an, of,
in, to, for, ...). Singularize trailing `s` as a second-chance variant, not
a replacement. `"Where is the character controller?"` → `[character,
controller]`. Zero terms after stopwording → print usage hint, exit 1.

Weak verbs (`work`, `use`, `code`, ...) are **soft** stopwords: dropped
only when at least one real term survives, so a symbol literally named
`work` stays askable (fixture:
`ask_drops_weak_verbs_when_real_terms_remain`).

### 1b. Term → candidate matching (per term, three channels)

1. **Name**: exact via `NAME_NODES`, fuzzy via `TRIGRAMS` (existing).
   Fuzzy matches score a flat "trigram-close" — deliberately not scaled by
   shared-gram fraction until a fixture demands it.
2. **Path**: term appears as a segment/substring of `node.file`
   (case-insensitive). Catches `Player/Traversal/` style answers where the
   concept lives in the path, not the symbol.
3. **Content**: term appears in `node.signature` or `node.doc`
   (case-insensitive word match). This is the channel the prototype could never
   have — its nodes had no content. A doc comment saying "main character
   controller" makes the class findable by concept.

### 1c. Scoring (deterministic, explainable, integer)

Per candidate node, sum over terms:

| signal | points |
|---|---|
| exact name match | 100 |
| name contains term / trigram-close | 60 |
| doc word match | 40 |
| signature word match | 30 |
| path segment match | 25 |

Exact formula (integer arithmetic, one expression — two implementations
must produce one ranking):

```
score = ⌊ (Σ signal points) × t × Kn × Pn  /  (T × Kd × Pd) ⌋  +  min(in_degree, 20)
```

where:
- **t/T — term coverage** (the anti-hairball rule): matched t of T
  distinct terms. A node matching both `character` and `controller` beats
  any single-term hit by construction. the prototype OR-ed terms; this is the
  single biggest precision fix.
- **Kn/Kd — kind prior**: class/struct/interface/trait 3/2,
  function/method 6/5, module/file 1/1, variable/field/constant 7/10.
  Vague "where is X" questions want types, not locals.
- **Pn/Pd — path penalties** (compose multiplicatively): test 1/2 when the
  file path matches test conventions (`tests/`, `_test.`, `.test.`,
  `test_`) and no query term is `test`; vendor 1/2 when a path segment is
  `vendor`/`third_party`/`node_modules` or contains `generated` (fixture:
  `ask_dampens_vendored_paths` — embedded third-party source must not
  outrank project code).
- **hub bonus** is added *after* the multiplied base (so a heavily-used
  test helper stays penalized: the cap of 20 cannot outweigh a 100-point
  base signal), from the `IN_EDGES` count per candidate.

All numerators multiply before any division — no intermediate truncation.
Ties break by (kind order, file, span.start) — output is stable run to run.

**Constants are policy under golden discipline**: every point value and
ratio above lives in one const table in the implementation, and any change
to any of them requires a fixture that motivates it — never interactive
tuning against a live repo until the ranking "looks right" (the
truth-tuning trap, harness rule applies).

### 1d. Answer shape (one screen, ranked, grouped — never a node dump)

```
$ sinter ask "where is the character controller?"

Best matches (2 terms: character, controller):

1. class ABLPlayerCharacterV2                    [name+doc 2/2 terms]
   Game/Source/.../Player/BLPlayerCharacterV2.h:24
   /// Main player character controller: movement, traversal, input routing.
   class ABLPlayERCharacterV2 : public ACharacter
   contains 31 methods · used by 12 files · extends ACharacter

2. class UBLPlayerClimbComponentV2               [name 1/2 + path 1/2]
   Game/Source/.../Traversal/BLPlayerClimbComponentV2.h:16
   class UBLPlayerClimbComponentV2 : public UActorComponent
   contains 14 methods · used by 3 files

3. class UBLPlayerLedgeComponentV2               [path 1/2]
   ...

7 more matches below cutoff · `sinter ask --limit 20` to widen

Next: sinter show ABLPlayerCharacterV2 · sinter affected ABLPlayerCharacterV2
```

Rules:
- Default top 5, `--limit N` to widen. Cutoff is rank-based, never a
  token budget; the tail is a **count**, not silently absent and never a
  mid-answer truncation warning.
- Every hit: kind + name, file:line (line derived from span), doc first
  line if present, signature (one line, middle-ellipsized past terminal
  width), and adjacency **counts** (contains / used-by / extends) from the
  store — counts, not the neighbor dump. The neighborhood expansion that
  the prototype inlined as 51 NODE lines becomes one `sinter show` away.
- Match provenance printed per hit (`[name+doc 2/2 terms]`) — the ranking
  is auditable, in the spirit of evidence-or-nothing.
- Zero hits → nearest trigram names: `no match; closest symbols:
  CharacterCtl, CharCfg, ...` (never an empty exit).
- `--json` for scripting; same objects the MCP `query` tool returns plus
  `score` and `matched` fields.
- File:line rendered as OSC 8 hyperlink when the terminal supports it
  (plain text otherwise).

## 2. New verb: `sinter show <symbol>`

The "I found it, now orient me" card — replaces the prototype's `get_node` and
its BFS dump in one bounded screen:

```
$ sinter show ABLPlayerCharacterV2

class ABLPlayerCharacterV2    Game/.../BLPlayerCharacterV2.h:24..312
  /// Main player character controller: movement, traversal, input routing.
  class ABLPlayerCharacterV2 : public ACharacter

contains (31)    TryJump, EnterHang, BeginClimb, Update, … (+27)
extends          ACharacter                              [import]
used by (12 files, 47 edges)
  Player/Traversal/BLPlayerClimbComponentV2.cpp   9 edges  [import]
  Player/Traversal/BLPlayerLedgeComponentV2.cpp   8 edges  [import]
  … (+10 files)
calls (23)       PlayAnimation, SetLoopRate, … (+21)     [scope 14 · import 9]
unresolved refs in this file: 6

Next: sinter affected ABLPlayerCharacterV2 --max-depth 3
```

Rules: every relation group capped (default 8 exemplars + count),
evidence tags shown per group, and the file's unresolved count printed —
the honesty line the prototype never had. Ambiguous symbol name (multiple
nodes) → disambiguation list (kind + file each), exit without guessing —
same contract as the resolver.

`show <file-path>` on a file node is defined: card shows contains
(top-level symbols), imports (outgoing import edges), and the file's
unresolved count.

## 3. `query` unchanged, plus shared polish

`query` stays the exact/trigram tool. Shared output helpers (span→line,
signature ellipsis, OSC 8 links, `--json`) live in one CLI module used by
query/ask/show/affected/path so all verbs render alike.

## 4. Store addition (the only non-CLI change)

Content matching (§1b channel 3) needs doc/signature words. Two options:

- **v1 (ship this): linear scan.** `all_nodes()` decode + word match at ask
  time. Fine to ~50k nodes. NOT fine at skaffold scale: that graph is
  271k nodes (vendor included) and full-node decode measures 150–400ms on
  reference hardware — v1 cannot meet <50ms there. ponytail: linear scan,
  TOKENS table (v2) required for corpora past ~50k nodes.
- **v2 (SHIPPED): `TOKENS_WORDS` multimap** `word → node id`, maintained in
  `update_files` from name subwords (camelCase/acronym/snake split), doc,
  signature, and path segments; schema v3. A recall filter only — the
  scorer re-runs its own substring logic over the candidate set, so
  over-inclusion is harmless; substrings crossing subword boundaries are
  not indexed (accepted). Measured on skaffold (271k nodes): ask went
  397ms → 66ms end-to-end, inside the <50ms query budget once process
  spawn and db open are excluded.

`ask` also wants in-degree for the hub bonus: `IN_EDGES.get(id).count()`
per candidate is a keyed read — no new index.

## 5. What this deliberately does not copy from the prototype

- No BFS neighborhood dump in answers — counts + one follow-up verb.
- No token-budget truncation — rank cutoff with a stated remainder.
- No community IDs in human output (unstable, meaningless to readers).
- No OR-matching across terms — coverage multiplier enforces AND-ish
  precision.
- No prose/LLM answer synthesis — the answer is ranked definitions with
  their own docs; the doc comment is the prose.

## 6. Acceptance (golden-style, before implementation counts as done)

- Fixture repo modeled on the Black Lantern shape (component classes +
  ctors named like the base class + a documented controller class):
  `ask "where is the character controller"` must rank the controller
  class #1; constructors must not appear in the top 5.
- `ask` on every existing golden fixture repo: top hit for each fixture's
  primary symbol name is the defining node, not a reference site.
- Determinism: two runs, byte-identical output.
- Perf gate: `ask` p50 < 50ms. Met at skaffold scale (271k nodes) by the
  TOKENS v2 index — 66ms end-to-end including process spawn/db open.
- Zero-hit and ambiguous-`show` paths covered by CLI tests.
- `--json` output includes the byte span so scripts never re-derive lines.
