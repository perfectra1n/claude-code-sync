//! Configuration command handlers
//!
//! Handles interactive configuration management including wizard mode
//! and menu-based configuration editing.

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Confirm, MultiSelect, Select, Text};

use crate::config::ConfigManager;
use crate::filter::FilterConfig;
use crate::scm;
use crate::sync::{MultiRepoState, RepoConfig};
use std::collections::HashMap;

/// Handle interactive configuration menu
///
/// Shows all configuration options and allows user to select which ones to modify
pub fn handle_config_interactive() -> Result<()> {
    println!("{}", "Interactive Configuration".cyan().bold());
    println!("{}", "=".repeat(80).cyan());
    println!();

    // Load current configuration
    let current_config = FilterConfig::load().context("Failed to load current configuration")?;

    // Display current configuration
    println!("{}", "Current Settings:".bold());
    display_config_summary(&current_config);
    println!();

    // Define available configuration options
    let options = vec![
        "Exclude older than (days)",
        "Include patterns",
        "Exclude patterns",
        "Exclude attachments",
        "Max file size",
    ];

    // Let user select which settings to modify
    let selections = MultiSelect::new(
        "Select settings to modify (Space to select, Enter to confirm):",
        options,
    )
    .with_help_message("Use arrow keys to navigate, Space to select/deselect, Enter when done")
    .prompt()
    .context("Failed to get user selections")?;

    if selections.is_empty() {
        println!("{}", "No settings selected. Configuration unchanged.".yellow());
        return Ok(());
    }

    println!();
    println!("{}", "Modifying selected settings:".cyan().bold());
    println!();

    // Process each selected setting
    let mut modified_config = current_config.clone();

    for selection in selections {
        match selection {
            "Exclude older than (days)" => {
                let current = modified_config
                    .exclude_older_than_days
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "Not set".to_string());

                let input = Text::new("Exclude older than (days):")
                    .with_help_message(&format!("Current: {}. Enter a number or leave empty to unset", current))
                    .prompt()?;

                if input.trim().is_empty() {
                    modified_config.exclude_older_than_days = None;
                    println!("  {} Unset exclude_older_than_days", "✓".green());
                } else {
                    let days: u32 = input.trim().parse()
                        .context("Invalid number. Must be a positive integer.")?;
                    modified_config.exclude_older_than_days = Some(days);
                    println!("  {} Set exclude_older_than_days to {} days", "✓".green(), days);
                }
            }

            "Include patterns" => {
                let current = if modified_config.include_patterns.is_empty() {
                    "None".to_string()
                } else {
                    modified_config.include_patterns.join(", ")
                };

                let input = Text::new("Include patterns (comma-separated):")
                    .with_help_message(&format!("Current: {}. Glob patterns like '*work*' or '/path/to/project'", current))
                    .prompt()?;

                if input.trim().is_empty() {
                    modified_config.include_patterns = Vec::new();
                    println!("  {} Cleared include patterns", "✓".green());
                } else {
                    modified_config.include_patterns = input
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    println!("  {} Set include patterns: {:?}", "✓".green(), modified_config.include_patterns);
                }
            }

            "Exclude patterns" => {
                let current = if modified_config.exclude_patterns.is_empty() {
                    "None".to_string()
                } else {
                    modified_config.exclude_patterns.join(", ")
                };

                let input = Text::new("Exclude patterns (comma-separated):")
                    .with_help_message(&format!("Current: {}. Glob patterns like '*test*' or '/tmp/*'", current))
                    .prompt()?;

                if input.trim().is_empty() {
                    modified_config.exclude_patterns = Vec::new();
                    println!("  {} Cleared exclude patterns", "✓".green());
                } else {
                    modified_config.exclude_patterns = input
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    println!("  {} Set exclude patterns: {:?}", "✓".green(), modified_config.exclude_patterns);
                }
            }

            "Exclude attachments" => {
                let current = modified_config.exclude_attachments;

                let exclude = Confirm::new("Exclude attachments (images, PDFs, etc.)?")
                    .with_default(current)
                    .with_help_message(&format!("Current: {}. If yes, only .jsonl files will be synced", current))
                    .prompt()?;

                modified_config.exclude_attachments = exclude;
                println!("  {} Set exclude_attachments to {}", "✓".green(), exclude);
            }

            "Max file size" => {
                let current_mb = modified_config.max_file_size_bytes as f64 / (1024.0 * 1024.0);

                let input = Text::new("Max file size (MB):")
                    .with_default(&format!("{:.1}", current_mb))
                    .with_help_message("Maximum size for individual files (e.g., 10 for 10MB)")
                    .prompt()?;

                let size_mb: f64 = input.trim().parse()
                    .context("Invalid number. Must be a positive number.")?;

                modified_config.max_file_size_bytes = (size_mb * 1024.0 * 1024.0) as u64;
                println!("  {} Set max_file_size to {:.1} MB", "✓".green(), size_mb);
            }

            _ => {}
        }
        println!();
    }

    // Show final configuration and confirm
    println!("{}", "New Configuration:".cyan().bold());
    display_config_summary(&modified_config);
    println!();

    let confirm = Confirm::new("Save this configuration?")
        .with_default(true)
        .prompt()?;

    if confirm {
        modified_config.save().context("Failed to save configuration")?;
        println!("\n{} Configuration saved successfully!", "✓".green().bold());
    } else {
        println!("\n{}", "Configuration not saved.".yellow());
    }

    Ok(())
}

/// Handle wizard-mode configuration
///
/// Steps through each configuration option one by one
pub fn handle_config_wizard() -> Result<()> {
    println!("{}", "Configuration Wizard".cyan().bold());
    println!("{}", "=".repeat(80).cyan());
    println!();
    println!("{}", "This wizard will walk you through all configuration options.".dimmed());
    println!("{}", "Press Enter to keep current value or enter a new value.".dimmed());
    println!();

    // Load current configuration
    let current_config = FilterConfig::load().context("Failed to load current configuration")?;
    let mut modified_config = current_config.clone();

    // 1. Exclude older than
    println!("{}", "1. Age Filter".bold().cyan());
    let current_age = modified_config
        .exclude_older_than_days
        .map(|d| d.to_string())
        .unwrap_or_else(|| "Not set".to_string());
    println!("   Current: {}", current_age.yellow());

    let exclude_old = Confirm::new("Do you want to exclude projects older than a certain number of days?")
        .with_default(modified_config.exclude_older_than_days.is_some())
        .prompt()?;

    if exclude_old {
        let default_days = modified_config.exclude_older_than_days.unwrap_or(30).to_string();
        let input = Text::new("How many days?")
            .with_default(&default_days)
            .prompt()?;

        let days: u32 = input.trim().parse()
            .context("Invalid number. Must be a positive integer.")?;
        modified_config.exclude_older_than_days = Some(days);
        println!("  {} Will exclude projects older than {} days\n", "✓".green(), days);
    } else {
        modified_config.exclude_older_than_days = None;
        println!("  {} Age filter disabled\n", "✓".green());
    }

    // 2. Include patterns
    println!("{}", "2. Include Patterns".bold().cyan());
    let current_include = if modified_config.include_patterns.is_empty() {
        "None (all projects included)".to_string()
    } else {
        modified_config.include_patterns.join(", ")
    };
    println!("   Current: {}", current_include.yellow());

    let use_include = Confirm::new("Do you want to limit sync to specific project patterns?")
        .with_default(!modified_config.include_patterns.is_empty())
        .with_help_message("Example: *work*, /home/user/important/*")
        .prompt()?;

    if use_include {
        let default = modified_config.include_patterns.join(", ");
        let input = Text::new("Enter include patterns (comma-separated):")
            .with_default(&default)
            .with_help_message("Glob patterns like '*work*' or '/specific/path'")
            .prompt()?;

        modified_config.include_patterns = input
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        println!("  {} Include patterns set: {:?}\n", "✓".green(), modified_config.include_patterns);
    } else {
        modified_config.include_patterns = Vec::new();
        println!("  {} All projects will be included\n", "✓".green());
    }

    // 3. Exclude patterns
    println!("{}", "3. Exclude Patterns".bold().cyan());
    let current_exclude = if modified_config.exclude_patterns.is_empty() {
        "None".to_string()
    } else {
        modified_config.exclude_patterns.join(", ")
    };
    println!("   Current: {}", current_exclude.yellow());

    let use_exclude = Confirm::new("Do you want to exclude specific project patterns?")
        .with_default(!modified_config.exclude_patterns.is_empty())
        .with_help_message("Example: *test*, *tmp*, /temp/*")
        .prompt()?;

    if use_exclude {
        let default = modified_config.exclude_patterns.join(", ");
        let input = Text::new("Enter exclude patterns (comma-separated):")
            .with_default(&default)
            .with_help_message("Glob patterns like '*test*' or '/tmp/*'")
            .prompt()?;

        modified_config.exclude_patterns = input
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        println!("  {} Exclude patterns set: {:?}\n", "✓".green(), modified_config.exclude_patterns);
    } else {
        modified_config.exclude_patterns = Vec::new();
        println!("  {} No exclusion patterns\n", "✓".green());
    }

    // 4. Exclude attachments
    println!("{}", "4. File Type Filter".bold().cyan());
    println!("   Current: {}",
        if modified_config.exclude_attachments { "Exclude attachments".yellow() }
        else { "Include all files".yellow() }
    );

    let exclude_attachments = Confirm::new("Exclude attachments (images, PDFs, etc.)?")
        .with_default(modified_config.exclude_attachments)
        .with_help_message("If yes, only .jsonl conversation files will be synced")
        .prompt()?;

    modified_config.exclude_attachments = exclude_attachments;
    println!("  {} Attachments will be {}\n",
        "✓".green(),
        if exclude_attachments { "excluded" } else { "included" }
    );

    // 5. Max file size
    println!("{}", "5. File Size Limit".bold().cyan());
    let current_mb = modified_config.max_file_size_bytes as f64 / (1024.0 * 1024.0);
    println!("   Current: {:.1} MB", current_mb);

    let change_size = Confirm::new("Do you want to change the maximum file size limit?")
        .with_default(false)
        .prompt()?;

    if change_size {
        let input = Text::new("Max file size (MB):")
            .with_default(&format!("{:.1}", current_mb))
            .prompt()?;

        let size_mb: f64 = input.trim().parse()
            .context("Invalid number. Must be a positive number.")?;

        modified_config.max_file_size_bytes = (size_mb * 1024.0 * 1024.0) as u64;
        println!("  {} Max file size set to {:.1} MB\n", "✓".green(), size_mb);
    } else {
        println!("  {} Keeping current max file size\n", "✓".green());
    }

    // Summary and confirmation
    println!("{}", "=".repeat(80).cyan());
    println!("{}", "Configuration Summary:".bold().cyan());
    println!("{}", "=".repeat(80).cyan());
    display_config_summary(&modified_config);
    println!();

    let confirm = Confirm::new("Save this configuration?")
        .with_default(true)
        .prompt()?;

    if confirm {
        modified_config.save().context("Failed to save configuration")?;
        println!("\n{} Configuration saved successfully!", "✓".green().bold());
    } else {
        println!("\n{}", "Configuration not saved.".yellow());
    }

    Ok(())
}

/// Display a compact configuration summary
fn display_config_summary(config: &FilterConfig) {
    println!("  {} {}",
        "Exclude older than:".cyan(),
        config.exclude_older_than_days
            .map(|d| format!("{} days", d))
            .unwrap_or_else(|| "Not set".dimmed().to_string())
    );

    println!("  {} {}",
        "Include patterns:".cyan(),
        if config.include_patterns.is_empty() {
            "None (all included)".dimmed().to_string()
        } else {
            config.include_patterns.join(", ")
        }
    );

    println!("  {} {}",
        "Exclude patterns:".cyan(),
        if config.exclude_patterns.is_empty() {
            "None".dimmed().to_string()
        } else {
            config.exclude_patterns.join(", ")
        }
    );

    println!("  {} {}",
        "Max file size:".cyan(),
        format!("{:.1} MB", config.max_file_size_bytes as f64 / (1024.0 * 1024.0))
    );

    println!("  {} {}",
        "Exclude attachments:".cyan(),
        if config.exclude_attachments {
            "Yes (only .jsonl files)".green().to_string()
        } else {
            "No (all files)".yellow().to_string()
        }
    );
}

/// Try to recover an existing repo if state.json is missing but repo exists
///
/// This handles the case where a user has a valid repo in the default location
/// but the state.json file is missing (e.g., from an older version or deletion).
fn try_recover_existing_repo() -> Result<Option<MultiRepoState>> {
    // Check if default repo directory exists and is a valid git repo
    let default_repo = match ConfigManager::default_repo_dir() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };

    if !default_repo.exists() || !scm::is_repo(&default_repo) {
        return Ok(None);
    }

    // Try to detect if it has a remote
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

    // Create the recovered state
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

    // Save the recovered state
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

    // Try to load state, but handle "not initialized" gracefully
    let mut state = match MultiRepoState::load() {
        Ok(s) => s,
        Err(e) => {
            let err_msg = e.to_string();
            if err_msg.contains("not initialized") || err_msg.contains("Run 'claude-code-sync init'") {
                // Check if there's an existing repo in the default location that we can recover
                if let Some(recovered) = try_recover_existing_repo()? {
                    println!("{}", "Found existing repository - recovered configuration!".green());
                    println!();
                    recovered
                } else {
                    println!("{}", "No repositories configured.".yellow());
                    println!();
                    println!("Run '{}' to set up your first repository.", "claude-code-sync init".cyan());
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
        println!("Run '{}' to set up your first repository.", "claude-code-sync init".cyan());
        return Ok(());
    }

    // Build sorted list of repos (active first, then alphabetical)
    let mut repo_entries: Vec<_> = state.repos.values().collect();
    repo_entries.sort_by(|a, b| {
        // Active repo first
        if a.name == state.active_repo {
            std::cmp::Ordering::Less
        } else if b.name == state.active_repo {
            std::cmp::Ordering::Greater
        } else {
            a.name.cmp(&b.name)
        }
    });

    // Build display options
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

            format!("{}{} - {}{}", repo.name, active_marker, path_str, remote_info)
        })
        .collect();

    // Add separator and management options
    options.push(format!("{}", "─── Actions ───".dimmed()));
    options.push("Configure filters (current repo)".to_string());
    options.push("Exit".to_string());

    let selection = Select::new("Select a repository to make active:", options.clone())
        .with_help_message("Use arrow keys to navigate, Enter to select")
        .prompt()
        .context("Failed to get user selection")?;

    // Handle selection
    if selection.contains("─── Actions ───") || selection == "Exit" {
        return Ok(());
    }

    if selection == "Configure filters (current repo)" {
        return handle_config_interactive();
    }

    // Extract repo name from selection (first word before space or marker)
    let repo_name = selection
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid selection"))?;

    // Check if this repo is already active
    if repo_name == state.active_repo {
        println!();
        println!(
            "{} '{}' is already the active repository.",
            "ℹ".blue(),
            repo_name.cyan()
        );
        return Ok(());
    }

    // Switch to selected repo
    if state.repos.contains_key(repo_name) {
        state.active_repo = repo_name.to_string();
        state.save()?;

        println!();
        println!(
            "{} Switched to repository '{}'",
            "✓".green().bold(),
            repo_name.cyan()
        );

        // Show repo details
        if let Some(repo) = state.repos.get(repo_name) {
            println!("  Path: {}", repo.sync_repo_path.display());
            if let Some(ref url) = repo.remote_url {
                println!("  Remote: {}", url);
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
