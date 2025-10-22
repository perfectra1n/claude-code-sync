use anyhow::{anyhow, Context, Result};
use colored::Colorize;

use crate::git::GitManager;

use super::state::SyncState;

/// Show current remote configuration
pub fn show_remote() -> Result<()> {
    let state = SyncState::load()?;
    let git_manager = GitManager::open(&state.sync_repo_path)?;

    println!("{}", "=== Git Remote Configuration ===".bold().cyan());
    println!();

    // Show sync repository directory
    println!(
        "{} {}",
        "Sync Directory:".bold(),
        state.sync_repo_path.display().to_string().cyan()
    );

    // Show current branch
    if let Ok(branch) = git_manager.current_branch() {
        println!("{} {}", "Current Branch:".bold(), branch.cyan());
    }

    println!();

    // Get repository
    let repo = git2::Repository::open(&state.sync_repo_path)?;

    // List all remotes
    let remotes = repo.remotes()?;

    if remotes.is_empty() {
        println!("{}", "No remotes configured".yellow());
        println!(
            "\n{} claude-code-sync remote set origin <url>",
            "Hint:".cyan()
        );
        return Ok(());
    }

    for name in remotes.iter().flatten() {
        if let Ok(remote) = repo.find_remote(name) {
            println!("{} {}", "Remote:".bold(), name.cyan());

            if let Some(url) = remote.url() {
                println!("  URL: {url}");
            } else {
                println!("  URL: {}", "None".yellow());
            }

            if let Some(push_url) = remote.pushurl() {
                println!("  Push URL: {push_url}");
            }

            println!();
        }
    }

    Ok(())
}

/// Set or update remote URL
pub fn set_remote(name: &str, url: &str) -> Result<()> {
    let state = SyncState::load()?;
    let repo = git2::Repository::open(&state.sync_repo_path)?;

    // Validate URL format
    if !url.starts_with("http://") && !url.starts_with("https://") && !url.starts_with("git@") {
        return Err(anyhow!(
            "Invalid URL format: {url}\n\
            \n\
            URL must start with:\n\
            - https:// (e.g., https://github.com/user/repo.git)\n\
            - http:// (e.g., http://gitlab.com/user/repo.git)\n\
            - git@ (e.g., git@github.com:user/repo.git)"
        ));
    }

    // Check if remote exists
    let remote_exists = repo.find_remote(name).is_ok();

    if remote_exists {
        // Update existing remote
        repo.remote_set_url(name, url)
            .with_context(|| format!("Failed to update remote '{name}' URL"))?;

        println!(
            "{} Updated remote '{}' to: {}",
            "✓".green().bold(),
            name.cyan(),
            url
        );
    } else {
        // Create new remote
        repo.remote(name, url)
            .with_context(|| format!("Failed to create remote '{name}'"))?;

        println!(
            "{} Created remote '{}': {}",
            "✓".green().bold(),
            name.cyan(),
            url
        );
    }

    // Update state if this is the origin remote
    if name == "origin" {
        let mut state = state;
        state.has_remote = true;
        state.save()?;
    }

    println!("\n{} claude-code-sync push", "Next:".cyan());

    Ok(())
}

/// Remove a remote
pub fn remove_remote(name: &str) -> Result<()> {
    let state = SyncState::load()?;
    let repo = git2::Repository::open(&state.sync_repo_path)?;

    // Check if remote exists
    if repo.find_remote(name).is_err() {
        return Err(anyhow!("Remote '{name}' not found"));
    }

    // Remove the remote
    repo.remote_delete(name)
        .with_context(|| format!("Failed to remove remote '{name}'"))?;

    println!("{} Removed remote '{}'", "✓".green().bold(), name.cyan());

    // Update state if this was the origin remote
    if name == "origin" {
        let mut state = state;
        state.has_remote = false;
        state.save()?;
    }

    Ok(())
}
