use anyhow::Result;
use log::warn;
use std::fs;
use std::path::Path;

use crate::history::OperationType;
use super::snapshot::Snapshot;

/// Configuration for snapshot cleanup
pub struct SnapshotCleanupConfig {
    /// Keep at most this many snapshots per operation type (pull/push)
    pub max_count_per_type: usize,
    /// Keep snapshots newer than this many days
    pub max_age_days: i64,
}

impl Default for SnapshotCleanupConfig {
    fn default() -> Self {
        Self {
            max_count_per_type: 5,
            max_age_days: 7,
        }
    }
}

/// Clean up old snapshots based on age and count limits
///
/// This removes snapshots that don't meet EITHER of the following criteria:
/// - Within the last N snapshots of each operation type
/// - Created within the last X days
///
/// # Arguments
/// * `config` - Cleanup configuration (defaults: keep last 5 per type, last 7 days)
/// * `dry_run` - If true, show what would be deleted without actually deleting
/// * `snapshots_dir` - Optional custom snapshots directory (for testing)
///
/// # Returns
/// Number of snapshots deleted
pub fn cleanup_old_snapshots_with_dir(
    config: Option<SnapshotCleanupConfig>,
    dry_run: bool,
    snapshots_dir: Option<&Path>,
) -> Result<usize> {
    let config = config.unwrap_or_default();
    let snapshots_dir = if let Some(dir) = snapshots_dir {
        dir.to_path_buf()
    } else {
        Snapshot::snapshots_dir()?
    };

    if !snapshots_dir.exists() {
        return Ok(0);
    }

    // Collect all snapshots with metadata
    let mut pull_snapshots: Vec<(std::path::PathBuf, chrono::DateTime<chrono::Utc>)> = Vec::new();
    let mut push_snapshots: Vec<(std::path::PathBuf, chrono::DateTime<chrono::Utc>)> = Vec::new();

    for entry in fs::read_dir(&snapshots_dir)? {
        let entry = entry?;
        let path = entry.path();

        if !path.extension().map_or(false, |ext| ext == "json") {
            continue;
        }

        // Load snapshot metadata
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(snapshot) = serde_json::from_str::<Snapshot>(&content) {
                match snapshot.operation_type {
                    OperationType::Pull => pull_snapshots.push((path, snapshot.timestamp)),
                    OperationType::Push => push_snapshots.push((path, snapshot.timestamp)),
                }
            }
        }
    }

    // Sort by timestamp descending (newest first)
    pull_snapshots.sort_by(|a, b| b.1.cmp(&a.1));
    push_snapshots.sort_by(|a, b| b.1.cmp(&a.1));

    // Determine which snapshots to keep
    let now = chrono::Utc::now();
    let age_threshold = now - chrono::Duration::days(config.max_age_days);

    let mut to_delete = Vec::new();

    // Process pull snapshots
    for (idx, (path, timestamp)) in pull_snapshots.iter().enumerate() {
        let within_count_limit = idx < config.max_count_per_type;
        let within_age_limit = *timestamp >= age_threshold;

        // Delete if it doesn't meet EITHER criterion
        if !within_count_limit && !within_age_limit {
            to_delete.push(path.clone());
        }
    }

    // Process push snapshots
    for (idx, (path, timestamp)) in push_snapshots.iter().enumerate() {
        let within_count_limit = idx < config.max_count_per_type;
        let within_age_limit = *timestamp >= age_threshold;

        if !within_count_limit && !within_age_limit {
            to_delete.push(path.clone());
        }
    }

    // Delete the snapshots (or just report in dry run mode)
    let deleted_count = to_delete.len();

    if dry_run {
        println!("Would delete {} snapshots:", deleted_count);
        for path in &to_delete {
            println!("  - {}", path.display());
        }
    } else {
        for path in &to_delete {
            if let Err(e) = fs::remove_file(path) {
                warn!("Failed to delete snapshot {}: {}", path.display(), e);
            }
        }
    }

    Ok(deleted_count)
}

/// Clean up old snapshots using the default snapshots directory
///
/// This is a convenience wrapper around `cleanup_old_snapshots_with_dir`.
///
/// # Arguments
/// * `config` - Cleanup configuration (defaults: keep last 5 per type, last 7 days)
/// * `dry_run` - If true, show what would be deleted without actually deleting
///
/// # Returns
/// Number of snapshots deleted
pub fn cleanup_old_snapshots(
    config: Option<SnapshotCleanupConfig>,
    dry_run: bool,
) -> Result<usize> {
    cleanup_old_snapshots_with_dir(config, dry_run, None)
}
