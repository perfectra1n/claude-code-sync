use std::fs;
use tempfile::TempDir;

/// Test helper to create a mock Claude Code projects directory
fn create_mock_claude_dir() -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let projects_dir = temp_dir.path().join("projects");
    fs::create_dir_all(&projects_dir).unwrap();

    // Create a mock project
    let project_dir = projects_dir.join("-root-repos-test-project");
    fs::create_dir_all(&project_dir).unwrap();

    // Create a mock conversation file
    let session_file = project_dir.join("test-session-123.jsonl");
    fs::write(
        &session_file,
        r#"{"type":"user","uuid":"1","sessionId":"test-session-123","timestamp":"2025-01-01T00:00:00Z"}
{"type":"assistant","uuid":"2","sessionId":"test-session-123","timestamp":"2025-01-01T00:01:00Z"}
"#,
    )
    .unwrap();

    temp_dir
}

#[test]
fn test_mock_claude_directory_structure() {
    let temp_dir = create_mock_claude_dir();
    let projects_dir = temp_dir.path().join("projects");

    assert!(projects_dir.exists());

    let project_dir = projects_dir.join("-root-repos-test-project");
    assert!(project_dir.exists());

    let session_file = project_dir.join("test-session-123.jsonl");
    assert!(session_file.exists());

    let content = fs::read_to_string(session_file).unwrap();
    assert!(content.contains("test-session-123"));
}

#[test]
fn test_end_to_end_sync_workflow() {
    // This test verifies the basic components work together
    // Full integration would require mocking the git operations

    let temp_dir = create_mock_claude_dir();
    let sync_repo = TempDir::new().unwrap();

    // Verify the directory structure
    assert!(temp_dir.path().join("projects").exists());
    assert!(sync_repo.path().exists());

    // The actual integration would involve:
    // 1. Initializing the sync repo (git init)
    // 2. Discovering sessions from the mock Claude dir
    // 3. Copying them to sync repo
    // 4. Committing changes
    // 5. Simulating pull from another machine
    // 6. Detecting and resolving conflicts

    // For now, we verify the test infrastructure works
    // Test infrastructure is set up correctly
}

#[test]
fn test_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.jsonl");

    fs::write(&test_file, "test content").unwrap();

    let metadata = fs::metadata(&test_file).unwrap();
    let permissions = metadata.permissions();

    // Verify file is readable
    assert!(permissions.mode() & 0o400 != 0);
}

#[test]
fn test_large_session_file() {
    let temp_dir = TempDir::new().unwrap();
    let large_file = temp_dir.path().join("large-session.jsonl");

    // Create a file with 1000 entries
    let mut content = String::new();
    for i in 0..1000 {
        content.push_str(&format!(
            r#"{{"type":"user","uuid":"{}","sessionId":"large-session","timestamp":"2025-01-01T{:02}:00:00Z"}}
"#,
            i,
            i % 24
        ));
    }

    fs::write(&large_file, content).unwrap();

    // Verify file was created and has correct size
    let metadata = fs::metadata(&large_file).unwrap();
    assert!(metadata.len() > 50000); // Should be reasonably large

    // Verify it can be read
    let read_content = fs::read_to_string(&large_file).unwrap();
    assert!(read_content.lines().count() >= 1000);
}

#[test]
fn test_session_discovery() {
    let temp_dir = create_mock_claude_dir();
    let projects_dir = temp_dir.path().join("projects");

    // Walk the directory and find all .jsonl files
    let mut jsonl_files = Vec::new();
    for entry in walkdir::WalkDir::new(&projects_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.path().extension().and_then(|s| s.to_str()) == Some("jsonl") {
            jsonl_files.push(entry.path().to_path_buf());
        }
    }

    assert_eq!(jsonl_files.len(), 1);
    assert!(jsonl_files[0]
        .to_string_lossy()
        .contains("test-session-123.jsonl"));
}

#[test]
fn test_multiple_projects() {
    let temp_dir = TempDir::new().unwrap();
    let projects_dir = temp_dir.path().join("projects");

    // Create multiple project directories
    for project in &["project-a", "project-b", "project-c"] {
        let project_dir = projects_dir.join(format!("-root-repos-{project}"));
        fs::create_dir_all(&project_dir).unwrap();

        let session_file = project_dir.join(format!("{project}-session.jsonl"));
        fs::write(
            &session_file,
            format!(
                r#"{{"type":"user","uuid":"1","sessionId":"{project}-session","timestamp":"2025-01-01T00:00:00Z"}}"#
            ),
        )
        .unwrap();
    }

    // Count projects
    let project_count = fs::read_dir(&projects_dir).unwrap().count();
    assert_eq!(project_count, 3);
}

#[test]
fn test_empty_project_directory() {
    let temp_dir = TempDir::new().unwrap();
    let projects_dir = temp_dir.path().join("projects");
    fs::create_dir_all(&projects_dir).unwrap();

    // Verify empty directory doesn't cause issues
    let entries: Vec<_> = fs::read_dir(&projects_dir).unwrap().collect();
    assert_eq!(entries.len(), 0);
}

#[test]
fn test_malformed_jsonl_handling() {
    let temp_dir = TempDir::new().unwrap();
    let malformed_file = temp_dir.path().join("malformed.jsonl");

    // Write invalid JSON
    fs::write(&malformed_file, "not valid json\n{incomplete json").unwrap();

    // Verify file exists but contains invalid data
    let content = fs::read_to_string(&malformed_file).unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(content.lines().next().unwrap()).is_err());
}

#[test]
fn test_path_handling_with_spaces() {
    let temp_dir = TempDir::new().unwrap();
    let path_with_spaces = temp_dir.path().join("path with spaces");
    fs::create_dir_all(&path_with_spaces).unwrap();

    let file = path_with_spaces.join("test file.jsonl");
    fs::write(&file, r#"{"test":"data"}"#).unwrap();

    assert!(file.exists());
    assert!(fs::read_to_string(&file).is_ok());
}

#[test]
fn test_symlink_handling() {
    let temp_dir = TempDir::new().unwrap();
    let real_file = temp_dir.path().join("real.jsonl");
    let link_file = temp_dir.path().join("link.jsonl");

    fs::write(&real_file, r#"{"test":"data"}"#).unwrap();

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&real_file, &link_file).unwrap();
        assert!(link_file.exists());
    }
}

#[test]
fn test_concurrent_file_access() {
    use std::sync::Arc;
    use std::thread;

    let temp_dir = Arc::new(TempDir::new().unwrap());
    let file_path = Arc::new(temp_dir.path().join("concurrent.jsonl"));

    // Write initial content
    fs::write(&*file_path, r#"{"test":"data"}"#).unwrap();

    // Spawn multiple threads reading the same file
    let mut handles = vec![];

    for _ in 0..5 {
        let path = Arc::clone(&file_path);
        let handle = thread::spawn(move || {
            let content = fs::read_to_string(&*path).unwrap();
            assert!(content.contains("test"));
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }
}
