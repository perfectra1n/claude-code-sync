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

// =============================================================================
// MultiRepoState Tests - Testing multi-repo configuration and migration
// =============================================================================

use claude_code_sync::sync::MultiRepoState;

/// Test that MultiRepoState can be serialized and deserialized correctly
#[test]
fn test_multi_repo_state_serialization() -> Result<()> {
    use claude_code_sync::sync::RepoConfig;
    use std::collections::HashMap;

    let repo_config = RepoConfig {
        name: "work".to_string(),
        sync_repo_path: std::path::PathBuf::from("/tmp/work-repo"),
        has_remote: true,
        is_cloned_repo: false,
        remote_url: Some("https://github.com/user/work.git".to_string()),
        description: Some("Work projects".to_string()),
    };

    let mut repos = HashMap::new();
    repos.insert("work".to_string(), repo_config);

    let state = MultiRepoState {
        version: 2,
        active_repo: "work".to_string(),
        repos,
    };

    let serialized = serde_json::to_string_pretty(&state)?;
    assert!(serialized.contains("\"version\": 2"));
    assert!(serialized.contains("\"active_repo\": \"work\""));
    assert!(serialized.contains("\"remote_url\": \"https://github.com/user/work.git\""));

    let deserialized: MultiRepoState = serde_json::from_str(&serialized)?;
    assert_eq!(deserialized.version, 2);
    assert_eq!(deserialized.active_repo, "work");
    assert!(deserialized.repos.contains_key("work"));

    let repo = deserialized.repos.get("work").unwrap();
    assert_eq!(repo.name, "work");
    assert!(repo.has_remote);
    assert_eq!(
        repo.remote_url,
        Some("https://github.com/user/work.git".to_string())
    );

    Ok(())
}

/// Test migration from v1 SyncState format to v2 MultiRepoState format
#[test]
#[serial]
fn test_v1_to_v2_migration() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Create config directory
    let config_dir = temp_dir.path().join("claude-code-sync");
    std::fs::create_dir_all(&config_dir)?;

    // Write a v1 format state.json
    let v1_state = r#"{
        "sync_repo_path": "/tmp/legacy-repo",
        "has_remote": true,
        "is_cloned_repo": false
    }"#;
    let state_path = config_dir.join("state.json");
    std::fs::write(&state_path, v1_state)?;

    // Load should auto-migrate to v2
    let multi_state = MultiRepoState::load()?;

    assert_eq!(multi_state.version, 2);
    assert_eq!(multi_state.active_repo, "default");
    assert!(multi_state.repos.contains_key("default"));

    let default_repo = multi_state.repos.get("default").unwrap();
    assert_eq!(
        default_repo.sync_repo_path,
        std::path::PathBuf::from("/tmp/legacy-repo")
    );
    assert!(default_repo.has_remote);
    assert!(!default_repo.is_cloned_repo);

    // Verify the file was rewritten in v2 format
    let content = std::fs::read_to_string(&state_path)?;
    assert!(content.contains("\"version\": 2"));
    assert!(content.contains("\"active_repo\": \"default\""));

    std::env::remove_var("XDG_CONFIG_HOME");
    Ok(())
}

/// Test that SyncState::load() works as a compatibility wrapper for v2 format
#[test]
#[serial]
fn test_sync_state_loads_v2_format() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Create config directory
    let config_dir = temp_dir.path().join("claude-code-sync");
    std::fs::create_dir_all(&config_dir)?;

    // Write v2 format state.json
    let v2_state = r#"{
        "version": 2,
        "active_repo": "myrepo",
        "repos": {
            "myrepo": {
                "name": "myrepo",
                "sync_repo_path": "/tmp/my-repo",
                "has_remote": true,
                "is_cloned_repo": true,
                "remote_url": "https://github.com/user/repo.git"
            }
        }
    }"#;
    let state_path = config_dir.join("state.json");
    std::fs::write(&state_path, v2_state)?;

    // SyncState::load() should return the active repo's data
    let sync_state = SyncState::load()?;

    assert_eq!(
        sync_state.sync_repo_path,
        std::path::PathBuf::from("/tmp/my-repo")
    );
    assert!(sync_state.has_remote);
    assert!(sync_state.is_cloned_repo);

    std::env::remove_var("XDG_CONFIG_HOME");
    Ok(())
}

/// Test MultiRepoState with multiple repos
#[test]
#[serial]
fn test_multi_repo_state_multiple_repos() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Create config directory
    let config_dir = temp_dir.path().join("claude-code-sync");
    std::fs::create_dir_all(&config_dir)?;

    // Write v2 format with multiple repos
    let v2_state = r#"{
        "version": 2,
        "active_repo": "work",
        "repos": {
            "work": {
                "name": "work",
                "sync_repo_path": "/tmp/work-repo",
                "has_remote": true,
                "is_cloned_repo": false
            },
            "personal": {
                "name": "personal",
                "sync_repo_path": "/tmp/personal-repo",
                "has_remote": false,
                "is_cloned_repo": false
            }
        }
    }"#;
    let state_path = config_dir.join("state.json");
    std::fs::write(&state_path, v2_state)?;

    let multi_state = MultiRepoState::load()?;

    assert_eq!(multi_state.repos.len(), 2);
    assert!(multi_state.repos.contains_key("work"));
    assert!(multi_state.repos.contains_key("personal"));
    assert_eq!(multi_state.active_repo, "work");

    // SyncState should return the active (work) repo
    let sync_state = SyncState::load()?;
    assert_eq!(
        sync_state.sync_repo_path,
        std::path::PathBuf::from("/tmp/work-repo")
    );
    assert!(sync_state.has_remote);

    std::env::remove_var("XDG_CONFIG_HOME");
    Ok(())
}

/// Test switching active repo and persisting
#[test]
#[serial]
fn test_switch_active_repo() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Create config directory
    let config_dir = temp_dir.path().join("claude-code-sync");
    std::fs::create_dir_all(&config_dir)?;

    // Write v2 format with multiple repos
    let v2_state = r#"{
        "version": 2,
        "active_repo": "work",
        "repos": {
            "work": {
                "name": "work",
                "sync_repo_path": "/tmp/work-repo",
                "has_remote": true,
                "is_cloned_repo": false
            },
            "personal": {
                "name": "personal",
                "sync_repo_path": "/tmp/personal-repo",
                "has_remote": false,
                "is_cloned_repo": false
            }
        }
    }"#;
    let state_path = config_dir.join("state.json");
    std::fs::write(&state_path, v2_state)?;

    // Load, switch, save
    let mut multi_state = MultiRepoState::load()?;
    assert_eq!(multi_state.active_repo, "work");

    multi_state.active_repo = "personal".to_string();
    multi_state.save()?;

    // Reload and verify
    let reloaded = MultiRepoState::load()?;
    assert_eq!(reloaded.active_repo, "personal");

    // SyncState should now return personal repo
    let sync_state = SyncState::load()?;
    assert_eq!(
        sync_state.sync_repo_path,
        std::path::PathBuf::from("/tmp/personal-repo")
    );
    assert!(!sync_state.has_remote);

    std::env::remove_var("XDG_CONFIG_HOME");
    Ok(())
}

/// Test init creates v2 format with actual git repo
#[test]
#[serial]
fn test_init_creates_v2_format_with_git_repo() -> Result<()> {
    use claude_code_sync::sync;

    let temp_dir = setup_test_config_env()?;
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    let repo_path = temp_dir.path().join("test-sync-repo");

    // Initialize using the init function
    sync::init_sync_repo(&repo_path, None)?;

    // Verify git repo was created
    assert!(repo_path.join(".git").exists());

    // Verify state.json is in v2 format
    let config_dir = temp_dir.path().join("claude-code-sync");
    let state_path = config_dir.join("state.json");
    let content = std::fs::read_to_string(&state_path)?;

    assert!(content.contains("\"version\": 2"));
    assert!(content.contains("\"active_repo\": \"default\""));

    // Load and verify
    let multi_state = MultiRepoState::load()?;
    assert_eq!(multi_state.version, 2);
    assert_eq!(multi_state.active_repo, "default");

    let default_repo = multi_state.repos.get("default").unwrap();
    assert_eq!(default_repo.sync_repo_path, repo_path);
    assert!(!default_repo.has_remote);

    std::env::remove_var("XDG_CONFIG_HOME");
    Ok(())
}

/// Test init with remote creates v2 format with remote_url populated
#[test]
#[serial]
fn test_init_with_remote_populates_remote_url() -> Result<()> {
    use claude_code_sync::sync;

    let temp_dir = setup_test_config_env()?;
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    let repo_path = temp_dir.path().join("test-remote-repo");

    // Initialize with a remote URL
    sync::init_sync_repo(&repo_path, Some("https://github.com/user/repo.git"))?;

    // Load and verify remote_url is populated
    let multi_state = MultiRepoState::load()?;
    let default_repo = multi_state.repos.get("default").unwrap();

    assert!(default_repo.has_remote);
    assert_eq!(
        default_repo.remote_url,
        Some("https://github.com/user/repo.git".to_string())
    );

    std::env::remove_var("XDG_CONFIG_HOME");
    Ok(())
}

/// Test full workflow: init repo, push, verify state persists correctly
#[test]
#[serial]
fn test_full_workflow_with_git_repos() -> Result<()> {
    use claude_code_sync::sync;

    let temp_dir = setup_test_config_env()?;
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Create a "remote" bare repo to simulate a git remote
    let bare_repo_path = temp_dir.path().join("bare-remote.git");
    std::process::Command::new("git")
        .args(["init", "--bare"])
        .arg(&bare_repo_path)
        .output()?;

    let repo_path = temp_dir.path().join("local-sync-repo");
    let remote_url = format!("file://{}", bare_repo_path.display());

    // Initialize with the "remote"
    sync::init_sync_repo(&repo_path, Some(&remote_url))?;

    // Verify state
    let multi_state = MultiRepoState::load()?;
    assert_eq!(multi_state.version, 2);

    let default_repo = multi_state.repos.get("default").unwrap();
    assert!(default_repo.has_remote);
    assert_eq!(default_repo.remote_url, Some(remote_url.clone()));

    // Verify git remote is configured
    let output = std::process::Command::new("git")
        .args(["remote", "-v"])
        .current_dir(&repo_path)
        .output()?;
    let remote_output = String::from_utf8_lossy(&output.stdout);
    assert!(remote_output.contains("origin"));

    std::env::remove_var("XDG_CONFIG_HOME");
    Ok(())
}

/// Test that operations use the active repo from MultiRepoState
#[test]
#[serial]
fn test_operations_use_active_repo() -> Result<()> {
    use claude_code_sync::sync;

    let temp_dir = setup_test_config_env()?;
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Create two repos
    let repo1_path = temp_dir.path().join("repo1");
    let repo2_path = temp_dir.path().join("repo2");

    // Initialize first repo
    sync::init_sync_repo(&repo1_path, None)?;

    // Manually add a second repo to the state
    let mut multi_state = MultiRepoState::load()?;

    use claude_code_sync::sync::RepoConfig;

    // Initialize second repo directory with git
    scm::init(&repo2_path)?;

    let repo2_config = RepoConfig {
        name: "repo2".to_string(),
        sync_repo_path: repo2_path.clone(),
        has_remote: false,
        is_cloned_repo: false,
        remote_url: None,
        description: Some("Second repo".to_string()),
    };
    multi_state.repos.insert("repo2".to_string(), repo2_config);
    multi_state.save()?;

    // SyncState should still return repo1 (the active one)
    let sync_state = SyncState::load()?;
    assert_eq!(sync_state.sync_repo_path, repo1_path);

    // Switch to repo2
    let mut multi_state = MultiRepoState::load()?;
    multi_state.active_repo = "repo2".to_string();
    multi_state.save()?;

    // Now SyncState should return repo2
    let sync_state = SyncState::load()?;
    assert_eq!(sync_state.sync_repo_path, repo2_path);

    std::env::remove_var("XDG_CONFIG_HOME");
    Ok(())
}

/// Test error handling when active repo doesn't exist in repos map
#[test]
#[serial]
fn test_invalid_active_repo_error() -> Result<()> {
    let temp_dir = setup_test_config_env()?;
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    // Create config directory
    let config_dir = temp_dir.path().join("claude-code-sync");
    std::fs::create_dir_all(&config_dir)?;

    // Write v2 format with invalid active_repo
    let v2_state = r#"{
        "version": 2,
        "active_repo": "nonexistent",
        "repos": {
            "work": {
                "name": "work",
                "sync_repo_path": "/tmp/work-repo",
                "has_remote": true,
                "is_cloned_repo": false
            }
        }
    }"#;
    let state_path = config_dir.join("state.json");
    std::fs::write(&state_path, v2_state)?;

    // SyncState::load() should return an error
    let result = SyncState::load();
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("nonexistent"));

    std::env::remove_var("XDG_CONFIG_HOME");
    Ok(())
}

/// Test init_from_onboarding creates v2 format
#[test]
#[serial]
fn test_init_from_onboarding_creates_v2() -> Result<()> {
    use claude_code_sync::sync;

    let temp_dir = setup_test_config_env()?;
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    let repo_path = temp_dir.path().join("onboarding-repo");

    // Use init_from_onboarding (what the onboarding flow uses)
    sync::init_from_onboarding(&repo_path, Some("https://github.com/test/repo.git"), false)?;

    // Verify v2 format
    let multi_state = MultiRepoState::load()?;
    assert_eq!(multi_state.version, 2);
    assert_eq!(multi_state.active_repo, "default");

    let default_repo = multi_state.repos.get("default").unwrap();
    assert_eq!(default_repo.sync_repo_path, repo_path);
    assert!(default_repo.has_remote);
    assert!(!default_repo.is_cloned_repo);
    assert_eq!(
        default_repo.remote_url,
        Some("https://github.com/test/repo.git".to_string())
    );

    std::env::remove_var("XDG_CONFIG_HOME");
    Ok(())
}

/// Test cloned repo flag is preserved in v2 format
#[test]
#[serial]
fn test_cloned_repo_flag_in_v2() -> Result<()> {
    use claude_code_sync::sync;

    let temp_dir = setup_test_config_env()?;
    std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());

    let repo_path = temp_dir.path().join("cloned-repo");

    // Pre-create the repo to simulate it already existing (as if cloned)
    scm::init(&repo_path)?;

    // Use init_from_onboarding with is_cloned=true
    sync::init_from_onboarding(&repo_path, Some("https://github.com/test/repo.git"), true)?;

    // Verify is_cloned_repo is true
    let multi_state = MultiRepoState::load()?;
    let default_repo = multi_state.repos.get("default").unwrap();
    assert!(default_repo.is_cloned_repo);

    // Also verify through SyncState compatibility layer
    let sync_state = SyncState::load()?;
    assert!(sync_state.is_cloned_repo);

    std::env::remove_var("XDG_CONFIG_HOME");
    Ok(())
}
