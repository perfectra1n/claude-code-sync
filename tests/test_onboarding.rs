//! Repository initialization: the onboarding flow, the `--repo` flag, cloning,
//! and `claude-code-sync-init.toml` validation.

mod common;

use anyhow::Result;
use claude_code_sync::config::ConfigManager;
use claude_code_sync::filter::FilterConfig;
use claude_code_sync::onboarding::InitConfig;
use claude_code_sync::scm;
use claude_code_sync::sync::{self, SyncState};
use common::ConfigEnv;
use serial_test::serial;
use std::io::Write;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// init_from_onboarding
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn test_init_from_onboarding() -> Result<()> {
    let env = ConfigEnv::new();
    let repo_path = env.join("onboarding-test-repo");

    scm::init(&repo_path)?;
    sync::init_from_onboarding(&repo_path, None, false)?;

    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(!state.has_remote);
    assert!(!state.is_cloned_repo);

    Ok(())
}

#[test]
#[serial]
fn test_init_from_onboarding_with_remote() -> Result<()> {
    let env = ConfigEnv::new();
    let repo_path = env.join("onboarding-remote-test");

    scm::init(&repo_path)?;
    sync::init_from_onboarding(&repo_path, Some("https://github.com/user/repo.git"), true)?;

    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(state.has_remote);
    assert!(state.is_cloned_repo);

    Ok(())
}

#[test]
#[serial]
fn test_init_from_onboarding_sets_is_cloned_flag() -> Result<()> {
    let env = ConfigEnv::new();
    let repo_path = env.join("cloned-repo-test");

    // Simulating the post-clone state.
    scm::init(&repo_path)?;
    sync::init_from_onboarding(&repo_path, Some("https://github.com/user/repo.git"), true)?;

    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(state.has_remote);
    assert!(
        state.is_cloned_repo,
        "is_cloned_repo should be true for cloned repos"
    );

    Ok(())
}

#[test]
#[serial]
fn test_init_from_onboarding_local_repo_not_cloned() -> Result<()> {
    let env = ConfigEnv::new();
    let repo_path = env.join("local-repo-test");

    scm::init(&repo_path)?;
    sync::init_from_onboarding(&repo_path, None, false)?;

    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(!state.has_remote);
    assert!(
        !state.is_cloned_repo,
        "is_cloned_repo should be false for local repos"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// init_sync_repo (the `--repo` flag) also has to create a FilterConfig
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn test_init_sync_repo_creates_filter_config() -> Result<()> {
    let env = ConfigEnv::new();
    let repo_path = env.join("cli-init-test-repo");

    sync::init_sync_repo(&repo_path, None)?;

    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(!state.has_remote);
    assert!(!state.is_cloned_repo);

    assert!(
        ConfigManager::filter_config_path()?.exists(),
        "Filter config should be created by init_sync_repo"
    );

    let filter_config = FilterConfig::load()?;
    assert!(!filter_config.exclude_attachments);

    Ok(())
}

#[test]
#[serial]
fn test_init_sync_repo_with_remote_creates_filter_config() -> Result<()> {
    let env = ConfigEnv::new();
    let repo_path = env.join("cli-remote-test-repo");

    sync::init_sync_repo(&repo_path, Some("https://github.com/user/repo.git"))?;

    let state = SyncState::load()?;
    assert_eq!(state.sync_repo_path, repo_path);
    assert!(state.has_remote);
    assert!(!state.is_cloned_repo); // Not cloned, just added as origin

    assert!(
        ConfigManager::filter_config_path()?.exists(),
        "Filter config should be created by init_sync_repo with remote"
    );

    Ok(())
}

#[test]
#[serial]
fn test_init_sync_repo_does_not_overwrite_existing_filter_config() -> Result<()> {
    let env = ConfigEnv::new();
    let repo_path = env.join("no-overwrite-test-repo");

    let custom_config = FilterConfig {
        exclude_attachments: true,
        exclude_older_than_days: Some(42),
        ..Default::default()
    };
    custom_config.save()?;

    sync::init_sync_repo(&repo_path, None)?;

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

    Ok(())
}

// ---------------------------------------------------------------------------
// Cloning
// ---------------------------------------------------------------------------

#[test]
fn test_scm_clone_validates_path() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let clone_path = temp_dir.path().join("cloned-repo");

    let result = scm::clone("invalid-url", &clone_path);
    assert!(result.is_err());

    let err_msg = result.err().unwrap().to_string();
    assert!(err_msg.contains("clone failed") || err_msg.contains("Failed"));

    Ok(())
}

#[test]
fn test_clone_with_invalid_url_fails() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let clone_path = temp_dir.path().join("clone-test-repo");

    let result = scm::clone("not-a-valid-url", &clone_path);
    assert!(result.is_err(), "Clone should fail with invalid URL");

    Ok(())
}

#[test]
fn test_clone_creates_parent_directories() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let nested_path = temp_dir
        .path()
        .join("deeply")
        .join("nested")
        .join("path")
        .join("repo");

    let result = scm::clone(
        "https://invalid-url-that-wont-work.example.com/repo.git",
        &nested_path,
    );

    // The clone fails, but the parent directories should have been created first.
    assert!(result.is_err());
    assert!(nested_path.parent().unwrap().exists());

    Ok(())
}

// ---------------------------------------------------------------------------
// InitConfig validation
// ---------------------------------------------------------------------------

/// Write an init TOML into `dir` and try to load it.
fn load_init_config(dir: &TempDir, body: &str) -> Result<InitConfig> {
    let config_path = dir.path().join("init.toml");
    let mut file = std::fs::File::create(&config_path)?;
    writeln!(file, "{body}")?;
    InitConfig::load(&config_path)
}

#[test]
fn test_init_config_clone_requires_remote_url() -> Result<()> {
    let temp_dir = TempDir::new()?;

    let result = load_init_config(
        &temp_dir,
        r#"
repo_path = "/tmp/test"
clone = true
"#,
    );

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
    let temp_dir = TempDir::new()?;

    let result = load_init_config(
        &temp_dir,
        r#"
repo_path = "/tmp/test"
remote_url = "https://github.com/user/repo.git"
clone = true
"#,
    );

    assert!(
        result.is_ok(),
        "clone=true with remote_url should be valid: {:?}",
        result.err()
    );

    Ok(())
}
