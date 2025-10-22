use anyhow::{Context, Result};
use std::path::PathBuf;

/// Cross-platform configuration directory manager
pub struct ConfigManager;

impl ConfigManager {
    /// Get the main configuration directory path following platform conventions:
    /// - Linux: $XDG_CONFIG_HOME/claude-sync or ~/.config/claude-sync
    /// - macOS: ~/Library/Application Support/claude-sync
    /// - Windows: %APPDATA%\claude-sync
    pub fn config_dir() -> Result<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            // Follow XDG Base Directory Specification
            if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
                Ok(PathBuf::from(xdg_config).join("claude-sync"))
            } else {
                let home = dirs::home_dir().context("Failed to get home directory")?;
                Ok(home.join(".config").join("claude-sync"))
            }
        }

        #[cfg(target_os = "macos")]
        {
            // Follow macOS conventions
            let home = dirs::home_dir().context("Failed to get home directory")?;
            Ok(home.join("Library").join("Application Support").join("claude-sync"))
        }

        #[cfg(target_os = "windows")]
        {
            // Use Windows APPDATA
            dirs::config_dir()
                .context("Failed to get Windows config directory")?
                .join("claude-sync")
                .into()
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            // Fallback for other platforms
            let home = dirs::home_dir().context("Failed to get home directory")?;
            Ok(home.join(".claude-sync"))
        }
    }

    /// Get the state file path (state.json)
    pub fn state_file_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("state.json"))
    }

    /// Get the filter config file path (config.toml)
    pub fn filter_config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    /// Get the operation history file path
    pub fn operation_history_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("operation-history.json"))
    }

    /// Get the snapshots directory path
    pub fn snapshots_dir() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("snapshots"))
    }

    /// Get the default repository clone directory
    pub fn default_repo_dir() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("repo"))
    }

    /// Get the latest conflict report path
    pub fn conflict_report_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("latest-conflict-report.json"))
    }

    /// Get the log file path
    pub fn log_file_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("claude-sync.log"))
    }

    /// Ensure the configuration directory exists
    pub fn ensure_config_dir() -> Result<PathBuf> {
        let config_dir = Self::config_dir()?;
        std::fs::create_dir_all(&config_dir)
            .with_context(|| format!("Failed to create config directory: {}", config_dir.display()))?;
        Ok(config_dir)
    }

    /// Ensure the snapshots directory exists
    pub fn ensure_snapshots_dir() -> Result<PathBuf> {
        let snapshots_dir = Self::snapshots_dir()?;
        std::fs::create_dir_all(&snapshots_dir)
            .with_context(|| format!("Failed to create snapshots directory: {}", snapshots_dir.display()))?;
        Ok(snapshots_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_paths() {
        // Just ensure they don't panic and return valid paths
        let config_dir = ConfigManager::config_dir().unwrap();
        assert!(config_dir.to_string_lossy().contains("claude-sync"));

        let state_path = ConfigManager::state_file_path().unwrap();
        assert!(state_path.to_string_lossy().contains("state.json"));

        let filter_path = ConfigManager::filter_config_path().unwrap();
        assert!(filter_path.to_string_lossy().contains("config.toml"));

        let history_path = ConfigManager::operation_history_path().unwrap();
        assert!(history_path.to_string_lossy().contains("operation-history.json"));

        let snapshots = ConfigManager::snapshots_dir().unwrap();
        assert!(snapshots.to_string_lossy().contains("snapshots"));

        let repo = ConfigManager::default_repo_dir().unwrap();
        assert!(repo.to_string_lossy().contains("repo"));

        let conflict = ConfigManager::conflict_report_path().unwrap();
        assert!(conflict.to_string_lossy().contains("latest-conflict-report.json"));

        let log = ConfigManager::log_file_path().unwrap();
        assert!(log.to_string_lossy().contains("claude-sync.log"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_xdg_config_home_respected() {
        // Set XDG_CONFIG_HOME and verify it's used
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/test-xdg-config");
        let config_dir = ConfigManager::config_dir().unwrap();
        assert!(config_dir.to_string_lossy().contains("/tmp/test-xdg-config/claude-sync"));
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_macos_library_path() {
        let config_dir = ConfigManager::config_dir().unwrap();
        assert!(config_dir.to_string_lossy().contains("Library/Application Support/claude-sync"));
    }
}
