# Sinter

Sinter is a local code graph for coding agents. It indexes symbol relationships
and cites the source evidence behind every edge. The graph stays current as the
repository changes.

The installed command is `sinter`. It can run as a CLI or a local MCP server.

## Install

Install a prebuilt wheel with `uv`:

```sh
uv tool install sinter-io
```

You can also install from crates.io or Homebrew:

```sh
cargo install sinter-io
brew install shellfu/tap/sinter
```

## Connect a repository

Run `init` from the repository you want Sinter to index:

```sh
sinter init .
```

Before writing anything, `init` prints its plan and asks for confirmation. It
creates the local graph and registers the MCP server for supported coding
agents. Compiler indexers require separate consent because they can execute
repository build scripts.

Use `ensure` when you only want the derived graph and no client configuration:

```sh
sinter ensure .
```

## Query the graph

These commands answer common structural questions:

```sh
sinter map
sinter show MySymbol
sinter affected MySymbol
sinter impact HEAD~1..HEAD
```

See [getsinter.io](https://getsinter.io) for a terminal demo and measured
results. The site also documents the graph's limits. The complete command
reference is available from `sinter --help`.

<!-- mcp-name: io.github.shellfu/sinter -->
