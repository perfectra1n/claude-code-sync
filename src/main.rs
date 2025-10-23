mod config;
mod conflict;
mod filter;
mod git;
mod handlers;
mod history;
mod interactive_conflict;
mod logger;
mod merge;
mod onboarding;
mod parser;
mod report;
mod sync;
mod undo;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

// Import all handler functions
use handlers::*;

// Import VerbosityLevel from lib
use claude_code_sync::VerbosityLevel;

#[derive(Parser)]
#[command(name = "claude-code-sync")]
#[command(about = "Sync Claude Code conversation history with git repositories", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
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

        /// Branch to push to (default: current branch)
        #[arg(short, long)]
        branch: Option<String>,

        /// Exclude file attachments (images, etc.) from sync
        #[arg(long)]
        exclude_attachments: bool,

        /// Interactive mode - preview changes and confirm before pushing
        #[arg(short, long)]
        interactive: bool,

        /// Show detailed verbose output
        #[arg(short, long)]
        verbose: bool,

        /// Show minimal quiet output
        #[arg(short, long, conflicts_with = "verbose")]
        quiet: bool,
    },

    /// Pull and merge history from the sync repository
    Pull {
        /// Pull from remote before merging
        #[arg(long, default_value_t = true)]
        fetch_remote: bool,

        /// Branch to pull from (default: current branch)
        #[arg(short, long)]
        branch: Option<String>,

        /// Interactive mode - preview changes and confirm before pulling
        #[arg(short, long)]
        interactive: bool,

        /// Show detailed verbose output
        #[arg(short, long)]
        verbose: bool,

        /// Show minimal quiet output
        #[arg(short, long, conflicts_with = "verbose")]
        quiet: bool,
    },

    /// Sync bidirectionally (pull then push)
    Sync {
        /// Commit message for push (optional)
        #[arg(short, long)]
        message: Option<String>,

        /// Branch to sync with (default: current branch)
        #[arg(short, long)]
        branch: Option<String>,

        /// Exclude file attachments (images, etc.) from sync
        #[arg(long)]
        exclude_attachments: bool,

        /// Interactive mode - preview changes and confirm before syncing
        #[arg(short, long)]
        interactive: bool,

        /// Show detailed verbose output
        #[arg(short, long)]
        verbose: bool,

        /// Show minimal quiet output
        #[arg(short, long, conflicts_with = "verbose")]
        quiet: bool,
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

        /// Exclude file attachments (images, etc.) from sync
        #[arg(long)]
        exclude_attachments: Option<bool>,

        /// Show current configuration
        #[arg(long)]
        show: bool,

        /// Interactive configuration menu (select settings to modify)
        #[arg(short, long)]
        interactive: bool,

        /// Step-by-step configuration wizard
        #[arg(short, long)]
        wizard: bool,
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

    /// Manage git remote configuration
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },

    /// Undo the last sync operation
    Undo {
        #[command(subcommand)]
        operation: UndoOperation,

        /// Show detailed verbose output
        #[arg(short, long, global = true)]
        verbose: bool,

        /// Show minimal quiet output
        #[arg(short, long, global = true, conflicts_with = "verbose")]
        quiet: bool,
    },

    /// View and manage operation history
    History {
        #[command(subcommand)]
        action: HistoryAction,
    },

    /// Clean up old snapshot files
    CleanupSnapshots {
        /// Show what would be deleted without actually deleting
        #[arg(long)]
        dry_run: bool,

        /// Maximum number of snapshots to keep per operation type
        #[arg(long, default_value_t = 5)]
        max_count: usize,

        /// Maximum age of snapshots to keep (in days)
        #[arg(long, default_value_t = 7)]
        max_age_days: i64,

        /// Interactive mode with detailed confirmation
        #[arg(short, long)]
        interactive: bool,

        /// Show detailed verbose output
        #[arg(short, long)]
        verbose: bool,

        /// Show minimal quiet output
        #[arg(short, long, conflicts_with = "verbose")]
        quiet: bool,
    },
}

#[derive(Subcommand)]
enum RemoteAction {
    /// Show current remote URL
    Show,

    /// Set or update remote URL
    Set {
        /// Remote name (default: origin)
        #[arg(short, long, default_value = "origin")]
        name: String,

        /// Remote URL (e.g., https://github.com/user/repo.git)
        url: String,
    },

    /// Remove remote
    Remove {
        /// Remote name (default: origin)
        #[arg(short, long, default_value = "origin")]
        name: String,
    },
}

#[derive(Subcommand)]
enum UndoOperation {
    /// Undo the last pull operation
    Pull {
        /// Preview the undo without executing it
        #[arg(long)]
        preview: bool,
    },

    /// Undo the last push operation
    Push {
        /// Preview the undo without executing it
        #[arg(long)]
        preview: bool,
    },
}

#[derive(Subcommand)]
enum HistoryAction {
    /// List recent sync operations
    List {
        /// Number of operations to show (default: 10)
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },

    /// Show details of the last operation
    Last {
        /// Filter by operation type (pull or push)
        #[arg(short = 't', long)]
        operation_type: Option<String>,
    },

    /// Interactively review and select operations to view details
    Review {
        /// Number of operations to show for selection (default: 10)
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },

    /// Clear all operation history
    Clear,
}

fn main() -> Result<()> {
    // Initialize logging (rotate log if needed, then set up logger)
    logger::rotate_log_if_needed().ok(); // Ignore errors during log rotation
    logger::init_logger().ok(); // Ignore errors during logger init

    log::debug!("claude-code-sync started");

    let cli = Cli::parse();

    // Check if initialization is needed (before processing any command)
    let needs_onboarding = !is_initialized()?;

    // Determine the actual command to run
    let command = if let Some(cmd) = cli.command {
        cmd
    } else {
        // No command provided
        if needs_onboarding {
            // Will trigger onboarding below, then default to sync
            Commands::Sync {
                message: None,
                branch: None,
                exclude_attachments: false,
                interactive: false,
                verbose: false,
                quiet: false,
            }
        } else {
            // Already initialized, default to sync
            Commands::Sync {
                message: None,
                branch: None,
                exclude_attachments: false,
                interactive: false,
                verbose: false,
                quiet: false,
            }
        }
    };

    // Run onboarding if needed
    if needs_onboarding {
        log::info!("Running onboarding flow - first time setup detected");
        run_onboarding_flow()?;
        log::info!("Onboarding completed successfully");
    }

    match command {
        Commands::Init { repo, remote } => {
            sync::init_sync_repo(&repo, remote.as_deref())?;
        }
        Commands::Push {
            message,
            push_remote,
            branch,
            exclude_attachments,
            interactive,
            verbose,
            quiet,
        } => {
            // Determine verbosity level
            let verbosity = if verbose {
                VerbosityLevel::Verbose
            } else if quiet {
                VerbosityLevel::Quiet
            } else {
                VerbosityLevel::Normal
            };

            sync::push_history(
                message.as_deref(),
                push_remote,
                branch.as_deref(),
                exclude_attachments,
                interactive,
                verbosity,
            )?;
        }
        Commands::Pull {
            fetch_remote,
            branch,
            interactive,
            verbose,
            quiet,
        } => {
            // Determine verbosity level
            let verbosity = if verbose {
                VerbosityLevel::Verbose
            } else if quiet {
                VerbosityLevel::Quiet
            } else {
                VerbosityLevel::Normal
            };

            sync::pull_history(fetch_remote, branch.as_deref(), interactive, verbosity)?;
        }
        Commands::Sync {
            message,
            branch,
            exclude_attachments,
            interactive,
            verbose,
            quiet,
        } => {
            // Determine verbosity level
            let verbosity = if verbose {
                VerbosityLevel::Verbose
            } else if quiet {
                VerbosityLevel::Quiet
            } else {
                VerbosityLevel::Normal
            };

            sync::sync_bidirectional(
                message.as_deref(),
                branch.as_deref(),
                exclude_attachments,
                interactive,
                verbosity,
            )?;
        }
        Commands::Status {
            show_conflicts,
            show_files,
        } => {
            sync::show_status(show_conflicts, show_files)?;
        }
        Commands::Config {
            exclude_older_than,
            include_projects,
            exclude_projects,
            exclude_attachments,
            show,
            interactive,
            wizard,
        } => {
            // Priority: interactive > wizard > show > individual settings
            if interactive {
                handle_config_interactive()?;
            } else if wizard {
                handle_config_wizard()?;
            } else if show {
                filter::show_config()?;
            } else {
                filter::update_config(
                    exclude_older_than,
                    include_projects,
                    exclude_projects,
                    exclude_attachments,
                )?;
            }
        }
        Commands::Report { format, output } => {
            report::generate_report(&format, output.as_deref())?;
        }
        Commands::Remote { action } => match action {
            RemoteAction::Show => {
                sync::show_remote()?;
            }
            RemoteAction::Set { name, url } => {
                sync::set_remote(&name, &url)?;
            }
            RemoteAction::Remove { name } => {
                sync::remove_remote(&name)?;
            }
        },
        Commands::Undo { operation, verbose, quiet } => {
            // Determine verbosity level
            let verbosity = if verbose {
                VerbosityLevel::Verbose
            } else if quiet {
                VerbosityLevel::Quiet
            } else {
                VerbosityLevel::Normal
            };

            match operation {
                UndoOperation::Pull { preview } => {
                    handle_undo_pull(preview, verbosity)?;
                }
                UndoOperation::Push { preview } => {
                    handle_undo_push(preview, verbosity)?;
                }
            }
        },
        Commands::History { action } => match action {
            HistoryAction::List { limit } => {
                handle_history_list(limit)?;
            }
            HistoryAction::Last { operation_type } => {
                handle_history_last(operation_type.as_deref())?;
            }
            HistoryAction::Review { limit } => {
                handle_history_review(limit)?;
            }
            HistoryAction::Clear => {
                handle_history_clear()?;
            }
        },
        Commands::CleanupSnapshots {
            dry_run,
            max_count,
            max_age_days,
            interactive,
            verbose,
            quiet,
        } => {
            // Determine verbosity level
            let verbosity = if verbose {
                VerbosityLevel::Verbose
            } else if quiet {
                VerbosityLevel::Quiet
            } else {
                VerbosityLevel::Normal
            };

            handle_cleanup_snapshots(dry_run, max_count, max_age_days, interactive, verbosity)?;
        }
    }

    Ok(())
}
