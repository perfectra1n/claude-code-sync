//! MultiRepoState: the v2 on-disk format, migration from v1, and switching the
//! active repository.

mod common;

use anyhow::Result;
use claude_code_sync::scm;
use claude_code_sync::sync::{self, MultiRepoState, RepoConfig, SyncState};
use common::ConfigEnv;
use serial_test::serial;
use std::collections::HashMap;
use std::path::PathBuf;

/// A v2 state.json with two repos, `work` active.
const TWO_REPOS_V2: &str = r#"{
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

// ---------------------------------------------------------------------------
// Format
// ---------------------------------------------------------------------------

#[test]
fn test_multi_repo_state_serialization() -> Result<()> {
    let repo_config = RepoConfig {
        name: "work".to_string(),
        sync_repo_path: PathBuf::from("/tmp/work-repo"),
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

    let repo = deserialized.repos.get("work").unwrap();
    assert_eq!(repo.name, "work");
    assert!(repo.has_remote);
    assert_eq!(
        repo.remote_url,
        Some("https://github.com/user/work.git".to_string())
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration and the SyncState compatibility layer
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn test_v1_to_v2_migration() -> Result<()> {
    let env = ConfigEnv::new();

    let state_path = env.write_state_json(
        r#"{
        "sync_repo_path": "/tmp/legacy-repo",
        "has_remote": true,
        "is_cloned_repo": false
    }"#,
    );

    // Loading a v1 file must transparently upgrade it.
    let multi_state = MultiRepoState::load()?;

    assert_eq!(multi_state.version, 2);
    assert_eq!(multi_state.active_repo, "default");

    let default_repo = multi_state.repos.get("default").unwrap();
    assert_eq!(
        default_repo.sync_repo_path,
        PathBuf::from("/tmp/legacy-repo")
    );
    assert!(default_repo.has_remote);
    assert!(!default_repo.is_cloned_repo);

    // ...and persist the upgrade, not just hold it in memory.
    let content = std::fs::read_to_string(&state_path)?;
    assert!(content.contains("\"version\": 2"));
    assert!(content.contains("\"active_repo\": \"default\""));

    Ok(())
}

#[test]
#[serial]
fn test_sync_state_loads_v2_format() -> Result<()> {
    let env = ConfigEnv::new();

    env.write_state_json(
        r#"{
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
    }"#,
    );

    // SyncState is the v1-shaped view over the active repo.
    let sync_state = SyncState::load()?;

    assert_eq!(sync_state.sync_repo_path, PathBuf::from("/tmp/my-repo"));
    assert!(sync_state.has_remote);
    assert!(sync_state.is_cloned_repo);

    Ok(())
}

#[test]
#[serial]
fn test_multi_repo_state_multiple_repos() -> Result<()> {
    let env = ConfigEnv::new();
    env.write_state_json(TWO_REPOS_V2);

    let multi_state = MultiRepoState::load()?;

    assert_eq!(multi_state.repos.len(), 2);
    assert!(multi_state.repos.contains_key("work"));
    assert!(multi_state.repos.contains_key("personal"));
    assert_eq!(multi_state.active_repo, "work");

    let sync_state = SyncState::load()?;
    assert_eq!(sync_state.sync_repo_path, PathBuf::from("/tmp/work-repo"));
    assert!(sync_state.has_remote);

    Ok(())
}

#[test]
#[serial]
fn test_invalid_active_repo_error() -> Result<()> {
    let env = ConfigEnv::new();

    // active_repo names a repo that isn't in the map.
    env.write_state_json(
        r#"{
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
    }"#,
    );

    let result = SyncState::load();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("nonexistent"));

    Ok(())
}

#[test]
#[serial]
fn test_config_handles_uninitialized_state() -> Result<()> {
    // No state file at all — a fresh install.
    let _env = ConfigEnv::new();

    let result = MultiRepoState::load();
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not initialized") || err_msg.contains("Run 'claude-code-sync init'"),
        "Error message should mention not initialized: {err_msg}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Switching the active repo
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn test_switch_active_repo() -> Result<()> {
    let env = ConfigEnv::new();
    env.write_state_json(TWO_REPOS_V2);

    let mut multi_state = MultiRepoState::load()?;
    assert_eq!(multi_state.active_repo, "work");

    multi_state.active_repo = "personal".to_string();
    multi_state.save()?;

    let reloaded = MultiRepoState::load()?;
    assert_eq!(reloaded.active_repo, "personal");

    // The switch has to be visible through the compatibility layer too.
    let sync_state = SyncState::load()?;
    assert_eq!(
        sync_state.sync_repo_path,
        PathBuf::from("/tmp/personal-repo")
    );
    assert!(!sync_state.has_remote);

    Ok(())
}

#[test]
#[serial]
fn test_operations_use_active_repo() -> Result<()> {
    let env = ConfigEnv::new();
    let repo1_path = env.join("repo1");
    let repo2_path = env.join("repo2");

    sync::init_sync_repo(&repo1_path, None)?;

    // Add a second repo by hand.
    scm::init(&repo2_path)?;
    let mut multi_state = MultiRepoState::load()?;
    multi_state.repos.insert(
        "repo2".to_string(),
        RepoConfig {
            name: "repo2".to_string(),
            sync_repo_path: repo2_path.clone(),
            has_remote: false,
            is_cloned_repo: false,
            remote_url: None,
            description: Some("Second repo".to_string()),
        },
    );
    multi_state.save()?;

    // Merely existing must not make repo2 active.
    assert_eq!(SyncState::load()?.sync_repo_path, repo1_path);

    let mut multi_state = MultiRepoState::load()?;
    multi_state.active_repo = "repo2".to_string();
    multi_state.save()?;

    assert_eq!(SyncState::load()?.sync_repo_path, repo2_path);

    Ok(())
}

// ---------------------------------------------------------------------------
// init writes v2
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn test_init_creates_v2_format_with_git_repo() -> Result<()> {
    let env = ConfigEnv::new();
    let repo_path = env.join("test-sync-repo");

    sync::init_sync_repo(&repo_path, None)?;

    assert!(repo_path.join(".git").exists());

    let content = std::fs::read_to_string(env.config_dir().join("state.json"))?;
    assert!(content.contains("\"version\": 2"));
    assert!(content.contains("\"active_repo\": \"default\""));

    let multi_state = MultiRepoState::load()?;
    assert_eq!(multi_state.version, 2);
    assert_eq!(multi_state.active_repo, "default");

    let default_repo = multi_state.repos.get("default").unwrap();
    assert_eq!(default_repo.sync_repo_path, repo_path);
    assert!(!default_repo.has_remote);

    Ok(())
}

#[test]
#[serial]
fn test_init_with_remote_populates_remote_url() -> Result<()> {
    let env = ConfigEnv::new();
    let repo_path = env.join("test-remote-repo");

    sync::init_sync_repo(&repo_path, Some("https://github.com/user/repo.git"))?;

    let multi_state = MultiRepoState::load()?;
    let default_repo = multi_state.repos.get("default").unwrap();

    assert!(default_repo.has_remote);
    assert_eq!(
        default_repo.remote_url,
        Some("https://github.com/user/repo.git".to_string())
    );

    Ok(())
}

#[test]
#[serial]
fn test_init_from_onboarding_creates_v2() -> Result<()> {
    let env = ConfigEnv::new();
    let repo_path = env.join("onboarding-repo");

    sync::init_from_onboarding(&repo_path, Some("https://github.com/test/repo.git"), false)?;

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

    Ok(())
}

#[test]
#[serial]
fn test_cloned_repo_flag_in_v2() -> Result<()> {
    let env = ConfigEnv::new();
    let repo_path = env.join("cloned-repo");

    // Pre-create the repo, as a clone would have.
    scm::init(&repo_path)?;
    sync::init_from_onboarding(&repo_path, Some("https://github.com/test/repo.git"), true)?;

    let multi_state = MultiRepoState::load()?;
    assert!(multi_state.repos.get("default").unwrap().is_cloned_repo);

    // And through the compatibility layer.
    assert!(SyncState::load()?.is_cloned_repo);

    Ok(())
}

#[test]
#[serial]
fn test_full_workflow_with_git_repos() -> Result<()> {
    let env = ConfigEnv::new();

    // A bare repo standing in for a real remote.
    let bare_repo_path = env.join("bare-remote.git");
    std::process::Command::new("git")
        .args(["init", "--bare"])
        .arg(&bare_repo_path)
        .output()?;

    let repo_path = env.join("local-sync-repo");
    let remote_url = format!("file://{}", bare_repo_path.display());

    sync::init_sync_repo(&repo_path, Some(&remote_url))?;

    let multi_state = MultiRepoState::load()?;
    assert_eq!(multi_state.version, 2);

    let default_repo = multi_state.repos.get("default").unwrap();
    assert!(default_repo.has_remote);
    assert_eq!(default_repo.remote_url, Some(remote_url.clone()));

    // The remote must exist in git itself, not just in our state file.
    let output = std::process::Command::new("git")
        .args(["remote", "-v"])
        .current_dir(&repo_path)
        .output()?;
    assert!(String::from_utf8_lossy(&output.stdout).contains("origin"));

    Ok(())
}
