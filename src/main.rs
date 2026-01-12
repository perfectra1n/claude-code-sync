mod config;
mod conflict;
mod filter;
mod handlers;
mod history;
mod interactive_conflict;
mod logger;
mod merge;
mod onboarding;
mod parser;
mod report;
mod scm;
mod sync;
mod undo;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
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
        /// Local filesystem path where the sync repository will be stored
        #[arg(short, long)]
        repo: Option<PathBuf>,

        /// Remote git URL for cloning or pushing (e.g., git@github.com:user/repo.git)
        #[arg(short = 'R', long)]
        remote: Option<String>,

        /// Clone from the remote URL instead of initializing a new local repo
        #[arg(long)]
        clone: bool,

        /// Path to a TOML configuration file for non-interactive setup
        #[arg(short, long)]
        config: Option<PathBuf>,
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

        /// Enable Git LFS for large files
        #[arg(long)]
        enable_lfs: Option<bool>,

        /// File patterns to track with LFS (comma-separated, e.g., "*.jsonl,*.png")
        #[arg(long)]
        lfs_patterns: Option<String>,

        /// SCM backend: git or mercurial (default: git)
        #[arg(long)]
        scm_backend: Option<String>,

        /// Subdirectory within sync repo for storing projects (default: "projects")
        #[arg(long)]
        sync_subdirectory: Option<String>,

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

    // Check if this is an Init command (skip auto-onboarding for Init)
    let is_init_command = matches!(command, Commands::Init { .. });

    // Run onboarding if needed (but not for Init command - it handles its own setup)
    if needs_onboarding && !is_init_command {
        log::info!("Running onboarding flow - first time setup detected");

        // Try non-interactive init first (from config file)
        let initialized = try_init_from_config().unwrap_or(false);

        if !initialized {
            // Fall back to interactive onboarding
            run_onboarding_flow()?;
        }

        log::info!("Onboarding completed successfully");
    }

    match command {
        Commands::Init { repo, remote, clone, config } => {
            // If config file is provided, use non-interactive init
            if config.is_some() {
                run_init_from_config(config)?;
            } else if clone {
                // Clone mode: requires remote URL
                let remote_url = remote.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("--clone requires --remote <URL> to be specified")
                })?;

                // Determine clone destination
                let clone_path = if let Some(ref path) = repo {
                    path.clone()
                } else {
                    config::ConfigManager::default_repo_dir()?
                };

                println!(
                    "{}",
                    format!("Cloning from {} to {}...", remote_url, clone_path.display()).cyan()
                );

                scm::clone(remote_url, &clone_path)?;
                sync::init_from_onboarding(&clone_path, Some(remote_url), true)?;

                // Save default filter configuration if it doesn't exist
                let filter_config_path = config::ConfigManager::filter_config_path()?;
                if !filter_config_path.exists() {
                    filter::FilterConfig::default().save()?;
                }

                println!(
                    "{}",
                    "Clone and initialization complete!".green().bold()
                );
            } else if let Some(repo_path) = repo {
                // Use CLI args for init (local repo path)
                sync::init_sync_repo(&repo_path, remote.as_deref())?;
            } else if let Some(remote_url) = remote {
                // Just --remote provided: clone to default location
                let default_path = config::ConfigManager::default_repo_dir()?;

                println!(
                    "{}",
                    format!("Cloning from {} to {}...", remote_url, default_path.display()).cyan()
                );

                scm::clone(&remote_url, &default_path)?;
                sync::init_from_onboarding(&default_path, Some(&remote_url), true)?;

                // Save default filter configuration if it doesn't exist
                let filter_config_path = config::ConfigManager::filter_config_path()?;
                if !filter_config_path.exists() {
                    filter::FilterConfig::default().save()?;
                }

                println!(
                    "{}",
                    "Clone and initialization complete!".green().bold()
                );
            } else {
                // No args provided, try config file first, then fall back to interactive onboarding
                if !try_init_from_config()? {
                    // No config file found, run interactive onboarding
                    run_onboarding_flow()?;
                }
            }
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
            enable_lfs,
            lfs_patterns,
            scm_backend,
            sync_subdirectory,
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
                    enable_lfs,
                    lfs_patterns,
                    scm_backend,
                    sync_subdirectory,
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
