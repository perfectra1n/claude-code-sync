use anyhow::Result;
use colored::Colorize;
use std::collections::HashMap;
use std::path::Path;

use crate::scm;

use super::state::{MultiRepoState, RepoConfig};

/// Initialize sync repository from onboarding config
pub fn init_from_onboarding(
    repo_path: &Path,
    remote_url: Option<&str>,
    is_cloned: bool,
) -> Result<()> {
    crate::config::ConfigManager::ensure_config_dir()?;

    // If this is a cloned repo and it already exists, just open it
    // Otherwise, initialize a new one
    let scm = if repo_path.exists() && scm::is_repo(repo_path) {
        scm::open(repo_path)?
    } else {
        scm::init(repo_path)?
    };

    // Add remote if specified
    let has_remote = if let Some(url) = remote_url {
        if !scm.has_remote("origin") {
            scm.add_remote("origin", url)?;
        }
        true
    } else {
        false
    };

    // Create repo config
    let repo_name = "default".to_string();
    let repo_config = RepoConfig {
        name: repo_name.clone(),
        sync_repo_path: repo_path.to_path_buf(),
        has_remote,
        is_cloned_repo: is_cloned,
        remote_url: remote_url.map(String::from),
        description: None,
    };

    // Save multi-repo state (v2 format)
    let mut repos = HashMap::new();
    repos.insert(repo_name.clone(), repo_config);
    let state = MultiRepoState {
        version: 2,
        active_repo: repo_name,
        repos,
    };
    state.save()?;

    Ok(())
}

/// Initialize a new sync repository
pub fn init_sync_repo(repo_path: &Path, remote_url: Option<&str>) -> Result<()> {
    // Ensure config directory exists
    crate::config::ConfigManager::ensure_config_dir()?;

    println!(
        "{}",
        "Initializing Claude Code sync repository...".cyan().bold()
    );

    // Create/open the repository
    let scm = if repo_path.exists() && scm::is_repo(repo_path) {
        println!(
            "  {} existing repository at {}",
            "Using".green(),
            repo_path.display()
        );
        scm::open(repo_path)?
    } else {
        println!(
            "  {} new repository at {}",
            "Creating".green(),
            repo_path.display()
        );
        scm::init(repo_path)?
    };

    // Add remote if specified
    let has_remote = if let Some(url) = remote_url {
        if !scm.has_remote("origin") {
            scm.add_remote("origin", url)?;
            println!("  {} remote 'origin' -> {}", "Added".green(), url);
        } else {
            println!("  {} Remote 'origin' already exists", "Note:".yellow());
        }
        true
    } else {
        false
    };

    // Create repo config
    let repo_name = "default".to_string();
    let repo_config = RepoConfig {
        name: repo_name.clone(),
        sync_repo_path: repo_path.to_path_buf(),
        has_remote,
        is_cloned_repo: false,
        remote_url: remote_url.map(String::from),
        description: None,
    };

    // Save multi-repo state (v2 format)
    let mut repos = HashMap::new();
    repos.insert(repo_name.clone(), repo_config);
    let state = MultiRepoState {
        version: 2,
        active_repo: repo_name,
        repos,
    };
    state.save()?;

    // Save default filter configuration if it doesn't exist
    let filter_config_path = crate::config::ConfigManager::filter_config_path()?;
    if !filter_config_path.exists() {
        crate::filter::FilterConfig::default().save()?;
    }

    println!(
        "{}",
        "Sync repository initialized successfully!".green().bold()
    );
    println!("\n{} claude-code-sync push", "Next steps:".cyan().bold());

    Ok(())
}
