use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Confirm, Select, Text};
use std::path::PathBuf;

use crate::config::ConfigManager;

/// Repository type for onboarding
#[derive(Debug, Clone)]
enum RepoType {
    Remote,
    Local,
}

impl std::fmt::Display for RepoType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RepoType::Remote => write!(f, "Remote Git Repository (clone from GitHub/GitLab/etc.)"),
            RepoType::Local => write!(f, "Local Directory (new or existing)"),
        }
    }
}

/// Clone location option
#[derive(Debug, Clone)]
enum CloneLocation {
    Default,
    Custom,
}

impl std::fmt::Display for CloneLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloneLocation::Default => {
                let default_path = ConfigManager::default_repo_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "~/.claude-sync/repo/".to_string());
                write!(f, "Default ({})", default_path)
            }
            CloneLocation::Custom => write!(f, "Custom path"),
        }
    }
}

/// Onboarding configuration result
#[derive(Debug)]
pub struct OnboardingConfig {
    pub repo_path: PathBuf,
    pub remote_url: Option<String>,
    pub is_cloned: bool,
    pub exclude_attachments: bool,
    pub exclude_older_than_days: Option<u32>,
}

/// Run the interactive onboarding flow
pub fn run_onboarding() -> Result<OnboardingConfig> {
    println!("\n{}", "⚙️  First time setup detected. Let's configure claude-sync!".cyan().bold());
    println!();

    // Step 1: Ask for repository type
    let repo_type = Select::new(
        "Repository type:",
        vec![RepoType::Remote, RepoType::Local],
    )
    .prompt()
    .context("Failed to get repository type")?;

    let (repo_path, remote_url, is_cloned) = match repo_type {
        RepoType::Remote => {
            // Get remote URL
            let url = Text::new("Enter remote repository URL:")
                .with_placeholder("git@github.com:user/claude-history.git or https://github.com/user/claude-history.git")
                .with_help_message("The git repository URL for syncing your Claude Code conversations")
                .prompt()
                .context("Failed to get remote URL")?;

            // Validate URL format
            if !is_valid_git_url(&url) {
                return Err(anyhow::anyhow!(
                    "Invalid git URL. Must start with 'https://', 'http://', or 'git@'"
                ));
            }

            // Ask for clone location
            let clone_loc = Select::new(
                "Clone location:",
                vec![CloneLocation::Default, CloneLocation::Custom],
            )
            .prompt()
            .context("Failed to get clone location")?;

            let path = match clone_loc {
                CloneLocation::Default => ConfigManager::default_repo_dir()?,
                CloneLocation::Custom => {
                    let custom_path = Text::new("Enter custom clone path:")
                        .with_placeholder("~/Documents/claude-sync-repo")
                        .prompt()
                        .context("Failed to get custom path")?;

                    // Expand tilde if present
                    expand_tilde(&custom_path)?
                }
            };

            (path, Some(url), true)
        }
        RepoType::Local => {
            let path_str = Text::new("Enter local repository path:")
                .with_placeholder("~/claude-sync-repo")
                .with_help_message("Path to a new or existing git repository on your local filesystem")
                .prompt()
                .context("Failed to get local path")?;

            let path = expand_tilde(&path_str)?;

            // Ask if they want to add a remote later
            let add_remote = Confirm::new("Do you want to add a remote repository for backup/sync?")
                .with_default(false)
                .prompt()
                .context("Failed to get remote preference")?;

            let remote = if add_remote {
                let url = Text::new("Enter remote repository URL:")
                    .with_placeholder("git@github.com:user/claude-history.git")
                    .prompt()
                    .context("Failed to get remote URL")?;

                if !is_valid_git_url(&url) {
                    return Err(anyhow::anyhow!(
                        "Invalid git URL. Must start with 'https://', 'http://', or 'git@'"
                    ));
                }

                Some(url)
            } else {
                None
            };

            (path, remote, false)
        }
    };

    println!();

    // Step 2: Filter preferences
    let exclude_attachments = Confirm::new("Exclude file attachments (images, PDFs, etc.)?")
        .with_default(true)
        .with_help_message("Only sync .jsonl conversation files, excluding any attached files")
        .prompt()
        .context("Failed to get attachment preference")?;

    let exclude_old = Confirm::new("Exclude old conversations?")
        .with_default(false)
        .with_help_message("Only sync conversations modified within a certain time period")
        .prompt()
        .context("Failed to get old conversation preference")?;

    let exclude_older_than_days = if exclude_old {
        let days_str = Text::new("Exclude conversations older than (days):")
            .with_default("30")
            .with_help_message("Conversations not modified in this many days will be excluded")
            .prompt()
            .context("Failed to get days threshold")?;

        Some(
            days_str
                .parse::<u32>()
                .context("Invalid number of days")?,
        )
    } else {
        None
    };

    println!();
    println!("{}", "✓ Configuration complete!".green().bold());

    Ok(OnboardingConfig {
        repo_path,
        remote_url,
        is_cloned,
        exclude_attachments,
        exclude_older_than_days,
    })
}

/// Validate git URL format
fn is_valid_git_url(url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("git@")
        || url.starts_with("ssh://")
}

/// Expand tilde in path
fn expand_tilde(path: &str) -> Result<PathBuf> {
    if path.starts_with("~/") || path == "~" {
        let home = dirs::home_dir().context("Failed to get home directory")?;
        if path == "~" {
            Ok(home)
        } else {
            Ok(home.join(&path[2..]))
        }
    } else {
        Ok(PathBuf::from(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_git_url() {
        assert!(is_valid_git_url("https://github.com/user/repo.git"));
        assert!(is_valid_git_url("http://gitlab.com/user/repo.git"));
        assert!(is_valid_git_url("git@github.com:user/repo.git"));
        assert!(is_valid_git_url("ssh://git@github.com/user/repo.git"));
        assert!(!is_valid_git_url("invalid-url"));
        assert!(!is_valid_git_url("/local/path"));
    }

    #[test]
    fn test_expand_tilde() {
        let home = dirs::home_dir().unwrap();

        // Test tilde expansion
        let expanded = expand_tilde("~/test").unwrap();
        assert_eq!(expanded, home.join("test"));

        // Test just tilde
        let expanded = expand_tilde("~").unwrap();
        assert_eq!(expanded, home);

        // Test non-tilde path
        let expanded = expand_tilde("/absolute/path").unwrap();
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }
}
