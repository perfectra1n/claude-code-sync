use anyhow::{anyhow, Context, Result};
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
    pub fn load() -> Result<Self> {
        let state_path = Self::state_file_path()?;

        if !state_path.exists() {
            return Err(anyhow!(
                "Sync not initialized. Run 'claude-code-sync init' first."
            ));
        }

        let content = fs::read_to_string(&state_path).context("Failed to read sync state")?;

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
