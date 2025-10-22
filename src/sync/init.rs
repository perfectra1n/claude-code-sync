use anyhow::Result;
use colored::Colorize;
use std::path::Path;

use crate::git::GitManager;

use super::state::SyncState;

/// Initialize sync repository from onboarding config
pub fn init_from_onboarding(
    repo_path: &Path,
    remote_url: Option<&str>,
    is_cloned: bool,
) -> Result<()> {
    crate::config::ConfigManager::ensure_config_dir()?;

    // If this is a cloned repo and it already exists, just open it
    // Otherwise, initialize a new one
    let git_manager = if repo_path.exists() && repo_path.join(".git").exists() {
        GitManager::open(repo_path)?
    } else {
        GitManager::init(repo_path)?
    };

    // Add remote if specified
    let has_remote = if let Some(url) = remote_url {
        if !git_manager.has_remote("origin") {
            git_manager.add_remote("origin", url)?;
        }
        true
    } else {
        false
    };

    // Save sync state
    let state = SyncState {
        sync_repo_path: repo_path.to_path_buf(),
        has_remote,
        is_cloned_repo: is_cloned,
    };
    state.save()?;

    Ok(())
}

/// Initialize a new sync repository
pub fn init_sync_repo(repo_path: &Path, remote_url: Option<&str>) -> Result<()> {
    println!(
        "{}",
        "Initializing Claude Code sync repository...".cyan().bold()
    );

    // Create/open the git repository
    let git_manager = if repo_path.exists() && repo_path.join(".git").exists() {
        println!(
            "  {} existing repository at {}",
            "Using".green(),
            repo_path.display()
        );
        GitManager::open(repo_path)?
    } else {
        println!(
            "  {} new repository at {}",
            "Creating".green(),
            repo_path.display()
        );
        GitManager::init(repo_path)?
    };

    // Add remote if specified
    let has_remote = if let Some(url) = remote_url {
        if !git_manager.has_remote("origin") {
            git_manager.add_remote("origin", url)?;
            println!("  {} remote 'origin' -> {}", "Added".green(), url);
        } else {
            println!("  {} Remote 'origin' already exists", "Note:".yellow());
        }
        true
    } else {
        false
    };

    // Save sync state
    let state = SyncState {
        sync_repo_path: repo_path.to_path_buf(),
        has_remote,
        is_cloned_repo: false,
    };
    state.save()?;

    println!(
        "{}",
        "Sync repository initialized successfully!".green().bold()
    );
    println!("\n{} claude-code-sync push", "Next steps:".cyan().bold());

    Ok(())
}
