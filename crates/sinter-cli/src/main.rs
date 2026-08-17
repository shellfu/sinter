mod affected;
mod ask;
mod build;
mod doctor;
mod hooks;
mod impact;
mod init;
mod install;
mod lookup;
mod overlap;
mod pathcmd;
mod pipeline;
mod query;
mod render;
mod scip;
mod serve;
mod show;
mod watch;
mod workspace;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

/// sinter — code knowledge-graph engine.
#[derive(Parser)]
#[command(name = "sinter", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct FilterArgs {
    /// Restrict traversal to these evidence kinds (structural, scope, import, scip)
    #[arg(long, value_delimiter = ',')]
    evidence: Vec<String>,
    /// Follow only compiler-grade (Certain) edges
    #[arg(long)]
    certain: bool,
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
        #[arg(default_value = ".")]
        repo: PathBuf,
        /// Also write the Cursor rule file
        #[arg(long)]
        cursor: bool,
        /// Write a starter workspace manifest instead (path defaults to ws.toml)
        #[arg(long, value_name = "MANIFEST")]
        workspace: Option<Option<PathBuf>>,
        /// Workspace name for --workspace (defaults to manifest's parent dir name)
        #[arg(long, requires = "workspace")]
        name: Option<String>,
    },
    /// Build or incrementally refresh the graph for a repository
    Build {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    /// Watch a repository and keep the graph fresh
    Watch {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    /// Diagnose the installation and a repo's graph; every finding names its fix
    Doctor {
        #[arg(default_value = ".")]
        repo: PathBuf,
        /// Diagnose a workspace instead (path to manifest)
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Install the assistant integration card (embedded, drift-proof)
    Install {
        /// Targets: claude (global skill), cursor (.cursor/rules),
        /// agents (AGENTS.md managed block: Codex/Gemini/etc), all
        #[arg(long = "for", value_delimiter = ',', default_value = "claude")]
        targets: Vec<String>,
        /// Claude skill directory override (default: ~/.claude/skills/sinter)
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Also register the MCP server in the repo's project-scope .mcp.json
        #[arg(long)]
        mcp: bool,
        /// Repo for cursor/agents/--mcp targets
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Install git hooks that refresh the graph after commits/checkouts
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
    /// Ask a vague question, get a ranked, content-bearing starting point
    Ask {
        question: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Fan out across a workspace (path to manifest)
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long, default_value_t = 5)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// One-screen orientation card for a symbol or file
    Show {
        symbol: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Search symbols (exact + trigram), content-bearing results
    Query {
        symbol: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Reverse blast radius of a symbol
    Affected {
        symbol: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Traverse across a workspace (path to manifest)
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long, default_value_t = 10)]
        max_depth: usize,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// How one symbol reaches another
    Path {
        from: String,
        to: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Traverse across a workspace (path to manifest)
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Changed symbols, blast radius, and affected tests for a rev range
    Impact {
        rev_range: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Follow boundary links into other workspace members
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Map several in-flight changes (open PRs) onto the graph and rank
    /// pairwise merge risk (direct/radius/file tiers)
    Overlap {
        /// Two or more rev-ranges, optionally labeled: `pr-12=main...branch`
        ranges: Vec<String>,
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
    },
    /// Run the repo's SCIP indexer and rebuild with compiler-grade evidence
    Scip {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    /// Print version, graph schema, and language packs (for bug reports)
    Version,
}

#[derive(Subcommand)]
enum HooksAction {
    /// Write post-commit/post-checkout/post-merge hooks
    Install {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
}

fn main() -> ExitCode {
    // Die quietly when the read end of a pipe closes (`sinter ask | head`).
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Workspace { manifest } => workspace::run(&manifest),
        Command::Init {
            repo,
            cursor,
            workspace,
            name,
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
                return match init::run_workspace(&path, &ws_name) {
                    Ok(true) => ExitCode::SUCCESS,
                    Ok(false) => ExitCode::FAILURE,
                    Err(e) => {
                        eprintln!("error: {e:#}");
                        ExitCode::FAILURE
                    }
                };
            }
            return match init::run(&repo, cursor) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(e) => {
                    eprintln!("sinter: {e:#}");
                    ExitCode::FAILURE
                }
            };
        }
        Command::Build { repo } => build::run(&repo),
        Command::Watch { repo } => watch::run(&repo),
        Command::Doctor { repo, workspace } => {
            let result = match workspace {
                Some(manifest) => doctor::run_workspace(&manifest),
                None => doctor::run(&repo),
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
            dir,
            mcp,
            repo,
        } => install::run_targets(&targets, dir, mcp, &repo),
        Command::Hooks {
            action: HooksAction::Install { repo },
        } => hooks::install(&repo),
        Command::Ask {
            question,
            repo,
            workspace,
            limit,
            json,
        } => match workspace {
            Some(manifest) => ask::run_workspace(&manifest, &question, limit),
            None => ask::run(&repo, &question, limit, json),
        },
        Command::Show { symbol, repo } => show::run(&repo, &symbol),
        Command::Query {
            symbol,
            repo,
            limit,
        } => query::run(&repo, &symbol, limit),
        Command::Affected {
            symbol,
            repo,
            workspace,
            max_depth,
            filter,
        } => match workspace {
            Some(manifest) => affected::run_workspace(
                &manifest,
                &symbol,
                &filter.evidence,
                filter.certain,
                max_depth,
            ),
            None => affected::run(&repo, &symbol, &filter.evidence, filter.certain, max_depth),
        },
        Command::Path {
            from,
            to,
            repo,
            workspace,
            filter,
        } => match workspace {
            Some(manifest) => {
                pathcmd::run_workspace(&manifest, &from, &to, &filter.evidence, filter.certain)
            }
            None => pathcmd::run(&repo, &from, &to, &filter.evidence, filter.certain),
        },
        Command::Impact {
            rev_range,
            repo,
            workspace,
        } => impact::run(&repo, &rev_range, workspace.as_deref()),
        Command::Overlap { ranges, repo, json } => overlap::run(&repo, &ranges, json),
        Command::Serve { repo } => serve::run(&repo),
        Command::Scip { repo } => scip::run(&repo),
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
