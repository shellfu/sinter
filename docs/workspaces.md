# How To Query Multiple Repositories with Sinter Workspaces

### Introduction

A Sinter workspace connects the graphs of separate repositories so that
`ask`, `affected`, `path`, and `impact` can cross repository boundaries. Each
member keeps its own `.sinter/graph.redb`. Sinter stores the edges between
members in `.sinter-workspace/links.redb` next to the workspace manifest.

A Sinter workspace is different from a Cargo workspace or another monorepo
package layout. Build one repository graph at the root of a monorepo. Use a
Sinter workspace when the code is split across separate repository roots.

| Source layout | Sinter setup |
|---|---|
| Several crates or packages in one repository | One graph at the repository root |
| Services or libraries in separate repositories | One graph per repository, joined by a Sinter workspace |
| Unrelated repositories under one parent directory | Separate graphs; create a workspace only when their dependencies should be traversed together |

This guide creates a workspace, builds its member graphs, checks the boundary
links, and runs cross-repository queries.

## Prerequisites

- [Sinter installed](../README.md#install) and available as `sinter` on
  `PATH`.
- Two or more repositories available on the local filesystem.
- Git, when using `sinter impact` with revisions or working-tree changes.

The examples use two repositories named `auth` and `billing` under a common
`payments-system` directory:

```text
payments-system/
├── auth/
└── billing/
```

Replace those names and paths with the repositories in your system.

## Step 1 — Choosing the Graph Boundary

For a monorepo, build the graph at the repository root. The first build uses
the path supplied to Sinter as the graph root; it does not search for a Cargo
workspace or Git root.

The following command creates or refreshes one graph for every supported
source file below `your_monorepo`:

```bash
sinter ensure /path/to/your_monorepo
```

Do not create a graph per crate unless those crates are intentionally operated
as separate repositories. A repository-level graph preserves cross-crate and
cross-package dependencies.

For separate repositories, continue with a Sinter workspace. The workspace
will preserve each repository graph and add a store for boundary edges.

## Step 2 — Creating the Workspace Manifest

Run this command from the `payments-system` directory. It validates each
member directory and writes a commented manifest without changing either
repository:

```bash
sinter init --workspace ./sinter-workspace.toml --name payments --member auth=./auth --member billing=./billing
```

The generated manifest contains these active fields; comments in the generated
file also show how to add runtime links:

```toml
# sinter-workspace.toml
[workspace]
name = "payments"

[members]
auth    = "./auth"
billing = "./billing"
```

Member paths are resolved relative to the manifest directory. Absolute paths
and paths beginning with `~/` are also accepted. The member key becomes the
prefix used in workspace symbols, such as `auth:Login`.

The manifest now names the repositories that Sinter will build and query
together.

## Step 3 — Building the Workspace

This command incrementally builds every member graph and then replaces the
workspace boundary links:

```bash
sinter workspace ./sinter-workspace.toml
```

Sinter writes each member graph under that repository's `.sinter/` directory.
It writes the boundary-link store to
`./.sinter-workspace/links.redb`, relative to the manifest. Both locations are
derived state and should not be committed.

Boundary links come from two sources:

- **Import evidence:** Sinter binds a member's unresolved external references
  to definitions in other members when its imports provide enough evidence.
- **Declared evidence:** Entries in `[[links]]` record runtime coupling that
  source analysis cannot observe, such as a queue topic or a configured RPC.

Sinter leaves ambiguous references unresolved. It does not choose a target by
name alone.

The member graphs and their boundary links are now ready for workspace
queries.

## Step 4 — Searching Across Members

Workspace symbols use `member:Symbol`. A bare symbol is accepted when exactly
one member resolves it. Prefixing symbols makes commands repeatable when
several repositories use the same name.

Search every member for starting points related to payment settlement with this
command:

```bash
sinter ask "where is payment settlement handled?" --workspace ./sinter-workspace.toml
```

The ranked results include each symbol's member prefix. Replace the example
symbols in later commands with names returned from your workspace.

## Step 5 — Declaring Runtime Coupling

Static imports cannot describe every distributed-system dependency. Add a
`[[links]]` entry when an operator knows that one symbol depends on another
through configuration, messaging, or another runtime mechanism:

```toml
# sinter-workspace.toml
[[links]]
from_member = "billing"
from_symbol = "consume_settled"
to_member   = "auth"
to_symbol   = "publish_settled"
via         = "topic payments.settled"
```

`from_symbol` is the dependent and `to_symbol` is its dependency. The optional
`via` value is recorded for human context; Sinter does not interpret it.

Refresh the workspace after editing the manifest:

```bash
sinter workspace ./sinter-workspace.toml
```

Every declared symbol must resolve uniquely inside its named member. Qualify a
symbol further, such as `settlement::publish_settled`, when the short name is
ambiguous. A missing member, missing symbol, or ambiguous symbol stops the
refresh with an error instead of creating an uncertain edge.

Skip declared links for dependencies already represented by imports or
compiler evidence. The manifest should contain runtime facts that the member
graphs cannot derive.

## Step 6 — Traversing and Maintaining the Workspace

Trace callers and dependents across member boundaries by naming the member
that owns the starting symbol:

```bash
sinter affected auth:publish_settled --workspace ./sinter-workspace.toml
```

Find the declared path from the billing consumer to the authentication
publisher with this command:

```bash
sinter path billing:consume_settled auth:publish_settled --workspace ./sinter-workspace.toml
```

To include other members in a change-impact report, set `--repo` to the member
whose Git diff should be analyzed and pass the workspace manifest separately:

```bash
sinter impact HEAD~1..HEAD --repo ./auth --workspace ./sinter-workspace.toml
```

The `--repo` path for workspace impact must resolve to one of the manifest's
members. Sinter calculates changed symbols in that repository, then follows
boundary links into the others.

CLI workspace traversals read the persisted member graphs and boundary-link
store. Run `sinter workspace` after a member graph or declared link changes.
MCP tools served by `sinter serve --workspace` build the members and refresh
stale links before each tool call.

Use the workspace doctor to list missing member graphs and compare the current
member stores with the fingerprints recorded during the last boundary refresh:

```bash
sinter doctor --workspace ./sinter-workspace.toml
```

Add `--fix` when the doctor should build every member and refresh the links
before running its checks.

## Workspace Command Reference

| Command | Workspace behavior |
|---|---|
| `sinter init --workspace <manifest>` | Writes a starter manifest. Repeat `--member [name=]path` to populate `[members]`. Existing files are never overwritten. |
| `sinter workspace <manifest>` | Incrementally builds every member and refreshes inferred and declared boundary links. |
| `sinter doctor --workspace <manifest>` | Reports missing member graphs and stale boundary links. Add `--fix` to rebuild and refresh before diagnosis. |
| `sinter ask <question> --workspace <manifest>` | Searches and ranks symbols across all members. |
| `sinter affected <symbol> --workspace <manifest>` | Traverses reverse dependencies through member graphs and boundary links. Workspace mode uses human-readable output. |
| `sinter path <from> <to> --workspace <manifest>` | Finds the shortest observed path across members. Workspace mode uses human-readable output. |
| `sinter impact [rev-range] --repo <member-path> --workspace <manifest>` | Maps a member's Git changes to cross-workspace dependents and affected tests. |
| `sinter serve --workspace <manifest>` | Serves the federated scope over MCP; tools resolve `member:Symbol` handles. |

## Manifest Reference

| Field | Required | Meaning |
|---|---|---|
| `[workspace].name` | Yes | Display name and part of the workspace snapshot identity. |
| `[members].<name>` | One or more | Repository path. `<name>` becomes the workspace symbol prefix. |
| `[[links]].from_member` | Per link | Member that owns the dependent symbol. |
| `[[links]].from_symbol` | Per link | Dependent symbol, uniquely resolved inside `from_member`. |
| `[[links]].to_member` | Per link | Member that owns the dependency. |
| `[[links]].to_symbol` | Per link | Dependency symbol, uniquely resolved inside `to_member`. |
| `[[links]].via` | No | Note describing the runtime mechanism. It is stored but not interpreted. |

## Troubleshooting

### A member has no graph

Run `sinter workspace <manifest>` to build every member. To build only one
member, run `sinter build <member-path>`, followed by `sinter workspace
<manifest>` to refresh boundary links.

### Boundary links are stale

Member graphs can be refreshed by hooks or repository queries without
refreshing the workspace link store. Run `sinter workspace <manifest>`, or use
`sinter doctor --workspace <manifest> --fix` when a health report is also
needed.

### A bare symbol matches several members

Prefix it with the member name, for example `auth:Login`. If the symbol remains
ambiguous inside that repository, use its qualified name or a stable handle
reported by a repository query.

### No cross-repository path is found

The result means that Sinter did not observe a path with the available
evidence. Check unresolved references in the member repositories with `sinter
unresolved --repo <member-path>`. Generate compiler evidence with `sinter scip
<member-path>` when the missing edge depends on receiver types, re-exports, or
other compiler-resolved behavior, then refresh the workspace.

Use a declared link only when the dependency is a known runtime fact that
source or compiler evidence cannot express.

## Conclusion

The workspace now has one graph per repository and a separate store for
cross-repository edges. Re-run `sinter workspace <manifest>` whenever member
graphs or declared links change, and use `sinter doctor --workspace <manifest>`
to check freshness before relying on a cross-repository negative result.
