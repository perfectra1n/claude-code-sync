mod config;
mod conflict;
mod filter;
mod git;
mod history;
mod interactive_conflict;
mod logger;
mod merge;
mod onboarding;
mod parser;
mod report;
mod sync;
mod undo;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use inquire::Confirm;
use std::path::PathBuf;

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
    },

    /// Pull and merge history from the sync repository
    Pull {
        /// Pull from remote before merging
        #[arg(long, default_value_t = true)]
        fetch_remote: bool,

        /// Branch to pull from (default: current branch)
        #[arg(short, long)]
        branch: Option<String>,
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
    Pull,

    /// Undo the last push operation
    Push,
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

    /// Clear all operation history
    Clear,
}

fn main() -> Result<()> {
    // Initialize logging (rotate log if needed, then set up logger)
    logger::rotate_log_if_needed().ok(); // Ignore errors during log rotation
    logger::init_logger().ok(); // Ignore errors during logger init

    log::info!("claude-code-sync started");

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
            }
        } else {
            // Already initialized, default to sync
            Commands::Sync {
                message: None,
                branch: None,
                exclude_attachments: false,
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
        } => {
            sync::push_history(
                message.as_deref(),
                push_remote,
                branch.as_deref(),
                exclude_attachments,
            )?;
        }
        Commands::Pull {
            fetch_remote,
            branch,
        } => {
            sync::pull_history(fetch_remote, branch.as_deref())?;
        }
        Commands::Sync {
            message,
            branch,
            exclude_attachments,
        } => {
            sync::sync_bidirectional(message.as_deref(), branch.as_deref(), exclude_attachments)?;
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
        } => {
            if show {
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
        Commands::Undo { operation } => match operation {
            UndoOperation::Pull => {
                handle_undo_pull()?;
            }
            UndoOperation::Push => {
                handle_undo_push()?;
            }
        },
        Commands::History { action } => match action {
            HistoryAction::List { limit } => {
                handle_history_list(limit)?;
            }
            HistoryAction::Last { operation_type } => {
                handle_history_last(operation_type.as_deref())?;
            }
            HistoryAction::Clear => {
                handle_history_clear()?;
            }
        },
        Commands::CleanupSnapshots {
            dry_run,
            max_count,
            max_age_days,
        } => {
            handle_cleanup_snapshots(dry_run, max_count, max_age_days)?;
        }
    }

    Ok(())
}

// ============================================================================
// Undo Command Handlers
// ============================================================================

/// Handle undo pull command
fn handle_undo_pull() -> Result<()> {
    println!("{}", "Preparing to undo last pull operation...".cyan());

    // Check if we're in an interactive terminal
    let is_interactive = interactive_conflict::is_interactive();

    if is_interactive {
        // Show preview
        let preview = undo::preview_undo_pull(None).context("Failed to preview undo operation")?;

        preview.display();

        // Ask for confirmation
        let confirm = Confirm::new("Do you want to proceed with this undo operation?")
            .with_default(false)
            .with_help_message("This will restore files to their pre-pull state")
            .prompt()
            .context("Failed to get confirmation")?;

        if !confirm {
            println!("\n{}", "Undo operation cancelled.".yellow());
            return Ok(());
        }
    }

    println!("\n{}", "Undoing last pull operation...".cyan());

    // Call undo_pull with None for both history_path and allowed_base_dir
    // This uses the default locations for production use
    let summary = undo::undo_pull(None, None).context("Failed to undo pull operation")?;

    println!("\n{}", "SUCCESS".green().bold());
    println!("{summary}");

    Ok(())
}

/// Handle undo push command
fn handle_undo_push() -> Result<()> {
    println!("{}", "Preparing to undo last push operation...".cyan());

    // Load sync state to get repository path
    let state = sync::SyncState::load()
        .context("Sync not initialized. Run 'claude-code-sync init' first.")?;

    // Check if we're in an interactive terminal
    let is_interactive = interactive_conflict::is_interactive();

    if is_interactive {
        // Show preview
        let preview = undo::preview_undo_push(None).context("Failed to preview undo operation")?;

        preview.display();

        // Ask for confirmation
        let confirm = Confirm::new("Do you want to proceed with this undo operation?")
            .with_default(false)
            .with_help_message("This will reset the git repository to the previous commit")
            .prompt()
            .context("Failed to get confirmation")?;

        if !confirm {
            println!("\n{}", "Undo operation cancelled.".yellow());
            return Ok(());
        }
    }

    println!("\n{}", "Undoing last push operation...".cyan());

    // Call undo_push with repository path and default history path
    let summary =
        undo::undo_push(&state.sync_repo_path, None).context("Failed to undo push operation")?;

    println!("\n{}", "SUCCESS".green().bold());
    println!("{summary}");

    Ok(())
}

// ============================================================================
// History Command Handlers
// ============================================================================

/// Handle history list command
fn handle_history_list(limit: usize) -> Result<()> {
    let history = history::OperationHistory::load().context("Failed to load operation history")?;

    if history.is_empty() {
        println!("{}", "No operations in history.".yellow());
        return Ok(());
    }

    println!("{}", "Operation History".cyan().bold());
    println!("{}", "=".repeat(80).cyan());

    let operations = history.list_operations();
    let display_count = operations.len().min(limit);

    for (idx, op) in operations.iter().take(display_count).enumerate() {
        let num = format!("{}.", idx + 1);
        let op_type = match op.operation_type {
            history::OperationType::Pull => "PULL".green(),
            history::OperationType::Push => "PUSH".blue(),
        };

        println!("\n{} {}", num.bold(), op_type.bold());
        println!(
            "   {} {}",
            "Time:".dimmed(),
            op.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
        );

        if let Some(branch) = &op.branch {
            println!("   {} {}", "Branch:".dimmed(), branch);
        }

        println!(
            "   {} {}",
            "Conversations:".dimmed(),
            op.affected_conversations.len()
        );

        // Show operation statistics
        let stats = op.operation_stats();
        if !stats.is_empty() {
            let mut stat_parts = Vec::new();
            for (sync_op, count) in &stats {
                let stat_str = format!("{} {}", count, sync_op.as_str());
                stat_parts.push(stat_str);
            }
            println!("   {} {}", "Changes:".dimmed(), stat_parts.join(", "));
        }

        if op.snapshot_path.is_some() {
            println!("   {} {}", "Snapshot:".dimmed(), "Available".green());
        }
    }

    if operations.len() > display_count {
        println!(
            "\n{} Showing {} of {} operations",
            "Note:".yellow(),
            display_count,
            operations.len()
        );
    }

    Ok(())
}

/// Handle history last command
fn handle_history_last(operation_type: Option<&str>) -> Result<()> {
    let history = history::OperationHistory::load().context("Failed to load operation history")?;

    let operation = if let Some(op_type) = operation_type {
        // Filter by operation type
        let filter_type = match op_type.to_lowercase().as_str() {
            "pull" => history::OperationType::Pull,
            "push" => history::OperationType::Push,
            _ => {
                return Err(anyhow::anyhow!(
                    "Invalid operation type '{op_type}'. Must be 'pull' or 'push'."
                ));
            }
        };

        history
            .get_last_operation_by_type(filter_type)
            .ok_or_else(|| {
                anyhow::anyhow!("No {} operation found in history.", filter_type.as_str())
            })?
    } else {
        // Get the last operation of any type
        history
            .get_last_operation()
            .ok_or_else(|| anyhow::anyhow!("No operations in history."))?
    };

    println!("{}", "Last Operation Details".cyan().bold());
    println!("{}", "=".repeat(80).cyan());

    let op_type = match operation.operation_type {
        history::OperationType::Pull => "PULL".green(),
        history::OperationType::Push => "PUSH".blue(),
    };

    println!("\n{} {}", "Type:".bold(), op_type.bold());
    println!(
        "{} {}",
        "Time:".bold(),
        operation.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
    );

    if let Some(branch) = &operation.branch {
        println!("{} {}", "Branch:".bold(), branch);
    }

    println!(
        "{} {}",
        "Total Conversations:".bold(),
        operation.affected_conversations.len()
    );

    // Show operation statistics
    let stats = operation.operation_stats();
    if !stats.is_empty() {
        println!("\n{}", "Changes:".bold());
        for (sync_op, count) in &stats {
            let label = match sync_op {
                history::SyncOperation::Added => "Added".green(),
                history::SyncOperation::Modified => "Modified".yellow(),
                history::SyncOperation::Conflict => "Conflicts".red(),
                history::SyncOperation::Unchanged => "Unchanged".dimmed(),
            };
            println!("  {label} {count}");
        }
    }

    if let Some(snapshot_path) = &operation.snapshot_path {
        println!(
            "\n{} {}",
            "Snapshot:".bold(),
            snapshot_path.display().to_string().dimmed()
        );
    }

    // Show some conversation details
    if !operation.affected_conversations.is_empty() {
        println!("\n{}", "Affected Conversations:".bold());
        let display_count = operation.affected_conversations.len().min(10);

        for (idx, conv) in operation
            .affected_conversations
            .iter()
            .take(display_count)
            .enumerate()
        {
            let status = match conv.operation {
                history::SyncOperation::Added => "added".green(),
                history::SyncOperation::Modified => "modified".yellow(),
                history::SyncOperation::Conflict => "conflict".red(),
                history::SyncOperation::Unchanged => "unchanged".dimmed(),
            };

            println!(
                "  {}. {} ({} messages) - {}",
                idx + 1,
                conv.project_path.dimmed(),
                conv.message_count,
                status
            );
        }

        if operation.affected_conversations.len() > display_count {
            println!(
                "  {} and {} more...",
                "...".dimmed(),
                operation.affected_conversations.len() - display_count
            );
        }
    }

    Ok(())
}

/// Handle history clear command
fn handle_history_clear() -> Result<()> {
    // Load the history
    let mut history =
        history::OperationHistory::load().context("Failed to load operation history")?;

    if history.is_empty() {
        println!("{}", "No history to clear.".yellow());
        return Ok(());
    }

    let count = history.len();

    // Clear the history
    history.clear().context("Failed to clear history")?;

    println!(
        "{} Cleared {} operation(s) from history.",
        "SUCCESS:".green().bold(),
        count
    );

    Ok(())
}

// ============================================================================
// Onboarding & Initialization Helpers
// ============================================================================

/// Check if claude-code-sync has been initialized
fn is_initialized() -> Result<bool> {
    let state_path = config::ConfigManager::state_file_path()?;
    Ok(state_path.exists())
}

/// Run the onboarding flow and initialize the system
fn run_onboarding_flow() -> Result<()> {
    use colored::Colorize;

    // Run the interactive onboarding
    let onboarding_config =
        onboarding::run_onboarding().context("Onboarding cancelled or failed")?;

    // Handle cloning if needed
    if onboarding_config.is_cloned {
        if let Some(ref remote_url) = onboarding_config.remote_url {
            println!();
            println!("{}", "✓ Cloning repository...".cyan());

            git::GitManager::clone(remote_url, &onboarding_config.repo_path)
                .context("Failed to clone repository")?;

            println!("{}", "✓ Repository cloned successfully!".green());
        }
    }

    // Initialize sync state
    sync::init_from_onboarding(
        &onboarding_config.repo_path,
        onboarding_config.remote_url.as_deref(),
        onboarding_config.is_cloned,
    )
    .context("Failed to initialize sync state")?;

    // Save filter configuration
    let filter_config = filter::FilterConfig {
        exclude_attachments: onboarding_config.exclude_attachments,
        exclude_older_than_days: onboarding_config.exclude_older_than_days,
        ..Default::default()
    };
    filter_config
        .save()
        .context("Failed to save filter configuration")?;

    println!("{}", "✓ Ready to sync!".green().bold());
    println!();

    Ok(())
}

// ============================================================================
// Snapshot Cleanup Handler
// ============================================================================

/// Handle cleanup snapshots command
fn handle_cleanup_snapshots(dry_run: bool, max_count: usize, max_age_days: i64) -> Result<()> {
    use colored::Colorize;

    if dry_run {
        println!("{}", "Snapshot cleanup (dry run)".cyan().bold());
        println!("  Would keep: last {} snapshots per type OR last {} days", max_count, max_age_days);
        println!();
    } else {
        println!("{}", "Cleaning up old snapshots...".cyan().bold());
        println!("  Keeping: last {} snapshots per type OR last {} days", max_count, max_age_days);
        println!();
    }

    let config = undo::SnapshotCleanupConfig {
        max_count_per_type: max_count,
        max_age_days,
    };

    let deleted_count = undo::cleanup_old_snapshots(Some(config), dry_run)
        .context("Failed to cleanup snapshots")?;

    if dry_run {
        if deleted_count > 0 {
            println!(
                "{} {} snapshots would be deleted",
                "✓".green(),
                deleted_count
            );
        } else {
            println!("{}", "No snapshots to delete".dimmed());
        }
    } else {
        if deleted_count > 0 {
            println!(
                "{} Deleted {} old snapshots",
                "✓".green(),
                deleted_count
            );
        } else {
            println!("{}", "No old snapshots to delete".dimmed());
        }
    }

    Ok(())
}
