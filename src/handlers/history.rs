//! History command handlers
//!
//! Handles viewing and managing sync operation history including
//! listing operations, viewing details, and clearing history.

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::Select;

use crate::history;
use crate::interactive_conflict;

/// Handle history list command
pub fn handle_history_list(limit: usize) -> Result<()> {
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
pub fn handle_history_last(operation_type: Option<&str>) -> Result<()> {
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
pub fn handle_history_clear() -> Result<()> {
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

/// Handle history review command (interactive selection)
pub fn handle_history_review(limit: usize) -> Result<()> {
    // Check if we're in an interactive terminal
    if !interactive_conflict::is_interactive() {
        println!("{}", "Review mode requires an interactive terminal.".yellow());
        println!("{}", "Use 'history list' for non-interactive viewing.".dimmed());
        return Ok(());
    }

    let history = history::OperationHistory::load().context("Failed to load operation history")?;

    if history.is_empty() {
        println!("{}", "No operations in history.".yellow());
        return Ok(());
    }

    println!("{}", "Interactive Operation Review".cyan().bold());
    println!("{}", "=".repeat(80).cyan());
    println!();

    let operations = history.list_operations();
    let display_count = operations.len().min(limit);

    // Create selection options
    let options: Vec<String> = operations
        .iter()
        .take(display_count)
        .enumerate()
        .map(|(idx, op)| {
            let op_type = match op.operation_type {
                history::OperationType::Pull => "PULL".to_string(),
                history::OperationType::Push => "PUSH".to_string(),
            };

            let timestamp = op.timestamp.format("%Y-%m-%d %H:%M").to_string();
            let branch = op.branch.as_deref().unwrap_or("unknown");
            let conv_count = op.affected_conversations.len();

            format!(
                "{:2}. [{}] {} | {} | {} conversations",
                idx + 1,
                op_type,
                timestamp,
                branch,
                conv_count
            )
        })
        .collect();

    // Add an option to exit
    let mut options_with_exit = options.clone();
    options_with_exit.push("← Exit review".to_string());

    loop {
        let selection = Select::new("Select an operation to review (or Exit):", options_with_exit.clone())
            .with_help_message("Use arrow keys to navigate, Enter to select")
            .prompt();

        match selection {
            Ok(selected) => {
                if selected == "← Exit review" {
                    println!("\n{}", "Exiting review.".dimmed());
                    break;
                }

                // Find the index of the selected operation
                if let Some(idx) = options.iter().position(|opt| opt == &selected) {
                    if idx < operations.len() {
                        let operation = &operations[idx];

                        // Display detailed information
                        println!("\n{}", "=".repeat(80).yellow());
                        println!("{}", "Operation Details".bold().yellow());
                        println!("{}", "=".repeat(80).yellow());

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
                                println!("  {} {}", label, count);
                            }
                        }

                        if let Some(snapshot_path) = &operation.snapshot_path {
                            println!(
                                "\n{} {}",
                                "Snapshot:".bold(),
                                if snapshot_path.exists() {
                                    "Available".green()
                                } else {
                                    "Missing".red()
                                }
                            );
                            println!("  {}", snapshot_path.display().to_string().dimmed());
                        }

                        // Show affected conversations
                        if !operation.affected_conversations.is_empty() {
                            println!("\n{}", "Affected Conversations:".bold());
                            let conv_display_count = operation.affected_conversations.len().min(20);

                            for (idx, conv) in operation
                                .affected_conversations
                                .iter()
                                .take(conv_display_count)
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

                            if operation.affected_conversations.len() > conv_display_count {
                                println!(
                                    "  {} and {} more...",
                                    "...".dimmed(),
                                    operation.affected_conversations.len() - conv_display_count
                                );
                            }
                        }

                        println!("{}", "=".repeat(80).yellow());
                        println!();
                    }
                }
            }
            Err(_) => {
                // User cancelled (Ctrl+C)
                println!("\n{}", "Review cancelled.".yellow());
                break;
            }
        }
    }

    Ok(())
}
