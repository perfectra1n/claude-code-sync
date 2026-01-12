use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Confirm, Select, Text};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::ConfigManager;

/// Configuration file for non-interactive initialization.
///
/// This struct can be loaded from a TOML file to initialize claude-code-sync
/// without requiring interactive prompts. Useful for automation, CI/CD, and
/// headless environments.
///
/// # Example TOML file
///
/// ```toml
/// # Required: Path to the local git repository
/// repo_path = "~/claude-history-sync"
///
/// # Optional: Remote git URL for syncing
/// remote_url = "https://github.com/user/claude-history.git"
///
/// # Optional: Clone from remote (default: false)
/// # Set to true if the repo doesn't exist locally and should be cloned
/// clone = true
///
/// # Optional: Exclude file attachments (default: false)
/// exclude_attachments = true
///
/// # Optional: Exclude conversations older than N days
/// exclude_older_than_days = 30
///
/// # Optional: Enable Git LFS for large files (default: false)
/// enable_lfs = true
///
/// # Optional: SCM backend - "git" or "mercurial" (default: "git")
/// scm_backend = "git"
///
/// # Optional: Subdirectory for storing projects (default: "projects")
/// sync_subdirectory = "claude-history"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitConfig {
    /// Path to the local git repository for storing conversation history.
    pub repo_path: String,

    /// Optional remote git repository URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,

    /// Whether to clone from the remote URL (default: false).
    #[serde(default)]
    pub clone: bool,

    /// Whether to exclude file attachments (default: false).
    #[serde(default)]
    pub exclude_attachments: bool,

    /// Exclude conversations older than N days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_older_than_days: Option<u32>,

    /// Enable Git LFS for large files (default: false).
    #[serde(default)]
    pub enable_lfs: bool,

    /// SCM backend: "git" or "mercurial" (default: "git").
    #[serde(default = "default_scm_backend")]
    pub scm_backend: String,

    /// Subdirectory within sync repo for storing projects (default: "projects").
    #[serde(default = "default_sync_subdirectory")]
    pub sync_subdirectory: String,
}

fn default_scm_backend() -> String {
    "git".to_string()
}

fn default_sync_subdirectory() -> String {
    "projects".to_string()
}

impl InitConfig {
    /// Load configuration from a TOML file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read config file: {}", path.as_ref().display()))?;

        let config: InitConfig =
            toml::from_str(&content).context("Failed to parse init config file")?;

        // Validate the config
        config.validate()?;

        Ok(config)
    }

    /// Load configuration from the default location.
    ///
    /// Checks the following locations in order:
    /// 1. `CLAUDE_CODE_SYNC_INIT_CONFIG` environment variable
    /// 2. `~/.claude-code-sync-init.toml`
    /// 3. Config directory: `init.toml`
    pub fn load_default() -> Result<Option<Self>> {
        // Check environment variable first
        if let Ok(path) = std::env::var("CLAUDE_CODE_SYNC_INIT_CONFIG") {
            let path = PathBuf::from(&path);
            if path.exists() {
                log::info!("Loading init config from CLAUDE_CODE_SYNC_INIT_CONFIG: {}", path.display());
                return Ok(Some(Self::load(&path)?));
            }
        }

        // Check ~/.claude-code-sync-init.toml
        if let Some(home) = dirs::home_dir() {
            let home_config = home.join(".claude-code-sync-init.toml");
            if home_config.exists() {
                log::info!("Loading init config from: {}", home_config.display());
                return Ok(Some(Self::load(&home_config)?));
            }
        }

        // Check config directory
        if let Ok(config_dir) = ConfigManager::config_dir() {
            let config_path = config_dir.join("init.toml");
            if config_path.exists() {
                log::info!("Loading init config from: {}", config_path.display());
                return Ok(Some(Self::load(&config_path)?));
            }
        }

        Ok(None)
    }

    /// Validate the configuration.
    fn validate(&self) -> Result<()> {
        // Validate remote URL if provided
        if let Some(ref url) = self.remote_url {
            if !is_valid_git_url(url) {
                return Err(anyhow::anyhow!(
                    "Invalid git URL '{}'. Must start with 'https://', 'http://', 'git@', or 'ssh://'",
                    url
                ));
            }
        }

        // If clone is true, remote_url must be provided
        if self.clone && self.remote_url.is_none() {
            return Err(anyhow::anyhow!(
                "clone = true requires remote_url to be set"
            ));
        }

        // Validate SCM backend
        let backend = self.scm_backend.to_lowercase();
        if backend != "git" && backend != "mercurial" && backend != "hg" {
            return Err(anyhow::anyhow!(
                "Invalid scm_backend '{}'. Use 'git' or 'mercurial'.",
                self.scm_backend
            ));
        }

        // LFS only works with git
        if self.enable_lfs && backend != "git" {
            return Err(anyhow::anyhow!(
                "enable_lfs = true requires scm_backend = 'git'"
            ));
        }

        Ok(())
    }

    /// Convert to OnboardingConfig for use with existing initialization flow.
    pub fn to_onboarding_config(&self) -> Result<OnboardingConfig> {
        let repo_path = expand_tilde(&self.repo_path)?;

        Ok(OnboardingConfig {
            repo_path,
            remote_url: self.remote_url.clone(),
            is_cloned: self.clone,
            exclude_attachments: self.exclude_attachments,
            exclude_older_than_days: self.exclude_older_than_days,
        })
    }
}

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
                    .unwrap_or_else(|_| "~/.claude-code-sync/repo/".to_string());
                write!(f, "Default ({default_path})")
            }
            CloneLocation::Custom => write!(f, "Custom path"),
        }
    }
}

/// Onboarding configuration result
///
/// Contains all configuration settings gathered during the interactive onboarding process.
/// This configuration determines how claude-code-sync will manage and sync Claude Code conversations.
#[derive(Debug)]
pub struct OnboardingConfig {
    /// Path to the local git repository for storing conversation history.
    ///
    /// This can be either:
    /// - A directory where a remote repository will be cloned
    /// - A path to an existing local git repository
    /// - A path where a new git repository will be initialized
    pub repo_path: PathBuf,

    /// Optional remote git repository URL for syncing conversations.
    ///
    /// If present, this URL will be used to:
    /// - Clone the repository (when `is_cloned` is true)
    /// - Configure the remote origin for pushing/pulling changes
    ///
    /// Supported URL formats:
    /// - HTTPS: `https://github.com/user/repo.git`
    /// - SSH: `git@github.com:user/repo.git`
    /// - SSH protocol: `ssh://git@github.com/user/repo.git`
    ///
    /// None indicates a local-only repository with no remote configured.
    pub remote_url: Option<String>,

    /// Indicates whether the repository should be cloned from the remote URL.
    ///
    /// - `true`: The repository will be cloned from `remote_url` to `repo_path`
    /// - `false`: Use an existing local repository at `repo_path`, or initialize a new one
    pub is_cloned: bool,

    /// Whether to exclude file attachments when syncing conversations.
    ///
    /// - `true`: Only sync `.jsonl` conversation files, excluding images, PDFs, and other attachments
    /// - `false`: Sync all conversation data including attachments
    ///
    /// Excluding attachments reduces storage size and sync time while preserving conversation text.
    pub exclude_attachments: bool,

    /// Optional threshold (in days) for excluding old conversations from sync.
    ///
    /// If set, only conversations modified within the last N days will be synced.
    /// Conversations older than this threshold will be excluded.
    ///
    /// - `Some(30)`: Only sync conversations modified in the last 30 days
    /// - `None`: Sync all conversations regardless of age
    pub exclude_older_than_days: Option<u32>,
}

/// Run the interactive onboarding flow
pub fn run_onboarding() -> Result<OnboardingConfig> {
    println!(
        "\n{}",
        "⚙️  First time setup detected. Let's configure claude-code-sync!"
            .cyan()
            .bold()
    );
    println!();

    // Step 1: Ask for repository type
    let repo_type = Select::new("Repository type:", vec![RepoType::Remote, RepoType::Local])
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
                        .with_placeholder("~/Documents/claude-code-sync-repo")
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
                .with_placeholder("~/claude-code-sync-repo")
                .with_help_message(
                    "Path to a new or existing git repository on your local filesystem",
                )
                .prompt()
                .context("Failed to get local path")?;

            let path = expand_tilde(&path_str)?;

            // Ask if they want to add a remote later
            let add_remote =
                Confirm::new("Do you want to add a remote repository for backup/sync?")
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

        Some(days_str.parse::<u32>().context("Invalid number of days")?)
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

    #[test]
    fn test_init_config_parse_minimal() {
        let toml = r#"
            repo_path = "/tmp/test-repo"
        "#;
        let config: InitConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.repo_path, "/tmp/test-repo");
        assert!(config.remote_url.is_none());
        assert!(!config.clone);
        assert!(!config.exclude_attachments);
        assert!(!config.enable_lfs);
        assert_eq!(config.scm_backend, "git");
        assert_eq!(config.sync_subdirectory, "projects");
    }

    #[test]
    fn test_init_config_parse_full() {
        let toml = r#"
            repo_path = "~/claude-sync"
            remote_url = "https://github.com/user/repo.git"
            clone = true
            exclude_attachments = true
            exclude_older_than_days = 30
            enable_lfs = true
            scm_backend = "git"
            sync_subdirectory = "history"
        "#;
        let config: InitConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.repo_path, "~/claude-sync");
        assert_eq!(config.remote_url, Some("https://github.com/user/repo.git".to_string()));
        assert!(config.clone);
        assert!(config.exclude_attachments);
        assert_eq!(config.exclude_older_than_days, Some(30));
        assert!(config.enable_lfs);
        assert_eq!(config.scm_backend, "git");
        assert_eq!(config.sync_subdirectory, "history");
    }

    #[test]
    fn test_init_config_validate_clone_requires_remote() {
        let config = InitConfig {
            repo_path: "/tmp/test".to_string(),
            remote_url: None,
            clone: true,
            exclude_attachments: false,
            exclude_older_than_days: None,
            enable_lfs: false,
            scm_backend: "git".to_string(),
            sync_subdirectory: "projects".to_string(),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_init_config_validate_lfs_requires_git() {
        let config = InitConfig {
            repo_path: "/tmp/test".to_string(),
            remote_url: None,
            clone: false,
            exclude_attachments: false,
            exclude_older_than_days: None,
            enable_lfs: true,
            scm_backend: "mercurial".to_string(),
            sync_subdirectory: "projects".to_string(),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_init_config_validate_invalid_backend() {
        let config = InitConfig {
            repo_path: "/tmp/test".to_string(),
            remote_url: None,
            clone: false,
            exclude_attachments: false,
            exclude_older_than_days: None,
            enable_lfs: false,
            scm_backend: "svn".to_string(),
            sync_subdirectory: "projects".to_string(),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_init_config_to_onboarding_config() {
        let config = InitConfig {
            repo_path: "/tmp/test".to_string(),
            remote_url: Some("https://github.com/user/repo.git".to_string()),
            clone: true,
            exclude_attachments: true,
            exclude_older_than_days: Some(30),
            enable_lfs: true,
            scm_backend: "git".to_string(),
            sync_subdirectory: "projects".to_string(),
        };
        let onboarding = config.to_onboarding_config().unwrap();
        assert_eq!(onboarding.repo_path, PathBuf::from("/tmp/test"));
        assert_eq!(onboarding.remote_url, Some("https://github.com/user/repo.git".to_string()));
        assert!(onboarding.is_cloned);
        assert!(onboarding.exclude_attachments);
        assert_eq!(onboarding.exclude_older_than_days, Some(30));
    }
}
