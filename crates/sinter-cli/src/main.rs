mod affected;
mod agent_protocol;
mod ask;
mod build;
mod citation;
mod context;
mod corpus;
mod coverage;
mod deps;
mod doctor;
mod freshness;
mod graph_tool;
mod grep;
mod hooks;
mod impact;
mod init;
mod install;
mod lookup;
mod map;
mod no_callers;
mod overlap;
mod pathcmd;
mod pipeline;
mod progress;
mod query;
mod render;
mod repository_tools;
mod scip;
mod serve;
mod show;
mod tool_catalog;
mod uninit;
mod unresolved;
mod update;
mod watch;
mod workspace;
mod workspace_context;
mod workspace_tools;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand};

/// sinter — code knowledge-graph engine.
#[derive(Parser)]
#[command(name = "sinter", version, about, after_help = EXIT_CODES)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Cap `--json` output at this many bytes: text fields are shortened,
    /// then trailing entries dropped (`truncated`, `totals`, `next_cursor`
    /// say what was cut). Unlimited by default — MCP defaults to 8000
    #[arg(long, global = true, value_name = "N")]
    budget_bytes: Option<usize>,
    /// Resume result lists at this offset (a previous `next_cursor`;
    /// `--cursor` is taken by `init`'s Cursor rule file)
    #[arg(long, global = true, value_name = "N", default_value_t = 0)]
    offset: usize,
}

const EXIT_CODES: &str = "Exit codes (grep-style, for read and assertion commands):
  0  found/current; assertions hold
  1  valid query with no results; assertion or document gate did not pass
  2  usage or execution error";

#[derive(Args, Default)]
struct FilterArgs {
    /// Restrict traversal to these evidence kinds (structural, scope, import, scip, dynamic)
    #[arg(long, value_delimiter = ',')]
    evidence: Vec<String>,
    /// Follow only compiler-grade (Certain) edges
    #[arg(long)]
    certain: bool,
}

/// Relation restriction, on the traversal verbs that walk edges
/// transitively (affected/path/deps) — e.g. `--relations calls,uses` to
/// keep file-level import edges out of a blast radius.
#[derive(Args)]
struct RelationsArg {
    /// Follow only these relations (calls, uses, imports, implements, extends,
    /// reads, writes, creates, alters, drops)
    #[arg(long, value_delimiter = ',')]
    relations: Vec<String>,
}

/// Repository path, accepted both as a positional and as `--repo` so
/// lifecycle commands (`sinter build`) and read commands (`sinter ask
/// --repo`) share one habit.
#[derive(Args)]
struct RepoArg {
    /// Repository path
    #[arg(default_value = ".", value_name = "REPO")]
    repo: PathBuf,
    /// Repository path (flag form; same as the positional)
    #[arg(long = "repo", value_name = "REPO", conflicts_with = "repo")]
    repo_flag: Option<PathBuf>,
}

impl RepoArg {
    fn path(&self) -> &PathBuf {
        self.repo_flag.as_ref().unwrap_or(&self.repo)
    }
}

#[derive(Subcommand)]
enum Command {
    /// Build all workspace members and refresh cross-repo boundary links
    Workspace {
        /// Path to the workspace manifest (TOML)
        manifest: PathBuf,
    },
    /// Create or refresh only the derived code graph; install no integrations
    Ensure {
        #[command(flatten)]
        repo: RepoArg,
    },
    /// Onboard a repo: build + git hooks + agent integration + MCP, then doctor
    Init {
        #[command(flatten)]
        repo: RepoArg,
        /// Also write the Cursor rule file
        #[arg(long)]
        cursor: bool,
        /// Run compiler indexers (they execute repository build scripts)
        #[arg(long, overrides_with = "no_scip")]
        scip: bool,
        /// Skip compiler indexers without prompting
        #[arg(long)]
        no_scip: bool,
        /// Also install the skill card and enforcement hooks machine-wide (~/.claude)
        #[arg(short = 'g', long)]
        global: bool,
        /// Skip the confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
        /// Write a starter workspace manifest instead (path defaults to ws.toml)
        #[arg(long, value_name = "MANIFEST")]
        workspace: Option<Option<PathBuf>>,
        /// Workspace name for --workspace (defaults to manifest's parent dir name)
        #[arg(long, requires = "workspace")]
        name: Option<String>,
        /// Member repos for --workspace: `[name=]path`, repeatable
        /// (name defaults to the repo's directory name)
        #[arg(
            short = 'm',
            long = "member",
            requires = "workspace",
            value_name = "[NAME=]PATH"
        )]
        members: Vec<String>,
    },
    /// Offboard a repo: remove the graph and every sinter-managed artifact
    Uninit {
        #[command(flatten)]
        repo: RepoArg,
        /// Also remove the global skill card and ~/.claude enforcement hooks
        #[arg(short = 'g', long)]
        global: bool,
    },
    /// Build or incrementally refresh the graph for a repository
    Build {
        #[command(flatten)]
        repo: RepoArg,
    },
    /// Watch a repository and keep the graph fresh
    Watch {
        #[command(flatten)]
        repo: RepoArg,
    },
    /// Diagnose the installation and a repo's graph; every finding names its fix
    Doctor {
        #[command(flatten)]
        repo: RepoArg,
        /// Diagnose a workspace instead (path to manifest)
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Repair what doctor can: refresh installed cards/hooks, rebuild
        /// the graph. Never makes new opt-in decisions.
        #[arg(long, short = 'f')]
        fix: bool,
        /// Emit findings as JSON: {version, graph: [...], integration: [...], summary}
        #[arg(long)]
        json: bool,
    },
    /// Install the assistant integration card (embedded, drift-proof)
    Install {
        /// Targets: claude (global skill), cursor (.cursor/rules),
        /// agents (AGENTS.md managed block: Codex/Gemini/etc),
        /// enforce (Claude Code hooks: sinter-first routing), all —
        /// e.g. `sinter install enforce`
        #[arg(value_delimiter = ',', default_value = "claude")]
        targets: Vec<String>,
        /// Deprecated alias for the positional targets
        #[arg(
            long = "for",
            value_delimiter = ',',
            hide = true,
            conflicts_with = "targets"
        )]
        for_targets: Option<Vec<String>>,
        /// Claude skill directory override (default: ~/.claude/skills/sinter)
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Also register the MCP server in the repo's project-scope .mcp.json
        #[arg(long)]
        mcp: bool,
        /// Repo for cursor/agents/enforce/--mcp targets
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Install enforcement hooks globally (~/.claude) instead of in the repo
        #[arg(short = 'g', long)]
        global: bool,
        /// Strict enforcement (enforce target only): the first raw
        /// recursive search of a session is blocked with a sinter
        /// redirect; retries pass with a nudge
        #[arg(long)]
        strict: bool,
    },
    /// Install git hooks that refresh the graph after commits/checkouts
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
    /// Ask a vague question, get a ranked, content-bearing starting point
    Ask {
        /// Natural-language question ("where is auth handled")
        question: String,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Traverse across the workspace (path to manifest)
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Maximum hits to print
        #[arg(long, default_value_t = 5)]
        limit: usize,
        /// Corpus roles to search (comma-separated, or `all`)
        #[arg(long, value_delimiter = ',', default_value = corpus::ASK_DEFAULT_SCOPE)]
        scope: Vec<String>,
        /// Compact `sinter.agent.v1` data (MCP `structuredContent.data`)
        #[arg(long)]
        json: bool,
        /// Include per-hit scoring diagnostics in JSON output
        #[arg(long, requires = "json")]
        explain: bool,
    },
    /// Evidence packet for a coding task: edit candidates, direct
    /// deps/dependents, relevant tests, gaps, next commands
    Context {
        /// Task description ("cap every agent JSON response at 8 KB")
        task: String,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Build one packet across a declared multi-repository workspace
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Compact `sinter.agent.v1` data (MCP `structuredContent.data`)
        #[arg(long)]
        json: bool,
    },
    /// Evaluate a bounded structural assertion against the current graph
    Assert {
        #[command(subcommand)]
        assertion: Assertion,
    },
    /// Emit a durable Markdown citation for a symbol's current location
    Cite {
        /// Symbol: stable key, name, qualified suffix, or name@file
        symbol: String,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Structured output including symbol identity and snapshot
        #[arg(long)]
        json: bool,
        /// Fail if the graph changed since this snapshot token was returned
        #[arg(long)]
        if_snapshot: Option<String>,
    },
    /// Check managed citations and bare path/line references in Markdown
    VerifyDoc {
        /// Markdown document, relative to the repository root
        document: PathBuf,
        /// Repository whose graph and source files citations address
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Structured per-citation results
        #[arg(long)]
        json: bool,
        /// Fail if the graph changed since this snapshot token was returned
        #[arg(long)]
        if_snapshot: Option<String>,
    },
    /// One-screen orientation card for a symbol or file
    Show {
        /// Symbol: name, qualified suffix (`Config::new`), or node id
        symbol: String,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Maximum rows per relation group (the rest collapse to `… (+N)`)
        #[arg(long, default_value_t = show::DEFAULT_LIMIT)]
        limit: usize,
        /// Corpus roles the far end of an edge may be in (comma-separated,
        /// or `all`)
        #[arg(long, value_delimiter = ',', default_value = corpus::DEFAULT_SCOPE)]
        scope: Vec<String>,
        /// Compact `sinter.agent.v1` data (MCP `structuredContent.data`)
        #[arg(long)]
        json: bool,
        /// Fail if the graph changed since this snapshot token was returned
        #[arg(long)]
        if_snapshot: Option<String>,
        /// Include a bounded source excerpt for the symbol
        #[arg(long)]
        body: bool,
        /// Lines of the excerpt to print with --body
        #[arg(long, default_value_t = show::DEFAULT_BODY_LINES, requires = "body")]
        context_lines: usize,
        #[command(flatten)]
        relations: RelationsArg,
    },
    /// Find symbols by name or fragment (exact + trigram); for relations
    /// use `show`, `affected`, `deps`, `path`
    Query {
        /// Symbol name, qualified suffix, fuzzy fragment, or glob with one `*`
        /// (`Type::*`, `*::method`, `prefix*`)
        symbol: String,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Maximum results to print
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Corpus roles to search (comma-separated, or `all`)
        #[arg(long, value_delimiter = ',', default_value = corpus::DEFAULT_SCOPE)]
        scope: Vec<String>,
        /// Compact `sinter.agent.v1` data (MCP `structuredContent.data`)
        #[arg(long)]
        json: bool,
        /// Fail if the graph changed since this snapshot token was returned
        #[arg(long)]
        if_snapshot: Option<String>,
    },
    /// Reverse blast radius of a symbol
    Affected {
        /// Symbols: name, qualified suffix, or node id. Repeatable; results
        /// are unioned and deduplicated, each row naming the seeds that
        /// reached it.
        #[arg(required = true, num_args = 1.., value_name = "SYMBOL")]
        symbols: Vec<String>,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Traverse across the workspace (path to manifest)
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Maximum traversal depth (1 = direct dependents only)
        #[arg(long, visible_alias = "depth", default_value_t = 10)]
        max_depth: usize,
        /// Maximum dependents to print
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Corpus roles traversal may enter (comma-separated, or `all`).
        /// Symbol lookup prefers production when a name is ambiguous;
        /// pass an explicit scope or `name@file` to pick another copy
        #[arg(long, value_delimiter = ',', default_value = corpus::DEFAULT_SCOPE)]
        scope: Vec<String>,
        /// Compact `sinter.agent.v1` data (MCP `structuredContent.data`; not
        /// available with --workspace)
        #[arg(long, conflicts_with = "workspace")]
        json: bool,
        /// Fail if the repository/workspace graph changed since this token
        #[arg(long)]
        if_snapshot: Option<String>,
        #[command(flatten)]
        filter: FilterArgs,
        #[command(flatten)]
        relations: RelationsArg,
    },
    /// Forward blast radius: everything a symbol transitively depends on
    Deps {
        /// Symbol: name, qualified suffix, or node id
        symbol: String,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Traverse across the workspace (path to manifest)
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Maximum traversal depth
        #[arg(long, default_value_t = 10)]
        max_depth: usize,
        /// Maximum dependencies to print
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Corpus roles traversal may enter (comma-separated, or `all`).
        /// Symbol lookup prefers production when a name is ambiguous;
        /// pass an explicit scope or `name@file` to pick another copy
        #[arg(long, value_delimiter = ',', default_value = corpus::DEFAULT_SCOPE)]
        scope: Vec<String>,
        /// Compact `sinter.agent.v1` data (MCP `structuredContent.data`; not
        /// available with --workspace)
        #[arg(long, conflicts_with = "workspace")]
        json: bool,
        /// Fail if the repository/workspace graph changed since this token
        #[arg(long)]
        if_snapshot: Option<String>,
        #[command(flatten)]
        filter: FilterArgs,
        #[command(flatten)]
        relations: RelationsArg,
    },
    /// List unresolved references — the graph's honest gaps
    Unresolved {
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Only references in this repo-relative file
        #[arg(long)]
        file: Option<String>,
        /// Only references whose name ends at this name
        #[arg(long)]
        name: Option<String>,
        /// Maximum references to print
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Also list likely-external and unsupported-syntax references
        #[arg(long)]
        all: bool,
        /// Structured output
        #[arg(long)]
        json: bool,
    },
    /// How one symbol reaches another
    Path {
        /// Source symbol
        from: String,
        /// Destination symbol
        to: String,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Traverse across the workspace (path to manifest)
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Corpus roles traversal may enter (comma-separated, or `all`).
        /// Symbol lookup prefers production when a name is ambiguous;
        /// pass an explicit scope or `name@file` to pick another copy
        #[arg(long, value_delimiter = ',', default_value = corpus::DEFAULT_SCOPE)]
        scope: Vec<String>,
        /// Compact `sinter.agent.v1` data (MCP `structuredContent.data`; not
        /// available with --workspace)
        #[arg(long, conflicts_with = "workspace")]
        json: bool,
        /// Fail if the repository/workspace graph changed since this token
        #[arg(long)]
        if_snapshot: Option<String>,
        #[command(flatten)]
        filter: FilterArgs,
        #[command(flatten)]
        relations: RelationsArg,
    },
    /// Changed symbols, blast radius, and affected tests for a rev range
    /// or, with no range, for the uncommitted working tree
    Impact {
        /// Git rev range (e.g. HEAD~1..HEAD, main...branch). Omitted (or a
        /// single rev such as `HEAD`): diff the working tree against it,
        /// including untracked non-ignored files
        rev_range: Option<String>,
        /// Diff the index (staged changes) against HEAD instead of the
        /// working tree; untracked files are not included
        #[arg(long, conflicts_with = "rev_range")]
        staged: bool,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Traverse across the workspace (path to manifest)
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Maximum entries returned independently for changed symbols, blast
        /// radius, and affected tests; 0 returns all entries
        #[arg(long, default_value_t = impact::DEFAULT_LIMIT)]
        limit: usize,
        /// Compact `sinter.agent.v1` data (MCP `structuredContent.data`)
        #[arg(long)]
        json: bool,
        /// Refactor completeness: report direct dependents of these symbols
        /// that the diff did NOT touch. Repeatable.
        #[arg(long = "expect", value_name = "SYMBOL")]
        expect: Vec<String>,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Text search bounded by a graph traversal: `sinter grep '<pat>'
    /// --within 'affected(Decision)'`
    Grep {
        /// Regular expression to search for
        pattern: String,
        /// Traversal that bounds the search: `affected(SYM)`, `deps(SYM)`,
        /// `file(PATH)`. Repeatable; the bounded file sets are unioned.
        #[arg(long = "within", required = true, value_name = "TRAVERSAL")]
        within: Vec<String>,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Maximum traversal depth for affected()/deps()
        #[arg(long, default_value_t = 10)]
        max_depth: usize,
        /// Maximum matches to print
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Corpus roles traversal may enter (comma-separated, or `all`)
        #[arg(long, value_delimiter = ',', default_value = corpus::DEFAULT_SCOPE)]
        scope: Vec<String>,
        /// Compact `sinter.agent.v1` data (MCP `structuredContent.data`)
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        filter: FilterArgs,
        #[command(flatten)]
        relations: RelationsArg,
    },
    /// Map several in-flight changes (open PRs) onto the graph and rank
    /// pairwise merge risk (direct/radius/file tiers)
    Overlap {
        /// Two or more rev-ranges, optionally labeled: `pr-12=main...branch`
        ranges: Vec<String>,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Structured output
        #[arg(long)]
        json: bool,
    },
    /// MCP server over stdio
    Serve {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Serve a whole workspace instead (path to manifest); tools then
        /// resolve `member:Symbol` and traverse across repositories
        #[arg(long, conflicts_with = "repo")]
        workspace: Option<PathBuf>,
    },
    /// Run the repo's SCIP indexer and rebuild with compiler-grade
    /// evidence. Idempotent: a fresh index is a one-line no-op.
    Scip {
        #[command(subcommand)]
        action: Option<ScipAction>,
        #[command(flatten)]
        repo: RepoArg,
        /// Reindex even when the index is fresh
        #[arg(long)]
        force: bool,
    },
    /// Structural repo inventory: modules, dependency hubs, docs, graph health
    Map {
        #[command(flatten)]
        repo: RepoArg,
        /// Corpus roles to include (comma-separated, or `all`)
        #[arg(long, value_delimiter = ',', default_value = corpus::DEFAULT_SCOPE)]
        scope: Vec<String>,
        /// Structured output
        #[arg(long)]
        json: bool,
    },
    /// Download and install the latest release over this binary
    Update {
        /// Report what would happen without downloading anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Print version, graph schema, and language packs (for bug reports)
    Version,
    /// Shell completions: `source <(sinter completion bash)`
    Completion { shell: clap_complete::Shell },
}

#[derive(Subcommand)]
enum ScipAction {
    /// Exit 0 iff the index exists and no source file is newer
    /// (CI guard; runs no indexer). Uses the `sinter scip` repo argument.
    Check,
}

#[derive(Subcommand)]
enum Assertion {
    /// Assert that a symbol has no observed depth-one call edges in scope
    NoCallers {
        /// Symbol: stable key, name, qualified suffix, or name@file
        symbol: String,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Assert across a declared multi-repository workspace
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Corpus roles callers may be in (production by default)
        #[arg(long, value_delimiter = ',', default_value = "production")]
        scope: Vec<String>,
        /// Count only compiler-grade (Certain) call edges
        #[arg(long)]
        certain: bool,
        /// Maximum caller rows to print (the decision always counts all)
        #[arg(long, default_value_t = no_callers::DEFAULT_LIMIT)]
        limit: usize,
        /// Compact `sinter.agent.v1` assertion data
        #[arg(long)]
        json: bool,
        /// Fail if the repository/workspace graph changed since this token
        #[arg(long)]
        if_snapshot: Option<String>,
    },
}

#[derive(Subcommand)]
enum HooksAction {
    /// Write post-commit/post-checkout/post-merge hooks
    Install {
        #[command(flatten)]
        repo: RepoArg,
    },
}

/// --evidence/--certain/--relations flags to the traversal's EdgeFilter.
fn traversal_filter(
    filter: &FilterArgs,
    relations: &RelationsArg,
    scope: &[String],
) -> anyhow::Result<sinter_store::EdgeFilter> {
    let mut f = lookup::edge_filter(&filter.evidence, filter.certain)?;
    f.relations = lookup::relation_set(&relations.relations)?;
    let selection = corpus::ScopeSelection::parse(scope, corpus::ScopeSelection::all())?;
    if !selection.is_all() {
        f.scopes = Some(selection.as_set());
    }
    Ok(f)
}

/// Grep-style exit for read commands: 0 found results, 1 valid query with
/// no results, 2 usage or execution error (clap's own errors are 2).
fn grep_exit(result: anyhow::Result<bool>) -> ExitCode {
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("sinter: {e:#}");
            if e.is::<lookup::NoMatch>() {
                ExitCode::FAILURE
            } else {
                ExitCode::from(2)
            }
        }
    }
}

/// JSON mode must never mix machine data with prose diagnostics. Successful
/// commands already wrote their compact payload; failures use the same
/// versioned outcome contract carried in MCP JSON-RPC `error.data`.
fn grep_exit_json(operation: &str, result: anyhow::Result<bool>) -> ExitCode {
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            let no_match = error.is::<lookup::NoMatch>();
            let failure = agent_protocol::failure(operation, &error);
            if let Err(write_error) = agent_protocol::write_json(&failure) {
                eprintln!("sinter: failed to encode {operation} error: {write_error:#}");
                return ExitCode::from(2);
            }
            if no_match {
                ExitCode::FAILURE
            } else {
                ExitCode::from(2)
            }
        }
    }
}

fn exit_json(operation: &str, result: anyhow::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let failure = agent_protocol::failure(operation, &error);
            if let Err(write_error) = agent_protocol::write_json(&failure) {
                eprintln!("sinter: failed to encode {operation} error: {write_error:#}");
            }
            ExitCode::from(2)
        }
    }
}

fn main() -> ExitCode {
    // Die quietly when the read end of a pipe closes (`sinter ask | head`).
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    // `context` is the one CLI verb with a default budget: its packet is
    // agent-facing by definition (it exists to be pasted into a context
    // window), so it gets the MCP default unless `--budget-bytes` says
    // otherwise (0 = unlimited).
    let default_budget = matches!(cli.command, Command::Context { .. })
        .then_some(agent_protocol::MCP_DEFAULT_BUDGET_BYTES);
    agent_protocol::set_cli_budget(agent_protocol::Budget {
        bytes: cli.budget_bytes.or(default_budget).filter(|&n| n > 0),
        cursor: cli.offset,
    });
    // Stale-artifact nudge on maintenance commands only: one stderr line
    // per installed artifact that differs from this binary's embedded
    // copy. Query verbs stay clean — agents read stderr with stdout, and
    // a nag beside every answer obscures the answer. `doctor` owns the
    // full diagnosis.
    if matches!(
        cli.command,
        Command::Init { .. }
            | Command::Build { .. }
            | Command::Watch { .. }
            | Command::Hooks { .. }
            | Command::Scip { .. }
            | Command::Workspace { .. }
            | Command::Version
    ) && let Ok(cwd) = std::env::current_dir()
        && update::nudge_due()
    {
        let mut nudged = false;
        for warning in install::stale_artifacts(&pipeline::discover_root(&cwd)) {
            eprintln!("sinter: {warning}");
            nudged = true;
        }
        // Cache read only — the network check lives in `doctor`.
        if let Some(latest) = update::cached_newer() {
            eprintln!(
                "sinter: {latest} is available (running {}) — run `sinter update`",
                env!("CARGO_PKG_VERSION")
            );
            nudged = true;
        }
        if nudged {
            update::mark_nudged();
        }
    }
    let result = match cli.command {
        Command::Workspace { manifest } => workspace::run(&manifest),
        Command::Ensure { repo } => init::ensure(repo.path()),
        Command::Uninit { repo, global } => uninit::run(repo.path(), global).map(|_| ()),
        Command::Init {
            repo,
            cursor,
            scip,
            no_scip,
            global,
            yes,
            workspace,
            name,
            members,
        } => {
            if let Some(manifest) = workspace {
                let path = manifest.unwrap_or_else(|| PathBuf::from("ws.toml"));
                let ws_name = name.unwrap_or_else(|| {
                    path.canonicalize()
                        .ok()
                        .and_then(|p| {
                            p.parent()
                                .and_then(|d| d.file_name())
                                .map(|n| n.to_string_lossy().into_owned())
                        })
                        .or_else(|| {
                            std::env::current_dir().ok().and_then(|d| {
                                d.file_name().map(|n| n.to_string_lossy().into_owned())
                            })
                        })
                        .unwrap_or_else(|| "workspace".to_string())
                });
                return match init::run_workspace(&path, &ws_name, &members) {
                    Ok(true) => ExitCode::SUCCESS,
                    Ok(false) => ExitCode::FAILURE,
                    Err(e) => {
                        eprintln!("error: {e:#}");
                        ExitCode::FAILURE
                    }
                };
            }
            let scip_consent = if scip {
                Some(true)
            } else if no_scip {
                Some(false)
            } else {
                None
            };
            return match init::run(repo.path(), cursor, scip_consent, global, yes) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(e) => {
                    eprintln!("sinter: {e:#}");
                    ExitCode::FAILURE
                }
            };
        }
        Command::Build { repo } => build::run(repo.path()),
        Command::Watch { repo } => watch::run(repo.path()),
        Command::Doctor {
            repo,
            workspace,
            fix,
            json,
        } => {
            let result = match workspace {
                Some(manifest) => doctor::run_workspace(&manifest, fix, json),
                None => doctor::run(repo.path(), fix, json),
            };
            return match result {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(e) => {
                    eprintln!("sinter: {e:#}");
                    ExitCode::FAILURE
                }
            };
        }
        Command::Install {
            targets,
            for_targets,
            dir,
            mcp,
            repo,
            global,
            strict,
        } => install::run_targets(
            &for_targets.unwrap_or(targets),
            dir,
            mcp,
            &repo,
            global,
            strict,
        ),
        Command::Hooks {
            action: HooksAction::Install { repo },
        } => hooks::install(repo.path()),
        Command::Ask {
            question,
            repo,
            workspace,
            limit,
            scope,
            json,
            explain,
        } => {
            let result =
                corpus::ScopeSelection::parse(&scope, corpus::ScopeSelection::ask_default())
                    .and_then(|scopes| match workspace {
                        Some(manifest) => {
                            ask::run_workspace(&manifest, &question, limit, json, explain, &scopes)
                        }
                        None => ask::run(&repo, &question, limit, json, explain, &scopes),
                    });
            return if json {
                grep_exit_json("ask", result)
            } else {
                grep_exit(result)
            };
        }
        Command::Context {
            task,
            repo,
            workspace,
            json,
        } => {
            let result = match workspace {
                Some(manifest) => workspace_context::run(&manifest, &task, json),
                None => context::run(&repo, &task, json),
            };
            return if json {
                grep_exit_json("context", result)
            } else {
                grep_exit(result)
            };
        }
        Command::Assert { assertion } => match assertion {
            Assertion::NoCallers {
                symbol,
                repo,
                workspace,
                scope,
                certain,
                limit,
                json,
                if_snapshot,
            } => {
                let result =
                    corpus::ScopeSelection::parse(&scope, corpus::ScopeSelection::agent_default())
                        .and_then(|selection| match workspace {
                            Some(manifest) => no_callers::run_workspace(
                                &manifest,
                                &symbol,
                                selection.as_set(),
                                certain,
                                limit,
                                json,
                                if_snapshot.as_deref(),
                            ),
                            None => no_callers::run_repository(
                                &repo,
                                &symbol,
                                selection.as_set(),
                                certain,
                                limit,
                                json,
                                if_snapshot.as_deref(),
                            ),
                        });
                return if json {
                    grep_exit_json("assert_no_callers", result)
                } else {
                    grep_exit(result)
                };
            }
        },
        Command::Cite {
            symbol,
            repo,
            json,
            if_snapshot,
        } => {
            let result = citation::run_cite(&repo, &symbol, json, if_snapshot.as_deref());
            return if json {
                grep_exit_json("cite", result)
            } else {
                grep_exit(result)
            };
        }
        Command::VerifyDoc {
            document,
            repo,
            json,
            if_snapshot,
        } => {
            let result = citation::run_verify(&repo, &document, json, if_snapshot.as_deref());
            return if json {
                grep_exit_json("verify_doc", result)
            } else {
                grep_exit(result)
            };
        }
        Command::Show {
            symbol,
            repo,
            limit,
            scope,
            json,
            if_snapshot,
            body,
            context_lines,
            relations,
        } => {
            let excerpt = body.then_some(context_lines);
            let result =
                traversal_filter(&FilterArgs::default(), &relations, &scope).and_then(|f| {
                    show::run(
                        &repo,
                        &symbol,
                        &f,
                        limit,
                        json,
                        if_snapshot.as_deref(),
                        excerpt,
                    )
                });
            return if json {
                grep_exit_json("show", result)
            } else {
                grep_exit(result)
            };
        }
        Command::Query {
            symbol,
            repo,
            limit,
            scope,
            json,
            if_snapshot,
        } => {
            let result = corpus::ScopeSelection::parse(&scope, corpus::ScopeSelection::all())
                .and_then(|scopes| {
                    query::run(&repo, &symbol, limit, json, if_snapshot.as_deref(), &scopes)
                });
            return if json {
                grep_exit_json("query", result)
            } else {
                grep_exit(result)
            };
        }
        Command::Affected {
            symbols,
            repo,
            workspace,
            max_depth,
            limit,
            scope,
            json,
            if_snapshot,
            filter,
            relations,
        } => {
            let result =
                traversal_filter(&filter, &relations, &scope).and_then(|f| match workspace {
                    Some(manifest) => affected::run_workspace(
                        &manifest,
                        &symbols,
                        &f,
                        max_depth,
                        limit,
                        if_snapshot.as_deref(),
                    ),
                    None => affected::run(
                        &repo,
                        &symbols,
                        &f,
                        max_depth,
                        limit,
                        json,
                        if_snapshot.as_deref(),
                    ),
                });
            return if json {
                grep_exit_json("affected", result)
            } else {
                grep_exit(result)
            };
        }
        Command::Deps {
            symbol,
            repo,
            workspace,
            max_depth,
            limit,
            scope,
            json,
            if_snapshot,
            filter,
            relations,
        } => {
            let result =
                traversal_filter(&filter, &relations, &scope).and_then(|f| match workspace {
                    Some(manifest) => deps::run_workspace(
                        &manifest,
                        &symbol,
                        &f,
                        max_depth,
                        limit,
                        if_snapshot.as_deref(),
                    ),
                    None => deps::run(
                        &repo,
                        &symbol,
                        &f,
                        max_depth,
                        limit,
                        json,
                        if_snapshot.as_deref(),
                    ),
                });
            return if json {
                grep_exit_json("deps", result)
            } else {
                grep_exit(result)
            };
        }
        Command::Unresolved {
            repo,
            file,
            name,
            limit,
            all,
            json,
        } => {
            let result = unresolved::run(&repo, file.as_deref(), name.as_deref(), limit, all, json);
            return if json {
                grep_exit_json("unresolved", result)
            } else {
                grep_exit(result)
            };
        }
        Command::Path {
            from,
            to,
            repo,
            workspace,
            scope,
            json,
            if_snapshot,
            filter,
            relations,
        } => {
            let result =
                traversal_filter(&filter, &relations, &scope).and_then(|f| match workspace {
                    Some(manifest) => {
                        pathcmd::run_workspace(&manifest, &from, &to, &f, if_snapshot.as_deref())
                    }
                    None => pathcmd::run(&repo, &from, &to, &f, json, if_snapshot.as_deref()),
                });
            return if json {
                grep_exit_json("path", result)
            } else {
                grep_exit(result)
            };
        }
        Command::Grep {
            pattern,
            within,
            repo,
            max_depth,
            limit,
            scope,
            json,
            filter,
            relations,
        } => {
            let result = traversal_filter(&filter, &relations, &scope)
                .and_then(|f| grep::run(&repo, &pattern, &within, &f, max_depth, limit, json));
            return if json {
                grep_exit_json("grep", result)
            } else {
                grep_exit(result)
            };
        }
        Command::Impact {
            rev_range,
            staged,
            repo,
            workspace,
            limit,
            json,
            expect,
            filter,
        } => {
            let result = impact::run(
                &repo,
                rev_range.as_deref(),
                staged,
                workspace.as_deref(),
                &expect,
                &filter.evidence,
                filter.certain,
                limit,
                json,
            );
            if json {
                return exit_json("impact", result);
            }
            result
        }
        Command::Overlap { ranges, repo, json } => {
            let result = overlap::run(&repo, &ranges, json);
            if json {
                return exit_json("overlap", result);
            }
            result
        }
        Command::Serve { repo, workspace } => match workspace {
            Some(manifest) => serve::run_workspace(&manifest),
            None => serve::run(&repo),
        },
        Command::Scip {
            action,
            repo,
            force,
        } => match action {
            Some(ScipAction::Check) => scip::check(repo.path()),
            None if force => scip::run(repo.path()),
            None => scip::run_if_stale(repo.path()),
        },
        Command::Map { repo, scope, json } => {
            let result =
                corpus::ScopeSelection::parse(&scope, corpus::ScopeSelection::agent_default())
                    .and_then(|scopes| map::run(repo.path(), json, &scopes));
            if json {
                return exit_json("map", result);
            }
            result
        }
        Command::Update { dry_run } => update::run(dry_run),
        Command::Completion { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "sinter", &mut std::io::stdout());
            Ok(())
        }
        Command::Version => {
            let languages: Vec<&str> = sinter_extract::LANGUAGES.iter().map(|l| l.name).collect();
            println!(
                "sinter {} (graph schema v{}, languages: {})",
                env!("CARGO_PKG_VERSION"),
                sinter_store::Store::CURRENT_SCHEMA,
                languages.join(", ")
            );
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sinter: {e:#}");
            ExitCode::FAILURE
        }
    }
}
