use anyhow::{anyhow, Result};
use std::path::PathBuf;

use crate::history::{OperationHistory, OperationType};
use super::snapshot::Snapshot;

/// Preview information for an undo operation
#[derive(Debug)]
pub struct UndoPreview {
    /// Operation type being undone
    pub operation_type: OperationType,
    /// When the original operation occurred
    pub operation_timestamp: chrono::DateTime<chrono::Utc>,
    /// Branch name
    pub branch: Option<String>,
    /// List of files that will be affected
    pub affected_files: Vec<String>,
    /// Number of conversations affected
    pub conversation_count: usize,
    /// Git commit hash (for push operations)
    pub commit_hash: Option<String>,
    /// Snapshot creation timestamp
    pub snapshot_timestamp: chrono::DateTime<chrono::Utc>,
}

/// Verbosity level for preview display
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbosityLevel {
    Quiet,   // Minimal output
    Normal,  // Standard output
    Verbose, // Detailed output
}

impl UndoPreview {
    /// Display a formatted preview of the undo operation with specified verbosity
    pub fn display(&self, verbosity: VerbosityLevel) {
        use colored::Colorize;

        match verbosity {
            VerbosityLevel::Quiet => {
                // Minimal output - just operation type and counts
                let op_type = match self.operation_type {
                    OperationType::Pull => "Pull",
                    OperationType::Push => "Push",
                };
                println!("Undo {}: {} conversations affected", op_type, self.conversation_count);
                if !self.affected_files.is_empty() {
                    println!("  {} files will be restored", self.affected_files.len());
                }
            }

            VerbosityLevel::Normal => {
                // Standard output - current behavior
                println!("\n{}", "=".repeat(80).yellow());
                println!("{}", "Undo Preview".bold().yellow());
                println!("{}", "=".repeat(80).yellow());

                let op_type = match self.operation_type {
                    OperationType::Pull => "PULL".green(),
                    OperationType::Push => "PUSH".blue(),
                };

                println!("\n{} {}", "Operation:".bold(), op_type);
                println!(
                    "{} {}",
                    "Performed:".bold(),
                    self.operation_timestamp
                        .format("%Y-%m-%d %H:%M:%S UTC")
                        .to_string()
                        .cyan()
                );

                if let Some(branch) = &self.branch {
                    println!("{} {}", "Branch:".bold(), branch.cyan());
                }

                if let Some(commit) = &self.commit_hash {
                    println!("{} {}", "Will reset to:".bold(), commit[..8].yellow());
                }

                println!(
                    "\n{} {}",
                    "Conversations affected:".bold(),
                    self.conversation_count.to_string().yellow()
                );

                if !self.affected_files.is_empty() {
                    println!("\n{}", "Files to be restored:".bold());
                    let display_count = self.affected_files.len().min(10);
                    for file in self.affected_files.iter().take(display_count) {
                        println!("  • {}", file.dimmed());
                    }
                    if self.affected_files.len() > display_count {
                        println!(
                            "  ... and {} more files",
                            (self.affected_files.len() - display_count)
                                .to_string()
                                .dimmed()
                        );
                    }
                }

                println!(
                    "\n{} {}",
                    "Snapshot taken:".bold(),
                    self.snapshot_timestamp
                        .format("%Y-%m-%d %H:%M:%S UTC")
                        .to_string()
                        .dimmed()
                );

                println!("{}", "=".repeat(80).yellow());
            }

            VerbosityLevel::Verbose => {
                // Verbose output - show all details including file sizes and previews
                println!("\n{}", "=".repeat(80).yellow());
                println!("{}", "Undo Preview (Verbose Mode)".bold().yellow());
                println!("{}", "=".repeat(80).yellow());

                let op_type = match self.operation_type {
                    OperationType::Pull => "PULL".green(),
                    OperationType::Push => "PUSH".blue(),
                };

                println!("\n{} {}", "Operation Type:".bold(), op_type);
                println!(
                    "{} {}",
                    "Performed at:".bold(),
                    self.operation_timestamp
                        .format("%Y-%m-%d %H:%M:%S UTC")
                        .to_string()
                        .cyan()
                );

                if let Some(branch) = &self.branch {
                    println!("{} {}", "Branch:".bold(), branch.cyan());
                }

                if let Some(commit) = &self.commit_hash {
                    println!("{} {} (full: {})", "Will reset to:".bold(), commit[..8].yellow(), commit.dimmed());
                }

                println!(
                    "\n{} {}",
                    "Total conversations affected:".bold(),
                    self.conversation_count.to_string().yellow()
                );

                if !self.affected_files.is_empty() {
                    println!("\n{} ({} total)", "Files to be restored:".bold(), self.affected_files.len());
                    for (idx, file) in self.affected_files.iter().enumerate() {
                        println!("  {}. {}", idx + 1, file);

                        // Try to show file size if file exists
                        if let Ok(metadata) = std::fs::metadata(file) {
                            let size_kb = metadata.len() as f64 / 1024.0;
                            println!("     {} {:.1} KB", "Size:".dimmed(), size_kb);
                        }
                    }
                }

                println!(
                    "\n{} {}",
                    "Snapshot created:".bold(),
                    self.snapshot_timestamp
                        .format("%Y-%m-%d %H:%M:%S UTC")
                        .to_string()
                        .cyan()
                );

                let time_diff = chrono::Utc::now().signed_duration_since(self.snapshot_timestamp);
                let days = time_diff.num_days();
                let hours = time_diff.num_hours() % 24;
                let mins = time_diff.num_minutes() % 60;
                println!("  {} {} days, {} hours, {} minutes ago",
                    "Age:".dimmed(),
                    days, hours, mins
                );

                println!("{}", "=".repeat(80).yellow());
            }
        }
    }
}

/// Preview the last pull operation without executing it
///
/// # Arguments
/// * `history_path` - Optional custom path for operation history (for testing)
///
/// # Returns
/// An `UndoPreview` with information about what would be undone
pub fn preview_undo_pull(history_path: Option<PathBuf>) -> Result<UndoPreview> {
    // Load operation history
    let history = OperationHistory::from_path(history_path)?;

    // Find the last pull operation
    let last_pull = history
        .get_last_operation_by_type(OperationType::Pull)
        .ok_or_else(|| anyhow!("No pull operation found in history to undo"))?;

    // Get the snapshot path
    let snapshot_path = last_pull.snapshot_path.as_ref().ok_or_else(|| {
        anyhow!(
            "No snapshot found for last pull operation. \
                Cannot undo without a snapshot."
        )
    })?;

    // Verify snapshot exists
    if !snapshot_path.exists() {
        return Err(anyhow!(
            "Snapshot file not found: {}. \
            The snapshot may have been deleted.",
            snapshot_path.display()
        ));
    }

    // Load the snapshot
    let snapshot = Snapshot::load_from_disk(snapshot_path)?;

    // Get list of affected files
    let affected_files: Vec<String> = snapshot.files.keys().cloned().collect();

    Ok(UndoPreview {
        operation_type: OperationType::Pull,
        operation_timestamp: last_pull.timestamp,
        branch: last_pull.branch.clone(),
        affected_files,
        conversation_count: last_pull.affected_conversations.len(),
        commit_hash: None,
        snapshot_timestamp: snapshot.timestamp,
    })
}

/// Preview the last push operation without executing it
///
/// # Arguments
/// * `history_path` - Optional custom path for operation history (for testing)
///
/// # Returns
/// An `UndoPreview` with information about what would be undone
pub fn preview_undo_push(history_path: Option<PathBuf>) -> Result<UndoPreview> {
    // Load operation history
    let history = OperationHistory::from_path(history_path)?;

    // Find the last push operation
    let last_push = history
        .get_last_operation_by_type(OperationType::Push)
        .ok_or_else(|| anyhow!("No push operation found in history to undo"))?;

    // Get the snapshot path
    let snapshot_path = last_push.snapshot_path.as_ref().ok_or_else(|| {
        anyhow!(
            "No snapshot found for last push operation. \
                Cannot undo without a snapshot."
        )
    })?;

    // Verify snapshot exists
    if !snapshot_path.exists() {
        return Err(anyhow!(
            "Snapshot file not found: {}. \
            The snapshot may have been deleted.",
            snapshot_path.display()
        ));
    }

    // Load the snapshot
    let snapshot = Snapshot::load_from_disk(snapshot_path)?;

    Ok(UndoPreview {
        operation_type: OperationType::Push,
        operation_timestamp: last_push.timestamp,
        branch: snapshot.branch.clone(),
        affected_files: Vec::new(), // Push doesn't restore files, just resets git
        conversation_count: last_push.affected_conversations.len(),
        commit_hash: snapshot.git_commit_hash.clone(),
        snapshot_timestamp: snapshot.timestamp,
    })
}
