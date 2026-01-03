use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::history::{OperationHistory, OperationType};
use crate::scm;
use super::snapshot::Snapshot;

/// Undo the last pull operation
///
/// This function:
/// 1. Loads the operation history
/// 2. Finds the most recent pull operation
/// 3. Loads the snapshot that was taken before that pull
/// 4. Restores all files to their pre-pull state
/// 5. Updates the operation history to mark the pull as undone
///
/// # Arguments
/// * `history_path` - Optional custom path for operation history (for testing)
/// * `allowed_base_dir` - Optional base directory for path validation (for testing)
///
/// # Returns
/// A summary message describing what was undone
pub fn undo_pull(history_path: Option<PathBuf>, allowed_base_dir: Option<&Path>) -> Result<String> {
    // Load operation history
    let history = OperationHistory::from_path(history_path.clone())?;

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

    // Verify this is indeed a pull snapshot
    if snapshot.operation_type != OperationType::Pull {
        return Err(anyhow!(
            "Snapshot type mismatch: expected pull, found {}",
            snapshot.operation_type.as_str()
        ));
    }

    // Get the list of files that will be restored
    let restored_files: Vec<String> = snapshot.files.keys().cloned().collect();
    let file_count = restored_files.len();

    // TRANSACTION-LIKE ORDERING: Update history FIRST, then restore files.
    // This ensures that if file restoration fails, the history is still consistent
    // and accurately reflects that we've attempted the undo. The snapshot file
    // remains on disk until we successfully complete the restoration.

    // Step 1: Remove the pull operation from history
    let mut history = OperationHistory::from_path(history_path.clone())?;
    history
        .remove_last_operation_by_type(OperationType::Pull, history_path.clone())
        .context("Failed to remove pull operation from history")?;

    // Step 2: Restore the snapshot files
    // If this fails, the history is already updated (which is safer than having
    // an inconsistent history state)
    snapshot
        .restore_with_base(allowed_base_dir)
        .context("Failed to restore snapshot")?;

    // Step 3: Clean up the snapshot file (only after successful restoration)
    if let Err(e) = fs::remove_file(snapshot_path) {
        eprintln!(
            "Warning: Failed to remove snapshot file {}: {}",
            snapshot_path.display(),
            e
        );
    }

    Ok(format!(
        "Successfully undone last pull operation.\n\
        Restored {} files to their pre-pull state.\n\
        Snapshot taken at: {}",
        file_count,
        snapshot.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
    ))
}

/// Undo the last push operation
///
/// This function:
/// 1. Loads the operation history
/// 2. Finds the most recent push operation
/// 3. Gets the commit hash from the operation record (no snapshot needed!)
/// 4. Uses SCM abstraction to reset the repository to the previous commit
/// 5. Updates the operation history to mark the push as undone
/// 6. Warns the user if they need to force push to the remote
///
/// Note: Push operations no longer create file snapshots. Git/Mercurial already
/// tracks history, so we just store the commit hash and use `reset` to undo.
///
/// # Arguments
/// * `repo_path` - Path to the SCM repository
/// * `history_path` - Optional custom path for operation history (for testing)
///
/// # Returns
/// A summary message describing what was undone and any required follow-up actions
pub fn undo_push(repo_path: &Path, history_path: Option<PathBuf>) -> Result<String> {
    // Load operation history
    let history = OperationHistory::from_path(history_path.clone())?;

    // Find the last push operation
    let last_push = history
        .get_last_operation_by_type(OperationType::Push)
        .ok_or_else(|| anyhow!("No push operation found in history to undo"))?;

    // Get the commit hash to reset to
    // New operations store this in commit_hash field directly
    // Legacy operations may have it in a snapshot file
    let target_commit = if let Some(ref hash) = last_push.commit_hash {
        hash.clone()
    } else if let Some(ref snapshot_path) = last_push.snapshot_path {
        // Legacy: load from snapshot file
        if !snapshot_path.exists() {
            return Err(anyhow!(
                "No commit hash in operation record and snapshot file not found: {}",
                snapshot_path.display()
            ));
        }
        let snapshot = Snapshot::load_from_disk(snapshot_path)?;
        snapshot.git_commit_hash.ok_or_else(|| {
            anyhow!("No commit hash found in snapshot")
        })?
    } else {
        return Err(anyhow!(
            "No commit hash found for last push operation. Cannot undo."
        ));
    };

    // Open the SCM repository
    let repo = scm::open(repo_path)
        .with_context(|| format!("Failed to open repository at {}", repo_path.display()))?;

    // Check if we need to warn about remote (before reset)
    let branch_name = last_push.branch.as_deref().unwrap_or("unknown");
    let needs_force_push = repo.has_remote("origin");

    // TRANSACTION-LIKE ORDERING: Update history FIRST, then perform reset.
    // This ensures that if the reset fails, the history is still consistent.

    // Step 1: Remove the push operation from history
    let mut history = OperationHistory::from_path(history_path.clone())?;
    history
        .remove_last_operation_by_type(OperationType::Push, history_path.clone())
        .context("Failed to remove push operation from history")?;

    // Step 2: Perform the reset
    repo.reset_soft(&target_commit)
        .context("Failed to reset repository to previous commit")?;

    // Step 3: Clean up legacy snapshot file if it exists
    if let Some(ref snapshot_path) = last_push.snapshot_path {
        if snapshot_path.exists() {
            if let Err(e) = fs::remove_file(snapshot_path) {
                eprintln!(
                    "Warning: Failed to remove snapshot file {}: {}",
                    snapshot_path.display(),
                    e
                );
            }
        }
    }

    let short_commit = if target_commit.len() >= 8 {
        &target_commit[..8]
    } else {
        &target_commit
    };

    let mut summary = format!(
        "Successfully undone last push operation.\n\
        Reset repository to commit: {}\n\
        Branch: {}\n\
        Operation was at: {}",
        short_commit,
        branch_name,
        last_push.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
    );

    if needs_force_push {
        summary.push_str(&format!(
            "\n\n\
            WARNING: The remote repository was updated by the push.\n\
            You will need to force push to update the remote:\n\
            (For Git: git push --force origin {branch_name})"
        ));
    }

    Ok(summary)
}
