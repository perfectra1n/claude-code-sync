//! Config paths, filter configuration, and SyncState serialization.

mod common;

use anyhow::Result;
use claude_code_sync::config::ConfigManager;
use claude_code_sync::filter::FilterConfig;
use claude_code_sync::sync::SyncState;
use common::ConfigEnv;
use serial_test::serial;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// ConfigManager paths
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn test_config_manager_paths() -> Result<()> {
    let _env = ConfigEnv::new();

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
#[serial]
fn test_default_repo_dir_exists() -> Result<()> {
    let _env = ConfigEnv::new();

    let default_dir = ConfigManager::default_repo_dir()?;
    assert!(default_dir.ends_with("repo"));
    assert!(default_dir.to_string_lossy().contains("claude-code-sync"));

    Ok(())
}

#[test]
#[serial]
fn test_ensure_config_dir_creates_directory() -> Result<()> {
    // Guarded: this mkdir's, and without the override it would do so in the
    // developer's real home directory.
    let _env = ConfigEnv::new();

    let config_dir = ConfigManager::ensure_config_dir()?;
    assert!(config_dir.exists());
    assert!(config_dir.is_dir());

    Ok(())
}

#[test]
#[serial]
fn test_config_directory_structure() -> Result<()> {
    let _env = ConfigEnv::new();

    let config_dir = ConfigManager::ensure_config_dir()?;
    assert!(config_dir.exists());
    assert!(config_dir.is_dir());

    let snapshots_dir = ConfigManager::ensure_snapshots_dir()?;
    assert!(snapshots_dir.exists());
    assert!(snapshots_dir.is_dir());

    assert!(snapshots_dir.starts_with(&config_dir));

    Ok(())
}

// ---------------------------------------------------------------------------
// FilterConfig
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn test_filter_config_save_and_load() -> Result<()> {
    let _env = ConfigEnv::new();

    let config = FilterConfig {
        exclude_attachments: true,
        exclude_older_than_days: Some(30),
        ..Default::default()
    };
    config.save()?;

    let loaded = FilterConfig::load()?;
    assert!(loaded.exclude_attachments);
    assert_eq!(loaded.exclude_older_than_days, Some(30));

    Ok(())
}

#[test]
#[serial]
fn test_multiple_config_operations() -> Result<()> {
    let _env = ConfigEnv::new();

    // Loading from an empty config dir must yield defaults rather than erroring.
    let _loaded = FilterConfig::load()?;

    let config = FilterConfig {
        exclude_attachments: true,
        exclude_older_than_days: Some(99),
        ..Default::default()
    };
    config.save()?;

    let loaded2 = FilterConfig::load()?;
    assert!(loaded2.exclude_attachments);
    assert_eq!(loaded2.exclude_older_than_days, Some(99));

    Ok(())
}

#[test]
fn test_filter_config_with_attachments() -> Result<()> {
    let config = FilterConfig {
        exclude_attachments: true,
        ..Default::default()
    };

    assert!(config.should_include(&PathBuf::from("session.jsonl")));

    assert!(!config.should_include(&PathBuf::from("image.png")));
    assert!(!config.should_include(&PathBuf::from("document.pdf")));
    assert!(!config.should_include(&PathBuf::from("video.mp4")));

    Ok(())
}

// ---------------------------------------------------------------------------
// SyncState (de)serialization
// ---------------------------------------------------------------------------

#[test]
fn test_sync_state_with_cloned_flag() -> Result<()> {
    let state = SyncState {
        sync_repo_path: PathBuf::from("/tmp/test-repo"),
        has_remote: true,
        is_cloned_repo: true,
    };

    let serialized = serde_json::to_string(&state)?;
    assert!(serialized.contains("is_cloned_repo"));
    assert!(serialized.contains("true"));

    let deserialized: SyncState = serde_json::from_str(&serialized)?;
    assert_eq!(deserialized.sync_repo_path, PathBuf::from("/tmp/test-repo"));
    assert!(deserialized.has_remote);
    assert!(deserialized.is_cloned_repo);

    Ok(())
}

#[test]
fn test_sync_state_backwards_compatible() -> Result<()> {
    // State files written before `is_cloned_repo` existed must still load.
    let old_state_json = r#"{
        "sync_repo_path": "/tmp/test-repo",
        "has_remote": true
    }"#;

    let state: SyncState = serde_json::from_str(old_state_json)?;
    assert!(state.has_remote);
    assert!(!state.is_cloned_repo); // Should default to false

    Ok(())
}
