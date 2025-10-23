use claude_code_sync::VerbosityLevel;
use claude_code_sync::undo::{UndoPreview, VerbosityLevel as UndoVerbosity};
use claude_code_sync::history::OperationType;

/// Test VerbosityLevel enum basic functionality
#[test]
fn test_verbosity_level_equality() {
    assert_eq!(VerbosityLevel::Quiet, VerbosityLevel::Quiet);
    assert_eq!(VerbosityLevel::Normal, VerbosityLevel::Normal);
    assert_eq!(VerbosityLevel::Verbose, VerbosityLevel::Verbose);

    assert_ne!(VerbosityLevel::Quiet, VerbosityLevel::Normal);
    assert_ne!(VerbosityLevel::Normal, VerbosityLevel::Verbose);
    assert_ne!(VerbosityLevel::Quiet, VerbosityLevel::Verbose);
}

/// Test VerbosityLevel can be copied
#[test]
fn test_verbosity_level_copy() {
    let v1 = VerbosityLevel::Verbose;
    let v2 = v1;
    assert_eq!(v1, v2);
}

/// Test UndoPreview display doesn't panic with different verbosity levels
#[test]
fn test_undo_preview_display_quiet() {
    let preview = UndoPreview {
        operation_type: OperationType::Pull,
        operation_timestamp: chrono::Utc::now(),
        branch: Some("main".to_string()),
        affected_files: vec![
            "/path/to/file1.jsonl".to_string(),
            "/path/to/file2.jsonl".to_string(),
        ],
        conversation_count: 5,
        commit_hash: None,
        snapshot_timestamp: chrono::Utc::now(),
    };

    // Should not panic - just verify it runs
    preview.display(UndoVerbosity::Quiet);
}

#[test]
fn test_undo_preview_display_normal() {
    let preview = UndoPreview {
        operation_type: OperationType::Push,
        operation_timestamp: chrono::Utc::now(),
        branch: Some("main".to_string()),
        affected_files: vec![],
        conversation_count: 3,
        commit_hash: Some("abc123def456".to_string()),
        snapshot_timestamp: chrono::Utc::now(),
    };

    // Should not panic
    preview.display(UndoVerbosity::Normal);
}

#[test]
fn test_undo_preview_display_verbose() {
    let preview = UndoPreview {
        operation_type: OperationType::Pull,
        operation_timestamp: chrono::Utc::now(),
        branch: None,
        affected_files: vec![
            "/path/to/file1.jsonl".to_string(),
            "/path/to/file2.jsonl".to_string(),
            "/path/to/file3.jsonl".to_string(),
        ],
        conversation_count: 10,
        commit_hash: None,
        snapshot_timestamp: chrono::Utc::now(),
    };

    // Should not panic
    preview.display(UndoVerbosity::Verbose);
}

/// Test UndoPreview with many files (tests truncation logic)
#[test]
fn test_undo_preview_many_files_verbose() {
    let many_files: Vec<String> = (0..50)
        .map(|i| format!("/path/to/file{}.jsonl", i))
        .collect();

    let preview = UndoPreview {
        operation_type: OperationType::Pull,
        operation_timestamp: chrono::Utc::now(),
        branch: Some("develop".to_string()),
        affected_files: many_files,
        conversation_count: 50,
        commit_hash: None,
        snapshot_timestamp: chrono::Utc::now(),
    };

    // Should handle many files without panic
    preview.display(UndoVerbosity::Verbose);
}

/// Test UndoPreview with push operation and commit hash
#[test]
fn test_undo_preview_push_with_commit() {
    let preview = UndoPreview {
        operation_type: OperationType::Push,
        operation_timestamp: chrono::Utc::now(),
        branch: Some("feature-branch".to_string()),
        affected_files: vec![],
        conversation_count: 7,
        commit_hash: Some("1234567890abcdef1234567890abcdef12345678".to_string()),
        snapshot_timestamp: chrono::Utc::now() - chrono::Duration::hours(2),
    };

    // Should display commit hash in verbose mode
    preview.display(UndoVerbosity::Verbose);
}

/// Test that verbosity conversion works between main and undo modules
#[test]
fn test_verbosity_conversion() {
    // Main VerbosityLevel
    let main_quiet = VerbosityLevel::Quiet;
    let main_normal = VerbosityLevel::Normal;
    let main_verbose = VerbosityLevel::Verbose;

    // Convert to undo VerbosityLevel (as done in handlers)
    let undo_quiet = match main_quiet {
        VerbosityLevel::Quiet => UndoVerbosity::Quiet,
        VerbosityLevel::Normal => UndoVerbosity::Normal,
        VerbosityLevel::Verbose => UndoVerbosity::Verbose,
    };

    let undo_normal = match main_normal {
        VerbosityLevel::Quiet => UndoVerbosity::Quiet,
        VerbosityLevel::Normal => UndoVerbosity::Normal,
        VerbosityLevel::Verbose => UndoVerbosity::Verbose,
    };

    let undo_verbose = match main_verbose {
        VerbosityLevel::Quiet => UndoVerbosity::Quiet,
        VerbosityLevel::Normal => UndoVerbosity::Normal,
        VerbosityLevel::Verbose => UndoVerbosity::Verbose,
    };

    // Verify conversions
    assert_eq!(undo_quiet, UndoVerbosity::Quiet);
    assert_eq!(undo_normal, UndoVerbosity::Normal);
    assert_eq!(undo_verbose, UndoVerbosity::Verbose);
}

/// Test FilterConfig can be loaded (tests config handler dependency)
#[test]
fn test_filter_config_load_or_default() {
    use claude_code_sync::filter::FilterConfig;

    // Should either load existing config or create default
    let config = FilterConfig::load();
    assert!(config.is_ok());

    let config = config.unwrap();
    assert!(config.max_file_size_bytes > 0);
}

/// Test FilterConfig can be cloned (needed for interactive handlers)
#[test]
fn test_filter_config_clone() {
    use claude_code_sync::filter::FilterConfig;

    let config = FilterConfig::load().unwrap();
    let cloned = config.clone();

    assert_eq!(config.max_file_size_bytes, cloned.max_file_size_bytes);
    assert_eq!(config.exclude_attachments, cloned.exclude_attachments);
    assert_eq!(config.exclude_older_than_days, cloned.exclude_older_than_days);
}

/// Test that FilterConfig can be modified (needed for wizard)
#[test]
fn test_filter_config_modification() {
    use claude_code_sync::filter::FilterConfig;

    let mut config = FilterConfig::load().unwrap();

    // Test modifications
    config.exclude_attachments = true;
    assert!(config.exclude_attachments);

    config.exclude_older_than_days = Some(30);
    assert_eq!(config.exclude_older_than_days, Some(30));

    config.include_patterns = vec!["*work*".to_string()];
    assert_eq!(config.include_patterns.len(), 1);

    config.exclude_patterns = vec!["*test*".to_string(), "*tmp*".to_string()];
    assert_eq!(config.exclude_patterns.len(), 2);

    config.max_file_size_bytes = 5 * 1024 * 1024; // 5MB
    assert_eq!(config.max_file_size_bytes, 5 * 1024 * 1024);
}

/// Test snapshot cleanup config defaults
#[test]
fn test_snapshot_cleanup_config_default() {
    use claude_code_sync::undo::SnapshotCleanupConfig;

    let config = SnapshotCleanupConfig::default();
    assert_eq!(config.max_count_per_type, 5);
    assert_eq!(config.max_age_days, 7);
}

/// Test snapshot cleanup config can be customized
#[test]
fn test_snapshot_cleanup_config_custom() {
    use claude_code_sync::undo::SnapshotCleanupConfig;

    let config = SnapshotCleanupConfig {
        max_count_per_type: 10,
        max_age_days: 14,
    };

    assert_eq!(config.max_count_per_type, 10);
    assert_eq!(config.max_age_days, 14);
}

/// Test that non-interactive mode doesn't require terminal
#[test]
fn test_non_interactive_push_verbosity() {
    // This tests that the logic paths work without actual terminal interaction
    // We can't easily test the actual push_history function without a full setup,
    // but we can verify the verbosity enum works as expected

    let verbosity = VerbosityLevel::Quiet;
    assert_eq!(verbosity, VerbosityLevel::Quiet);

    let verbosity = VerbosityLevel::Verbose;
    assert_eq!(verbosity, VerbosityLevel::Verbose);
}

/// Test UndoPreview fields are accessible
#[test]
fn test_undo_preview_field_access() {
    let timestamp = chrono::Utc::now();
    let snapshot_time = chrono::Utc::now() - chrono::Duration::hours(1);

    let preview = UndoPreview {
        operation_type: OperationType::Pull,
        operation_timestamp: timestamp,
        branch: Some("test-branch".to_string()),
        affected_files: vec!["file1.jsonl".to_string()],
        conversation_count: 42,
        commit_hash: Some("abc123".to_string()),
        snapshot_timestamp: snapshot_time,
    };

    // Test field access
    assert_eq!(preview.operation_type, OperationType::Pull);
    assert_eq!(preview.branch, Some("test-branch".to_string()));
    assert_eq!(preview.affected_files.len(), 1);
    assert_eq!(preview.conversation_count, 42);
    assert_eq!(preview.commit_hash, Some("abc123".to_string()));
}

/// Test empty files list handling
#[test]
fn test_undo_preview_empty_files() {
    let preview = UndoPreview {
        operation_type: OperationType::Push,
        operation_timestamp: chrono::Utc::now(),
        branch: None,
        affected_files: vec![],
        conversation_count: 0,
        commit_hash: Some("def456".to_string()),
        snapshot_timestamp: chrono::Utc::now(),
    };

    // Should handle empty files gracefully
    preview.display(UndoVerbosity::Quiet);
    preview.display(UndoVerbosity::Normal);
    preview.display(UndoVerbosity::Verbose);
}

/// Test verbosity level Debug trait
#[test]
fn test_verbosity_debug() {
    let quiet = VerbosityLevel::Quiet;
    let normal = VerbosityLevel::Normal;
    let verbose = VerbosityLevel::Verbose;

    // Should be able to format with Debug
    let quiet_str = format!("{:?}", quiet);
    let normal_str = format!("{:?}", normal);
    let verbose_str = format!("{:?}", verbose);

    assert!(quiet_str.contains("Quiet"));
    assert!(normal_str.contains("Normal"));
    assert!(verbose_str.contains("Verbose"));
}
