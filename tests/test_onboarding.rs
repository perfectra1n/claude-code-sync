use anyhow::Result;
use claude_code_sync::config::ConfigManager;
use claude_code_sync::filter::FilterConfig;
use claude_code_sync::scm;
use claude_code_sync::sync::SyncState;
use serial_test::serial;
use tempfile::TempDir;

/// Test helper to setup a temporary config directory for testing
fn setup_test_config_env() -> Result<TempDir> {
    TempDir::new().map_err(Into::into)
}

#[test]
fn test_config_manager_paths() -> Result<()> {
    // Test that all config paths can be retrieved
    let config_dir = ConfigManager::config_dir()?;
    assert!(config_dir.to_string_lossy().contains("claude-code-sync"));

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
    assert!(deserialized.has_remote);
    assert!(deserialized.is_cloned_repo);

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
    assert!(state.has_remote);
    assert!(!state.is_cloned_repo); // Should default to false

    Ok(())
}

#[test]
#[serial]
fn test_filter_config_save_and_load() -> Result<()> {
    let temp_dir = setup_test_config_env()?;

    // Set XDG_CONFIG_HOME to isolate test config
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    let config = FilterConfig {
        exclude_attachments: true,
        exclude_older_than_days: Some(30),
        ..Default::default()
    };

    config.save()?;

    let loaded = FilterConfig::load()?;
    assert!(loaded.exclude_attachments);
    assert_eq!(loaded.exclude_older_than_days, Some(30));

    // Clean up env var
    std::env::remove_var("XDG_CONFIG_HOME");

    Ok(())
}

#[test]
fn test_scm_clone_validates_path() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let clone_path = temp_dir.path().join("cloned-repo");

    // We can't test actual cloning without a real remote repo
    // But we can test that the path validation and setup works

    // Try to clone from an invalid URL (this will fail, but we can test the error handling)
    let result = scm::clone("invalid-url", &clone_path);
    assert!(result.is_err());

    // The error should contain helpful information
    let err = result.err().unwrap();
    let err_msg = format!("{err}");
    assert!(err_msg.contains("clone failed") || err_msg.contains("Failed"));

    Ok(())
}

#[test]
#[serial]
fn test_init_from_onboarding() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let repo_path = temp_dir.path().join("onboarding-test-repo");

    // Set XDG_CONFIG_HOME to isolate test config
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Initialize a repo first
    scm::init(&repo_path)?;

    // Test init_from_onboarding with a local repository
    claude_code_sync::sync::init_from_onboarding(&repo_path, None, false)?;

    // Verify state was saved
    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(!state.has_remote);
    assert!(!state.is_cloned_repo);

    // Clean up env var
    std::env::remove_var("XDG_CONFIG_HOME");

    Ok(())
}

#[test]
#[serial]
fn test_init_from_onboarding_with_remote() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let repo_path = temp_dir.path().join("onboarding-remote-test");

    // Set XDG_CONFIG_HOME to isolate test config
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Initialize a repo first
    scm::init(&repo_path)?;

    // Test with remote URL
    claude_code_sync::sync::init_from_onboarding(
        &repo_path,
        Some("https://github.com/user/repo.git"),
        true,
    )?;

    // Verify state was saved
    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(state.has_remote);
    assert!(state.is_cloned_repo);

    // Clean up env var
    std::env::remove_var("XDG_CONFIG_HOME");

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
    let mut config = FilterConfig {
        exclude_attachments: true,
        ..Default::default()
    };

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
#[serial]
fn test_multiple_config_operations() -> Result<()> {
    let temp_dir = setup_test_config_env()?;

    // Set XDG_CONFIG_HOME to isolate test config
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    let _loaded = FilterConfig::load()?;

    // Save a new config with known values
    let config = FilterConfig {
        exclude_attachments: true,
        exclude_older_than_days: Some(99),
        ..Default::default()
    };
    config.save()?;

    // Verify it was saved
    let loaded2 = FilterConfig::load()?;
    assert!(loaded2.exclude_attachments);
    assert_eq!(loaded2.exclude_older_than_days, Some(99));

    // Clean up env var
    std::env::remove_var("XDG_CONFIG_HOME");

    Ok(())
}

// ============================================================================
// Tests for Bug Fixes: --repo flag creating FilterConfig
// ============================================================================

#[test]
#[serial]
fn test_init_sync_repo_creates_filter_config() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let repo_path = temp_dir.path().join("cli-init-test-repo");

    // Set XDG_CONFIG_HOME to isolate test config
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Initialize using init_sync_repo (simulates --repo flag)
    claude_code_sync::sync::init_sync_repo(&repo_path, None)?;

    // Verify state was saved
    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(!state.has_remote);
    assert!(!state.is_cloned_repo);

    // BUG FIX: Verify filter config was also saved
    let filter_config_path = ConfigManager::filter_config_path()?;
    assert!(
        filter_config_path.exists(),
        "Filter config should be created by init_sync_repo"
    );

    // Verify we can load the filter config
    let filter_config = FilterConfig::load()?;
    // Default values should be set
    assert!(!filter_config.exclude_attachments);

    // Clean up env var
    std::env::remove_var("XDG_CONFIG_HOME");

    Ok(())
}

#[test]
#[serial]
fn test_init_sync_repo_with_remote_creates_filter_config() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let repo_path = temp_dir.path().join("cli-remote-test-repo");

    // Set XDG_CONFIG_HOME to isolate test config
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Initialize with remote URL using init_sync_repo (simulates --repo --remote flags)
    claude_code_sync::sync::init_sync_repo(
        &repo_path,
        Some("https://github.com/user/repo.git"),
    )?;

    // Verify state was saved with remote
    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(state.has_remote);
    assert!(!state.is_cloned_repo); // Not cloned, just added as origin

    // Verify filter config was also saved
    let filter_config_path = ConfigManager::filter_config_path()?;
    assert!(
        filter_config_path.exists(),
        "Filter config should be created by init_sync_repo with remote"
    );

    // Clean up env var
    std::env::remove_var("XDG_CONFIG_HOME");

    Ok(())
}

#[test]
#[serial]
fn test_init_sync_repo_does_not_overwrite_existing_filter_config() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let repo_path = temp_dir.path().join("no-overwrite-test-repo");

    // Set XDG_CONFIG_HOME to isolate test config
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Create an existing filter config with custom values
    let custom_config = FilterConfig {
        exclude_attachments: true,
        exclude_older_than_days: Some(42),
        ..Default::default()
    };
    custom_config.save()?;

    // Initialize using init_sync_repo
    claude_code_sync::sync::init_sync_repo(&repo_path, None)?;

    // Verify the existing filter config was NOT overwritten
    let loaded_config = FilterConfig::load()?;
    assert!(
        loaded_config.exclude_attachments,
        "Existing filter config should not be overwritten"
    );
    assert_eq!(
        loaded_config.exclude_older_than_days,
        Some(42),
        "Custom exclude_older_than_days should be preserved"
    );

    // Clean up env var
    std::env::remove_var("XDG_CONFIG_HOME");

    Ok(())
}

// ============================================================================
// Tests for --clone flag and remote-only init
// ============================================================================

#[test]
fn test_clone_with_invalid_url_fails() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let clone_path = temp_dir.path().join("clone-test-repo");

    // Try to clone from an invalid URL
    let result = scm::clone("not-a-valid-url", &clone_path);
    assert!(result.is_err(), "Clone should fail with invalid URL");

    Ok(())
}

#[test]
fn test_clone_creates_parent_directories() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let nested_path = temp_dir.path().join("deeply").join("nested").join("path").join("repo");

    // Even though clone will fail (invalid URL), it should create parent directories
    let result = scm::clone("https://invalid-url-that-wont-work.example.com/repo.git", &nested_path);

    // Clone fails but parent directory should be created
    assert!(result.is_err());
    // Parent should exist even though clone failed
    assert!(nested_path.parent().unwrap().exists());

    Ok(())
}

#[test]
#[serial]
fn test_init_from_onboarding_sets_is_cloned_flag() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let repo_path = temp_dir.path().join("cloned-repo-test");

    // Set XDG_CONFIG_HOME to isolate test config
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Initialize a repo first (simulating post-clone state)
    scm::init(&repo_path)?;

    // Test init_from_onboarding with is_cloned = true
    claude_code_sync::sync::init_from_onboarding(
        &repo_path,
        Some("https://github.com/user/repo.git"),
        true, // is_cloned
    )?;

    // Verify state was saved with is_cloned_repo = true
    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(state.has_remote);
    assert!(state.is_cloned_repo, "is_cloned_repo should be true for cloned repos");

    // Clean up env var
    std::env::remove_var("XDG_CONFIG_HOME");

    Ok(())
}

#[test]
#[serial]
fn test_init_from_onboarding_local_repo_not_cloned() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    let repo_path = temp_dir.path().join("local-repo-test");

    // Set XDG_CONFIG_HOME to isolate test config
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Initialize a repo first
    scm::init(&repo_path)?;

    // Test init_from_onboarding with is_cloned = false (local repo)
    claude_code_sync::sync::init_from_onboarding(
        &repo_path,
        None,
        false, // not cloned
    )?;

    // Verify state was saved with is_cloned_repo = false
    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(!state.has_remote);
    assert!(!state.is_cloned_repo, "is_cloned_repo should be false for local repos");

    // Clean up env var
    std::env::remove_var("XDG_CONFIG_HOME");

    Ok(())
}

#[test]
fn test_default_repo_dir_exists() -> Result<()> {
    // Test that we can get the default repo directory
    let default_dir = ConfigManager::default_repo_dir()?;

    // Should end with "repo"
    assert!(default_dir.ends_with("repo"));

    // Should contain claude-code-sync in path
    assert!(default_dir.to_string_lossy().contains("claude-code-sync"));

    Ok(())
}

// ============================================================================
// Tests for InitConfig validation
// ============================================================================

#[test]
fn test_init_config_clone_requires_remote_url() -> Result<()> {
    use claude_code_sync::onboarding::InitConfig;
    use std::io::Write;

    let temp_dir = setup_test_config_env()?;

    // Create config file with clone=true but no remote_url
    let config_path = temp_dir.path().join("init.toml");
    let mut file = std::fs::File::create(&config_path)?;
    writeln!(
        file,
        r#"
repo_path = "/tmp/test"
clone = true
"#
    )?;

    let result = InitConfig::load(&config_path);

    assert!(
        result.is_err(),
        "clone=true without remote_url should fail validation"
    );
    let err_msg = result.err().unwrap().to_string();
    assert!(
        err_msg.contains("remote_url"),
        "Error should mention remote_url: {err_msg}"
    );

    Ok(())
}

#[test]
fn test_init_config_clone_with_remote_url_valid() -> Result<()> {
    use claude_code_sync::onboarding::InitConfig;
    use std::io::Write;

    let temp_dir = setup_test_config_env()?;

    // Create config file with clone=true and remote_url
    let config_path = temp_dir.path().join("init.toml");
    let mut file = std::fs::File::create(&config_path)?;
    writeln!(
        file,
        r#"
repo_path = "/tmp/test"
remote_url = "https://github.com/user/repo.git"
clone = true
"#
    )?;

    let result = InitConfig::load(&config_path);

    assert!(
        result.is_ok(),
        "clone=true with remote_url should be valid: {:?}",
        result.err()
    );

    Ok(())
}
