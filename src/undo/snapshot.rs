use anyhow::{anyhow, Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::git::GitManager;
use crate::history::OperationType;

/// Represents a snapshot of conversation files at a point in time
///
/// Snapshots are created before each sync operation to enable undo functionality.
/// They capture the complete state of all conversation files that might be affected.
///
/// ## Differential Snapshots
///
/// To save disk space, snapshots can be differential - only storing files that changed
/// since the previous snapshot. This is controlled by the `base_snapshot_id` field:
/// - `None`: Full snapshot containing all files
/// - `Some(id)`: Differential snapshot containing only changes since base snapshot
///
/// When restoring a differential snapshot, we recursively load the chain of base
/// snapshots to reconstruct the full state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unique identifier for this snapshot
    pub snapshot_id: String,

    /// When this snapshot was created
    pub timestamp: chrono::DateTime<chrono::Utc>,

    /// Type of operation this snapshot was created for
    pub operation_type: OperationType,

    /// Git commit hash before the operation (for push operations)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit_hash: Option<String>,

    /// Mapping of file paths (relative to Claude projects dir) to their content
    ///
    /// We store the raw bytes to preserve exact file state including encoding.
    /// The HashMap key is a string path for JSON serialization compatibility.
    ///
    /// For differential snapshots, only contains files that changed/were added.
    #[serde(with = "base64_map")]
    pub files: HashMap<String, Vec<u8>>,

    /// Git branch name at the time of snapshot
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// Base snapshot ID for differential snapshots
    ///
    /// If present, this snapshot only contains changes relative to the base.
    /// The full state can be reconstructed by loading the chain of snapshots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_snapshot_id: Option<String>,

    /// Files that were deleted since the base snapshot
    ///
    /// Only populated for differential snapshots. Lists file paths that existed
    /// in the base snapshot but should be removed when restoring this snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted_files: Vec<String>,
}

/// Custom serialization for `HashMap<String, Vec<u8>>` using base64 encoding
///
/// This is necessary because JSON doesn't natively support binary data,
/// so we encode file contents as base64 strings for storage.
mod base64_map {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    pub fn serialize<S>(map: &HashMap<String, Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let base64_map: HashMap<String, String> = map
            .iter()
            .map(|(k, v)| (k.clone(), STANDARD.encode(v)))
            .collect();
        base64_map.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<String, Vec<u8>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let base64_map: HashMap<String, String> = HashMap::deserialize(deserializer)?;
        base64_map
            .into_iter()
            .map(|(k, v)| {
                STANDARD
                    .decode(&v)
                    .map(|bytes| (k, bytes))
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
}

impl Snapshot {
    /// Create a new snapshot from a set of file paths
    ///
    /// # Arguments
    /// * `operation_type` - Type of operation this snapshot is for
    /// * `file_paths` - Iterator of file paths to include in snapshot
    /// * `git_manager` - Optional git manager to capture commit hash
    ///
    /// # Returns
    /// A new Snapshot instance with all file contents captured
    pub fn create<P, I>(
        operation_type: OperationType,
        file_paths: I,
        git_manager: Option<&GitManager>,
    ) -> Result<Self>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = P>,
    {
        let snapshot_id = Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now();
        let mut files = HashMap::new();

        // Capture current state of all specified files
        for path in file_paths {
            let path = path.as_ref();

            // Use direct read instead of checking existence first to avoid TOCTOU
            match fs::read(path) {
                Ok(content) => {
                    // Store with path as string for JSON compatibility
                    files.insert(path.to_string_lossy().to_string(), content);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // File doesn't exist - this is expected in some cases (e.g., deleted files)
                    // Just skip it without error
                    continue;
                }
                Err(e) => {
                    // Other errors should be reported
                    return Err(e).with_context(|| {
                        format!("Failed to read file for snapshot: {}", path.display())
                    });
                }
            }
        }

        // Capture git commit hash if available (for push operations)
        let (git_commit_hash, branch) = if let Some(git) = git_manager {
            let hash = git.current_commit_hash()?;
            let branch = git.current_branch().ok();
            (Some(hash), branch)
        } else {
            (None, None)
        };

        Ok(Snapshot {
            snapshot_id,
            timestamp,
            operation_type,
            git_commit_hash,
            files,
            branch,
            base_snapshot_id: None,
            deleted_files: Vec::new(),
        })
    }

    /// Create a differential snapshot that only stores changes since the last snapshot
    ///
    /// This significantly reduces disk usage by only storing files that have changed.
    ///
    /// # Arguments
    /// * `operation_type` - Type of operation this snapshot is for
    /// * `file_paths` - Iterator of file paths to include in snapshot
    /// * `git_manager` - Optional git manager to capture commit hash
    /// * `snapshots_dir` - Optional custom snapshots directory (for testing)
    ///
    /// # Returns
    /// A new differential Snapshot, or a full snapshot if no base exists
    pub fn create_differential_with_dir<P, I>(
        operation_type: OperationType,
        file_paths: I,
        git_manager: Option<&GitManager>,
        snapshots_dir: Option<&Path>,
    ) -> Result<Self>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = P>,
    {
        // Try to find the most recent snapshot of the same operation type
        let base_snapshot = Self::find_latest_snapshot(operation_type, snapshots_dir)?;

        let snapshot_id = Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now();

        // Collect current file paths and their content
        let mut current_files: HashMap<String, Vec<u8>> = HashMap::new();
        for path in file_paths {
            let path = path.as_ref();
            match fs::read(path) {
                Ok(content) => {
                    current_files.insert(path.to_string_lossy().to_string(), content);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("Failed to read file for snapshot: {}", path.display())
                    });
                }
            }
        }

        // If no base snapshot exists, create a full snapshot
        let (files, base_snapshot_id, deleted_files) = if let Some(base) = base_snapshot {
            let mut changed_files = HashMap::new();
            let mut deleted = Vec::new();

            // Reconstruct the full state from the base snapshot chain
            // This is crucial: if the base is differential, we need the complete state,
            // not just the changed files in that differential snapshot
            let base_full_state = base.reconstruct_full_state_with_dir(snapshots_dir)?;

            // Find files that changed or are new
            for (path, content) in &current_files {
                if let Some(base_content) = base_full_state.get(path) {
                    // File exists in base - only include if content changed
                    if base_content != content {
                        changed_files.insert(path.clone(), content.clone());
                    }
                } else {
                    // New file - always include
                    changed_files.insert(path.clone(), content.clone());
                }
            }

            // Find files that were deleted
            for path in base_full_state.keys() {
                if !current_files.contains_key(path) {
                    deleted.push(path.clone());
                }
            }

            (changed_files, Some(base.snapshot_id), deleted)
        } else {
            // No base snapshot - include all files (full snapshot)
            (current_files, None, Vec::new())
        };

        // Capture git commit hash if available
        let (git_commit_hash, branch) = if let Some(git) = git_manager {
            let hash = git.current_commit_hash()?;
            let branch = git.current_branch().ok();
            (Some(hash), branch)
        } else {
            (None, None)
        };

        Ok(Snapshot {
            snapshot_id,
            timestamp,
            operation_type,
            git_commit_hash,
            files,
            branch,
            base_snapshot_id,
            deleted_files,
        })
    }

    /// Create a differential snapshot using the default snapshots directory
    ///
    /// This is a convenience wrapper around `create_differential_with_dir` that
    /// uses the default snapshots directory.
    ///
    /// # Arguments
    /// * `operation_type` - Type of operation this snapshot is for
    /// * `file_paths` - Iterator of file paths to include in snapshot
    /// * `git_manager` - Optional git manager to capture commit hash
    ///
    /// # Returns
    /// A new differential Snapshot, or a full snapshot if no base exists
    pub fn create_differential<P, I>(
        operation_type: OperationType,
        file_paths: I,
        git_manager: Option<&GitManager>,
    ) -> Result<Self>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = P>,
    {
        Self::create_differential_with_dir(operation_type, file_paths, git_manager, None)
    }

    /// Find the most recent snapshot of a given operation type
    ///
    /// # Arguments
    /// * `operation_type` - Type of operation to find snapshots for
    /// * `custom_dir` - Optional custom snapshots directory (for testing)
    ///
    /// # Returns
    /// The most recent snapshot, or None if no snapshots exist
    pub(crate) fn find_latest_snapshot(operation_type: OperationType, custom_dir: Option<&Path>) -> Result<Option<Snapshot>> {
        let snapshots_dir = if let Some(dir) = custom_dir {
            dir.to_path_buf()
        } else {
            Self::snapshots_dir()?
        };

        if !snapshots_dir.exists() {
            return Ok(None);
        }

        let mut snapshots: Vec<(PathBuf, chrono::DateTime<chrono::Utc>)> = Vec::new();

        // Scan snapshots directory
        for entry in fs::read_dir(&snapshots_dir)? {
            let entry = entry?;
            let path = entry.path();

            if !path.extension().map_or(false, |ext| ext == "json") {
                continue;
            }

            // Quick parse to get timestamp and operation type without loading full snapshot
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(snapshot) = serde_json::from_str::<Snapshot>(&content) {
                    if snapshot.operation_type == operation_type {
                        snapshots.push((path, snapshot.timestamp));
                    }
                }
            }
        }

        // Sort by timestamp descending and get the most recent
        snapshots.sort_by(|a, b| b.1.cmp(&a.1));

        if let Some((path, _)) = snapshots.first() {
            let snapshot = Self::load_from_disk(path)?;
            Ok(Some(snapshot))
        } else {
            Ok(None)
        }
    }

    /// Save this snapshot to disk
    ///
    /// Snapshots are saved to `~/.claude-code-sync/snapshots/{snapshot_id}.json`
    ///
    /// # Arguments
    /// * `custom_path` - Optional custom directory to save snapshot (for testing)
    pub fn save_to_disk(&self, custom_path: Option<&Path>) -> Result<PathBuf> {
        let snapshot_dir = if let Some(path) = custom_path {
            path.to_path_buf()
        } else {
            Self::snapshots_dir()?
        };

        // Ensure snapshots directory exists
        fs::create_dir_all(&snapshot_dir).with_context(|| {
            format!(
                "Failed to create snapshots directory: {}",
                snapshot_dir.display()
            )
        })?;

        let snapshot_path = snapshot_dir.join(format!("{}.json", self.snapshot_id));

        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize snapshot to JSON")?;

        fs::write(&snapshot_path, &json).with_context(|| {
            format!(
                "Failed to write snapshot to disk: {}",
                snapshot_path.display()
            )
        })?;

        // Log snapshot size information
        let size_mb = json.len() as f64 / (1024.0 * 1024.0);
        let snapshot_type = if self.base_snapshot_id.is_some() {
            "differential"
        } else {
            "full"
        };

        log::info!(
            "Created {} snapshot: {} ({:.1} MB, {} files)",
            snapshot_type,
            self.snapshot_id,
            size_mb,
            self.files.len()
        );

        if size_mb > 100.0 {
            log::warn!("Large snapshot size - consider cleaning up old conversation files");
        }

        Ok(snapshot_path)
    }

    /// Load a snapshot from disk
    ///
    /// # Arguments
    /// * `snapshot_path` - Path to the snapshot JSON file
    ///
    /// # Behavior
    /// - Logs snapshot size information
    /// - Warns if snapshot is unusually large (>100MB) but still loads it
    /// - Snapshots are critical for undo functionality and should never be skipped
    ///
    /// # Errors
    /// Returns error only if file cannot be read or parsed
    pub fn load_from_disk<P: AsRef<Path>>(snapshot_path: P) -> Result<Self> {
        let snapshot_path = snapshot_path.as_ref();

        // Get file size for logging
        let metadata = fs::metadata(snapshot_path).with_context(|| {
            format!(
                "Failed to read snapshot file metadata: {}",
                snapshot_path.display()
            )
        })?;

        let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);

        // Log size information - always show this for visibility
        if size_mb > 100.0 {
            println!(
                "    {} Loading large snapshot: {} ({:.1} MB) - This may take a moment...",
                "⚠".yellow(),
                snapshot_path.file_name().unwrap().to_string_lossy().cyan(),
                size_mb
            );
        } else {
            println!(
                "    {} snapshot: {} ({:.1} MB)",
                "Loading".dimmed(),
                snapshot_path.file_name().unwrap().to_string_lossy().cyan(),
                size_mb
            );
        }

        let content = fs::read_to_string(snapshot_path).with_context(|| {
            format!("Failed to read snapshot file: {}", snapshot_path.display())
        })?;

        let snapshot: Snapshot = serde_json::from_str(&content).with_context(|| {
            format!("Failed to parse snapshot JSON: {}", snapshot_path.display())
        })?;

        // Log file count information
        println!("    {} {} files", "Contains".dimmed(), snapshot.files.len());

        // Warn if unusually large number of files
        if snapshot.files.len() > 1000 {
            println!(
                "    {} Large number of files - this is a full (non-differential) snapshot",
                "Note:".yellow()
            );
        }

        Ok(snapshot)
    }

    /// Reconstruct the full file state by walking the snapshot chain
    ///
    /// For differential snapshots, this loads all base snapshots recursively
    /// and merges them to produce the complete file state.
    ///
    /// # Arguments
    /// * `snapshots_dir` - Optional custom snapshots directory (for testing)
    ///
    /// # Returns
    /// A HashMap containing the full state of all files
    pub fn reconstruct_full_state_with_dir(&self, snapshots_dir: Option<&Path>) -> Result<HashMap<String, Vec<u8>>> {
        let mut state = HashMap::new();

        // If this is a differential snapshot, load the base chain
        if let Some(base_id) = &self.base_snapshot_id {
            let snapshots_dir = if let Some(dir) = snapshots_dir {
                dir.to_path_buf()
            } else {
                Self::snapshots_dir()?
            };
            let base_path = snapshots_dir.join(format!("{}.json", base_id));

            if !base_path.exists() {
                return Err(anyhow!(
                    "Base snapshot not found: {}. \
                    The snapshot chain is broken. Cannot restore differential snapshot.",
                    base_id
                ));
            }

            // Recursively load the base snapshot's state
            let base_snapshot = Self::load_from_disk(&base_path)?;
            state = base_snapshot.reconstruct_full_state_with_dir(Some(&snapshots_dir))?;
        }

        // Apply this snapshot's changes on top of the base state
        for (path, content) in &self.files {
            state.insert(path.clone(), content.clone());
        }

        // Remove deleted files
        for deleted_path in &self.deleted_files {
            state.remove(deleted_path);
        }

        Ok(state)
    }

    /// Reconstruct the full file state using the default snapshots directory
    ///
    /// This is a convenience wrapper around `reconstruct_full_state_with_dir`.
    pub(crate) fn reconstruct_full_state(&self) -> Result<HashMap<String, Vec<u8>>> {
        self.reconstruct_full_state_with_dir(None)
    }

    /// Get the default snapshots directory
    pub(crate) fn snapshots_dir() -> Result<PathBuf> {
        crate::config::ConfigManager::snapshots_dir()
    }

}
