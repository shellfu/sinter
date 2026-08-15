# Decisions

One paragraph per decision, alternatives named. Costly-to-reverse or contested
decisions graduate to a full ADR.

## D1 — Storage engine: redb (Phase 1)

redb over SQLite (rusqlite) and sled. Pure Rust keeps the single static binary
trivial (no C toolchain, no bundled sqlite3), its typed tables and multimap
tables map directly onto the node table + forward/reverse adjacency we need,
and cold open + point read is microseconds, comfortably inside the 100ms R5
budget. SQLite would buy an ad-hoc relational query surface we have no consumer
for; the trigram index (Phase 5) is a multimap table either way. sled is
effectively unmaintained. Revisit if Phase 5 query shapes turn genuinely
relational.

## D2 — Record encoding: postcard

postcard (serde) for node/edge values inside redb. Compact, fast, pure Rust,
one small dependency. bincode rejected for v2 API churn; JSON stays an export
format only per R5, never the runtime format.

## D3 — Property testing: proptest

proptest over quickcheck: better strategy composition, actively maintained.

## D4 — Name: sinter

Repository is named `sinter`; kickoff working name `korpus` said rename freely.
Crates `sinter-core`/`sinter-store`/`sinter-cli`, binary `sinter`.

## D5 — In-memory Graph is BTreeMap + BTreeSet

Deterministic iteration (stable serialization and test output) and free
dedup of byte-identical edges. Neighbor lookup on the in-memory graph is a
linear scan — acceptable because all at-scale queries go through the store's
adjacency tables, never the in-memory graph.

## D6 — Extraction primitive: tree-sitter query captures, languages are data

One generic engine (`sinter-extract`) consumes a fixed capture contract —
`@def.<kind>`, `@name`, `@scope`, `@qualifier`, `@ref.<rel>`, `@import` —
and never branches on language. A language is one `LanguageSpec` row
(grammar fn, `.scm` query, comment node kinds). Adding a language = new row +
new query file + golden fixture; zero engine edits. Alternative rejected:
per-language extractor impls (the prototype's forked-resolver mistake, R9).
Qualification (nesting, impl blocks, Go receivers) is computed generically
from span containment plus the `@scope`/`@qualifier` captures.

## D7 — Phase 2 ships two languages (Rust + Go), not one

Kickoff said one language first; user directed Go + Rust. Two languages also
give the extraction abstraction its two required concrete consumers on day
one, which is what keeps the capture contract honest.

## D8 — Resolution: hand-rolled evidence resolver, not stack-graphs

Kickoff required evaluating `stack-graphs` before hand-rolling. Evaluated
2026-08-15: github/stack-graphs was archived read-only on 2025-09-09 and is
explicitly "no longer supported or updated by GitHub". Building the
resolution foundation on a dead dependency, plus authoring per-language
`.tsg` rule files ourselves (none exist for Rust/Go at production quality),
loses its advantage over a small resolver we own. Fallback: `sinter-resolve`
with three evidence tiers — SCIP index (compiler-grade, `Certain`), import
matching, same-file scope matching (both `Inferred`). Ambiguity resolves to
nothing: one candidate or unresolved, never a guess.

## D9 — No type evidence in Phase 3

Method calls through values (`x.foo()`, `self.db.begin()`) need type
inference we don't have; they stay unresolved rather than name-guessed,
which is why the measured unresolved rate sits near 90% on real repos
(sinter self-build 93.7%, skaffold 92.2%). SCIP ingest is the sanctioned
path to bind them (compilers already did the inference); a native type-
evidence tier is a Phase 7+ candidate only with a golden-corpus case for it.

## D10 — Incrementality: hand-rolled content-addressed derivation, not salsa

Kickoff offered salsa or a hand-rolled content-addressed DAG. Chose
hand-rolled: the dependency structure is two levels deep and static
(file bytes -> FileFacts -> derived tables + resolution), so salsa's dynamic
dependency tracking buys nothing for its learning and compile cost.
Mechanism: `FILE_FACTS` is per-file truth keyed by blake3; `build` diffs
stored vs current hashes, so every build (and every git hook) is
incremental by construction. Resolution invalidation: an update returns the
set of definition names it touched; `NAME_REFS` (name -> files referencing
it) maps that to the exact files to re-resolve. Measured on skaffold
(~9.2k files): no-op rebuild 73ms, one-file edit 498ms with 4 files
re-resolved (budget: <1s).

## D11 — MCP server is hand-rolled ndjson JSON-RPC

The needed MCP subset (initialize, tools/list, tools/call, ping over stdio)
is ~150 lines; an SDK dependency (rmcp) would exceed the code it replaces
and pin protocol churn. Revisit if sinter needs server-initiated features
(sampling, notifications, resources).

## D12 — Blast radius excludes Contains edges

`affected`/`path`/`impact` traverse only dependency relations (calls, uses,
imports, ...). Containment is structure, not dependency: a file does not
"depend on" the functions it contains for change-impact purposes, and
including it would make every file node a false dependent of its own
symbols.

## D13 — Phase 7 expansion validated the language-as-data claim

Python and TypeScript were added by independent agents editing only a `.scm`
query file and a golden fixture each — zero engine changes, both at P/R 1.0
first run. One engine primitive was added ahead of them
(`@import.module`/`@import.name` composition for from-style imports)
because Rust/Go path-shaped imports don't cover named imports. That is the
expected shape of future language work: if a language needs engine code,
the capture contract is wrong, not the language.

## D14 — A call binding to a type is recorded as a use

`model.ID(raw)` is syntactically a call; whether it is a conversion is
resolution-time knowledge. Extraction honestly emits `calls`; when the
binding target is a type kind (struct/enum/interface/trait/typealias) the
edge is relabeled `uses`. Class kinds are exempt — `Server()` instantiation
really is a call. Similarly, name collisions resolve by namespace: a call
prefers callables, a use prefers types; any remaining tie is unresolved.

## D15 — Local evidence tiers are language data, not engine policy

The corpus mining showed each language wants different local knowledge:
Go grants method binding through typed locals (`c *Counter` → `c.Reset()` is
`Counter::Reset`) and embedded-struct promotion; TypeScript deliberately
does not (its fixtures keep `store.save()` unresolved); Python and Rust
bind through receiver keywords (`self`). All of it is expressed as capture
data (`@local`, `@local.type`, `@embed`) plus one spec field (`receivers`)
— the resolver implements the mechanisms once and stays language-blind.
Shadowing is the same machinery: a bare `@local` in scope suppresses any
outward binding, which killed the fabricated-edge family (params, let,
loop vars, catch) in all four languages.

## D16 — Resolved tuples support full file qualification (fixed)

Golden `resolved` tuples originally carried file-unqualified names, making
same-named symbols in different files indistinguishable. The runner now
emits fully qualified `[evidence, relation, src, dst, src_file, dst_file]`
tuples and matches expected tuples by prefix, so legacy 4-tuples keep
working while collision-prone fixtures use the 6-form
(`go-same-package-xfile`'s two `init` functions are the proof case). New
fixtures must use the 6-form whenever a name repeats across files.

## D17 — Unresolved splits into internal vs external

`unresolved_internal` = evidence pointed into the corpus but binding failed
(ambiguity, missing member on a known module/type) — the resolver accuracy
gauge, reported per build. `unresolved_external` = no corpus-anchored
evidence (external imports, builtins, value-receiver calls without type
evidence) — dependency-index/SCIP territory, not resolver defects.
Shadow-suppressed references count external: no edge is the correct
outcome. Measured on skaffold: 81.3% raw unresolved decomposes into a
12.2% internal gauge (with real scip-go index) + external mass.

## D18 — Bash lands as language #5; sourcing is a glob import

Bash has no module system: a file is its module, and `source`/`.` binds
every function of the sourced file — modeled with the existing glob-import
primitive (`@import.star` alongside `@import`). `bash_absolutize` resolves
the `$(dirname "$0")/...` and `${BASH_SOURCE%/*}/` idioms against the
sourcing file's directory. `source` is isolated from ordinary commands via
tree-sitter's built-in `#any-of?` text predicate, which the Rust binding
evaluates natively — no engine change. Fixture correction recorded per the
harness rule: `$(dirname "$0")` is a real invocation of `dirname`
(command substitution executes), so it appears as a call reference.

## D19 — C++ lands as language #6; header/impl semantics forced 3 engine rules

The cpp-header-impl fixture was authored red (C++ semantics first) and
drove three generic fixes: (1) an import naming a literal repo file binds
that file exactly — `#include "player/character.h"` stays unambiguous even
though header and impl share module [player, character]; (2) `Class` joins
the member-scope kinds for typed-local/receiver lookup while staying
exempt from the call→uses relabel (instantiation is a call, D14);
(3) when a member has both an in-type declaration and an out-of-type
definition in one module, the declaration inside the type's own file is
the entity. `#include` itself is a glob import (the bash `source`
precedent — textual splice makes every top-level name visible), and
member access (`.`/`->`) splits in `cpp_absolutize` so receiver/typed-
local tiers see a prefix. TOKENS v2 (schema v3) and the ask
vendor/soft-stopword penalties landed in the same change set, each behind
its own fixture.

## D20 — Unreal C++ misparse resilience (Black Lantern findings)

tree-sitter-cpp cannot parse `class GAME_API Name : public Base` — the
export macro becomes the class name and the body degrades to statements.
Handled as data, gated on the `*_API` convention: query patterns match the
two misparse wrapper shapes and recover the real name; the misparsed body's
member declarations surface as call references that self-suppress against
the class (no wrong edges), recorded as such in the fixture. A new spec
primitive `doc_skip_kinds` lets the doc-comment walk step over decorator
macro lines (`UCLASS(...)`), so UE classes keep their doc — the content
channel `ask` depends on. The same trial exposed an ask precision bug:
trigram closeness was one global set, granting name credit for every term;
now per-term (fixture: ask_trigram_credit_is_per_term). Honest limit,
measured on the real repo: `ABLPlayerCharacterV2`'s doc never says
"controller", so evidence ranks the control-mode FSM (which says both
words) above it — the class surfaces at #9 with three methods in the top
11, versus the prototype's #45 of 51. Ranking cannot exceed its evidence;
if concept-level aggregation is ever wanted, it is a new design
(family-boost), not a constant tweak.
