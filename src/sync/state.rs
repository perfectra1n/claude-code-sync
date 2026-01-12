use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Sync state and configuration
///
/// This struct stores the persistent state of the Claude Code sync system.
/// It tracks where the sync repository is located, whether it's configured
/// with a remote, and whether it was originally cloned from a remote URL.
///
/// The state is serialized to JSON and stored in the user's configuration
/// directory, allowing the sync system to remember its configuration across
/// multiple command invocations.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SyncState {
    /// Path to the local git repository used for syncing Claude Code conversations
    ///
    /// This is the directory where all conversation sessions are stored in git format.
    /// The conversations are organized under a `projects/` subdirectory within this path.
    /// This repository can be a local-only git repository or one that's synchronized
    /// with a remote origin.
    pub sync_repo_path: PathBuf,

    /// Whether the sync repository has a remote configured
    ///
    /// When `true`, the repository has a remote (typically named "origin") configured,
    /// allowing push and pull operations to synchronize with a remote git service
    /// (e.g., GitHub, GitLab). When `false`, the repository is local-only and cannot
    /// push to or pull from remote servers.
    pub has_remote: bool,

    /// Whether the repository was cloned from a remote URL
    ///
    /// This field distinguishes between repositories that were:
    /// - Cloned from an existing remote repository (`true`)
    /// - Initialized locally and optionally had a remote added later (`false`)
    ///
    /// This affects certain initialization behaviors, as cloned repositories
    /// may already have existing content and history.
    #[serde(default)]
    pub is_cloned_repo: bool,
}

impl SyncState {
    /// Loads the sync state from the user's configuration directory
    ///
    /// This function reads the persisted sync configuration from disk and deserializes
    /// it into a `SyncState` instance. The state file is stored in the user's Claude
    /// configuration directory and contains information about the sync repository location,
    /// remote configuration, and initialization method.
    ///
    /// # Returns
    ///
    /// Returns a `Result` containing the loaded `SyncState` on success.
    ///
    /// # Errors
    ///
    /// This function returns an error if:
    /// - The sync system has not been initialized (state file doesn't exist)
    /// - The state file cannot be read (permission errors, I/O errors)
    /// - The state file contains invalid JSON or cannot be deserialized
    ///
    /// If the sync is not initialized, the error message will instruct the user
    /// to run `claude-code-sync init` first.
    ///
    /// # Examples
    ///
    /// ```
    /// # use claude_code_sync::sync::SyncState;
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// match SyncState::load() {
    ///     Ok(state) => {
    ///         println!("Sync repo: {}", state.sync_repo_path.display());
    ///         println!("Has remote: {}", state.has_remote);
    ///     }
    ///     Err(e) => log::error!("Failed to load sync state: {}", e),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    /// Loads the sync state from the user's configuration directory.
    ///
    /// This is a compatibility wrapper that supports both v1 (SyncState) and v2 (MultiRepoState)
    /// formats. For v2 format, it returns the active repository's state.
    pub fn load() -> Result<Self> {
        let state_path = Self::state_file_path()?;

        if !state_path.exists() {
            return Err(anyhow!(
                "Sync not initialized. Run 'claude-code-sync init' first."
            ));
        }

        let content = fs::read_to_string(&state_path).context("Failed to read sync state")?;

        // Try v2 format first (MultiRepoState)
        if let Ok(multi_state) = serde_json::from_str::<MultiRepoState>(&content) {
            if multi_state.version >= 2 {
                // Get active repo and convert to SyncState
                if let Some(active) = multi_state.repos.get(&multi_state.active_repo) {
                    return Ok(SyncState {
                        sync_repo_path: active.sync_repo_path.clone(),
                        has_remote: active.has_remote,
                        is_cloned_repo: active.is_cloned_repo,
                    });
                } else {
                    return Err(anyhow!(
                        "Active repository '{}' not found in state",
                        multi_state.active_repo
                    ));
                }
            }
        }

        // Fall back to v1 format (direct SyncState)
        let state: SyncState =
            serde_json::from_str(&content).context("Failed to parse sync state")?;

        Ok(state)
    }

    pub(crate) fn save(&self) -> Result<()> {
        let state_path = Self::state_file_path()?;

        if let Some(parent) = state_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content =
            serde_json::to_string_pretty(self).context("Failed to serialize sync state")?;

        fs::write(&state_path, content).context("Failed to write sync state")?;

        Ok(())
    }

    fn state_file_path() -> Result<PathBuf> {
        crate::config::ConfigManager::state_file_path()
    }
}

/// Individual repository configuration
///
/// Represents a single sync repository with all its settings.
/// Multiple RepoConfig instances can be stored in a MultiRepoState.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RepoConfig {
    /// Unique identifier/name for this repo (e.g., "work", "personal", "default")
    pub name: String,

    /// Path to the local git repository for syncing Claude Code conversations
    pub sync_repo_path: PathBuf,

    /// Whether this repo has a remote configured
    pub has_remote: bool,

    /// Whether the repository was cloned from a remote URL
    #[serde(default)]
    pub is_cloned_repo: bool,

    /// Remote URL for display purposes (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,

    /// Description for the repo (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Multi-repo sync state (v2 format)
///
/// This struct stores multiple repository configurations and tracks
/// which one is currently active. It replaces the single-repo SyncState
/// for users who want to manage multiple sync repositories.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MultiRepoState {
    /// Schema version for future migrations (always 2 for this format)
    #[serde(default = "default_version")]
    pub version: u32,

    /// Name of the currently active repository
    pub active_repo: String,

    /// Map of repo name -> repo configuration
    pub repos: HashMap<String, RepoConfig>,
}

fn default_version() -> u32 {
    2
}

impl MultiRepoState {
    /// Get the active repository configuration
    pub fn active(&self) -> Option<&RepoConfig> {
        self.repos.get(&self.active_repo)
    }

    /// Get mutable reference to active repo
    pub fn active_mut(&mut self) -> Option<&mut RepoConfig> {
        self.repos.get_mut(&self.active_repo)
    }

    /// Load the multi-repo state, with automatic migration from v1 format
    pub fn load() -> Result<Self> {
        let state_path = SyncState::state_file_path()?;

        if !state_path.exists() {
            return Err(anyhow!(
                "Sync not initialized. Run 'claude-code-sync init' first."
            ));
        }

        let content = fs::read_to_string(&state_path).context("Failed to read sync state")?;

        // Try v2 format first (has "version" field)
        if let Ok(state) = serde_json::from_str::<MultiRepoState>(&content) {
            if state.version >= 2 {
                return Ok(state);
            }
        }

        // Fall back to v1 format and migrate
        let legacy: SyncState = serde_json::from_str(&content)
            .context("Failed to parse sync state (neither v1 nor v2 format)")?;

        let migrated = Self::migrate_from_v1(legacy)?;

        // Save the migrated state
        migrated.save()?;

        log::info!("Migrated state.json from v1 to v2 format");

        Ok(migrated)
    }

    /// Migrate from v1 SyncState to v2 MultiRepoState
    fn migrate_from_v1(legacy: SyncState) -> Result<Self> {
        let repo_name = "default".to_string();

        let repo_config = RepoConfig {
            name: repo_name.clone(),
            sync_repo_path: legacy.sync_repo_path,
            has_remote: legacy.has_remote,
            is_cloned_repo: legacy.is_cloned_repo,
            remote_url: None,
            description: Some("Migrated from single-repo configuration".to_string()),
        };

        let mut repos = HashMap::new();
        repos.insert(repo_name.clone(), repo_config);

        Ok(MultiRepoState {
            version: 2,
            active_repo: repo_name,
            repos,
        })
    }

    /// Save the multi-repo state to disk
    pub fn save(&self) -> Result<()> {
        let state_path = SyncState::state_file_path()?;

        if let Some(parent) = state_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content =
            serde_json::to_string_pretty(self).context("Failed to serialize multi-repo state")?;

        fs::write(&state_path, content).context("Failed to write multi-repo state")?;

        Ok(())
    }

    /// Check if a repo with the given name exists
    pub fn has_repo(&self, name: &str) -> bool {
        self.repos.contains_key(name)
    }

    /// Add a new repository
    pub fn add_repo(&mut self, config: RepoConfig) -> Result<()> {
        if self.repos.contains_key(&config.name) {
            return Err(anyhow!("Repository '{}' already exists", config.name));
        }
        self.repos.insert(config.name.clone(), config);
        Ok(())
    }

    /// Remove a repository by name
    pub fn remove_repo(&mut self, name: &str) -> Result<()> {
        if name == self.active_repo {
            return Err(anyhow!(
                "Cannot remove active repository '{}'. Switch to another repo first.",
                name
            ));
        }
        if self.repos.remove(name).is_none() {
            return Err(anyhow!("Repository '{}' not found", name));
        }
        Ok(())
    }

    /// Switch to a different active repository
    pub fn switch_active(&mut self, name: &str) -> Result<()> {
        if !self.repos.contains_key(name) {
            return Err(anyhow!("Repository '{}' not found", name));
        }
        self.active_repo = name.to_string();
        Ok(())
    }

    /// Get list of all repo names
    pub fn repo_names(&self) -> Vec<&String> {
        self.repos.keys().collect()
    }
}
