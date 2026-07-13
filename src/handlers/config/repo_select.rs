//! The repository selector shown by `claude-code-sync config` with no argument.

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::Select;
use std::collections::HashMap;

use super::interactive::handle_config_interactive;
use crate::config::ConfigManager;
use crate::scm;
use crate::sync::{MultiRepoState, RepoConfig};

/// Try to recover an existing repo if state.json is missing but repo exists
///
/// This handles the case where a user has a valid repo in the default location
/// but the state.json file is missing (e.g., from an older version or deletion).
fn try_recover_existing_repo() -> Result<Option<MultiRepoState>> {
    let default_repo = match ConfigManager::default_repo_dir() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };

    if !default_repo.exists() || !scm::is_repo(&default_repo) {
        return Ok(None);
    }

    let (has_remote, remote_url) = match scm::open(&default_repo) {
        Ok(repo) => {
            let has_remote = repo.has_remote("origin");
            let remote_url = if has_remote {
                repo.get_remote_url("origin").ok()
            } else {
                None
            };
            (has_remote, remote_url)
        }
        Err(_) => (false, None),
    };

    println!(
        "{} Found existing repo at: {}",
        "!".yellow(),
        default_repo.display()
    );
    if let Some(ref url) = remote_url {
        println!("  Remote: {}", url.cyan());
    }
    println!("  Recovering configuration...");
    println!();

    let repo_config = RepoConfig {
        name: "default".to_string(),
        sync_repo_path: default_repo,
        has_remote,
        is_cloned_repo: false, // We can't know this for sure
        remote_url,
        description: Some("Recovered from existing repository".to_string()),
    };

    let mut repos = HashMap::new();
    repos.insert("default".to_string(), repo_config);

    let state = MultiRepoState {
        version: 2,
        active_repo: "default".to_string(),
        repos,
    };

    state.save()?;

    Ok(Some(state))
}

/// Handle the repository selector menu
///
/// Shows when `claude-code-sync config` is run with no arguments.
/// Displays all configured repositories and allows switching between them.
pub fn handle_repo_selector() -> Result<()> {
    println!("{}", "Repository Configuration".cyan().bold());
    println!("{}", "=".repeat(60).cyan());
    println!();

    // "Not initialized" is recoverable if a repo happens to sit in the default
    // location; any other load failure is not ours to interpret.
    let mut state = match MultiRepoState::load() {
        Ok(s) => s,
        Err(e) => {
            let err_msg = e.to_string();
            if err_msg.contains("not initialized")
                || err_msg.contains("Run 'claude-code-sync init'")
            {
                if let Some(recovered) = try_recover_existing_repo()? {
                    println!(
                        "{}",
                        "Found existing repository - recovered configuration!".green()
                    );
                    println!();
                    recovered
                } else {
                    println!("{}", "No repositories configured.".yellow());
                    println!();
                    println!(
                        "Run '{}' to set up your first repository.",
                        "claude-code-sync init".cyan()
                    );
                    return Ok(());
                }
            } else {
                return Err(e);
            }
        }
    };

    if state.repos.is_empty() {
        println!("{}", "No repositories configured.".yellow());
        println!();
        println!(
            "Run '{}' to set up your first repository.",
            "claude-code-sync init".cyan()
        );
        return Ok(());
    }

    // Active repo first, then alphabetical.
    let mut repo_entries: Vec<_> = state.repos.values().collect();
    repo_entries.sort_by(|a, b| {
        if a.name == state.active_repo {
            std::cmp::Ordering::Less
        } else if b.name == state.active_repo {
            std::cmp::Ordering::Greater
        } else {
            a.name.cmp(&b.name)
        }
    });

    let mut options: Vec<String> = repo_entries
        .iter()
        .map(|repo| {
            let active_marker = if repo.name == state.active_repo {
                format!(" {}", "[ACTIVE]".green().bold())
            } else {
                String::new()
            };

            let path_str = repo.sync_repo_path.display().to_string();
            let remote_info = repo
                .remote_url
                .as_ref()
                .map(|u| format!(" ({})", u.dimmed()))
                .unwrap_or_default();

            format!(
                "{}{} - {}{}",
                repo.name, active_marker, path_str, remote_info
            )
        })
        .collect();

    options.push(format!("{}", "─── Actions ───".dimmed()));
    options.push("Configure filters (current repo)".to_string());
    options.push("Exit".to_string());

    let selection = Select::new("Select a repository to make active:", options.clone())
        .with_help_message("Use arrow keys to navigate, Enter to select")
        .prompt()
        .context("Failed to get user selection")?;

    if selection.contains("─── Actions ───") || selection == "Exit" {
        return Ok(());
    }

    if selection == "Configure filters (current repo)" {
        return handle_config_interactive();
    }

    // The repo name is the first token, before any [ACTIVE] marker.
    let repo_name = selection
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid selection"))?;

    if repo_name == state.active_repo {
        println!();
        println!(
            "{} '{}' is already the active repository.",
            "ℹ".blue(),
            repo_name.cyan()
        );
        return Ok(());
    }

    if state.repos.contains_key(repo_name) {
        state.active_repo = repo_name.to_string();
        state.save()?;

        println!();
        println!(
            "{} Switched to repository '{}'",
            "✓".green().bold(),
            repo_name.cyan()
        );

        if let Some(repo) = state.repos.get(repo_name) {
            println!("  Path: {}", repo.sync_repo_path.display());
            if let Some(ref url) = repo.remote_url {
                println!("  Remote: {url}");
            }
            if repo.has_remote {
                println!("  Has remote: {}", "Yes".green());
            } else {
                println!("  Has remote: {}", "No (local only)".yellow());
            }
        }
    } else {
        return Err(anyhow::anyhow!("Repository '{}' not found", repo_name));
    }

    Ok(())
}
