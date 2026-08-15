# Golden corpus (accuracy harness) — live since Phase 2

Accuracy is a measured number (R7). CI computes precision and recall of
extraction against hand-verified expectations; any change that moves the
metric fails CI with the delta (missing/extra tuples) printed.

Layout:

```
harness/golden/
  fixtures/<name>/        # small hand-verified source (one language each)
    ...source files
    expected.json         # {nodes: [[kind, qualified, file]],
                          #  contains: [[src, dst]],
                          #  references: [[relation, name]]}
```

Runners:
- extraction: `cargo test -p sinter-extract --test golden -- --nocapture`
- resolution: `cargo test -p sinter-resolve --test golden_resolution -- --nocapture`
  (checks `resolved` tuples and `unresolved_count`; tuples are
  `[evidence, relation, src, dst]` or the fully qualified
  `[evidence, relation, src, dst, src_file, dst_file]` — use the long form
  whenever same-named symbols exist in different files)

Rule: `expected.json` is derived from language semantics, never from engine
output. Correcting an expectation requires a written semantics argument
recorded in `DECISIONS.md` (see D14) — a failing test is never by itself a
reason to edit a fixture.

Current corpus: 49 fixtures across six languages (basics plus idiom
fixtures mined from the prototype's changelog: shadowing families,
alias/star/dot/relative imports, re-export chains, receivers, embeddings,
namespace collisions, macro misparse resilience) —
extraction and resolution all at P/R 1.0. Both runners carry a `KNOWN_FAIL`
ratchet (currently empty): a listed fixture that starts passing fails the
suite until delisted, so the list can only shrink. Every new language or
resolution rule lands with a fixture here before it ships.

Fixture sources: mine the prototype's `tests/fixtures/` and its
changelog (~839 fix bullets, each naming a real extraction idiom — loop-var
shadowing, `source "$(dirname ...)"`, re-exports, ...) into fixtures here.
