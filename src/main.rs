mod parser;
mod git;
mod sync;
mod conflict;
mod filter;
mod report;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "claude-sync")]
#[command(about = "Sync Claude Code conversation history with git repositories", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new sync repository
    Init {
        /// Path to the git repository for storing history
        #[arg(short, long)]
        repo: PathBuf,

        /// Remote git URL (optional, for pushing to remote)
        #[arg(short, long)]
        remote: Option<String>,
    },

    /// Push local Claude Code history to the sync repository
    Push {
        /// Commit message (optional)
        #[arg(short, long)]
        message: Option<String>,

        /// Push to remote after committing
        #[arg(long, default_value_t = true)]
        push_remote: bool,
    },

    /// Pull and merge history from the sync repository
    Pull {
        /// Pull from remote before merging
        #[arg(long, default_value_t = true)]
        fetch_remote: bool,
    },

    /// Show sync status and conflicts
    Status {
        /// Show detailed conflict information
        #[arg(long)]
        show_conflicts: bool,

        /// Show which files would be synced
        #[arg(long)]
        show_files: bool,
    },

    /// Configure sync settings
    Config {
        /// Exclude projects older than N days
        #[arg(long)]
        exclude_older_than: Option<u32>,

        /// Include only specific project paths (comma-separated patterns)
        #[arg(long)]
        include_projects: Option<String>,

        /// Exclude specific project paths (comma-separated patterns)
        #[arg(long)]
        exclude_projects: Option<String>,

        /// Show current configuration
        #[arg(long)]
        show: bool,
    },

    /// View conflict reports
    Report {
        /// Output format: json or markdown
        #[arg(short, long, default_value = "markdown")]
        format: String,

        /// Output file (default: print to stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { repo, remote } => {
            sync::init_sync_repo(&repo, remote.as_deref())?;
        }
        Commands::Push { message, push_remote } => {
            sync::push_history(message.as_deref(), push_remote)?;
        }
        Commands::Pull { fetch_remote } => {
            sync::pull_history(fetch_remote)?;
        }
        Commands::Status { show_conflicts, show_files } => {
            sync::show_status(show_conflicts, show_files)?;
        }
        Commands::Config {
            exclude_older_than,
            include_projects,
            exclude_projects,
            show,
        } => {
            if show {
                filter::show_config()?;
            } else {
                filter::update_config(exclude_older_than, include_projects, exclude_projects)?;
            }
        }
        Commands::Report { format, output } => {
            report::generate_report(&format, output.as_deref())?;
        }
    }

    Ok(())
}
