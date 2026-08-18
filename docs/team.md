# Teams: build the SCIP index once, in CI

## The shape

Two artifacts, two cost profiles:

- **The graph** (`.sinter/graph.redb`) is derived state. Every machine
  builds its own; a full build is seconds, an incremental sync is
  milliseconds, and the git hooks keep it current. There is nothing to
  share.
- **The SCIP index** (`.sinter/index.scip`) is compiler-grade evidence
  and costs minutes — rust-analyzer, scip-go, etc. have to typecheck the
  repo. This is the artifact worth building once and distributing.

Ingestion is automatic: `pipeline.rs` fingerprints the index (path +
mtime + size) and any `sinter build` — including the one a query
triggers — notices a changed fingerprint and re-resolves the corpus
against it. So distribution is literally "put the file at
`.sinter/index.scip`"; no import command exists because none is needed.

## The two flags

- `sinter scip --check` — exit 0 and print `index fresh` when
  `.sinter/index.scip` exists and no source file is newer than it (the
  doctor's staleness notion); exit 1 with the count of newer files when
  stale or missing. Runs no indexer. This is the CI guard.
- `sinter scip --if-stale` — run the full index + rebuild only when
  `--check` would fail; one-line no-op otherwise. Makes the CI job
  idempotent: a cache hit costs one directory walk.

## The recipe

CI builds the index per merge to main; teammates (and agents) download
it. The worked example is
[`docs/examples/scip-index.yml`](examples/scip-index.yml) — copy it into
`.github/workflows/`, swap in your language's indexer install step, done.
The job is: checkout, install sinter, install the indexer, restore
`.sinter/index.scip` from cache, `sinter scip --if-stale`, save cache and
upload the index as an artifact.

Adopting a downloaded index locally is one copy:

```sh
mkdir -p .sinter && cp ~/Downloads/index.scip .sinter/index.scip
```

That's it. The next `sinter build` (or any query, via the hooks) sees the
new fingerprint and ingests it. `sinter doctor` will confirm:
"SCIP index present and fresh".

Fetching the latest artifact from CI with `gh`:

```sh
gh run download --name scip-index --dir .sinter \
  --repo YOUR_ORG/YOUR_REPO \
  $(gh run list --workflow scip-index.yml --branch main \
      --status success --limit 1 --json databaseId -q '.[0].databaseId')
```

## Cache key guidance

Pick the key by what actually moves your index:

- **Lockfile hash + toolchain version**
  (`hashFiles('Cargo.lock') + rustc -V`) — right for dep-heavy indexes.
  The expensive part of indexing is typechecking dependencies; your own
  source changes are cheap to re-index on a miss, and `--if-stale` makes
  a stale-but-restored index a fast rebuild rather than a cold one.
- **Content hash of the source tree** (`hashFiles('**/*.rs')`) — exact:
  every merge that touches source misses and rebuilds. Costs an index
  build per merge; buys you an index that is never stale.

The honest tradeoff: the lockfile key serves slightly stale indexes
between dep bumps (newer files fall back to import/scope evidence —
sinter degrades, it doesn't break); the content key pays full price for
freshness. Most teams want the lockfile key plus the per-merge
`--if-stale` run in the example, which rebuilds on real staleness anyway.

## What is NOT shared

`graph.redb`. It is derived, machine-local, and rebuilds in seconds from
source + index; sharing it buys nothing and risks schema-version and
platform mismatches. Keep `.sinter/` gitignored; ship only `index.scip`.
