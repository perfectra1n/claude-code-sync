use anyhow::{anyhow, Result};
use std::path::PathBuf;

use super::snapshot::Snapshot;
use crate::history::{OperationHistory, OperationType};

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
    /// Snapshot creation timestamp (None when the operation has no snapshot,
    /// e.g. modern push records that only store a commit hash)
    pub snapshot_timestamp: Option<chrono::DateTime<chrono::Utc>>,
    /// Artifact record entries this undo forgets (this repository's only)
    /// instead of restoring record files wholesale — surfaced so the
    /// preview matches what the undo actually does.
    pub record_surgery: bool,
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
                println!(
                    "Undo {}: {} conversations affected",
                    op_type, self.conversation_count
                );
                if !self.affected_files.is_empty() {
                    println!("  {} files will be restored", self.affected_files.len());
                }
                if self.record_surgery {
                    println!(
                        "  artifact record entries for this repository may be forgotten or re-applied"
                    );
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
                    let short_hash = if commit.len() >= 8 {
                        &commit[..8]
                    } else {
                        commit.as_str()
                    };
                    println!("{} {}", "Will reset to:".bold(), short_hash.yellow());
                }

                println!(
                    "\n{} {}",
                    "Conversations affected:".bold(),
                    self.conversation_count.to_string().yellow()
                );

                if self.record_surgery {
                    println!(
                        "\n{} artifact record entries (this repository's) may be forgotten or re-applied",
                        "Record surgery:".bold()
                    );
                }
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

                if let Some(snapshot_ts) = self.snapshot_timestamp {
                    println!(
                        "\n{} {}",
                        "Snapshot taken:".bold(),
                        snapshot_ts
                            .format("%Y-%m-%d %H:%M:%S UTC")
                            .to_string()
                            .dimmed()
                    );
                }

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
                    let short_hash = if commit.len() >= 8 {
                        &commit[..8]
                    } else {
                        commit.as_str()
                    };
                    println!(
                        "{} {} (full: {})",
                        "Will reset to:".bold(),
                        short_hash.yellow(),
                        commit.dimmed()
                    );
                }

                println!(
                    "\n{} {}",
                    "Total conversations affected:".bold(),
                    self.conversation_count.to_string().yellow()
                );

                if !self.affected_files.is_empty() {
                    println!(
                        "\n{} ({} total)",
                        "Files to be restored:".bold(),
                        self.affected_files.len()
                    );
                    for (idx, file) in self.affected_files.iter().enumerate() {
                        println!("  {}. {}", idx + 1, file);

                        // Try to show file size if file exists
                        if let Ok(metadata) = std::fs::metadata(file) {
                            let size_kb = metadata.len() as f64 / 1024.0;
                            println!("     {} {:.1} KB", "Size:".dimmed(), size_kb);
                        }
                    }
                }

                if let Some(snapshot_ts) = self.snapshot_timestamp {
                    println!(
                        "\n{} {}",
                        "Snapshot created:".bold(),
                        snapshot_ts
                            .format("%Y-%m-%d %H:%M:%S UTC")
                            .to_string()
                            .cyan()
                    );

                    let time_diff = chrono::Utc::now().signed_duration_since(snapshot_ts);
                    let days = time_diff.num_days();
                    let hours = time_diff.num_hours() % 24;
                    let mins = time_diff.num_minutes() % 60;
                    println!(
                        "  {} {} days, {} hours, {} minutes ago",
                        "Age:".dimmed(),
                        days,
                        hours,
                        mins
                    );
                }

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

    // Find the last pull an undo can act on (snapshotless records are
    // skipped, matching undo_pull).
    let last_pull = history.get_last_undoable_pull().ok_or_else(|| {
        if history
            .list_operations()
            .iter()
            .any(|op| op.operation_type == OperationType::Pull)
        {
            anyhow!(
                "No pull to undo: the recorded pulls changed no machine state \
                 (kept local only), so there is nothing to restore"
            )
        } else {
            anyhow!("No pull operation found in history to undo")
        }
    })?;

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
    let mut snapshot = Snapshot::load_from_disk(snapshot_path)?;

    // Legacy snapshots (no declarations) get their record files
    // recognized by the same rule undo applies, so this preview and the
    // undo itself agree on what is restored surgically.
    // A stripped record blob is exactly what the undo will warn and pin
    // about — the preview must announce the same surgery (and the blob
    // already left `files`, so it does not preview as a plain restore).
    let stripped = snapshot.promote_legacy_records();

    // Affected files. Record files the snapshot declares are restored
    // surgically (this repository's entry only), so listing them as "will
    // be restored" would overstate the blast radius on files every sync
    // repository shares; an artifact sharing a record's file name is a
    // plain file as far as undo is concerned.
    let affected_files: Vec<String> = snapshot
        .files
        .keys()
        .filter(|k| !snapshot.record_files.iter().any(|r| r == *k))
        .cloned()
        .collect();

    Ok(UndoPreview {
        operation_type: OperationType::Pull,
        operation_timestamp: last_pull.timestamp,
        branch: last_pull.branch.clone(),
        affected_files,
        conversation_count: last_pull.affected_conversations.len(),
        commit_hash: None,
        snapshot_timestamp: Some(snapshot.timestamp),
        record_surgery: stripped
            || !snapshot.record_files.is_empty()
            || !snapshot.created_record_files.is_empty(),
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

    // Modern push records store the reset target directly in commit_hash and have
    // no snapshot file (git itself holds the history). Only legacy records carry a
    // snapshot; mirror the fallback order used by undo_push in operations.rs.
    if let Some(ref hash) = last_push.commit_hash {
        return Ok(UndoPreview {
            operation_type: OperationType::Push,
            operation_timestamp: last_push.timestamp,
            branch: last_push.branch.clone(),
            affected_files: Vec::new(), // Push doesn't restore files, just resets git
            conversation_count: last_push.affected_conversations.len(),
            commit_hash: Some(hash.clone()),
            snapshot_timestamp: None,
            record_surgery: false,
        });
    }

    // Legacy: get the snapshot path
    let snapshot_path = last_push.snapshot_path.as_ref().ok_or_else(|| {
        anyhow!(
            "No commit hash or snapshot found for last push operation. \
                Cannot undo."
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
        snapshot_timestamp: Some(snapshot.timestamp),
        record_surgery: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{OperationRecord, OperationType};
    use tempfile::TempDir;

    #[test]
    fn test_preview_undo_push_with_commit_hash_only() {
        // Modern push records store only commit_hash (no snapshot file); preview
        // must succeed from the record alone instead of demanding a snapshot.
        let temp_dir = TempDir::new().unwrap();
        let history_path = temp_dir.path().join("operation-history.json");

        let mut record =
            OperationRecord::new(OperationType::Push, Some("main".to_string()), vec![]);
        record.commit_hash = Some("abcdef1234567890".to_string());
        assert!(record.snapshot_path.is_none());

        let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();
        history.add_operation(record).unwrap();

        let preview = preview_undo_push(Some(history_path)).unwrap();
        assert_eq!(preview.commit_hash.as_deref(), Some("abcdef1234567890"));
        assert_eq!(preview.branch.as_deref(), Some("main"));
        assert!(preview.affected_files.is_empty());
        assert!(preview.snapshot_timestamp.is_none());
    }
}

#[cfg(test)]
mod legacy_tests {
    use super::*;
    use crate::undo::test_support::HistoryBuilder;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn preview_agrees_with_undo_on_a_legacy_snapshot() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");
        let repo_root = temp_dir.path().join("sync-repo");

        fs::create_dir_all(temp_dir.path().join("projects/x")).unwrap();
        let artifact = temp_dir.path().join("projects/x/fact.md");
        fs::write(&artifact, "pre-pull").unwrap();

        // A legacy snapshot: the shared record rides among `files` with no
        // declarations. The preview must agree with the undo — record
        // surgery, not a wholesale restore listing.
        let record_path = temp_dir.path().join(".claude-code-sync-tracked.json");
        // Built with serde_json so a Windows path's backslashes are escaped.
        let record = serde_json::json!({ "repos": {
            repo_root.to_string_lossy().into_owned(): ["projects/x/fact.md"],
        }})
        .to_string();
        let mut snapshot = Snapshot::create(OperationType::Pull, vec![&artifact], None).unwrap();
        snapshot.files.insert(
            record_path.to_string_lossy().to_string(),
            record.into_bytes(),
        );
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        HistoryBuilder::new(&history_path)
            .push_on_repo(
                OperationType::Pull,
                "main",
                Some(&snapshot_path),
                Some(&repo_root),
            )
            .save();

        let preview = preview_undo_pull(Some(history_path)).unwrap();
        assert!(
            preview.record_surgery,
            "the preview announces the surgical record restore the undo performs"
        );
        assert!(
            !preview
                .affected_files
                .iter()
                .any(|f| f.contains(".claude-code-sync-tracked.json")),
            "the shared record is not listed as an ordinary wholesale restore"
        );
        assert!(preview.affected_files.iter().any(|f| f.contains("fact.md")));
    }

    #[test]
    fn snapshotless_pulls_get_an_honest_error() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        HistoryBuilder::new(&history_path)
            .push(OperationType::Pull, "main", None)
            .push(OperationType::Pull, "main", None)
            .save();

        let err = preview_undo_pull(Some(history_path))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("changed no machine state"),
            "the error says WHY there is nothing to undo: {err}"
        );
    }
}
