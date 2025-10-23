//! Snapshot cleanup handler
//!
//! Handles cleaning up old snapshot files based on age and count limits.

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::Confirm;
use std::fs;

use crate::undo;
use crate::history::OperationType;
use crate::interactive_conflict;

/// Handle cleanup snapshots command
pub fn handle_cleanup_snapshots(
    dry_run: bool,
    max_count: usize,
    max_age_days: i64,
    interactive: bool,
    verbosity: crate::VerbosityLevel,
) -> Result<()> {
    if verbosity != crate::VerbosityLevel::Quiet {
        if dry_run {
            println!("{}", "Snapshot cleanup (dry run)".cyan().bold());
        } else {
            println!("{}", "Cleaning up old snapshots...".cyan().bold());
        }
        println!("  Keeping: last {} snapshots per type OR last {} days", max_count, max_age_days);
        println!();
    }

    // In interactive or verbose mode, show detailed information about snapshots
    if interactive || verbosity == crate::VerbosityLevel::Verbose {
        show_snapshot_details(max_count, max_age_days, verbosity)?;
    }

    // If interactive mode and not dry run, ask for confirmation
    if interactive && !dry_run && interactive_conflict::is_interactive() {
        let confirm = Confirm::new("Do you want to proceed with deleting these snapshots?")
            .with_default(false)
            .with_help_message("This cannot be undone")
            .prompt()
            .context("Failed to get confirmation")?;

        if !confirm {
            println!("\n{}", "Cleanup cancelled.".yellow());
            return Ok(());
        }
        println!();
    }

    let config = undo::SnapshotCleanupConfig {
        max_count_per_type: max_count,
        max_age_days,
    };

    let deleted_count = undo::cleanup_old_snapshots(Some(config), dry_run)
        .context("Failed to cleanup snapshots")?;

    if verbosity == crate::VerbosityLevel::Quiet {
        if !dry_run && deleted_count > 0 {
            println!("Deleted {} snapshots", deleted_count);
        }
    } else {
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
    }

    Ok(())
}

/// Show detailed information about snapshots before cleanup
fn show_snapshot_details(max_count: usize, max_age_days: i64, verbosity: crate::VerbosityLevel) -> Result<()> {
    let snapshots_dir = undo::Snapshot::snapshots_dir()?;

    if !snapshots_dir.exists() {
        println!("{}", "No snapshots directory found.".yellow());
        return Ok(());
    }

    // Collect all snapshots with metadata
    let mut pull_snapshots: Vec<(std::path::PathBuf, chrono::DateTime<chrono::Utc>, u64)> = Vec::new();
    let mut push_snapshots: Vec<(std::path::PathBuf, chrono::DateTime<chrono::Utc>, u64)> = Vec::new();

    for entry in fs::read_dir(&snapshots_dir)? {
        let entry = entry?;
        let path = entry.path();

        if !path.extension().map_or(false, |ext| ext == "json") {
            continue;
        }

        let metadata = fs::metadata(&path)?;
        let file_size = metadata.len();

        // Load snapshot metadata
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(snapshot) = serde_json::from_str::<undo::Snapshot>(&content) {
                match snapshot.operation_type {
                    OperationType::Pull => pull_snapshots.push((path, snapshot.timestamp, file_size)),
                    OperationType::Push => push_snapshots.push((path, snapshot.timestamp, file_size)),
                }
            }
        }
    }

    // Sort by timestamp descending (newest first)
    pull_snapshots.sort_by(|a, b| b.1.cmp(&a.1));
    push_snapshots.sort_by(|a, b| b.1.cmp(&a.1));

    // Calculate which ones will be kept/deleted
    let now = chrono::Utc::now();
    let age_threshold = now - chrono::Duration::days(max_age_days);

    println!("{}", "Current Snapshot Inventory:".bold().cyan());
    println!("{}", "=".repeat(80).cyan());

    // Show pull snapshots
    println!("\n{} ({} total)", "Pull Snapshots:".bold().green(), pull_snapshots.len());
    let mut pull_keep_count = 0;
    let mut pull_delete_count = 0;
    let mut pull_total_size = 0u64;

    for (idx, (path, timestamp, size)) in pull_snapshots.iter().enumerate() {
        pull_total_size += size;
        let within_count_limit = idx < max_count;
        let within_age_limit = *timestamp >= age_threshold;
        let will_keep = within_count_limit || within_age_limit;

        if will_keep {
            pull_keep_count += 1;
        } else {
            pull_delete_count += 1;
        }

        if verbosity == crate::VerbosityLevel::Verbose {
            let status = if will_keep {
                "KEEP".green()
            } else {
                "DELETE".red()
            };

            let age = now.signed_duration_since(*timestamp);
            let days = age.num_days();

            println!(
                "  [{}] {} - {} days old - {:.1} KB",
                status,
                path.file_name().unwrap().to_string_lossy().dimmed(),
                days,
                *size as f64 / 1024.0
            );
        }
    }

    if verbosity != crate::VerbosityLevel::Verbose {
        println!("  {} to keep, {} to delete ({:.1} KB total)",
            pull_keep_count.to_string().green(),
            pull_delete_count.to_string().red(),
            pull_total_size as f64 / 1024.0
        );
    }

    // Show push snapshots
    println!("\n{} ({} total)", "Push Snapshots:".bold().blue(), push_snapshots.len());
    let mut push_keep_count = 0;
    let mut push_delete_count = 0;
    let mut push_total_size = 0u64;

    for (idx, (path, timestamp, size)) in push_snapshots.iter().enumerate() {
        push_total_size += size;
        let within_count_limit = idx < max_count;
        let within_age_limit = *timestamp >= age_threshold;
        let will_keep = within_count_limit || within_age_limit;

        if will_keep {
            push_keep_count += 1;
        } else {
            push_delete_count += 1;
        }

        if verbosity == crate::VerbosityLevel::Verbose {
            let status = if will_keep {
                "KEEP".green()
            } else {
                "DELETE".red()
            };

            let age = now.signed_duration_since(*timestamp);
            let days = age.num_days();

            println!(
                "  [{}] {} - {} days old - {:.1} KB",
                status,
                path.file_name().unwrap().to_string_lossy().dimmed(),
                days,
                *size as f64 / 1024.0
            );
        }
    }

    if verbosity != crate::VerbosityLevel::Verbose {
        println!("  {} to keep, {} to delete ({:.1} KB total)",
            push_keep_count.to_string().green(),
            push_delete_count.to_string().red(),
            push_total_size as f64 / 1024.0
        );
    }

    // Summary
    println!("\n{}", "Summary:".bold());
    let total_keep = pull_keep_count + push_keep_count;
    let total_delete = pull_delete_count + push_delete_count;
    let total_size = (pull_total_size + push_total_size) as f64 / (1024.0 * 1024.0);

    println!("  {} Total snapshots: {}", "•".cyan(), (pull_snapshots.len() + push_snapshots.len()));
    println!("  {} Will keep: {}", "•".green(), total_keep);
    println!("  {} Will delete: {}", "•".red(), total_delete);
    println!("  {} Total disk space: {:.2} MB", "•".cyan(), total_size);

    if total_delete > 0 {
        let freed_space = ((pull_delete_count as u64 * (pull_total_size / pull_snapshots.len().max(1) as u64)) +
                           (push_delete_count as u64 * (push_total_size / push_snapshots.len().max(1) as u64))) as f64 / (1024.0 * 1024.0);
        println!("  {} Space to be freed: ~{:.2} MB", "•".yellow(), freed_space);
    }

    println!();

    Ok(())
}
