//! Snapshot cleanup handler
//!
//! Handles cleaning up old snapshot files based on age and count limits.

use anyhow::{Context, Result};
use colored::Colorize;

use crate::undo;

/// Handle cleanup snapshots command
pub fn handle_cleanup_snapshots(dry_run: bool, max_count: usize, max_age_days: i64) -> Result<()> {
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
