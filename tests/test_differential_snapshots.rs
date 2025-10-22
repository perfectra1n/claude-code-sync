use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

use claude_code_sync::git::GitManager;
use claude_code_sync::history::OperationType;
use claude_code_sync::parser::ConversationEntry;
use claude_code_sync::undo::Snapshot;

// ============================================================================
// Test Helper Functions
// ============================================================================

/// Create a large conversation file with realistic content
///
/// Generates a JSONL file with multiple conversation entries to simulate
/// a real Claude Code conversation. The size is controlled by the number
/// of messages and their content size.
fn create_large_conversation(
    base_dir: &Path,
    name: &str,
    target_size_bytes: usize,
) -> Result<PathBuf> {
    let file_path = base_dir.join(format!("{}.jsonl", name));

    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file_content = String::new();
    let session_id = format!("session-{}", name);

    // Calculate how many messages we need to reach target size
    // Each message is roughly 500-1000 bytes, so estimate conservatively
    let estimated_message_size = 800;
    let message_count = (target_size_bytes / estimated_message_size).max(10);

    for i in 0..message_count {
        let timestamp = format!("2025-10-22T{:02}:{:02}:00.000Z", i % 24, i % 60);

        // Alternate between user and assistant messages
        if i % 2 == 0 {
            // User message
            let entry = ConversationEntry {
                entry_type: "user".to_string(),
                uuid: Some(format!("uuid-user-{}", i)),
                parent_uuid: if i > 0 {
                    Some(format!("uuid-assistant-{}", i - 1))
                } else {
                    None
                },
                session_id: Some(session_id.clone()),
                timestamp: Some(timestamp),
                message: Some(serde_json::json!({
                    "text": format!("User message {} with some content to make it realistic. This simulates a typical user query that might be sent to Claude Code. It includes context and details about the task.", i),
                    "role": "user"
                })),
                cwd: Some("/home/user/project".to_string()),
                version: Some("1.0.0".to_string()),
                git_branch: Some("main".to_string()),
                extra: serde_json::json!({}),
            };

            file_content.push_str(&serde_json::to_string(&entry)?);
            file_content.push('\n');
        } else {
            // Assistant message with more content
            let entry = ConversationEntry {
                entry_type: "assistant".to_string(),
                uuid: Some(format!("uuid-assistant-{}", i)),
                parent_uuid: Some(format!("uuid-user-{}", i - 1)),
                session_id: Some(session_id.clone()),
                timestamp: Some(timestamp),
                message: Some(serde_json::json!({
                    "text": format!("Assistant response {} explaining the solution in detail. This is a longer response that includes code examples, explanations, and step-by-step instructions. The assistant provides comprehensive guidance and answers the user's question thoroughly with additional context and best practices. Here's some padding content to make this message larger and more realistic: Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.", i),
                    "role": "assistant"
                })),
                cwd: Some("/home/user/project".to_string()),
                version: Some("1.0.0".to_string()),
                git_branch: Some("main".to_string()),
                extra: serde_json::json!({}),
            };

            file_content.push_str(&serde_json::to_string(&entry)?);
            file_content.push('\n');
        }
    }

    fs::write(&file_path, file_content)?;

    // Verify size is close to target
    let actual_size = fs::metadata(&file_path)?.len();
    println!(
        "Created conversation {}: {} bytes (target: {} bytes)",
        name, actual_size, target_size_bytes
    );

    Ok(file_path)
}

/// Modify a conversation by appending a new message
fn modify_conversation(conv_path: &Path, additional_message: &str) -> Result<()> {
    let mut content = fs::read_to_string(conv_path)?;

    // Parse existing content to get session_id
    let first_line = content.lines().next().unwrap_or("{}");
    let first_entry: ConversationEntry = serde_json::from_str(first_line)?;
    let session_id = first_entry.session_id.unwrap_or_else(|| "session-unknown".to_string());

    // Count existing entries to generate unique UUID
    let entry_count = content.lines().filter(|l| !l.trim().is_empty()).count();

    let new_entry = ConversationEntry {
        entry_type: "user".to_string(),
        uuid: Some(format!("uuid-new-{}", entry_count)),
        parent_uuid: None,
        session_id: Some(session_id),
        timestamp: Some("2025-10-22T12:00:00.000Z".to_string()),
        message: Some(serde_json::json!({
            "text": additional_message,
            "role": "user"
        })),
        cwd: Some("/home/user/project".to_string()),
        version: Some("1.0.0".to_string()),
        git_branch: Some("main".to_string()),
        extra: serde_json::json!({}),
    };

    content.push_str(&serde_json::to_string(&new_entry)?);
    content.push('\n');

    fs::write(conv_path, content)?;
    Ok(())
}

/// Calculate the total size of a snapshot (JSON file size on disk)
fn calculate_snapshot_size(snapshot_path: &Path) -> Result<u64> {
    let metadata = fs::metadata(snapshot_path)?;
    Ok(metadata.len())
}

/// Helper to push conversations and get the created snapshot
fn push_and_get_snapshot(
    snapshots_dir: &Path,
    file_paths: &[PathBuf],
    git_manager: Option<&GitManager>,
) -> Result<(Snapshot, PathBuf)> {
    // Create differential snapshot
    let snapshot = Snapshot::create_differential_with_dir(
        OperationType::Push,
        file_paths.iter(),
        git_manager,
        Some(snapshots_dir),
    )?;

    // Save to disk
    let snapshot_path = snapshot.save_to_disk(Some(snapshots_dir))?;

    Ok((snapshot, snapshot_path))
}

// ============================================================================
// Integration Tests
// ============================================================================

#[test]
fn test_differential_snapshots_minimize_disk_usage() {
    println!("\n=== Test: Differential Snapshots Minimize Disk Usage ===\n");

    let temp_dir = TempDir::new().unwrap();
    let snapshots_dir = temp_dir.path().join("snapshots");
    let conversations_dir = temp_dir.path().join("conversations");

    fs::create_dir_all(&snapshots_dir).unwrap();
    fs::create_dir_all(&conversations_dir).unwrap();

    // Create large conversations (1 MB each) to make size differences obvious
    println!("Creating large conversation files...");
    let conv1 = create_large_conversation(&conversations_dir, "conv1", 1_000_000).unwrap();
    let conv2 = create_large_conversation(&conversations_dir, "conv2", 1_000_000).unwrap();
    let conv3 = create_large_conversation(&conversations_dir, "conv3", 1_000_000).unwrap();

    let all_files = vec![conv1.clone(), conv2.clone(), conv3.clone()];

    // ========================================================================
    // First Push: Creates full snapshot
    // ========================================================================
    println!("\nFirst push: Creating full snapshot...");
    let (snapshot1, snapshot1_path) = push_and_get_snapshot(
        &snapshots_dir,
        &all_files,
        None,
    ).unwrap();

    assert!(
        snapshot1.base_snapshot_id.is_none(),
        "First snapshot should be full (no base)"
    );

    let size1 = calculate_snapshot_size(&snapshot1_path).unwrap();
    println!("  Snapshot 1 size: {} bytes ({:.2} MB)", size1, size1 as f64 / 1_000_000.0);
    println!("  Files in snapshot: {}", snapshot1.files.len());

    // Full snapshot should contain all files
    assert_eq!(
        snapshot1.files.len(),
        3,
        "Full snapshot should contain all 3 files"
    );

    // Full snapshot should be substantial in size (at least 500KB after base64 encoding)
    assert!(
        size1 > 500_000,
        "Full snapshot should be > 500KB, got {} bytes",
        size1
    );

    // ========================================================================
    // Second Push: No changes, should create tiny differential
    // ========================================================================
    println!("\nSecond push: No changes, creating differential snapshot...");
    let (snapshot2, snapshot2_path) = push_and_get_snapshot(
        &snapshots_dir,
        &all_files,
        None,
    ).unwrap();

    assert!(
        snapshot2.base_snapshot_id.is_some(),
        "Second snapshot should be differential (has base)"
    );
    assert_eq!(
        snapshot2.base_snapshot_id.as_ref().unwrap(),
        &snapshot1.snapshot_id,
        "Base should reference first snapshot"
    );

    let size2 = calculate_snapshot_size(&snapshot2_path).unwrap();
    println!("  Snapshot 2 size: {} bytes ({:.2} KB)", size2, size2 as f64 / 1_000.0);
    println!("  Files in snapshot: {}", snapshot2.files.len());
    println!("  Deleted files: {}", snapshot2.deleted_files.len());

    // KEY ASSERTION: No changes means nearly empty differential
    assert!(
        snapshot2.files.is_empty(),
        "No files should have changed (expected 0, got {})",
        snapshot2.files.len()
    );
    assert!(
        snapshot2.deleted_files.is_empty(),
        "No files should be deleted"
    );
    assert!(
        size2 < 5_000,
        "Differential snapshot with no changes should be < 5KB, got {} bytes",
        size2
    );

    println!("  ✓ Space saved: {:.2}% compared to full snapshot",
        (1.0 - size2 as f64 / size1 as f64) * 100.0);

    // ========================================================================
    // Third Push: Small change to one file
    // ========================================================================
    println!("\nThird push: Modifying one conversation...");
    modify_conversation(&conv1, "This is a small additional message").unwrap();

    let (snapshot3, snapshot3_path) = push_and_get_snapshot(
        &snapshots_dir,
        &all_files,
        None,
    ).unwrap();

    assert!(
        snapshot3.base_snapshot_id.is_some(),
        "Third snapshot should be differential"
    );
    assert_eq!(
        snapshot3.base_snapshot_id.as_ref().unwrap(),
        &snapshot2.snapshot_id,
        "Base should reference second snapshot"
    );

    let size3 = calculate_snapshot_size(&snapshot3_path).unwrap();
    println!("  Snapshot 3 size: {} bytes ({:.2} KB)", size3, size3 as f64 / 1_000.0);
    println!("  Files in snapshot: {}", snapshot3.files.len());

    // KEY ASSERTION: Only one file should be in differential
    assert_eq!(
        snapshot3.files.len(),
        1,
        "Only one file should be in differential (expected 1, got {})",
        snapshot3.files.len()
    );

    // The snapshot should only contain the changed file
    let conv1_key = conv1.to_string_lossy().to_string();
    assert!(
        snapshot3.files.contains_key(&conv1_key),
        "Changed file should be in snapshot"
    );

    // Size should be proportional to one file, not all three
    assert!(
        size3 < size1 / 2,
        "Differential snapshot should be much smaller than full (got {} bytes, full was {} bytes)",
        size3,
        size1
    );

    println!("  ✓ Space saved: {:.2}% compared to full snapshot",
        (1.0 - size3 as f64 / size1 as f64) * 100.0);

    // ========================================================================
    // Fourth Push: Add a new file
    // ========================================================================
    println!("\nFourth push: Adding new conversation...");
    let conv4 = create_large_conversation(&conversations_dir, "conv4", 1_000_000).unwrap();
    let all_files_with_new = vec![conv1.clone(), conv2.clone(), conv3.clone(), conv4.clone()];

    let (snapshot4, snapshot4_path) = push_and_get_snapshot(
        &snapshots_dir,
        &all_files_with_new,
        None,
    ).unwrap();

    let size4 = calculate_snapshot_size(&snapshot4_path).unwrap();
    println!("  Snapshot 4 size: {} bytes ({:.2} KB)", size4, size4 as f64 / 1_000.0);
    println!("  Files in snapshot: {}", snapshot4.files.len());

    // Should contain only the new file
    assert_eq!(
        snapshot4.files.len(),
        1,
        "Only new file should be in differential"
    );

    let conv4_key = conv4.to_string_lossy().to_string();
    assert!(
        snapshot4.files.contains_key(&conv4_key),
        "New file should be in snapshot"
    );

    // ========================================================================
    // Verify overall space savings
    // ========================================================================
    let total_differential_size = size2 + size3 + size4;
    let would_be_full_size = size1 * 3; // If we had created three full snapshots
    let savings_ratio = 1.0 - (total_differential_size as f64 / would_be_full_size as f64);

    println!("\n=== Summary ===");
    println!("Full snapshot size: {:.2} MB", size1 as f64 / 1_000_000.0);
    println!("Total differential size: {:.2} MB", total_differential_size as f64 / 1_000_000.0);
    println!("Would-be full size: {:.2} MB", would_be_full_size as f64 / 1_000_000.0);
    println!("Space savings: {:.1}%", savings_ratio * 100.0);

    assert!(
        savings_ratio > 0.70,
        "Should save > 70% disk space (saved {:.1}%)",
        savings_ratio * 100.0
    );

    println!("\n✓ Test passed! Differential snapshots work correctly.\n");
}

#[test]
fn test_snapshot_chain_reconstruction() {
    println!("\n=== Test: Snapshot Chain Reconstruction ===\n");

    let temp_dir = TempDir::new().unwrap();
    let snapshots_dir = temp_dir.path().join("snapshots");
    let conversations_dir = temp_dir.path().join("conversations");

    fs::create_dir_all(&snapshots_dir).unwrap();
    fs::create_dir_all(&conversations_dir).unwrap();

    // Create test files with known content
    let file1 = conversations_dir.join("file1.txt");
    let file2 = conversations_dir.join("file2.txt");
    let file3 = conversations_dir.join("file3.txt");

    fs::write(&file1, b"version_1").unwrap();
    fs::write(&file2, b"version_1").unwrap();

    // Create chain of snapshots
    println!("Creating snapshot chain...");

    // Snapshot 1: Initial state (file1, file2)
    let (snapshot1, _) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone(), file2.clone()],
        None,
    ).unwrap();
    println!("  Snapshot 1: 2 files");

    // Snapshot 2: Modify file1, add file3
    fs::write(&file1, b"version_2").unwrap();
    fs::write(&file3, b"version_2").unwrap();

    let (snapshot2, _) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone(), file2.clone(), file3.clone()],
        None,
    ).unwrap();
    println!("  Snapshot 2: {} files (differential)", snapshot2.files.len());

    // Snapshot 3: Modify file2
    fs::write(&file2, b"version_3").unwrap();

    let (snapshot3, _) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone(), file2.clone(), file3.clone()],
        None,
    ).unwrap();
    println!("  Snapshot 3: {} files (differential)", snapshot3.files.len());

    // Snapshot 4: Modify file3
    fs::write(&file3, b"version_4").unwrap();

    let (snapshot4, _) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone(), file2.clone(), file3.clone()],
        None,
    ).unwrap();
    println!("  Snapshot 4: {} files (differential)", snapshot4.files.len());

    // Verify chain structure
    assert!(snapshot1.base_snapshot_id.is_none(), "First snapshot should have no base");
    assert_eq!(
        snapshot2.base_snapshot_id.as_ref().unwrap(),
        &snapshot1.snapshot_id,
        "Snapshot 2 should reference snapshot 1"
    );
    assert_eq!(
        snapshot3.base_snapshot_id.as_ref().unwrap(),
        &snapshot2.snapshot_id,
        "Snapshot 3 should reference snapshot 2"
    );
    assert_eq!(
        snapshot4.base_snapshot_id.as_ref().unwrap(),
        &snapshot3.snapshot_id,
        "Snapshot 4 should reference snapshot 3"
    );

    // Reconstruct full state from snapshot 4
    println!("\nReconstructing full state from snapshot 4...");
    let full_state = snapshot4.reconstruct_full_state_with_dir(Some(&snapshots_dir)).unwrap();

    println!("  Reconstructed {} files", full_state.len());

    // Verify reconstructed state
    assert_eq!(full_state.len(), 3, "Should have 3 files");

    let file1_key = file1.to_string_lossy().to_string();
    let file2_key = file2.to_string_lossy().to_string();
    let file3_key = file3.to_string_lossy().to_string();

    assert_eq!(
        full_state.get(&file1_key).unwrap(),
        b"version_2",
        "file1 should be version_2 (modified in snapshot 2)"
    );
    assert_eq!(
        full_state.get(&file2_key).unwrap(),
        b"version_3",
        "file2 should be version_3 (modified in snapshot 3)"
    );
    assert_eq!(
        full_state.get(&file3_key).unwrap(),
        b"version_4",
        "file3 should be version_4 (modified in snapshot 4)"
    );

    println!("\n✓ Test passed! Snapshot chain reconstruction works correctly.\n");
}

#[test]
fn test_deleted_files_tracking() {
    println!("\n=== Test: Deleted Files Tracking ===\n");

    let temp_dir = TempDir::new().unwrap();
    let snapshots_dir = temp_dir.path().join("snapshots");
    let conversations_dir = temp_dir.path().join("conversations");

    fs::create_dir_all(&snapshots_dir).unwrap();
    fs::create_dir_all(&conversations_dir).unwrap();

    let file1 = conversations_dir.join("file1.txt");
    let file2 = conversations_dir.join("file2.txt");
    let file3 = conversations_dir.join("file3.txt");

    // Create initial state with all three files
    fs::write(&file1, b"content1").unwrap();
    fs::write(&file2, b"content2").unwrap();
    fs::write(&file3, b"content3").unwrap();

    println!("Creating initial snapshot with 3 files...");
    let (snapshot1, _) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone(), file2.clone(), file3.clone()],
        None,
    ).unwrap();

    assert_eq!(snapshot1.files.len(), 3);
    assert!(snapshot1.deleted_files.is_empty());

    // Delete file2
    println!("Deleting file2...");
    fs::remove_file(&file2).unwrap();

    let (snapshot2, _) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone(), file3.clone()],
        None,
    ).unwrap();

    println!("  Snapshot 2 deleted files: {:?}", snapshot2.deleted_files);

    // Verify deletion tracking
    assert_eq!(
        snapshot2.deleted_files.len(),
        1,
        "Should track one deleted file"
    );

    let file2_key = file2.to_string_lossy().to_string();
    assert!(
        snapshot2.deleted_files.contains(&file2_key),
        "Should track file2 as deleted"
    );

    // Reconstruct and verify file2 is not in the state
    let full_state = snapshot2.reconstruct_full_state_with_dir(Some(&snapshots_dir)).unwrap();
    assert_eq!(full_state.len(), 2, "Should only have 2 files after deletion");
    assert!(!full_state.contains_key(&file2_key), "file2 should not be in reconstructed state");

    // Delete another file
    println!("Deleting file3...");
    fs::remove_file(&file3).unwrap();

    let (snapshot3, _) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone()],
        None,
    ).unwrap();

    println!("  Snapshot 3 deleted files: {:?}", snapshot3.deleted_files);

    // Verify incremental deletion tracking
    let file3_key = file3.to_string_lossy().to_string();
    assert!(
        snapshot3.deleted_files.contains(&file3_key),
        "Should track file3 as deleted"
    );

    // Reconstruct final state
    let final_state = snapshot3.reconstruct_full_state_with_dir(Some(&snapshots_dir)).unwrap();
    assert_eq!(final_state.len(), 1, "Should only have 1 file remaining");

    let file1_key = file1.to_string_lossy().to_string();
    assert!(final_state.contains_key(&file1_key), "file1 should remain");

    println!("\n✓ Test passed! Deleted files tracking works correctly.\n");
}

#[test]
fn test_broken_snapshot_chain() {
    println!("\n=== Test: Broken Snapshot Chain Error Handling ===\n");

    let temp_dir = TempDir::new().unwrap();
    let snapshots_dir = temp_dir.path().join("snapshots");
    let conversations_dir = temp_dir.path().join("conversations");

    fs::create_dir_all(&snapshots_dir).unwrap();
    fs::create_dir_all(&conversations_dir).unwrap();

    let file1 = conversations_dir.join("file1.txt");
    fs::write(&file1, b"content1").unwrap();

    // Create snapshot 1
    let (snapshot1, _snapshot1_path) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone()],
        None,
    ).unwrap();

    // Modify and create snapshot 2
    fs::write(&file1, b"content2").unwrap();
    let (_snapshot2, snapshot2_path) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone()],
        None,
    ).unwrap();

    // Modify and create snapshot 3
    fs::write(&file1, b"content3").unwrap();
    let (snapshot3, _snapshot3_path) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone()],
        None,
    ).unwrap();

    println!("Created chain: snapshot1 <- snapshot2 <- snapshot3");

    // Delete the middle snapshot (break the chain)
    println!("Deleting middle snapshot (snapshot2)...");
    fs::remove_file(&snapshot2_path).unwrap();

    // Try to reconstruct from snapshot3 - should fail
    println!("Attempting to reconstruct from snapshot3...");
    let result = snapshot3.reconstruct_full_state_with_dir(Some(&snapshots_dir));

    assert!(result.is_err(), "Should fail when base snapshot is missing");
    let error_msg = result.unwrap_err().to_string();
    println!("  Error: {}", error_msg);
    assert!(
        error_msg.contains("Base snapshot not found") || error_msg.contains("snapshot chain is broken"),
        "Error should mention missing base snapshot"
    );

    // Verify snapshot1 can still be reconstructed (it's a full snapshot)
    println!("Verifying snapshot1 can still be reconstructed...");
    let state1 = snapshot1.reconstruct_full_state_with_dir(Some(&snapshots_dir)).unwrap();
    assert_eq!(state1.len(), 1, "Should successfully reconstruct snapshot1");

    println!("\n✓ Test passed! Broken chain detection works correctly.\n");
}

#[test]
fn test_differential_snapshot_with_git() {
    println!("\n=== Test: Differential Snapshots with Git Integration ===\n");

    let temp_dir = TempDir::new().unwrap();
    let git_repo = temp_dir.path().join("repo");
    let snapshots_dir = temp_dir.path().join("snapshots");

    fs::create_dir_all(&git_repo).unwrap();
    fs::create_dir_all(&snapshots_dir).unwrap();

    // Initialize git repository
    let git_manager = GitManager::init(&git_repo).unwrap();

    // Create initial commit
    let file1 = git_repo.join("file1.txt");
    fs::write(&file1, b"initial content").unwrap();
    git_manager.stage_all().unwrap();
    git_manager.commit("Initial commit").unwrap();

    let initial_commit = git_manager.current_commit_hash().unwrap();
    println!("Initial commit: {}", &initial_commit[..8]);

    // Create first snapshot with git info
    println!("Creating snapshot with git information...");
    let (snapshot1, _) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone()],
        Some(&git_manager),
    ).unwrap();

    assert!(snapshot1.git_commit_hash.is_some(), "Snapshot should capture git commit hash");
    assert_eq!(
        snapshot1.git_commit_hash.as_ref().unwrap(),
        &initial_commit,
        "Should capture correct commit hash"
    );
    assert!(snapshot1.branch.is_some(), "Snapshot should capture branch name");

    // Make changes and commit
    fs::write(&file1, b"modified content").unwrap();
    git_manager.stage_all().unwrap();
    git_manager.commit("Second commit").unwrap();

    let second_commit = git_manager.current_commit_hash().unwrap();
    println!("Second commit: {}", &second_commit[..8]);

    // Create differential snapshot
    let (snapshot2, _) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone()],
        Some(&git_manager),
    ).unwrap();

    assert!(snapshot2.base_snapshot_id.is_some(), "Should be differential");
    assert_eq!(
        snapshot2.git_commit_hash.as_ref().unwrap(),
        &second_commit,
        "Should capture new commit hash"
    );

    println!("\n✓ Test passed! Git integration with differential snapshots works correctly.\n");
}

#[test]
fn test_performance_differential_vs_full() {
    println!("\n=== Test: Performance Comparison (Differential vs Full) ===\n");

    let temp_dir = TempDir::new().unwrap();
    let snapshots_dir = temp_dir.path().join("snapshots");
    let conversations_dir = temp_dir.path().join("conversations");

    fs::create_dir_all(&snapshots_dir).unwrap();
    fs::create_dir_all(&conversations_dir).unwrap();

    // Create multiple large conversation files
    println!("Creating 5 large conversation files...");
    let mut files = Vec::new();
    for i in 0..5 {
        let file = create_large_conversation(&conversations_dir, &format!("perf_test_{}", i), 500_000).unwrap();
        files.push(file);
    }

    // Measure full snapshot creation time
    println!("\nMeasuring full snapshot creation...");
    let start = std::time::Instant::now();
    let snapshot = Snapshot::create(
        OperationType::Push,
        files.iter(),
        None,
    ).unwrap();
    let full_duration = start.elapsed();
    let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
    let full_size = calculate_snapshot_size(&snapshot_path).unwrap();

    println!("  Full snapshot: {:.2}ms, {:.2} MB",
        full_duration.as_secs_f64() * 1000.0,
        full_size as f64 / 1_000_000.0
    );

    // Modify one file slightly
    modify_conversation(&files[0], "Small change for performance test").unwrap();

    // Measure differential snapshot creation time
    println!("Measuring differential snapshot creation...");
    let start = std::time::Instant::now();
    let diff_snapshot = Snapshot::create_differential_with_dir(
        OperationType::Push,
        files.iter(),
        None,
        Some(&snapshots_dir),
    ).unwrap();
    let diff_duration = start.elapsed();
    let diff_snapshot_path = diff_snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
    let diff_size = calculate_snapshot_size(&diff_snapshot_path).unwrap();

    println!("  Differential snapshot: {:.2}ms, {:.2} KB",
        diff_duration.as_secs_f64() * 1000.0,
        diff_size as f64 / 1_000.0
    );

    // Differential should be significantly faster and smaller
    let time_speedup = full_duration.as_secs_f64() / diff_duration.as_secs_f64();
    let space_savings = 1.0 - (diff_size as f64 / full_size as f64);

    println!("\n=== Results ===");
    println!("Time speedup: {:.1}x", time_speedup);
    println!("Space savings: {:.1}%", space_savings * 100.0);

    // Differential should save significant space (>70% for this test case)
    // Note: The exact savings depends on JSON overhead and base64 encoding
    assert!(
        space_savings > 0.70,
        "Differential should save >70% space (saved {:.1}%)",
        space_savings * 100.0
    );

    println!("\n✓ Test passed! Differential snapshots are more efficient.\n");
}

#[test]
fn test_empty_differential_snapshot() {
    println!("\n=== Test: Empty Differential Snapshot (No Changes) ===\n");

    let temp_dir = TempDir::new().unwrap();
    let snapshots_dir = temp_dir.path().join("snapshots");
    let conversations_dir = temp_dir.path().join("conversations");

    fs::create_dir_all(&snapshots_dir).unwrap();
    fs::create_dir_all(&conversations_dir).unwrap();

    let file1 = conversations_dir.join("file1.txt");
    fs::write(&file1, b"static content").unwrap();

    // Create first snapshot
    println!("Creating initial snapshot...");
    let (_snapshot1, snapshot1_path) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone()],
        None,
    ).unwrap();
    let size1 = calculate_snapshot_size(&snapshot1_path).unwrap();
    println!("  Initial snapshot: {} bytes", size1);

    // Create second snapshot with NO changes
    println!("Creating differential snapshot with no changes...");
    let (snapshot2, snapshot2_path) = push_and_get_snapshot(
        &snapshots_dir,
        &[file1.clone()],
        None,
    ).unwrap();
    let size2 = calculate_snapshot_size(&snapshot2_path).unwrap();
    println!("  Differential snapshot: {} bytes", size2);

    // Should be nearly empty
    assert!(snapshot2.files.is_empty(), "No files should have changed");
    assert!(snapshot2.deleted_files.is_empty(), "No files should be deleted");
    assert!(size2 < 1_000, "Should be < 1KB (got {} bytes)", size2);

    // Verify reconstruction still works
    let reconstructed = snapshot2.reconstruct_full_state_with_dir(Some(&snapshots_dir)).unwrap();
    assert_eq!(reconstructed.len(), 1, "Should reconstruct 1 file");

    let file1_key = file1.to_string_lossy().to_string();
    assert_eq!(
        reconstructed.get(&file1_key).unwrap(),
        b"static content",
        "Content should be preserved"
    );

    println!("\n✓ Test passed! Empty differential snapshots work correctly.\n");
}
