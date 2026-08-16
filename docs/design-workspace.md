# Design: cross-repo workspaces (`sinter workspace`, `--workspace`)

Status: DESIGN ONLY — not implemented. Owner: new workspace layer + CLI.
Core `NodeId`, member stores, and resolution semantics unchanged; the only
core addition is one `Evidence` variant (§4, phase 4).

## Goal

Answer distributed-system questions across N repos under the same
evidence-or-nothing rules that govern one repo: cross-repo blast radius
("what breaks in auth and billing if this function in lib/common
changes"), cross-repo paths, cross-repo PR impact.

## Non-goals

No daemon or server. No merged graph artifact — the prototype's
merge-graphs shape was instantly stale and non-incremental; rejected. No
cross-repo name-guessing: string-literal coincidence across repos is
explicitly NOT evidence. No automatic discovery of members — explicit
manifest only. No runtime-coupling inference (HTTP routes, queue topics
are declared or absent, §4).

## 1. Architecture: federation, not merger

- A workspace manifest (TOML): `[workspace]` name; `[members]`
  `name = path` entries; optional `[[links]]` declared bindings (phase 4).
- Each member repo keeps its own `.sinter/` store, built and refreshed
  exactly as standalone — unchanged, independently incremental (D10
  machinery untouched).
- A small link store next to the manifest
  (`.sinter-workspace/links.redb`) holds ONLY boundary edges:
  `(src_member, src_node) -> (dst_member, dst_node)` with
  relation/evidence/confidence. Nothing a member store already knows is
  copied out of it.
- Core `NodeId` is unchanged; the workspace layer keys by
  `(member, NodeId)` and displays as `member:path#symbol`.

Rules:
- A member store must be readable as-is by the workspace layer. Any
  change that would make a store workspace-aware is wrong by
  construction.
- The link store contains boundary edges and per-member fingerprints
  (§3) and nothing else. If it grows a second kind of content, this
  design has failed.

## 2. Cross-repo resolution: the input queue already exists

Each member store persists its unresolved references split
internal/external (D17, the `unresolved` table). Boundary resolution =
resolve each member's unresolved-**external** import/qualified references
against the OTHER members' def/module indexes, using ONLY the
import-evidence machinery `sinter-resolve` already has: module suffix
matching, item lookup, re-export chain walking.

Deliberately excluded: same-file, same-module, receiver, and typed-local
tiers (D15) — intra-repo by definition. This exclusion is also the guard
against false bindings between identically-named files in different
members. Ambiguity across members resolves to nothing, same contract as
D8: one candidate or unresolved, never a guess.

Evidence tiers across repos:

1. **Import** (`Evidence::Import`, `Inferred`) — source-level deps: Go
   module paths, Rust path/git deps, TS workspace packages, Python
   packages, matched by the existing module-suffix machinery against the
   target member's module index.
2. **Contract** — a proto/IDL repo joins as an ordinary member (proto
   language pack); services couple through the contract symbols, so
   service→proto→service paths exist with import evidence at each hop.
   No new mechanism: the contract tier is the import tier applied to a
   contract member.
3. **Declared** (`Evidence::Declared`, phase 4) — runtime coupling
   (HTTP routes, queue topics) is NOT statically inferable and is never
   guessed. The manifest's `[[links]]` entries produce edges with a
   dedicated `Evidence::Declared` kind, filterable in every query.
   Declared edges are the human's assertion, tagged as such, never
   silently mixed with inferred evidence.

## 3. Incrementality

Workspace refresh re-derives boundary edges. Staleness is detected per
member via a cheap fingerprint of the member's store; unchanged members
contribute nothing to the refresh.

A member build's `NameDelta` (`sinter-store/src/update.rs`: touched
def names + dependent files) bounds exactly what needs re-resolution —
phase-later optimization. v1 recomputes the whole boundary on any member
change, which is proportional to unresolved-**externals**, not corpus
size, so it stays cheap by the same D17 arithmetic that makes the
external set small relative to the corpus.
ponytail: full boundary recompute in v1; NameDelta-bounded re-resolution
when a fixture or measurement shows the recompute over budget.

Budget: workspace refresh <1s for typical service-repo counts after a
single member edit (matches the D10 single-repo budget).

## 4. Surface

- `sinter workspace <manifest>` builds all members (each an ordinary
  incremental `build`) then refreshes links.
- `--workspace <manifest>` flag on `affected` / `path` / `ask` /
  `impact` / `doctor`.
- `affected`/`path` traverse member store + link store as one logical
  graph; D12 still holds (Contains never traversed).
- `ask` fans out candidate gathering across members and merge-ranks
  with the existing deterministic integer formula
  (design-human-query.md §1c) — tie-break extended by member name so
  output stays byte-stable.
- `impact` takes the changed member's rev-range and follows links
  outward into the other members.
- `doctor` reports member freshness + link staleness.

Evidence tags print on every cross-member edge; `--evidence` filtering
works on `declared` like any other kind.

## 5. Phases and acceptance (fixture-first, golden discipline)

1. **Manifest + import-evidence links + affected/path.**
   Acceptance: three-mini-repo fixture (shared lib + two services); a
   change in the shared lib is reachable from both services via
   `affected --workspace`; output deterministic and evidence-tagged.
2. **ask/impact fan-out.**
   Acceptance: cross-repo `impact` of a shared-lib commit lists symbols
   in both services.
3. **Proto contract tier.**
   Acceptance: service→proto→service coupling resolves with contract
   (import-on-proto-member) evidence in the fixture.
4. **Declared links.**
   Acceptance: a manifest-declared topic binding appears as
   `Evidence::Declared` and is filterable out of query results.

Every phase: two runs byte-identical, and no cross-member edge exists
without a printed evidence kind.
