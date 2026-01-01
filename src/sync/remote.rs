use anyhow::{anyhow, Context, Result};
use colored::Colorize;

use crate::scm;

use super::state::SyncState;

/// Show current remote configuration
pub fn show_remote() -> Result<()> {
    let state = SyncState::load()?;
    let repo = scm::open(&state.sync_repo_path)?;

    println!("{}", "=== SCM Remote Configuration ===".bold().cyan());
    println!();

    // Show sync repository directory
    println!(
        "{} {}",
        "Sync Directory:".bold(),
        state.sync_repo_path.display().to_string().cyan()
    );

    // Show backend type
    println!("{} Git", "Backend:".bold());

    // Show current branch
    if let Ok(branch) = repo.current_branch() {
        println!("{} {}", "Current Branch:".bold(), branch.cyan());
    }

    println!();

    // List all remotes
    let remotes = repo.list_remotes()?;

    if remotes.is_empty() {
        println!("{}", "No remotes configured".yellow());
        println!(
            "\n{} claude-code-sync remote set origin <url>",
            "Hint:".cyan()
        );
        return Ok(());
    }

    for name in &remotes {
        println!("{} {}", "Remote:".bold(), name.cyan());

        if let Ok(url) = repo.get_remote_url(name) {
            println!("  URL: {url}");
        } else {
            println!("  URL: {}", "None".yellow());
        }

        println!();
    }

    Ok(())
}

/// Set or update remote URL
pub fn set_remote(name: &str, url: &str) -> Result<()> {
    let state = SyncState::load()?;
    let repo = scm::open(&state.sync_repo_path)?;

    // Validate URL format
    if !url.starts_with("http://") && !url.starts_with("https://") && !url.starts_with("git@") && !url.starts_with("ssh://") {
        return Err(anyhow!(
            "Invalid URL format: {url}\n\
            \n\
            URL must start with:\n\
            - https:// (e.g., https://github.com/user/repo.git)\n\
            - http:// (e.g., http://gitlab.com/user/repo.git)\n\
            - git@ (e.g., git@github.com:user/repo.git)\n\
            - ssh:// (e.g., ssh://git@github.com/user/repo.git)"
        ));
    }

    // Check if remote exists
    let remote_exists = repo.has_remote(name);

    if remote_exists {
        // Update existing remote
        repo.set_remote_url(name, url)
            .with_context(|| format!("Failed to update remote '{name}' URL"))?;

        println!(
            "{} Updated remote '{}' to: {}",
            "✓".green().bold(),
            name.cyan(),
            url
        );
    } else {
        // Create new remote
        repo.add_remote(name, url)
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
    let repo = scm::open(&state.sync_repo_path)?;

    // Check if remote exists
    if !repo.has_remote(name) {
        return Err(anyhow!("Remote '{name}' not found"));
    }

    // Remove the remote
    repo.remove_remote(name)
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
