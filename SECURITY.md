# Security

## Supported versions

Only the latest released version receives fixes. The graph database
(`.sinter/`) is derived state; deleting it and rebuilding is always safe.

## What sinter does and does not do

- **No network access.** Build, query, watch, and serve read the local
  filesystem only. `sinter scip` executes local indexer binaries
  (rust-analyzer, scip-go, scip-typescript, scip-python, scip-clang) found
  on PATH; whether those tools access the network is governed by the tools
  themselves.
- **Executes nothing from the repository.** Source files are parsed
  (tree-sitter) and hashed, never evaluated.
- **Writes** are confined to `.sinter/` inside the repo, the git hooks the
  user installs explicitly (`sinter hooks install` / `sinter init`), and
  the agent integration files `sinter install` documents.
- **MCP serve** is stdio-only; it binds no sockets.

## Untrusted repositories

Parsing is the attack surface: tree-sitter grammars and the SCIP protobuf
decoder process repo-controlled bytes. Treat indexing a hostile repo like
running any parser over it. `sinter scip` additionally runs language
toolchains, which on hostile repos can execute build scripts — do not run
it on repositories you would not build.

## Dependencies

CI runs `cargo audit` as a blocking job; a known-vulnerable dependency
fails the build.

## Reporting

Open a GitHub security advisory or issue on this repository.
