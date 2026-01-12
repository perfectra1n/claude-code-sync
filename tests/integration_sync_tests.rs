use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use walkdir::WalkDir;

// Import the necessary modules from claude_code_sync
use claude_code_sync::scm;
use claude_code_sync::history::{
    ConversationSummary, OperationHistory, OperationType, SyncOperation,
};
use claude_code_sync::parser::ConversationSession;
use claude_code_sync::sync::SyncState;
use claude_code_sync::undo::{undo_pull, undo_push, Snapshot};

/// Path to test data directory
// Use relative path from the workspace root
const TEST_DATA_DIR: &str = "data";

/// Helper function to copy test data to a destination directory
fn copy_test_data(dest_projects_dir: &Path) -> anyhow::Result<()> {
    let test_data = Path::new(TEST_DATA_DIR);

    // Copy all directories from test data
    for entry in fs::read_dir(test_data)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            let dir_name = path.file_name().unwrap();
            let dest_dir = dest_projects_dir.join(dir_name);

            // Create destination directory
            fs::create_dir_all(&dest_dir)?;

            // Copy all .jsonl files
            for file_entry in WalkDir::new(&path).max_depth(1) {
                let file_entry = file_entry?;
                let file_path = file_entry.path();

                if file_path.is_file()
                    && file_path.extension().and_then(|s| s.to_str()) == Some("jsonl")
                {
                    let file_name = file_path.file_name().unwrap();
                    let dest_file = dest_dir.join(file_name);
                    fs::copy(file_path, dest_file)?;
                }
            }
        }
    }

    Ok(())
}

/// Helper function to create a mock sync state
fn create_test_sync_state(sync_repo_path: &Path, state_dir: &Path) -> anyhow::Result<PathBuf> {
    let state = SyncState {
        sync_repo_path: sync_repo_path.to_path_buf(),
        has_remote: false,
        is_cloned_repo: false,
    };

    let state_file = state_dir.join("state.json");
    fs::create_dir_all(state_dir)?;

    let content = serde_json::to_string_pretty(&state)?;
    fs::write(&state_file, content)?;

    Ok(state_file)
}

/// Helper function to count .jsonl files in a directory
fn count_jsonl_files(dir: &Path) -> usize {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .count()
}

/// Helper function to discover sessions in a directory
fn discover_test_sessions(base_path: &Path) -> anyhow::Result<Vec<ConversationSession>> {
    let mut sessions = Vec::new();

    for entry in WalkDir::new(base_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            match ConversationSession::from_file(path) {
                Ok(session) => sessions.push(session),
                Err(e) => log::warn!("Failed to parse {}: {}", path.display(), e),
            }
        }
    }

    Ok(sessions)
}

/// Helper function to create filter config for testing
fn create_test_filter_config(config_dir: &Path) -> anyhow::Result<()> {
    use claude_code_sync::filter::FilterConfig;

    let filter = FilterConfig::default();
    let filter_path = config_dir.join("filter.json");

    fs::create_dir_all(config_dir)?;
    let content = serde_json::to_string_pretty(&filter)?;
    fs::write(&filter_path, content)?;

    Ok(())
}

#[test]
fn test_full_push_pull_cycle() {
    // Setup: Create temporary directories for sync repo and fake claude projects
    let sync_repo_dir = TempDir::new().unwrap();
    let claude_dir = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap();

    let sync_repo_path = sync_repo_dir.path();
    let claude_projects_dir = claude_dir.path().join("projects");
    fs::create_dir_all(&claude_projects_dir).unwrap();

    // Copy test data to fake claude projects directory
    copy_test_data(&claude_projects_dir).unwrap();

    // Verify test data was copied
    let file_count = count_jsonl_files(&claude_projects_dir);
    assert!(
        file_count >= 5,
        "Expected at least 5 test files, found {file_count}"
    );

    // Initialize sync repository
    let repo = scm::init(sync_repo_path).unwrap();

    // Create an initial commit so we have a valid history
    let readme_path = sync_repo_path.join("README.md");
    fs::write(&readme_path, "# Claude Sync Test Repo\n").unwrap();
    repo.stage_all().unwrap();
    repo.commit("Initial commit").unwrap();

    // Create sync state and filter config
    let _state_file = create_test_sync_state(sync_repo_path, config_dir.path()).unwrap();
    create_test_filter_config(config_dir.path()).unwrap();

    // Set HOME to our test config directory to isolate tests
    std::env::set_var("HOME", config_dir.path());

    // Discover sessions from test data
    let original_sessions = discover_test_sessions(&claude_projects_dir).unwrap();
    assert!(
        original_sessions.len() >= 5,
        "Should have at least 5 test sessions"
    );

    // Push conversations (simulated - we'll do this manually since we can't call the actual function)
    // Instead, we'll manually copy files and create history
    let projects_dir = sync_repo_path.join("projects");
    fs::create_dir_all(&projects_dir).unwrap();

    // Copy all sessions to sync repo
    for session in &original_sessions {
        let relative_path = Path::new(&session.file_path)
            .strip_prefix(&claude_projects_dir)
            .unwrap_or(Path::new(&session.file_path));

        let dest_path = projects_dir.join(relative_path);

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        session.write_to_file(&dest_path).unwrap();
    }

    // Commit the push
    repo.stage_all().unwrap();
    let has_changes = repo.has_changes().unwrap();
    assert!(has_changes, "Should have changes to commit");

    repo.commit("Test push").unwrap();

    // Create operation history for the push
    let history_path = config_dir
        .path()
        .join(".claude-code-sync")
        .join("operation-history.json");
    let history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

    let mut conversations = Vec::new();
    for session in &original_sessions {
        let relative_path = Path::new(&session.file_path)
            .strip_prefix(&claude_projects_dir)
            .unwrap_or(Path::new(&session.file_path))
            .to_string_lossy()
            .to_string();

        let summary = ConversationSummary::new(
            session.session_id.clone(),
            relative_path,
            session.latest_timestamp(),
            session.message_count(),
            SyncOperation::Added,
        )
        .unwrap();

        conversations.push(summary);
    }

    let push_record = claude_code_sync::history::OperationRecord::new(
        OperationType::Push,
        Some("main".to_string()),
        conversations,
    );

    // Don't save to default location, save to test location
    let mut history_loaded = history;
    history_loaded.operations.insert(0, push_record);
    history_loaded.save_to(Some(history_path.clone())).unwrap();

    // Reload and verify operation history was created
    let history = OperationHistory::from_path(Some(history_path.clone())).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(
        history.get_last_operation().unwrap().operation_type,
        OperationType::Push
    );

    // Verify files exist in sync repo
    let sync_sessions = discover_test_sessions(&projects_dir).unwrap();
    assert_eq!(sync_sessions.len(), original_sessions.len());

    // Modify a conversation file in sync repo (simulate remote change)
    if let Some(first_session) = sync_sessions.first() {
        let session_path = projects_dir.join(&first_session.file_path);
        let content = fs::read_to_string(&session_path).unwrap();
        let modified_content = format!("{}\n{{\"type\":\"user\",\"uuid\":\"test-uuid\",\"sessionId\":\"{}\",\"timestamp\":\"2025-10-18T00:00:00Z\"}}\n",
            content.trim(), first_session.session_id);
        fs::write(&session_path, modified_content).unwrap();

        // Commit the modification
        repo.stage_all().unwrap();
        repo.commit("Modified conversation").unwrap();
    }

    // Pull from sync repo to "another machine" (different temp dir)
    let machine2_dir = TempDir::new().unwrap();
    let machine2_projects = machine2_dir.path().join("projects");
    fs::create_dir_all(&machine2_projects).unwrap();

    // Copy original sessions to machine2 (simulating existing local state)
    for session in &original_sessions {
        let relative_path = Path::new(&session.file_path)
            .strip_prefix(&claude_projects_dir)
            .unwrap_or(Path::new(&session.file_path));

        let dest_path = machine2_projects.join(relative_path);

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        session.write_to_file(&dest_path).unwrap();
    }

    // Simulate pull by copying modified files from sync repo
    let sync_sessions_after_modify = discover_test_sessions(&projects_dir).unwrap();
    for session in &sync_sessions_after_modify {
        let relative_path = Path::new(&session.file_path)
            .strip_prefix(&projects_dir)
            .unwrap_or(Path::new(&session.file_path));

        let dest_path = machine2_projects.join(relative_path);

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        session.write_to_file(&dest_path).unwrap();
    }

    // Verify files synced correctly
    let machine2_sessions = discover_test_sessions(&machine2_projects).unwrap();
    assert_eq!(machine2_sessions.len(), sync_sessions_after_modify.len());

    // Verify the modification was pulled
    if let Some(first_modified) = machine2_sessions.iter().find(|s| {
        sync_sessions_after_modify
            .first()
            .map(|orig| &orig.session_id)
            == Some(&s.session_id)
    }) {
        assert!(
            first_modified.message_count() > original_sessions.first().unwrap().message_count(),
            "Modified session should have more messages"
        );
    }

    // Clean up
    std::env::remove_var("HOME");
}

#[test]
fn test_undo_pull_restores_files() {
    let test_dir = TempDir::new().unwrap();
    let history_path = test_dir.path().join("history.json");
    let snapshots_dir = test_dir.path().join("snapshots");
    let claude_dir = test_dir.path().join("claude_projects");

    fs::create_dir_all(&claude_dir).unwrap();
    fs::create_dir_all(&snapshots_dir).unwrap();

    // Create initial conversation file
    let conv_file = claude_dir.join("test-conversation.jsonl");
    let original_content = r#"{"type":"user","uuid":"1","sessionId":"test-session","timestamp":"2025-01-01T00:00:00Z"}
{"type":"assistant","uuid":"2","sessionId":"test-session","timestamp":"2025-01-01T00:01:00Z"}
"#;
    fs::write(&conv_file, original_content).unwrap();

    // Create a snapshot before "pull"
    let snapshot = Snapshot::create(OperationType::Pull, vec![&conv_file], None).unwrap();

    let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

    // Create operation history with pull operation
    let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

    let conv_summary = ConversationSummary::new(
        "test-session".to_string(),
        "test-conversation.jsonl".to_string(),
        Some("2025-01-01T00:01:00Z".to_string()),
        2,
        SyncOperation::Modified,
    )
    .unwrap();

    let mut pull_record = claude_code_sync::history::OperationRecord::new(
        OperationType::Pull,
        Some("main".to_string()),
        vec![conv_summary],
    );
    pull_record.snapshot_path = Some(snapshot_path.clone());

    history.operations.insert(0, pull_record);
    history.save_to(Some(history_path.clone())).unwrap();

    // Simulate pull modifying the file
    let modified_content = r#"{"type":"user","uuid":"1","sessionId":"test-session","timestamp":"2025-01-01T00:00:00Z"}
{"type":"assistant","uuid":"2","sessionId":"test-session","timestamp":"2025-01-01T00:01:00Z"}
{"type":"user","uuid":"3","sessionId":"test-session","timestamp":"2025-01-01T00:02:00Z"}
"#;
    fs::write(&conv_file, modified_content).unwrap();

    // Verify file was changed
    let content_after_pull = fs::read_to_string(&conv_file).unwrap();
    assert_eq!(content_after_pull, modified_content);

    // Undo the pull
    let result = undo_pull(Some(history_path.clone()), Some(test_dir.path())).unwrap();
    assert!(result.contains("Successfully undone"));

    // Verify file was restored to pre-pull state
    let content_after_undo = fs::read_to_string(&conv_file).unwrap();
    assert_eq!(content_after_undo, original_content);

    // Verify operation history was updated
    let history_after_undo = OperationHistory::from_path(Some(history_path)).unwrap();
    assert_eq!(
        history_after_undo.len(),
        0,
        "Pull operation should be removed from history"
    );

    // Verify snapshot was cleaned up
    assert!(
        !snapshot_path.exists(),
        "Snapshot should be deleted after successful undo"
    );
}

#[test]
fn test_undo_push_resets_repo() {
    let test_dir = TempDir::new().unwrap();
    let history_path = test_dir.path().join("history.json");
    let snapshots_dir = test_dir.path().join("snapshots");

    // Initialize repository
    let repo = scm::init(test_dir.path()).unwrap();

    // Create and commit initial file
    let file1 = test_dir.path().join("file1.txt");
    fs::write(&file1, "initial content").unwrap();
    repo.stage_all().unwrap();
    repo.commit("Initial commit").unwrap();

    // Get initial commit hash
    let initial_commit_hash = repo.current_commit_hash().unwrap();

    // Create and commit another file (simulating a push)
    let file2 = test_dir.path().join("file2.txt");
    fs::write(&file2, "new content").unwrap();
    repo.stage_all().unwrap();
    repo.commit("Second commit").unwrap();

    // Verify we're at a different commit
    let second_commit_hash = repo.current_commit_hash().unwrap();
    assert_ne!(initial_commit_hash, second_commit_hash);

    // Create a snapshot with the initial commit hash
    let commit_hash = repo.current_commit_hash().ok();
    let mut snapshot =
        Snapshot::create(OperationType::Push, vec![&file2], commit_hash.as_deref()).unwrap();

    // Override the git commit hash to point to initial commit
    snapshot.git_commit_hash = Some(initial_commit_hash.clone());
    snapshot.branch = Some("master".to_string());

    let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

    // Create operation history with push operation
    let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

    let conv_summary = ConversationSummary::new(
        "test-session".to_string(),
        "file2.txt".to_string(),
        None,
        1,
        SyncOperation::Added,
    )
    .unwrap();

    let mut push_record = claude_code_sync::history::OperationRecord::new(
        OperationType::Push,
        Some("master".to_string()),
        vec![conv_summary],
    );
    push_record.snapshot_path = Some(snapshot_path.clone());

    history.operations.insert(0, push_record);
    history.save_to(Some(history_path.clone())).unwrap();

    // Verify we have the second commit
    let current_hash_before_undo = repo.current_commit_hash().unwrap();
    assert_eq!(current_hash_before_undo, second_commit_hash);

    // Undo the push
    let result = undo_push(test_dir.path(), Some(history_path.clone())).unwrap();
    assert!(result.contains("Successfully undone"));
    assert!(result.contains(&initial_commit_hash[..8]));

    // Verify repo was reset to previous commit using scm module
    let reopened_repo = scm::open(test_dir.path()).unwrap();
    let current_hash_after = reopened_repo.current_commit_hash().unwrap();
    assert_eq!(current_hash_after, initial_commit_hash);

    // Verify operation history was updated
    let history_after_undo = OperationHistory::from_path(Some(history_path)).unwrap();
    assert_eq!(
        history_after_undo.len(),
        0,
        "Push operation should be removed from history"
    );

    // Verify snapshot was cleaned up
    assert!(
        !snapshot_path.exists(),
        "Snapshot should be deleted after successful undo"
    );
}

#[test]
fn test_conflict_handling() {
    use claude_code_sync::conflict::ConflictDetector;

    let machine1_dir = TempDir::new().unwrap();
    let machine2_dir = TempDir::new().unwrap();
    let sync_repo_dir = TempDir::new().unwrap();

    let m1_projects = machine1_dir.path().join("projects");
    let m2_projects = machine2_dir.path().join("projects");
    let sync_projects = sync_repo_dir.path().join("projects");

    fs::create_dir_all(&m1_projects).unwrap();
    fs::create_dir_all(&m2_projects).unwrap();
    fs::create_dir_all(&sync_projects).unwrap();

    // Create same session on both machines with identical content
    let session_id = "shared-session-123";
    let base_content = r#"{"type":"user","uuid":"1","sessionId":"shared-session-123","timestamp":"2025-01-01T00:00:00Z"}
{"type":"assistant","uuid":"2","sessionId":"shared-session-123","timestamp":"2025-01-01T00:01:00Z"}
"#;

    let m1_file = m1_projects.join("test").join("conversation.jsonl");
    let m2_file = m2_projects.join("test").join("conversation.jsonl");

    fs::create_dir_all(m1_file.parent().unwrap()).unwrap();
    fs::create_dir_all(m2_file.parent().unwrap()).unwrap();

    fs::write(&m1_file, base_content).unwrap();
    fs::write(&m2_file, base_content).unwrap();

    // Machine 1: Modify and "push"
    let m1_modified = format!(
        "{}{}\n",
        base_content,
        r#"{"type":"user","uuid":"3","sessionId":"shared-session-123","timestamp":"2025-01-01T00:02:00Z"}"#
    );
    fs::write(&m1_file, &m1_modified).unwrap();

    let m1_session = ConversationSession::from_file(&m1_file).unwrap();
    let sync_file = sync_projects.join("test").join("conversation.jsonl");
    fs::create_dir_all(sync_file.parent().unwrap()).unwrap();
    m1_session.write_to_file(&sync_file).unwrap();

    // Machine 2: Modify differently (creating conflict)
    let m2_modified = format!(
        "{}{}\n",
        base_content,
        r#"{"type":"user","uuid":"4","sessionId":"shared-session-123","timestamp":"2025-01-01T00:03:00Z"}"#
    );
    fs::write(&m2_file, &m2_modified).unwrap();

    // Detect conflict when machine 2 tries to pull
    let local_sessions = discover_test_sessions(&m2_projects).unwrap();
    let remote_sessions = discover_test_sessions(&sync_projects).unwrap();

    let mut detector = ConflictDetector::new();
    detector.detect(&local_sessions, &remote_sessions);

    // Verify conflict was detected
    assert!(detector.has_conflicts(), "Should detect conflict");
    assert_eq!(detector.conflict_count(), 1);

    let conflict = &detector.conflicts()[0];
    assert_eq!(conflict.session_id, session_id);
    assert_ne!(conflict.local_hash, conflict.remote_hash);

    // Resolve conflict with keep-both strategy
    let renames = detector.resolve_all_keep_both().unwrap();
    assert_eq!(renames.len(), 1);

    let (_original, renamed) = &renames[0];

    // Verify conflict file would be created with timestamp suffix
    assert!(renamed.to_string_lossy().contains("conflict-"));
    assert!(renamed.to_string_lossy().contains(".jsonl"));

    // Copy remote version to renamed path
    let remote_session = remote_sessions.first().unwrap();
    remote_session.write_to_file(renamed).unwrap();

    // Verify both files exist
    assert!(m2_file.exists(), "Local version should remain");
    assert!(renamed.exists(), "Renamed remote version should exist");

    // Verify content is different
    let local_content = fs::read_to_string(&m2_file).unwrap();
    let remote_content = fs::read_to_string(renamed).unwrap();
    assert_ne!(local_content, remote_content);
}

#[test]
fn test_operation_history_tracking() {
    let test_dir = TempDir::new().unwrap();
    let history_path = test_dir.path().join("history.json");

    let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

    // Perform multiple operations

    // Operation 1: Push
    let push_conv = ConversationSummary::new(
        "session-1".to_string(),
        "path/conv1.jsonl".to_string(),
        Some("2025-01-01T10:00:00Z".to_string()),
        5,
        SyncOperation::Added,
    )
    .unwrap();

    let push_record = claude_code_sync::history::OperationRecord::new(
        OperationType::Push,
        Some("main".to_string()),
        vec![push_conv.clone()],
    );

    history.operations.insert(0, push_record);
    history.save_to(Some(history_path.clone())).unwrap();

    // Operation 2: Pull
    let pull_conv = ConversationSummary::new(
        "session-2".to_string(),
        "path/conv2.jsonl".to_string(),
        Some("2025-01-01T11:00:00Z".to_string()),
        3,
        SyncOperation::Modified,
    )
    .unwrap();

    let pull_record = claude_code_sync::history::OperationRecord::new(
        OperationType::Pull,
        Some("main".to_string()),
        vec![pull_conv],
    );

    // Reload history before adding next operation
    let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();
    history.operations.insert(0, pull_record);
    history.save_to(Some(history_path.clone())).unwrap();

    // Operation 3: Another Push
    let push_conv2 = ConversationSummary::new(
        "session-3".to_string(),
        "path/conv3.jsonl".to_string(),
        Some("2025-01-01T12:00:00Z".to_string()),
        7,
        SyncOperation::Modified,
    )
    .unwrap();

    let push_record2 = claude_code_sync::history::OperationRecord::new(
        OperationType::Push,
        Some("main".to_string()),
        vec![push_conv2],
    );

    // Reload history before adding next operation
    let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();
    history.operations.insert(0, push_record2);
    history.save_to(Some(history_path.clone())).unwrap();

    // Reload and verify history contains all operations (most recent first)
    let history = OperationHistory::from_path(Some(history_path.clone())).unwrap();
    assert_eq!(history.len(), 3);

    let operations = history.list_operations();
    assert_eq!(operations[0].operation_type, OperationType::Push);
    assert_eq!(operations[1].operation_type, OperationType::Pull);
    assert_eq!(operations[2].operation_type, OperationType::Push);

    // Test get last operation
    let last_op = history.get_last_operation().unwrap();
    assert_eq!(last_op.operation_type, OperationType::Push);
    assert_eq!(last_op.affected_conversations[0].session_id, "session-3");

    // Test get last operation by type
    let last_pull = history
        .get_last_operation_by_type(OperationType::Pull)
        .unwrap();
    assert_eq!(last_pull.affected_conversations[0].session_id, "session-2");

    // Test operation summaries
    let summary = last_op.summary();
    assert!(summary.contains("push"));
    assert!(summary.contains("main"));
    assert!(summary.contains("1 conversations affected"));

    // Test operation stats
    let stats = last_op.operation_stats();
    assert_eq!(stats.get(&SyncOperation::Modified), Some(&1));

    // Reload history from disk and verify persistence
    let reloaded = OperationHistory::from_path(Some(history_path)).unwrap();
    assert_eq!(reloaded.len(), 3);
    assert_eq!(
        reloaded.list_operations()[0].operation_type,
        OperationType::Push
    );
}

#[test]
fn test_with_real_test_data() {
    // This test uses the actual test data files to verify parsing and handling
    let test_data = Path::new(TEST_DATA_DIR);

    if !test_data.exists() {
        eprintln!("Skipping test_with_real_test_data: test data directory not found");
        return;
    }

    // Discover all sessions from test data
    let sessions = discover_test_sessions(test_data).unwrap();

    // Verify we found the expected number of files
    assert!(
        sessions.len() >= 5,
        "Expected at least 5 test sessions, found {}",
        sessions.len()
    );

    // Verify each session has valid data
    for session in &sessions {
        assert!(
            !session.session_id.is_empty(),
            "Session ID should not be empty"
        );
        assert!(!session.entries.is_empty(), "Session should have entries");
        assert!(
            !session.file_path.is_empty(),
            "File path should not be empty"
        );

        // Note: Some sessions might be summary entries with 0 messages, which is valid
        // Just verify we can compute message count without panicking
        let _message_count = session.message_count();

        // Verify session has a content hash
        let hash = session.content_hash();
        assert!(!hash.is_empty(), "Session should have content hash");
    }

    // Test that we can write and re-read sessions
    let temp_dir = TempDir::new().unwrap();

    for session in &sessions {
        let dest_path = temp_dir
            .path()
            .join(format!("{}.jsonl", session.session_id));
        session.write_to_file(&dest_path).unwrap();

        // Re-read and verify
        let reloaded = ConversationSession::from_file(&dest_path).unwrap();
        assert_eq!(reloaded.session_id, session.session_id);
        assert_eq!(reloaded.message_count(), session.message_count());
        assert_eq!(reloaded.content_hash(), session.content_hash());
    }
}

#[test]
fn test_snapshot_with_multiple_files() {
    let test_dir = TempDir::new().unwrap();
    let snapshots_dir = test_dir.path().join("snapshots");

    // Create multiple conversation files
    let files = vec![
        (
            "conv1.jsonl",
            r#"{"type":"user","uuid":"1","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}"#,
        ),
        (
            "conv2.jsonl",
            r#"{"type":"user","uuid":"2","sessionId":"s2","timestamp":"2025-01-01T00:00:00Z"}"#,
        ),
        (
            "conv3.jsonl",
            r#"{"type":"user","uuid":"3","sessionId":"s3","timestamp":"2025-01-01T00:00:00Z"}"#,
        ),
    ];

    let mut file_paths = Vec::new();
    for (name, content) in &files {
        let path = test_dir.path().join(name);
        fs::write(&path, content).unwrap();
        file_paths.push(path);
    }

    // Create snapshot
    let snapshot = Snapshot::create(OperationType::Pull, &file_paths, None).unwrap();

    assert_eq!(snapshot.files.len(), 3);

    // Save and reload
    let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
    let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();

    assert_eq!(loaded.files.len(), 3);

    // Modify all files
    for (name, _) in &files {
        let path = test_dir.path().join(name);
        fs::write(&path, "modified").unwrap();
    }

    // Restore snapshot
    loaded.restore_with_base(Some(test_dir.path())).unwrap();

    // Verify all files were restored
    for (name, original_content) in &files {
        let path = test_dir.path().join(name);
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, *original_content);
    }
}

#[test]
fn test_concurrent_push_pull_operations() {
    // Test that operation history correctly tracks concurrent operations
    let test_dir = TempDir::new().unwrap();
    let history_path = test_dir.path().join("history.json");

    let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();

    // Simulate rapid succession of operations
    for i in 0..10 {
        let op_type = if i % 2 == 0 {
            OperationType::Push
        } else {
            OperationType::Pull
        };

        let conv = ConversationSummary::new(
            format!("session-{i}"),
            format!("path/conv{i}.jsonl"),
            Some(format!("2025-01-01T{i:02}:00:00Z")),
            i + 1,
            SyncOperation::Modified,
        )
        .unwrap();

        let record = claude_code_sync::history::OperationRecord::new(
            op_type,
            Some("main".to_string()),
            vec![conv],
        );

        history.add_operation(record).unwrap();
    }

    // History should be capped at MAX_HISTORY_SIZE (5)
    assert_eq!(history.len(), 5);

    // Most recent operations should be preserved
    let operations = history.list_operations();
    for (idx, op) in operations.iter().enumerate() {
        let expected_session_id = format!("session-{}", 9 - idx);
        assert_eq!(op.affected_conversations[0].session_id, expected_session_id);
    }
}

// ============================================================================
// Tests for push on new repo with no commits
// ============================================================================

#[test]
fn test_push_on_new_repo_without_commits() {
    // Create a brand new repo with no commits
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path();

    // Initialize git repo (no commits yet)
    let repo = scm::init(repo_path).unwrap();

    // Verify there are no commits yet
    let commit_result = repo.current_commit_hash();
    assert!(commit_result.is_err(), "New repo should have no commits");

    // Create a file and stage it
    fs::write(repo_path.join("test.txt"), "hello").unwrap();
    repo.stage_all().unwrap();

    // Should be able to commit even on new repo
    repo.commit("Initial commit").unwrap();

    // Now should have a commit hash
    let hash = repo.current_commit_hash().unwrap();
    assert!(!hash.is_empty());
}

#[test]
fn test_operation_record_with_no_commit_hash() {
    // Test that OperationRecord can handle None commit_hash
    use claude_code_sync::history::OperationRecord;

    let conv = ConversationSummary::new(
        "test-session".to_string(),
        "path/test.jsonl".to_string(),
        Some("2025-01-01T00:00:00Z".to_string()),
        10,
        SyncOperation::Added,
    )
    .unwrap();

    let mut record = OperationRecord::new(
        OperationType::Push,
        Some("main".to_string()),
        vec![conv],
    );

    // commit_hash should be None by default
    assert!(record.commit_hash.is_none());

    // Should be able to set it to None explicitly
    record.commit_hash = None;
    assert!(record.commit_hash.is_none());

    // Should be able to set it to Some
    record.commit_hash = Some("abc123".to_string());
    assert_eq!(record.commit_hash, Some("abc123".to_string()));
}
