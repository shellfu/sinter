mod affected;
mod ask;
mod build;
mod doctor;
mod hooks;
mod impact;
mod install;
mod lookup;
mod pathcmd;
mod pipeline;
mod query;
mod render;
mod serve;
mod show;
mod watch;

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
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Changed symbols, blast radius, and affected tests for a rev range
    Impact {
        rev_range: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// MCP server over stdio
    Serve {
        #[arg(long, default_value = ".")]
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
        Command::Build { repo } => build::run(&repo),
        Command::Watch { repo } => watch::run(&repo),
        Command::Doctor { repo } => {
            return match doctor::run(&repo) {
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
            limit,
            json,
        } => ask::run(&repo, &question, limit, json),
        Command::Show { symbol, repo } => show::run(&repo, &symbol),
        Command::Query {
            symbol,
            repo,
            limit,
        } => query::run(&repo, &symbol, limit),
        Command::Affected {
            symbol,
            repo,
            max_depth,
            filter,
        } => affected::run(&repo, &symbol, &filter.evidence, filter.certain, max_depth),
        Command::Path {
            from,
            to,
            repo,
            filter,
        } => pathcmd::run(&repo, &from, &to, &filter.evidence, filter.certain),
        Command::Impact { rev_range, repo } => impact::run(&repo, &rev_range),
        Command::Serve { repo } => serve::run(&repo),
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
