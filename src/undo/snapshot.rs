use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

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
    /// * `commit_hash` - Optional git commit hash to store in the snapshot
    ///
    /// # Returns
    /// A new Snapshot instance with all file contents captured
    pub fn create<P, I>(
        operation_type: OperationType,
        file_paths: I,
        commit_hash: Option<&str>,
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

        Ok(Snapshot {
            snapshot_id,
            timestamp,
            operation_type,
            git_commit_hash: commit_hash.map(|s| s.to_string()),
            files,
            branch: None,
            base_snapshot_id: None,
            deleted_files: Vec::new(),
        })
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

        // Log snapshot size information (to file only, UI output is handled by caller)
        let size_mb = json.len() as f64 / (1024.0 * 1024.0);
        let snapshot_type = if self.base_snapshot_id.is_some() {
            "differential"
        } else {
            "full"
        };

        log::debug!(
            "Created {} snapshot: {} ({:.1} MB, {} files)",
            snapshot_type,
            self.snapshot_id,
            size_mb,
            self.files.len()
        );

        if size_mb > 100.0 {
            log::debug!("Large snapshot size - consider cleaning up old conversation files");
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

    /// Get the default snapshots directory
    pub(crate) fn snapshots_dir() -> Result<PathBuf> {
        crate::config::ConfigManager::snapshots_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::undo::test_support::{create_test_file, setup_test_repo};
    use tempfile::tempdir;

    #[test]
    fn test_snapshot_create_and_save() {
        let temp_dir = tempdir().unwrap();
        let file1 = create_test_file(temp_dir.path(), "file1.txt", "content 1");
        let file2 = create_test_file(temp_dir.path(), "file2.txt", "content 2");

        let snapshot = Snapshot::create(OperationType::Pull, vec![&file1, &file2], None).unwrap();

        assert_eq!(snapshot.operation_type, OperationType::Pull);
        assert_eq!(snapshot.files.len(), 2);
        assert!(snapshot.git_commit_hash.is_none());

        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
        assert!(snapshot_path.exists());
    }

    #[test]
    fn test_snapshot_with_commit_hash() {
        let (temp_dir, repo) = setup_test_repo();
        let file1 = temp_dir.path().join("test.txt");
        let commit_hash = repo.current_commit_hash().unwrap();

        let snapshot =
            Snapshot::create(OperationType::Push, vec![&file1], Some(&commit_hash)).unwrap();

        assert_eq!(snapshot.operation_type, OperationType::Push);
        let stored_hash = snapshot.git_commit_hash.unwrap();
        assert_eq!(stored_hash.len(), 40); // Git SHA-1 hash length
        assert_eq!(stored_hash, commit_hash);
    }

    #[test]
    fn test_snapshot_save_and_load() {
        let temp_dir = tempdir().unwrap();
        let file1 = create_test_file(temp_dir.path(), "file1.txt", "test content");

        let original = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();

        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = original.save_to_disk(Some(&snapshots_dir)).unwrap();

        let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();

        assert_eq!(loaded.snapshot_id, original.snapshot_id);
        assert_eq!(loaded.operation_type, original.operation_type);
        assert_eq!(loaded.files.len(), original.files.len());
    }

    #[test]
    fn test_snapshot_handles_binary_files() {
        let temp_dir = tempdir().unwrap();
        let binary_file = temp_dir.path().join("binary.dat");

        let binary_content: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0x01, 0x02, 0x03];
        fs::write(&binary_file, &binary_content).unwrap();

        let snapshot = Snapshot::create(OperationType::Pull, vec![&binary_file], None).unwrap();

        let key = binary_file.to_string_lossy().to_string();
        assert_eq!(snapshot.files.get(&key).unwrap(), &binary_content);

        // The base64 serde shim has to survive a disk round-trip, not just
        // hold the bytes in memory.
        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();
        assert_eq!(loaded.files.get(&key).unwrap(), &binary_content);
    }

    #[test]
    fn test_snapshot_serialization_with_special_characters() {
        let temp_dir = tempdir().unwrap();
        let file_with_unicode = temp_dir.path().join("日本語.txt");
        fs::write(&file_with_unicode, "Hello 世界").unwrap();

        let snapshot =
            Snapshot::create(OperationType::Pull, vec![&file_with_unicode], None).unwrap();

        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();

        let content = loaded.files.values().next().unwrap();
        assert_eq!(String::from_utf8_lossy(content), "Hello 世界");
    }

    #[test]
    fn test_base64_encoding_for_binary_data() {
        let temp_dir = tempdir().unwrap();

        // Every possible byte value, including those that are not valid UTF-8.
        let binary_file = temp_dir.path().join("binary.dat");
        let binary_data: Vec<u8> = (0..=255).collect();
        fs::write(&binary_file, &binary_data).unwrap();

        let snapshot = Snapshot::create(OperationType::Pull, vec![&binary_file], None).unwrap();

        let json = serde_json::to_string(&snapshot).unwrap();
        let _parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        let deserialized: Snapshot = serde_json::from_str(&json).unwrap();

        let key = binary_file.to_string_lossy().to_string();
        assert_eq!(
            snapshot.files.get(&key).unwrap(),
            deserialized.files.get(&key).unwrap()
        );
        assert_eq!(deserialized.files.get(&key).unwrap(), &binary_data);
    }

    #[test]
    fn test_empty_snapshot() {
        let snapshot = Snapshot::create::<PathBuf, _>(OperationType::Pull, vec![], None).unwrap();

        assert_eq!(snapshot.files.len(), 0);
        assert!(snapshot.git_commit_hash.is_none());

        let temp_dir = tempdir().unwrap();
        let snapshot_path = snapshot.save_to_disk(Some(temp_dir.path())).unwrap();

        let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();
        assert_eq!(loaded.files.len(), 0);

        // Restoring nothing must be a no-op, not an error.
        loaded.restore().unwrap();
    }

    #[test]
    fn test_snapshot_create_handles_missing_files() {
        let temp_dir = tempdir().unwrap();

        let existing_file = create_test_file(temp_dir.path(), "exists.txt", "content");
        let missing_file = temp_dir.path().join("does_not_exist.txt");

        // A path that has already been deleted is skipped, not an error.
        let snapshot = Snapshot::create(
            OperationType::Pull,
            vec![&existing_file, &missing_file],
            None,
        )
        .unwrap();

        assert_eq!(snapshot.files.len(), 1);
        assert!(snapshot
            .files
            .contains_key(&existing_file.to_string_lossy().to_string()));
        assert!(!snapshot
            .files
            .contains_key(&missing_file.to_string_lossy().to_string()));
    }
}
