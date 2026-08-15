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
  (checks `resolved` tuples `[evidence, relation, src, dst]` and
  `unresolved_count`)

Current corpus: 43 fixtures (4 basics + 39 idiom fixtures mined from the
the prototype prototype's changelog: shadowing families, alias/star/dot/relative
imports, re-export chains, receivers, embeddings, namespace collisions) —
extraction and resolution all at P/R 1.0. Both runners carry a `KNOWN_FAIL`
ratchet (currently empty): a listed fixture that starts passing fails the
suite until delisted, so the list can only shrink. Every new language or
resolution rule lands with a fixture here before it ships.

Fixture sources: mine the the prototype prototype's `tests/fixtures/` and its
changelog (~839 fix bullets, each naming a real extraction idiom — loop-var
shadowing, `source "$(dirname ...)"`, re-exports, ...) into fixtures here.
