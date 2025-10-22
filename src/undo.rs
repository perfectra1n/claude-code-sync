use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::git::GitManager;
use crate::history::{OperationHistory, OperationType};

/// Represents a snapshot of conversation files at a point in time
///
/// Snapshots are created before each sync operation to enable undo functionality.
/// They capture the complete state of all conversation files that might be affected.
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
    #[serde(with = "base64_map")]
    pub files: HashMap<String, Vec<u8>>,

    /// Git branch name at the time of snapshot
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
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
        })
    }

    /// Save this snapshot to disk
    ///
    /// Snapshots are saved to `~/.claude-sync/snapshots/{snapshot_id}.json`
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

        fs::write(&snapshot_path, json).with_context(|| {
            format!(
                "Failed to write snapshot to disk: {}",
                snapshot_path.display()
            )
        })?;

        Ok(snapshot_path)
    }

    /// Load a snapshot from disk
    ///
    /// # Arguments
    /// * `snapshot_path` - Path to the snapshot JSON file
    ///
    /// # Validation
    /// This method validates:
    /// - Maximum snapshot file size: 100 MB
    /// - Maximum number of files in snapshot: 10,000
    ///
    /// # Errors
    /// Returns error if validation limits are exceeded or file cannot be read
    pub fn load_from_disk<P: AsRef<Path>>(snapshot_path: P) -> Result<Self> {
        let snapshot_path = snapshot_path.as_ref();

        // Validate file size before reading (100 MB limit)
        const MAX_SNAPSHOT_SIZE: u64 = 100 * 1024 * 1024; // 100 MB
        let metadata = fs::metadata(snapshot_path).with_context(|| {
            format!(
                "Failed to read snapshot file metadata: {}",
                snapshot_path.display()
            )
        })?;

        if metadata.len() > MAX_SNAPSHOT_SIZE {
            return Err(anyhow!(
                "Snapshot file exceeds maximum size limit. \
                File size: {} MB, Maximum: {} MB. \
                The snapshot file at {} is too large to load safely.",
                metadata.len() / (1024 * 1024),
                MAX_SNAPSHOT_SIZE / (1024 * 1024),
                snapshot_path.display()
            ));
        }

        let content = fs::read_to_string(snapshot_path).with_context(|| {
            format!("Failed to read snapshot file: {}", snapshot_path.display())
        })?;

        let snapshot: Snapshot = serde_json::from_str(&content).with_context(|| {
            format!("Failed to parse snapshot JSON: {}", snapshot_path.display())
        })?;

        // Validate number of files (10,000 file limit)
        const MAX_FILES: usize = 10_000;
        if snapshot.files.len() > MAX_FILES {
            return Err(anyhow!(
                "Snapshot contains too many files. \
                File count: {}, Maximum: {}. \
                The snapshot at {} exceeds the safety limit.",
                snapshot.files.len(),
                MAX_FILES,
                snapshot_path.display()
            ));
        }

        Ok(snapshot)
    }

    /// Restore files from this snapshot
    ///
    /// This writes all files from the snapshot back to their original locations,
    /// overwriting any current content.
    ///
    /// # Security
    /// This method validates all paths to prevent path traversal attacks.
    /// By default, only paths within the home directory are allowed.
    /// For testing, you can pass a custom allowed_base_dir.
    ///
    /// # Arguments
    /// * `allowed_base_dir` - Optional base directory for path validation.
    ///   If None, defaults to home directory for security.
    pub fn restore_with_base(&self, allowed_base_dir: Option<&Path>) -> Result<()> {
        // Determine the allowed base directory
        let allowed_base = if let Some(base) = allowed_base_dir {
            // For testing: use the provided base
            base.canonicalize().with_context(|| {
                format!("Failed to canonicalize base directory: {}", base.display())
            })?
        } else {
            // For production: use home directory
            let home_dir = dirs::home_dir().context("Failed to get home directory")?;
            home_dir
                .canonicalize()
                .context("Failed to canonicalize home directory")?
        };

        for (path_str, content) in &self.files {
            let path = PathBuf::from(path_str);

            // Canonicalize the path to resolve any symlinks or .. components
            // First ensure parent directory exists for canonicalization to work
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
            }

            // Create the file if it doesn't exist for canonicalization
            if !path.exists() {
                fs::write(&path, b"").with_context(|| {
                    format!("Failed to create temporary file: {}", path.display())
                })?;
            }

            let canonical_path = path
                .canonicalize()
                .with_context(|| format!("Failed to canonicalize path: {}", path.display()))?;

            // Validate the canonical path is within the allowed base directory
            if !canonical_path.starts_with(&allowed_base) {
                return Err(anyhow!(
                    "Security: Path traversal detected. Path {} is outside allowed directory {}",
                    path.display(),
                    allowed_base.display()
                ));
            }

            // Now write the actual content
            fs::write(&canonical_path, content)
                .with_context(|| format!("Failed to restore file: {}", canonical_path.display()))?;
        }

        Ok(())
    }

    /// Restore files from this snapshot
    ///
    /// This is a convenience wrapper that uses the home directory as the allowed base.
    pub fn restore(&self) -> Result<()> {
        self.restore_with_base(None)
    }

    /// Get the default snapshots directory
    fn snapshots_dir() -> Result<PathBuf> {
        crate::config::ConfigManager::snapshots_dir()
    }
}

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

impl UndoPreview {
    /// Display a formatted preview of the undo operation
    pub fn display(&self) {
        use colored::Colorize;

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
/// 3. Loads the snapshot to get the previous commit hash
/// 4. Uses git2 to reset the repository to the previous commit
/// 5. Updates the operation history to mark the push as undone
/// 6. Warns the user if they need to force push to the remote
///
/// # Arguments
/// * `repo_path` - Path to the git repository
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

    // Verify this is indeed a push snapshot
    if snapshot.operation_type != OperationType::Push {
        return Err(anyhow!(
            "Snapshot type mismatch: expected push, found {}",
            snapshot.operation_type.as_str()
        ));
    }

    // Get the commit hash to reset to
    let target_commit = snapshot.git_commit_hash.as_ref().ok_or_else(|| {
        anyhow!(
            "No git commit hash found in snapshot. \
            Cannot reset repository without a target commit."
        )
    })?;

    // Open the git repository
    let repo = git2::Repository::open(repo_path)
        .with_context(|| format!("Failed to open git repository at {}", repo_path.display()))?;

    // Find the target commit
    let oid = git2::Oid::from_str(target_commit)
        .with_context(|| format!("Invalid commit hash: {}", target_commit))?;

    let target_commit_obj = repo
        .find_commit(oid)
        .with_context(|| format!("Failed to find commit: {}", target_commit))?;

    // Check if we need to warn about remote (before reset)
    let branch_name = snapshot.branch.as_deref().unwrap_or("unknown");
    let needs_force_push = if let Ok(remote) = repo.find_remote("origin") {
        remote.url().is_some()
    } else {
        false
    };

    // TRANSACTION-LIKE ORDERING: Update history FIRST, then perform git reset.
    // This ensures that if the git reset fails, the history is still consistent.
    // The snapshot file remains on disk until we successfully complete the reset.

    // Step 1: Remove the push operation from history
    let mut history = OperationHistory::from_path(history_path.clone())?;
    history
        .remove_last_operation_by_type(OperationType::Push, history_path.clone())
        .context("Failed to remove push operation from history")?;

    // Step 2: Perform the git reset
    // If this fails, the history is already updated (which is safer than having
    // an inconsistent history state)
    repo.reset(target_commit_obj.as_object(), git2::ResetType::Soft, None)
        .context("Failed to reset repository to previous commit")?;

    // Step 3: Clean up the snapshot file (only after successful reset)
    if let Err(e) = fs::remove_file(snapshot_path) {
        eprintln!(
            "Warning: Failed to remove snapshot file {}: {}",
            snapshot_path.display(),
            e
        );
    }

    let mut summary = format!(
        "Successfully undone last push operation.\n\
        Reset repository to commit: {}\n\
        Branch: {}\n\
        Snapshot taken at: {}",
        &target_commit[..8],
        branch_name,
        snapshot.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
    );

    if needs_force_push {
        summary.push_str(&format!(
            "\n\n\
            WARNING: The remote repository was updated by the push.\n\
            You will need to force push to update the remote:\n\
            git push --force origin {}",
            branch_name
        ));
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{ConversationSummary, OperationRecord, SyncOperation};
    use tempfile::{tempdir, TempDir};

    /// Helper to create a test file with content
    fn create_test_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    /// Helper to setup test git repository
    fn setup_test_git_repo() -> (TempDir, GitManager) {
        let temp_dir = tempdir().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create and commit a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "initial content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial commit").unwrap();

        (temp_dir, git_manager)
    }

    #[test]
    fn test_snapshot_create_and_save() {
        let temp_dir = tempdir().unwrap();
        let file1 = create_test_file(temp_dir.path(), "file1.txt", "content 1");
        let file2 = create_test_file(temp_dir.path(), "file2.txt", "content 2");

        let snapshot = Snapshot::create(OperationType::Pull, vec![&file1, &file2], None).unwrap();

        assert_eq!(snapshot.operation_type, OperationType::Pull);
        assert_eq!(snapshot.files.len(), 2);
        assert!(snapshot.git_commit_hash.is_none());

        // Test save
        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
        assert!(snapshot_path.exists());
    }

    #[test]
    fn test_snapshot_with_git_commit() {
        let (temp_dir, git_manager) = setup_test_git_repo();
        let file1 = temp_dir.path().join("test.txt");

        let snapshot =
            Snapshot::create(OperationType::Push, vec![&file1], Some(&git_manager)).unwrap();

        assert_eq!(snapshot.operation_type, OperationType::Push);
        assert!(snapshot.git_commit_hash.is_some());
        assert!(snapshot.branch.is_some());

        let commit_hash = snapshot.git_commit_hash.unwrap();
        assert_eq!(commit_hash.len(), 40); // Git SHA-1 hash length
    }

    #[test]
    fn test_snapshot_restore() {
        let temp_dir = tempdir().unwrap();
        let file1 = create_test_file(temp_dir.path(), "file1.txt", "original content");

        // Create snapshot
        let snapshot = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();

        // Modify the file
        fs::write(&file1, "modified content").unwrap();
        assert_eq!(fs::read_to_string(&file1).unwrap(), "modified content");

        // Restore snapshot with temp dir as allowed base
        snapshot.restore_with_base(Some(temp_dir.path())).unwrap();

        // Verify original content is restored
        assert_eq!(fs::read_to_string(&file1).unwrap(), "original content");
    }

    #[test]
    fn test_snapshot_save_and_load() {
        let temp_dir = tempdir().unwrap();
        let file1 = create_test_file(temp_dir.path(), "file1.txt", "test content");

        let original_snapshot = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();

        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = original_snapshot
            .save_to_disk(Some(&snapshots_dir))
            .unwrap();

        // Load the snapshot
        let loaded_snapshot = Snapshot::load_from_disk(&snapshot_path).unwrap();

        assert_eq!(loaded_snapshot.snapshot_id, original_snapshot.snapshot_id);
        assert_eq!(
            loaded_snapshot.operation_type,
            original_snapshot.operation_type
        );
        assert_eq!(loaded_snapshot.files.len(), original_snapshot.files.len());
    }

    #[test]
    fn test_snapshot_handles_binary_files() {
        let temp_dir = tempdir().unwrap();
        let binary_file = temp_dir.path().join("binary.dat");

        // Create a binary file with non-UTF8 bytes
        let binary_content: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0x01, 0x02, 0x03];
        fs::write(&binary_file, &binary_content).unwrap();

        let snapshot = Snapshot::create(OperationType::Pull, vec![&binary_file], None).unwrap();

        // Verify binary content is preserved
        let stored_content = snapshot
            .files
            .get(&binary_file.to_string_lossy().to_string())
            .unwrap();
        assert_eq!(stored_content, &binary_content);

        // Test save/load preserves binary data
        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        let loaded_snapshot = Snapshot::load_from_disk(&snapshot_path).unwrap();
        let loaded_content = loaded_snapshot
            .files
            .get(&binary_file.to_string_lossy().to_string())
            .unwrap();
        assert_eq!(loaded_content, &binary_content);
    }

    #[test]
    fn test_undo_pull_no_history() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");

        let result = undo_pull(Some(history_path), Some(temp_dir.path()));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No pull operation found"));
    }

    #[test]
    fn test_undo_pull_success() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Create a test file
        let file1 = create_test_file(temp_dir.path(), "conversation.jsonl", "original");

        // Create a snapshot
        let snapshot = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Create operation history with a pull operation
        let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

        let conv_summary = ConversationSummary::new(
            "test-session".to_string(),
            "test/path".to_string(),
            None,
            5,
            SyncOperation::Modified,
        )
        .unwrap();

        let mut record = OperationRecord::new(
            OperationType::Pull,
            Some("main".to_string()),
            vec![conv_summary],
        );
        record.snapshot_path = Some(snapshot_path.clone());

        history.add_operation(record).unwrap();
        history.save_to(Some(history_path.clone())).unwrap();

        // Modify the file (simulating changes from pull)
        fs::write(&file1, "modified by pull").unwrap();

        // Undo the pull
        let result = undo_pull(Some(history_path.clone()), Some(temp_dir.path())).unwrap();
        assert!(result.contains("Successfully undone"));

        // Verify file is restored
        assert_eq!(fs::read_to_string(&file1).unwrap(), "original");

        // Verify snapshot is cleaned up
        assert!(!snapshot_path.exists());
    }

    #[test]
    fn test_undo_pull_missing_snapshot() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");

        // Create operation history with a pull but no snapshot file
        let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

        let conv_summary = ConversationSummary::new(
            "test-session".to_string(),
            "test/path".to_string(),
            None,
            5,
            SyncOperation::Modified,
        )
        .unwrap();

        let mut record = OperationRecord::new(
            OperationType::Pull,
            Some("main".to_string()),
            vec![conv_summary],
        );

        // Set a snapshot path that doesn't exist
        record.snapshot_path = Some(PathBuf::from("/nonexistent/snapshot.json"));

        history.add_operation(record).unwrap();
        history.save_to(Some(history_path.clone())).unwrap();

        // Try to undo
        let result = undo_pull(Some(history_path), Some(temp_dir.path()));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Snapshot file not found"));
    }

    #[test]
    fn test_undo_push_success() {
        let (temp_dir, git_manager) = setup_test_git_repo();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Get the initial commit hash
        let repo = git2::Repository::open(temp_dir.path()).unwrap();
        let initial_commit = repo.head().unwrap().peel_to_commit().unwrap();
        let initial_hash = initial_commit.id().to_string();

        // Create and commit a new file (simulating a push)
        let new_file = temp_dir.path().join("new.txt");
        fs::write(&new_file, "new content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Second commit").unwrap();

        // Create a snapshot with the initial commit hash
        let snapshot =
            Snapshot::create(OperationType::Push, vec![&new_file], Some(&git_manager)).unwrap();

        // Manually set the commit hash to the initial commit
        let mut snapshot = snapshot;
        snapshot.git_commit_hash = Some(initial_hash.clone());

        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Create operation history with a push operation
        let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

        let conv_summary = ConversationSummary::new(
            "test-session".to_string(),
            "test/path".to_string(),
            None,
            5,
            SyncOperation::Added,
        )
        .unwrap();

        let mut record = OperationRecord::new(
            OperationType::Push,
            Some("master".to_string()),
            vec![conv_summary],
        );
        record.snapshot_path = Some(snapshot_path.clone());

        history.add_operation(record).unwrap();
        history.save_to(Some(history_path.clone())).unwrap();

        // Undo the push
        let result = undo_push(temp_dir.path(), Some(history_path)).unwrap();
        assert!(result.contains("Successfully undone"));
        assert!(result.contains(&initial_hash[..8]));

        // Verify we're back at the initial commit
        let repo = git2::Repository::open(temp_dir.path()).unwrap();
        let current_commit = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(current_commit.id().to_string(), initial_hash);
    }

    #[test]
    fn test_undo_push_no_history() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");

        let result = undo_push(temp_dir.path(), Some(history_path));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No push operation found"));
    }

    #[test]
    fn test_undo_push_missing_commit_hash() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Create a snapshot without a commit hash
        let snapshot = Snapshot {
            snapshot_id: Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            operation_type: OperationType::Push,
            git_commit_hash: None, // Missing commit hash
            files: HashMap::new(),
            branch: Some("main".to_string()),
        };

        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Create operation history
        let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

        let conv_summary = ConversationSummary::new(
            "test-session".to_string(),
            "test/path".to_string(),
            None,
            5,
            SyncOperation::Added,
        )
        .unwrap();

        let mut record = OperationRecord::new(
            OperationType::Push,
            Some("main".to_string()),
            vec![conv_summary],
        );
        record.snapshot_path = Some(snapshot_path);

        history.add_operation(record).unwrap();
        history.save_to(Some(history_path.clone())).unwrap();

        // Initialize a git repo for testing
        let git_manager = GitManager::init(temp_dir.path()).unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial commit").unwrap();

        // Try to undo
        let result = undo_push(temp_dir.path(), Some(history_path));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No git commit hash found"));
    }

    #[test]
    fn test_snapshot_serialization_with_special_characters() {
        let temp_dir = tempdir().unwrap();
        let file_with_unicode = temp_dir.path().join("日本語.txt");
        fs::write(&file_with_unicode, "Hello 世界").unwrap();

        let snapshot =
            Snapshot::create(OperationType::Pull, vec![&file_with_unicode], None).unwrap();

        // Save and reload
        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();

        // Verify content is preserved
        let content = loaded.files.values().next().unwrap();
        assert_eq!(String::from_utf8_lossy(content), "Hello 世界");
    }

    #[test]
    fn test_base64_encoding_for_binary_data() {
        let temp_dir = tempdir().unwrap();

        // Create a file with various binary values
        let binary_file = temp_dir.path().join("binary.dat");
        let binary_data: Vec<u8> = (0..=255).collect();
        fs::write(&binary_file, &binary_data).unwrap();

        let snapshot = Snapshot::create(OperationType::Pull, vec![&binary_file], None).unwrap();

        // Serialize to JSON
        let json = serde_json::to_string(&snapshot).unwrap();

        // Verify it's valid JSON (shouldn't panic)
        let _parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Deserialize back
        let deserialized: Snapshot = serde_json::from_str(&json).unwrap();

        // Verify binary data is identical
        let original_data = snapshot
            .files
            .get(&binary_file.to_string_lossy().to_string())
            .unwrap();
        let restored_data = deserialized
            .files
            .get(&binary_file.to_string_lossy().to_string())
            .unwrap();

        assert_eq!(original_data, restored_data);
        assert_eq!(restored_data, &binary_data);
    }

    #[test]
    fn test_snapshot_restores_file_hierarchy() {
        let temp_dir = tempdir().unwrap();

        // Create nested directory structure
        let nested_dir = temp_dir.path().join("dir1").join("dir2");
        fs::create_dir_all(&nested_dir).unwrap();
        let nested_file = nested_dir.join("deep.txt");
        fs::write(&nested_file, "deep content").unwrap();

        // Create snapshot
        let snapshot = Snapshot::create(OperationType::Pull, vec![&nested_file], None).unwrap();

        // Delete the entire directory tree
        fs::remove_dir_all(temp_dir.path().join("dir1")).unwrap();
        assert!(!nested_file.exists());

        // Restore should recreate the directory structure
        snapshot.restore_with_base(Some(temp_dir.path())).unwrap();

        assert!(nested_file.exists());
        assert_eq!(fs::read_to_string(&nested_file).unwrap(), "deep content");
    }

    #[test]
    fn test_empty_snapshot() {
        let snapshot = Snapshot::create::<PathBuf, _>(OperationType::Pull, vec![], None).unwrap();

        assert_eq!(snapshot.files.len(), 0);
        assert!(snapshot.git_commit_hash.is_none());

        // Should be able to save and restore empty snapshot
        let temp_dir = tempdir().unwrap();
        let snapshot_path = snapshot.save_to_disk(Some(temp_dir.path())).unwrap();

        let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();
        assert_eq!(loaded.files.len(), 0);

        // Restore should not fail
        loaded.restore().unwrap();
    }

    #[test]
    fn test_snapshot_path_traversal_protection() {
        let _temp_dir = tempdir().unwrap();

        // Create a malicious snapshot that tries to write outside home directory
        let mut malicious_snapshot = Snapshot {
            snapshot_id: Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            operation_type: OperationType::Pull,
            git_commit_hash: None,
            files: HashMap::new(),
            branch: None,
        };

        // Try to add a path that escapes the home directory using ..
        // This should be caught by canonicalization
        let home = dirs::home_dir().unwrap();
        let evil_path = home.join("..").join("..").join("etc").join("passwd");

        malicious_snapshot.files.insert(
            evil_path.to_string_lossy().to_string(),
            b"malicious content".to_vec(),
        );

        // Attempting to restore should fail due to path traversal protection
        let result = malicious_snapshot.restore();

        // The restore should either fail during path validation
        // or the path should not be outside home dir after canonicalization
        if result.is_err() {
            let err_msg = result.unwrap_err().to_string();
            // Should contain security error message
            assert!(
                err_msg.contains("Security") || err_msg.contains("outside home"),
                "Error message should indicate security issue: {}",
                err_msg
            );
        } else {
            // If it didn't error, verify the file wasn't written outside home
            assert!(
                !PathBuf::from("/etc/passwd").exists()
                    || !fs::read_to_string("/etc/passwd")
                        .unwrap_or_default()
                        .contains("malicious")
            );
        }
    }

    #[test]
    fn test_snapshot_create_handles_missing_files() {
        let temp_dir = tempdir().unwrap();

        // Create one file that exists
        let existing_file = create_test_file(temp_dir.path(), "exists.txt", "content");

        // And one path that doesn't exist
        let missing_file = temp_dir.path().join("does_not_exist.txt");

        // Create snapshot with both paths
        let snapshot = Snapshot::create(
            OperationType::Pull,
            vec![&existing_file, &missing_file],
            None,
        )
        .unwrap();

        // Should only contain the existing file
        assert_eq!(snapshot.files.len(), 1);
        assert!(snapshot
            .files
            .contains_key(&existing_file.to_string_lossy().to_string()));
        assert!(!snapshot
            .files
            .contains_key(&missing_file.to_string_lossy().to_string()));
    }

    #[test]
    fn test_undo_pull_preserves_other_operations() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Create a test file
        let file1 = create_test_file(temp_dir.path(), "conversation.jsonl", "original");

        // Create TWO pull snapshots
        let snapshot1 = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();
        let snapshot_path1 = snapshot1.save_to_disk(Some(&snapshots_dir)).unwrap();

        let snapshot2 = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();
        let snapshot_path2 = snapshot2.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Create operation history with BOTH pull operations and a push
        let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

        let conv_summary = ConversationSummary::new(
            "test-session".to_string(),
            "test/path".to_string(),
            None,
            5,
            SyncOperation::Modified,
        )
        .unwrap();

        // Add first pull
        let mut record1 = OperationRecord::new(
            OperationType::Pull,
            Some("main".to_string()),
            vec![conv_summary.clone()],
        );
        record1.snapshot_path = Some(snapshot_path1.clone());
        history.add_operation(record1).unwrap();

        // Add a push operation
        let mut push_record = OperationRecord::new(
            OperationType::Push,
            Some("main".to_string()),
            vec![conv_summary.clone()],
        );
        push_record.snapshot_path = None;
        history.add_operation(push_record).unwrap();

        // Add second pull (most recent)
        let mut record2 = OperationRecord::new(
            OperationType::Pull,
            Some("main".to_string()),
            vec![conv_summary.clone()],
        );
        record2.snapshot_path = Some(snapshot_path2.clone());
        history.add_operation(record2).unwrap();

        history.save_to(Some(history_path.clone())).unwrap();

        // Verify we have 3 operations
        let loaded = OperationHistory::from_path(Some(history_path.clone())).unwrap();
        assert_eq!(loaded.len(), 3);

        // Undo the most recent pull
        let result = undo_pull(Some(history_path.clone()), Some(temp_dir.path())).unwrap();
        assert!(result.contains("Successfully undone"));

        // Verify we now have 2 operations (the first pull and the push remain)
        let loaded = OperationHistory::from_path(Some(history_path.clone())).unwrap();
        assert_eq!(loaded.len(), 2);

        // Verify the push is still there
        let operations = loaded.list_operations();
        assert_eq!(operations[0].operation_type, OperationType::Push);
        assert_eq!(operations[1].operation_type, OperationType::Pull);
    }

    #[test]
    fn test_undo_push_preserves_other_operations() {
        let (temp_dir, git_manager) = setup_test_git_repo();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Get the initial commit hash
        let repo = git2::Repository::open(temp_dir.path()).unwrap();
        let initial_commit = repo.head().unwrap().peel_to_commit().unwrap();
        let initial_hash = initial_commit.id().to_string();

        // Create and commit a new file
        let new_file = temp_dir.path().join("new.txt");
        fs::write(&new_file, "new content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Second commit").unwrap();

        // Create a snapshot with the initial commit hash
        let mut snapshot =
            Snapshot::create(OperationType::Push, vec![&new_file], Some(&git_manager)).unwrap();
        snapshot.git_commit_hash = Some(initial_hash.clone());
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Create operation history with a pull AND a push
        let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

        let conv_summary = ConversationSummary::new(
            "test-session".to_string(),
            "test/path".to_string(),
            None,
            5,
            SyncOperation::Added,
        )
        .unwrap();

        // Add a pull operation first
        let mut pull_record = OperationRecord::new(
            OperationType::Pull,
            Some("master".to_string()),
            vec![conv_summary.clone()],
        );
        pull_record.snapshot_path = None;
        history.add_operation(pull_record).unwrap();

        // Add the push operation
        let mut push_record = OperationRecord::new(
            OperationType::Push,
            Some("master".to_string()),
            vec![conv_summary],
        );
        push_record.snapshot_path = Some(snapshot_path.clone());
        history.add_operation(push_record).unwrap();

        history.save_to(Some(history_path.clone())).unwrap();

        // Verify we have 2 operations
        let loaded = OperationHistory::from_path(Some(history_path.clone())).unwrap();
        assert_eq!(loaded.len(), 2);

        // Undo the push
        let result = undo_push(temp_dir.path(), Some(history_path.clone())).unwrap();
        assert!(result.contains("Successfully undone"));

        // Verify we now have 1 operation (the pull remains)
        let loaded = OperationHistory::from_path(Some(history_path.clone())).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.list_operations()[0].operation_type,
            OperationType::Pull
        );
    }

    #[test]
    fn test_snapshot_validation_file_size_limit() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        fs::create_dir_all(&snapshots_dir).unwrap();

        // Create a snapshot file that exceeds 100 MB
        let snapshot_path = snapshots_dir.join("large_snapshot.json");

        // Create a large JSON structure (just over 100 MB)
        // We'll create a simple but large JSON manually
        let large_content = format!(
            r#"{{"snapshot_id":"test","timestamp":"2025-01-01T00:00:00Z","operation_type":"pull","files":{{"test":"{}"}}}}"#,
            "A".repeat(101 * 1024 * 1024) // 101 MB of 'A' characters
        );

        fs::write(&snapshot_path, large_content).unwrap();

        // Verify the file is over 100 MB
        let metadata = fs::metadata(&snapshot_path).unwrap();
        assert!(metadata.len() > 100 * 1024 * 1024);

        // Try to load the snapshot - should fail
        let result = Snapshot::load_from_disk(&snapshot_path);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("exceeds maximum size limit"));
        assert!(error_msg.contains("100 MB"));
    }

    #[test]
    fn test_snapshot_validation_file_count_limit() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Create a snapshot with more than 10,000 files
        let mut files = HashMap::new();
        for i in 0..10_001 {
            files.insert(format!("file_{}.txt", i), vec![0u8; 10]);
        }

        let snapshot = Snapshot {
            snapshot_id: Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            operation_type: OperationType::Pull,
            git_commit_hash: None,
            files,
            branch: None,
        };

        // Save the snapshot
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Try to load it - should fail
        let result = Snapshot::load_from_disk(&snapshot_path);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("too many files"));
        assert!(error_msg.contains("10001"));
        assert!(error_msg.contains("10,000") || error_msg.contains("10000"));
    }

    #[test]
    fn test_snapshot_validation_within_limits() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Create a snapshot within limits
        let mut files = HashMap::new();
        for i in 0..100 {
            files.insert(format!("file_{}.txt", i), b"small content".to_vec());
        }

        let snapshot = Snapshot {
            snapshot_id: Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            operation_type: OperationType::Pull,
            git_commit_hash: None,
            files,
            branch: None,
        };

        // Save the snapshot
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Load should succeed
        let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();
        assert_eq!(loaded.files.len(), 100);
        assert_eq!(loaded.snapshot_id, snapshot.snapshot_id);
    }

    #[test]
    fn test_undo_pull_transaction_safety() {
        // This test verifies that history is updated FIRST, then files are restored.
        // If file restoration fails, the history should already be updated.
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Create a test file
        let file1 = create_test_file(temp_dir.path(), "conversation.jsonl", "original");

        // Create a snapshot
        let snapshot = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Create operation history with a pull operation
        let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

        let conv_summary = ConversationSummary::new(
            "test-session".to_string(),
            "test/path".to_string(),
            None,
            5,
            SyncOperation::Modified,
        )
        .unwrap();

        let mut record = OperationRecord::new(
            OperationType::Pull,
            Some("main".to_string()),
            vec![conv_summary],
        );
        record.snapshot_path = Some(snapshot_path.clone());

        history.add_operation(record).unwrap();
        history.save_to(Some(history_path.clone())).unwrap();

        // Verify we have 1 operation before undo
        let loaded = OperationHistory::from_path(Some(history_path.clone())).unwrap();
        assert_eq!(loaded.len(), 1);

        // Modify the file (simulating changes from pull)
        fs::write(&file1, "modified by pull").unwrap();

        // Make the file read-only to cause restoration to potentially fail
        // (though on most systems this won't prevent writing, we can at least test the order)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&file1).unwrap().permissions();
            perms.set_mode(0o444); // read-only
            fs::set_permissions(&file1, perms).unwrap();
        }

        // Attempt undo - this might fail on file restoration
        let result = undo_pull(Some(history_path.clone()), Some(temp_dir.path()));

        // Whether it succeeds or fails, the history should be updated
        // (because we update history FIRST)
        let loaded_after = OperationHistory::from_path(Some(history_path.clone())).unwrap();

        // The key assertion: history should be updated (0 operations)
        // This proves we updated history before attempting file restoration
        assert_eq!(
            loaded_after.len(),
            0,
            "History should be updated even if file restoration fails"
        );

        // Verify the snapshot file is removed if successful, or remains if failed
        if result.is_ok() {
            assert!(
                !snapshot_path.exists(),
                "Snapshot should be cleaned up on success"
            );
        }

        // Clean up permissions for temp dir deletion
        #[cfg(unix)]
        {
            if file1.exists() {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&file1).unwrap().permissions();
                perms.set_mode(0o644);
                let _ = fs::set_permissions(&file1, perms);
            }
        }
    }

    #[test]
    fn test_undo_push_transaction_safety() {
        // This test verifies that history is updated FIRST, then git reset is performed.
        let (temp_dir, git_manager) = setup_test_git_repo();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Get the initial commit hash
        let repo = git2::Repository::open(temp_dir.path()).unwrap();
        let initial_commit = repo.head().unwrap().peel_to_commit().unwrap();
        let initial_hash = initial_commit.id().to_string();

        // Create and commit a new file (simulating a push)
        let new_file = temp_dir.path().join("new.txt");
        fs::write(&new_file, "new content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Second commit").unwrap();

        // Create a snapshot with the initial commit hash
        let mut snapshot =
            Snapshot::create(OperationType::Push, vec![&new_file], Some(&git_manager)).unwrap();
        snapshot.git_commit_hash = Some(initial_hash.clone());
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Create operation history with a push operation
        let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

        let conv_summary = ConversationSummary::new(
            "test-session".to_string(),
            "test/path".to_string(),
            None,
            5,
            SyncOperation::Added,
        )
        .unwrap();

        let mut record = OperationRecord::new(
            OperationType::Push,
            Some("master".to_string()),
            vec![conv_summary],
        );
        record.snapshot_path = Some(snapshot_path.clone());

        history.add_operation(record).unwrap();
        history.save_to(Some(history_path.clone())).unwrap();

        // Verify we have 1 operation before undo
        let loaded = OperationHistory::from_path(Some(history_path.clone())).unwrap();
        assert_eq!(loaded.len(), 1);

        // Perform undo
        let result = undo_push(temp_dir.path(), Some(history_path.clone()));

        // Whether it succeeds or fails, the history should be updated FIRST
        let loaded_after = OperationHistory::from_path(Some(history_path.clone())).unwrap();

        // The key assertion: history should be updated (0 operations)
        // This proves we updated history before attempting git reset
        assert_eq!(
            loaded_after.len(),
            0,
            "History should be updated even if git reset fails"
        );

        // If successful, verify we're back at the initial commit
        if result.is_ok() {
            let repo = git2::Repository::open(temp_dir.path()).unwrap();
            let current_commit = repo.head().unwrap().peel_to_commit().unwrap();
            assert_eq!(current_commit.id().to_string(), initial_hash);
            assert!(
                !snapshot_path.exists(),
                "Snapshot should be cleaned up on success"
            );
        }
    }
}
