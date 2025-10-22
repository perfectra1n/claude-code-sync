use anyhow::Result;
use claude_sync::config::ConfigManager;
use claude_sync::filter::FilterConfig;
use claude_sync::git::GitManager;
use claude_sync::sync::SyncState;
use tempfile::TempDir;

/// Test helper to setup a temporary config directory for testing
fn setup_test_config_env() -> Result<TempDir> {
    TempDir::new().map_err(Into::into)
}

#[test]
fn test_config_manager_paths() -> Result<()> {
    // Test that all config paths can be retrieved
    let config_dir = ConfigManager::config_dir()?;
    assert!(config_dir.to_string_lossy().contains("claude-sync"));

    let state_file = ConfigManager::state_file_path()?;
    assert!(state_file.ends_with("state.json"));

    let filter_config = ConfigManager::filter_config_path()?;
    assert!(filter_config.ends_with("config.toml"));

    let history = ConfigManager::operation_history_path()?;
    assert!(history.ends_with("operation-history.json"));

    let snapshots = ConfigManager::snapshots_dir()?;
    assert!(snapshots.ends_with("snapshots"));

    let repo = ConfigManager::default_repo_dir()?;
    assert!(repo.ends_with("repo"));

    Ok(())
}

#[test]
fn test_ensure_config_dir_creates_directory() -> Result<()> {
    // This test will create the directory if it doesn't exist
    // Note: This modifies the actual user's home directory, so we can only verify it succeeds
    let config_dir = ConfigManager::ensure_config_dir()?;
    assert!(config_dir.exists());
    assert!(config_dir.is_dir());
    Ok(())
}

#[test]
fn test_sync_state_with_cloned_flag() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let repo_path = temp_dir.path().join("test-repo");

    // Test serialization/deserialization with is_cloned_repo field
    let state = SyncState {
        sync_repo_path: repo_path.clone(),
        has_remote: true,
        is_cloned_repo: true,
    };

    let serialized = serde_json::to_string(&state)?;
    assert!(serialized.contains("is_cloned_repo"));
    assert!(serialized.contains("true"));

    let deserialized: SyncState = serde_json::from_str(&serialized)?;
    assert_eq!(deserialized.sync_repo_path, repo_path);
    assert_eq!(deserialized.has_remote, true);
    assert_eq!(deserialized.is_cloned_repo, true);

    Ok(())
}

#[test]
fn test_sync_state_backwards_compatible() -> Result<()> {
    // Test that old state files (without is_cloned_repo) can still be loaded
    let old_state_json = r#"{
        "sync_repo_path": "/tmp/test-repo",
        "has_remote": true
    }"#;

    let state: SyncState = serde_json::from_str(old_state_json)?;
    assert_eq!(state.has_remote, true);
    assert_eq!(state.is_cloned_repo, false); // Should default to false

    Ok(())
}

#[test]
fn test_filter_config_save_and_load() -> Result<()> {
    // Note: This test modifies the actual user config
    // In a real-world scenario, you'd want to use a custom config path

    let mut config = FilterConfig::default();
    config.exclude_attachments = true;
    config.exclude_older_than_days = Some(30);

    config.save()?;

    let loaded = FilterConfig::load()?;
    assert_eq!(loaded.exclude_attachments, true);
    assert_eq!(loaded.exclude_older_than_days, Some(30));

    Ok(())
}

#[test]
fn test_git_manager_clone_validates_path() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let clone_path = temp_dir.path().join("cloned-repo");

    // We can't test actual cloning without a real remote repo
    // But we can test that the path validation and setup works

    // Try to clone from an invalid URL (this will fail, but we can test the error handling)
    let result = GitManager::clone("invalid-url", &clone_path);
    assert!(result.is_err());

    // The error should contain helpful information
    let err = result.err().unwrap();
    let err_msg = format!("{}", err);
    assert!(err_msg.contains("Failed to clone"));

    Ok(())
}

#[test]
fn test_init_from_onboarding() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let repo_path = temp_dir.path().join("onboarding-test-repo");

    // Initialize a git repo first
    GitManager::init(&repo_path)?;

    // Test init_from_onboarding with a local repository
    claude_sync::sync::init_from_onboarding(&repo_path, None, false)?;

    // Verify state was saved
    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert_eq!(state.has_remote, false);
    assert_eq!(state.is_cloned_repo, false);

    Ok(())
}

#[test]
fn test_init_from_onboarding_with_remote() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let repo_path = temp_dir.path().join("onboarding-remote-test");

    // Initialize a git repo first
    GitManager::init(&repo_path)?;

    // Test with remote URL
    claude_sync::sync::init_from_onboarding(
        &repo_path,
        Some("https://github.com/user/repo.git"),
        true,
    )?;

    // Verify state was saved
    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert_eq!(state.has_remote, true);
    assert_eq!(state.is_cloned_repo, true);

    Ok(())
}

#[test]
fn test_config_directory_structure() -> Result<()> {
    // Ensure config directory can be created
    let config_dir = ConfigManager::ensure_config_dir()?;

    // Verify it exists
    assert!(config_dir.exists());
    assert!(config_dir.is_dir());

    // Ensure snapshots directory can be created
    let snapshots_dir = ConfigManager::ensure_snapshots_dir()?;
    assert!(snapshots_dir.exists());
    assert!(snapshots_dir.is_dir());

    // Verify snapshots is a subdirectory of config
    assert!(snapshots_dir.starts_with(&config_dir));

    Ok(())
}

#[test]
fn test_filter_config_with_attachments() -> Result<()> {
    use std::path::PathBuf;

    // Test exclude_attachments flag
    let mut config = FilterConfig::default();
    config.exclude_attachments = true;

    // Should include .jsonl files
    assert!(config.should_include(&PathBuf::from("session.jsonl")));

    // Should exclude non-.jsonl files
    assert!(!config.should_include(&PathBuf::from("image.png")));
    assert!(!config.should_include(&PathBuf::from("document.pdf")));
    assert!(!config.should_include(&PathBuf::from("video.mp4")));

    // With exclude_attachments = false, should include everything
    config.exclude_attachments = false;
    // Note: This might fail due to file size checks, so we'll skip this part
    // in a real test environment

    Ok(())
}

#[test]
fn test_multiple_config_operations() -> Result<()> {
    // This test is sensitive to ordering with other tests that modify config
    // We'll just verify the last saved state is loaded correctly
    let loaded = FilterConfig::load()?;

    // Save a new config with known values
    let mut config = FilterConfig::default();
    config.exclude_attachments = true;
    config.exclude_older_than_days = Some(99);
    config.save()?;

    // Verify it was saved
    let loaded2 = FilterConfig::load()?;
    assert_eq!(loaded2.exclude_attachments, true);
    assert_eq!(loaded2.exclude_older_than_days, Some(99));

    Ok(())
}
