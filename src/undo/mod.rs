//! Snapshot-based undo functionality for sync operations.
//!
//! Creates point-in-time snapshots of conversation files before sync operations.
//! Snapshots enable undoing pull operations (by restoring files) and push operations
//! (by resetting Git commits). Includes validation and security checks for safe restoration.

mod snapshot;
mod restore;
mod preview;
mod operations;
mod cleanup;

// Re-export public types and functions to maintain API compatibility
pub use snapshot::Snapshot;
pub use preview::{VerbosityLevel, preview_undo_pull, preview_undo_push};
pub use operations::{undo_pull, undo_push};
pub use cleanup::{SnapshotCleanupConfig, cleanup_old_snapshots};

// These are part of the public API but currently only used in tests
#[allow(unused_imports)]
pub use preview::UndoPreview;
#[allow(unused_imports)]
pub use cleanup::cleanup_old_snapshots_with_dir;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{ConversationSummary, OperationRecord, OperationType, SyncOperation, OperationHistory};
    use crate::scm::{self, Scm};
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::{tempdir, TempDir};
    use uuid::Uuid;

    /// Helper to create a test file with content
    fn create_test_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    /// Helper to setup test SCM repository
    fn setup_test_repo() -> (TempDir, Box<dyn Scm>) {
        let temp_dir = tempdir().unwrap();
        let repo = scm::init(temp_dir.path()).unwrap();

        // Create and commit a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "initial content").unwrap();
        repo.stage_all().unwrap();
        repo.commit("Initial commit").unwrap();

        (temp_dir, repo)
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
    fn test_snapshot_with_commit_hash() {
        let (temp_dir, repo) = setup_test_repo();
        let file1 = temp_dir.path().join("test.txt");
        let commit_hash = repo.current_commit_hash().unwrap();

        let snapshot =
            Snapshot::create(OperationType::Push, vec![&file1], Some(&commit_hash)).unwrap();

        assert_eq!(snapshot.operation_type, OperationType::Push);
        assert!(snapshot.git_commit_hash.is_some());

        let stored_hash = snapshot.git_commit_hash.unwrap();
        assert_eq!(stored_hash.len(), 40); // Git SHA-1 hash length
        assert_eq!(stored_hash, commit_hash);
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
        let (temp_dir, repo) = setup_test_repo();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Get the initial commit hash
        let initial_hash = repo.current_commit_hash().unwrap();

        // Create and commit a new file (simulating a push)
        let new_file = temp_dir.path().join("new.txt");
        fs::write(&new_file, "new content").unwrap();
        repo.stage_all().unwrap();
        repo.commit("Second commit").unwrap();

        // Create a snapshot with the initial commit hash
        let mut snapshot =
            Snapshot::create(OperationType::Push, vec![&new_file], Some(&initial_hash)).unwrap();

        // Set the commit hash to the initial commit (for undo)
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
        let repo_check = scm::open(temp_dir.path()).unwrap();
        let current_hash = repo_check.current_commit_hash().unwrap();
        assert_eq!(current_hash, initial_hash);
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
            base_snapshot_id: None,
            deleted_files: Vec::new(),
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

        // Initialize a repo for testing
        let repo = scm::init(temp_dir.path()).unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test").unwrap();
        repo.stage_all().unwrap();
        repo.commit("Initial commit").unwrap();

        // Try to undo
        let result = undo_push(temp_dir.path(), Some(history_path));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No commit hash found"));
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
            base_snapshot_id: None,
            deleted_files: Vec::new(),
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
                "Error message should indicate security issue: {err_msg}"
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
        let (temp_dir, repo) = setup_test_repo();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Get the initial commit hash
        let initial_hash = repo.current_commit_hash().unwrap();

        // Create and commit a new file
        let new_file = temp_dir.path().join("new.txt");
        fs::write(&new_file, "new content").unwrap();
        repo.stage_all().unwrap();
        repo.commit("Second commit").unwrap();

        // Create a snapshot with the initial commit hash
        let mut snapshot =
            Snapshot::create(OperationType::Push, vec![&new_file], Some(&initial_hash)).unwrap();
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
        // This test verifies that history is updated FIRST, then reset is performed.
        let (temp_dir, repo) = setup_test_repo();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        // Get the initial commit hash
        let initial_hash = repo.current_commit_hash().unwrap();

        // Create and commit a new file (simulating a push)
        let new_file = temp_dir.path().join("new.txt");
        fs::write(&new_file, "new content").unwrap();
        repo.stage_all().unwrap();
        repo.commit("Second commit").unwrap();

        // Create a snapshot with the initial commit hash
        let mut snapshot =
            Snapshot::create(OperationType::Push, vec![&new_file], Some(&initial_hash)).unwrap();
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
            let repo_check = scm::open(temp_dir.path()).unwrap();
            let current_hash = repo_check.current_commit_hash().unwrap();
            assert_eq!(current_hash, initial_hash);
            assert!(
                !snapshot_path.exists(),
                "Snapshot should be cleaned up on success"
            );
        }
    }

    // ============================================================================
    // Differential Snapshot Tests
    // ============================================================================

    #[test]
    fn test_differential_snapshot_first_snapshot_is_full() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        let file1 = temp_dir.path().join("file1.txt");
        let file2 = temp_dir.path().join("file2.txt");

        fs::write(&file1, b"content1").unwrap();
        fs::write(&file2, b"content2").unwrap();

        // First differential snapshot should be a full snapshot (no base)
        let snapshot = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1, &file2],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();

        snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Verify it's a full snapshot
        assert!(snapshot.base_snapshot_id.is_none(), "First snapshot should not have a base");
        assert_eq!(snapshot.files.len(), 2, "First snapshot should contain all files");
        assert!(snapshot.deleted_files.is_empty(), "First snapshot should have no deleted files");
    }

    #[test]
    fn test_differential_snapshot_only_stores_changes() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        let file1 = temp_dir.path().join("file1.txt");
        let file2 = temp_dir.path().join("file2.txt");
        let file3 = temp_dir.path().join("file3.txt");

        // Create initial state
        fs::write(&file1, b"content1").unwrap();
        fs::write(&file2, b"content2").unwrap();

        // First snapshot (full)
        let snapshot1 = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1, &file2],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();
        snapshot1.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Modify one file and add a new one
        fs::write(&file1, b"modified_content1").unwrap();
        fs::write(&file3, b"content3").unwrap();

        // Second snapshot (differential)
        let snapshot2 = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1, &file2, &file3],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();
        snapshot2.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Verify it's a differential snapshot
        assert!(snapshot2.base_snapshot_id.is_some(), "Second snapshot should have a base");
        assert_eq!(
            snapshot2.base_snapshot_id.as_ref().unwrap(),
            &snapshot1.snapshot_id,
            "Base should be the first snapshot"
        );

        // Should only contain changed file (file1) and new file (file3), not file2
        assert_eq!(snapshot2.files.len(), 2, "Should only contain changed and new files");
        assert!(snapshot2.files.contains_key(&file1.to_string_lossy().to_string()));
        assert!(snapshot2.files.contains_key(&file3.to_string_lossy().to_string()));
        assert!(!snapshot2.files.contains_key(&file2.to_string_lossy().to_string()));
    }

    #[test]
    fn test_differential_snapshot_tracks_deletions() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        let file1 = temp_dir.path().join("file1.txt");
        let file2 = temp_dir.path().join("file2.txt");

        // Create initial state
        fs::write(&file1, b"content1").unwrap();
        fs::write(&file2, b"content2").unwrap();

        // First snapshot
        let snapshot1 = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1, &file2],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();
        snapshot1.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Delete file2
        fs::remove_file(&file2).unwrap();

        // Second snapshot
        let snapshot2 = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();
        snapshot2.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Verify deletion tracking
        assert_eq!(snapshot2.deleted_files.len(), 1, "Should track one deleted file");
        assert!(
            snapshot2.deleted_files.contains(&file2.to_string_lossy().to_string()),
            "Should track file2 as deleted"
        );
    }

    #[test]
    fn test_differential_snapshot_reconstruction() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        let file1 = temp_dir.path().join("file1.txt");
        let file2 = temp_dir.path().join("file2.txt");
        let file3 = temp_dir.path().join("file3.txt");

        // Create chain of snapshots
        fs::write(&file1, b"v1").unwrap();
        fs::write(&file2, b"v1").unwrap();

        let snapshot1 = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1, &file2],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();
        snapshot1.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Modify file1, add file3
        fs::write(&file1, b"v2").unwrap();
        fs::write(&file3, b"v2").unwrap();

        let snapshot2 = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1, &file2, &file3],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();
        snapshot2.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Reconstruct full state from differential snapshot
        let full_state = snapshot2.reconstruct_full_state_with_dir(Some(&snapshots_dir)).unwrap();

        // Should contain all three files with correct content
        assert_eq!(full_state.len(), 3, "Should have 3 files after reconstruction");
        assert_eq!(full_state.get(&file1.to_string_lossy().to_string()).unwrap(), b"v2");
        assert_eq!(full_state.get(&file2.to_string_lossy().to_string()).unwrap(), b"v1"); // Unchanged from base
        assert_eq!(full_state.get(&file3.to_string_lossy().to_string()).unwrap(), b"v2"); // New file
    }

    #[test]
    fn test_differential_snapshot_restore_with_deletions() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        let file1 = temp_dir.path().join("file1.txt");
        let file2 = temp_dir.path().join("file2.txt");

        // Create initial snapshot
        fs::write(&file1, b"content1").unwrap();
        fs::write(&file2, b"content2").unwrap();

        let snapshot1 = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1, &file2],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();
        snapshot1.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Delete file2
        fs::remove_file(&file2).unwrap();

        let snapshot2 = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();
        snapshot2.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Restore files (create file2 again to test deletion)
        fs::write(&file2, b"should_be_deleted").unwrap();

        // Restore snapshot2 which should delete file2
        snapshot2.restore_with_base_and_snapshots(Some(temp_dir.path()), Some(&snapshots_dir)).unwrap();

        // Verify file1 exists and file2 was deleted
        assert!(file1.exists(), "file1 should exist after restore");
        assert!(!file2.exists(), "file2 should be deleted after restore");
    }

    #[test]
    fn test_differential_snapshot_broken_chain() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        let file1 = temp_dir.path().join("file1.txt");

        fs::write(&file1, b"content1").unwrap();

        // Create a differential snapshot with a fake base ID
        let mut snapshot = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();

        // Manually set a non-existent base
        snapshot.base_snapshot_id = Some("non-existent-base-id".to_string());
        snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Trying to reconstruct should fail gracefully
        let result = snapshot.reconstruct_full_state();
        assert!(result.is_err(), "Should fail when base snapshot is missing");
        assert!(
            result.unwrap_err().to_string().contains("Base snapshot not found"),
            "Error should mention missing base snapshot"
        );
    }

    #[test]
    fn test_differential_snapshot_long_chain() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        let file1 = temp_dir.path().join("file1.txt");

        // Create a chain of 5 snapshots
        let mut snapshots = Vec::new();

        for i in 1..=5 {
            fs::write(&file1, format!("version_{}", i).as_bytes()).unwrap();

            let snapshot = Snapshot::create_differential_with_dir(
                OperationType::Pull,
                vec![&file1],
                None,
                Some(&snapshots_dir),
            )
            .unwrap();
            snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
            snapshots.push(snapshot);
        }

        // Verify chain structure
        assert!(snapshots[0].base_snapshot_id.is_none(), "First should have no base");
        for i in 1..5 {
            assert_eq!(
                snapshots[i].base_snapshot_id.as_ref().unwrap(),
                &snapshots[i - 1].snapshot_id,
                "Snapshot {} should reference snapshot {}", i, i - 1
            );
        }

        // Reconstruct from the last snapshot
        let full_state = snapshots[4].reconstruct_full_state_with_dir(Some(&snapshots_dir)).unwrap();
        assert_eq!(
            full_state.get(&file1.to_string_lossy().to_string()).unwrap(),
            b"version_5",
            "Should reconstruct to the latest version"
        );
    }

    // ============================================================================
    // Snapshot Cleanup Tests
    // ============================================================================

    #[test]
    fn test_cleanup_snapshots_respects_count_limit() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        fs::create_dir_all(&snapshots_dir).unwrap();

        // Create 10 pull snapshots with different timestamps
        for i in 0..10 {
            let snapshot = Snapshot {
                snapshot_id: format!("snapshot_{}", i),
                timestamp: chrono::Utc::now() - chrono::Duration::days(i as i64),
                operation_type: OperationType::Pull,
                git_commit_hash: None,
                files: HashMap::new(),
                branch: None,
                base_snapshot_id: None,
                deleted_files: Vec::new(),
            };
            snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
        }

        // Cleanup keeping last 5
        let config = SnapshotCleanupConfig {
            max_count_per_type: 5,
            max_age_days: 0, // Only count matters, not age
        };

        let deleted = cleanup_old_snapshots_with_dir(Some(config), false, Some(&snapshots_dir)).unwrap();
        assert_eq!(deleted, 5, "Should delete 5 old snapshots");

        // Count remaining snapshots
        let remaining = fs::read_dir(&snapshots_dir).unwrap().count();
        assert_eq!(remaining, 5, "Should have 5 snapshots remaining");
    }

    #[test]
    fn test_cleanup_snapshots_respects_age_limit() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        fs::create_dir_all(&snapshots_dir).unwrap();

        // Capture "now" once to ensure consistent timestamp calculations
        let now = chrono::Utc::now();

        // Create snapshots with different ages
        // Use larger day offsets well away from the boundary to avoid timing issues
        for i in 0..10 {
            let snapshot = Snapshot {
                snapshot_id: format!("snapshot_{}", i),
                timestamp: now - chrono::Duration::days((5 + i * 10) as i64),
                operation_type: OperationType::Pull,
                git_commit_hash: None,
                files: HashMap::new(),
                branch: None,
                base_snapshot_id: None,
                deleted_files: Vec::new(),
            };
            snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
        }

        // Cleanup keeping last 50 days
        let config = SnapshotCleanupConfig {
            max_count_per_type: 0, // Count doesn't matter, only age
            max_age_days: 50,
        };

        let deleted = cleanup_old_snapshots_with_dir(Some(config), false, Some(&snapshots_dir)).unwrap();

        // Snapshots are at days: 5, 15, 25, 35, 45, 55, 65, 75, 85, 95
        // Age threshold is now - 50 days
        // Keep: days 5, 15, 25, 35, 45 (5 snapshots) - all clearly within 50 days
        // Delete: days 55, 65, 75, 85, 95 (5 snapshots) - all clearly older than 50 days
        assert_eq!(deleted, 5, "Should delete 5 old snapshots");

        let remaining = fs::read_dir(&snapshots_dir).unwrap().count();
        assert_eq!(remaining, 5, "Should have 5 snapshots remaining");
    }

    #[test]
    fn test_cleanup_snapshots_separates_operation_types() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        fs::create_dir_all(&snapshots_dir).unwrap();

        // Create 10 pull and 10 push snapshots with different timestamps
        for i in 0..10 {
            let pull_snapshot = Snapshot {
                snapshot_id: format!("pull_{}", i),
                timestamp: chrono::Utc::now() - chrono::Duration::days(i as i64),
                operation_type: OperationType::Pull,
                git_commit_hash: None,
                files: HashMap::new(),
                branch: None,
                base_snapshot_id: None,
                deleted_files: Vec::new(),
            };
            pull_snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

            let push_snapshot = Snapshot {
                snapshot_id: format!("push_{}", i),
                timestamp: chrono::Utc::now() - chrono::Duration::days(i as i64),
                operation_type: OperationType::Push,
                git_commit_hash: Some(format!("hash_{}", i)),
                files: HashMap::new(),
                branch: Some("main".to_string()),
                base_snapshot_id: None,
                deleted_files: Vec::new(),
            };
            push_snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
        }

        // Cleanup keeping last 3 per type
        let config = SnapshotCleanupConfig {
            max_count_per_type: 3,
            max_age_days: 0, // Don't keep by age
        };

        let deleted = cleanup_old_snapshots_with_dir(Some(config), false, Some(&snapshots_dir)).unwrap();

        // Should delete 7 pull + 7 push = 14 total
        assert_eq!(deleted, 14, "Should delete 14 old snapshots");

        // Should keep 3 pull + 3 push = 6 total
        let remaining = fs::read_dir(&snapshots_dir).unwrap().count();
        assert_eq!(remaining, 6, "Should have 6 snapshots remaining (3 per type)");
    }

    #[test]
    fn test_cleanup_snapshots_dry_run() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        fs::create_dir_all(&snapshots_dir).unwrap();

        // Create 10 snapshots with different timestamps
        for i in 0..10 {
            let snapshot = Snapshot {
                snapshot_id: format!("snapshot_{}", i),
                timestamp: chrono::Utc::now() - chrono::Duration::days(i as i64),
                operation_type: OperationType::Pull,
                git_commit_hash: None,
                files: HashMap::new(),
                branch: None,
                base_snapshot_id: None,
                deleted_files: Vec::new(),
            };
            snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
        }

        let config = SnapshotCleanupConfig {
            max_count_per_type: 3,
            max_age_days: 0, // Only count matters for this test
        };

        // Dry run should report but not delete
        let deleted = cleanup_old_snapshots_with_dir(Some(config), true, Some(&snapshots_dir)).unwrap();
        assert_eq!(deleted, 7, "Should report 7 snapshots would be deleted");

        // All snapshots should still exist
        let remaining = fs::read_dir(&snapshots_dir).unwrap().count();
        assert_eq!(remaining, 10, "All snapshots should still exist after dry run");
    }
}
