mod affected;
mod ask;
mod build;
mod corpus;
mod coverage;
mod deps;
mod doctor;
mod freshness;
mod hooks;
mod impact;
mod init;
mod install;
mod lookup;
mod map;
mod overlap;
mod pathcmd;
mod pipeline;
mod query;
mod render;
mod scip;
mod serve;
mod show;
mod uninit;
mod unresolved;
mod update;
mod watch;
mod workspace;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand};

/// sinter — code knowledge-graph engine.
#[derive(Parser)]
#[command(name = "sinter", version, about, after_help = EXIT_CODES)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

const EXIT_CODES: &str =
    "Exit codes (grep-style, for the read commands ask/query/show/path/affected/deps/unresolved):
  0  found results
  1  valid query, no results
  2  usage or execution error";

#[derive(Args)]
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
    /// Follow only these relations (calls, uses, imports, implements, extends)
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
        /// Also install enforcement hooks globally (~/.claude), not just this repo
        #[arg(short = 'g', long)]
        global: bool,
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
        /// Structured output (same shape as the MCP `ask` tool; not
        /// available with --workspace)
        #[arg(long, conflicts_with = "workspace")]
        json: bool,
    },
    /// One-screen orientation card for a symbol or file
    Show {
        /// Symbol: name, qualified suffix (`Config::new`), or node id
        symbol: String,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Structured output (same shape as the MCP `show` tool)
        #[arg(long)]
        json: bool,
    },
    /// Search symbols (exact + trigram), content-bearing results
    Query {
        /// Symbol name, qualified suffix, or fuzzy fragment
        symbol: String,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Maximum results to print
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Structured output (same shape as the MCP `query` tool)
        #[arg(long)]
        json: bool,
    },
    /// Reverse blast radius of a symbol
    Affected {
        /// Symbol: name, qualified suffix, or node id
        symbol: String,
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
        /// Structured output (same shape as the MCP `affected` tool; not
        /// available with --workspace)
        #[arg(long, conflicts_with = "workspace")]
        json: bool,
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
        /// Maximum traversal depth
        #[arg(long, default_value_t = 10)]
        max_depth: usize,
        /// Maximum dependencies to print
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Structured output (same shape as the MCP `deps` tool)
        #[arg(long)]
        json: bool,
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
        /// Structured output (same shape as the MCP `path` tool; not
        /// available with --workspace)
        #[arg(long, conflicts_with = "workspace")]
        json: bool,
        #[command(flatten)]
        filter: FilterArgs,
        #[command(flatten)]
        relations: RelationsArg,
    },
    /// Changed symbols, blast radius, and affected tests for a rev range
    Impact {
        /// Git rev range (e.g. HEAD~1..HEAD, main...branch). A single rev
        /// (`HEAD`) diffs the working tree against it: uncommitted edits to
        /// tracked files; untracked files are not included
        rev_range: String,
        /// Repository to query
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Traverse across the workspace (path to manifest)
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Structured output (same shape as the MCP `impact` tool)
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        filter: FilterArgs,
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
    /// One-screen orientation for a repo: modules, hubs, doc entry points
    Map {
        #[command(flatten)]
        repo: RepoArg,
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
) -> anyhow::Result<sinter_store::EdgeFilter> {
    let mut f = lookup::edge_filter(&filter.evidence, filter.certain)?;
    f.relations = lookup::relation_set(&relations.relations)?;
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

fn main() -> ExitCode {
    // Die quietly when the read end of a pipe closes (`sinter ask | head`).
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
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
        Command::Uninit { repo, global } => uninit::run(repo.path(), global).map(|_| ()),
        Command::Init {
            repo,
            cursor,
            scip,
            no_scip,
            global,
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
            return match init::run(repo.path(), cursor, scip_consent, global) {
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
        } => {
            let result = match workspace {
                Some(manifest) => doctor::run_workspace(&manifest, fix),
                None => doctor::run(repo.path(), fix),
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
            json,
        } => {
            return grep_exit(match workspace {
                Some(manifest) => ask::run_workspace(&manifest, &question, limit),
                None => ask::run(&repo, &question, limit, json),
            });
        }
        Command::Show { symbol, repo, json } => return grep_exit(show::run(&repo, &symbol, json)),
        Command::Query {
            symbol,
            repo,
            limit,
            json,
        } => return grep_exit(query::run(&repo, &symbol, limit, json)),
        Command::Affected {
            symbol,
            repo,
            workspace,
            max_depth,
            limit,
            json,
            filter,
            relations,
        } => {
            return grep_exit(traversal_filter(&filter, &relations).and_then(
                |f| match workspace {
                    Some(manifest) => {
                        affected::run_workspace(&manifest, &symbol, &f, max_depth, limit)
                    }
                    None => affected::run(&repo, &symbol, &f, max_depth, limit, json),
                },
            ));
        }
        Command::Deps {
            symbol,
            repo,
            max_depth,
            limit,
            json,
            filter,
            relations,
        } => {
            return grep_exit(
                traversal_filter(&filter, &relations)
                    .and_then(|f| deps::run(&repo, &symbol, &f, max_depth, limit, json)),
            );
        }
        Command::Unresolved {
            repo,
            file,
            name,
            limit,
            json,
        } => {
            return grep_exit(unresolved::run(
                &repo,
                file.as_deref(),
                name.as_deref(),
                limit,
                json,
            ));
        }
        Command::Path {
            from,
            to,
            repo,
            workspace,
            json,
            filter,
            relations,
        } => {
            return grep_exit(traversal_filter(&filter, &relations).and_then(
                |f| match workspace {
                    Some(manifest) => pathcmd::run_workspace(&manifest, &from, &to, &f),
                    None => pathcmd::run(&repo, &from, &to, &f, json),
                },
            ));
        }
        Command::Impact {
            rev_range,
            repo,
            workspace,
            json,
            filter,
        } => impact::run(
            &repo,
            &rev_range,
            workspace.as_deref(),
            &filter.evidence,
            filter.certain,
            json,
        ),
        Command::Overlap { ranges, repo, json } => overlap::run(&repo, &ranges, json),
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
        Command::Map { repo, json } => map::run(repo.path(), json),
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
