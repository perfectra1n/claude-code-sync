use anyhow::{Context, Result};
use git2::Repository;
use std::path::{Path, PathBuf};

use super::credentials;

/// Git repository manager for Claude Code history
pub struct GitManager {
    pub(super) repo: Repository,
}

impl GitManager {
    /// Open an existing repository
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let repo = Repository::open(path.as_ref()).with_context(|| {
            format!(
                "Failed to open git repository at {}",
                path.as_ref().display()
            )
        })?;
        Ok(GitManager { repo })
    }

    /// Initialize a new repository
    pub fn init<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        std::fs::create_dir_all(path)
            .with_context(|| format!("Failed to create directory: {}", path.display()))?;

        let repo = Repository::init(path).with_context(|| {
            format!("Failed to initialize git repository at {}", path.display())
        })?;

        Ok(GitManager { repo })
    }

    /// Clone a remote repository
    pub fn clone<P: AsRef<Path>>(url: &str, path: P) -> Result<Self> {
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create parent directory: {}", parent.display())
            })?;
        }

        // Set up fetch options with credential helper
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(credentials::credential_callback);

        let mut fetch_options = git2::FetchOptions::new();
        fetch_options.remote_callbacks(callbacks);

        let mut builder = git2::build::RepoBuilder::new();
        builder.fetch_options(fetch_options);

        // Perform the clone
        let repo = builder.clone(url, path).with_context(|| {
            format!(
                "Failed to clone repository from '{}' to '{}'.\n\
                \n\
                Possible causes:\n\
                1. Authentication failed - ensure credentials are configured\n\
                2. Invalid repository URL\n\
                3. Network connectivity issues\n\
                4. No permission to access this repository\n\
                \n\
                For HTTPS: Run 'git config --global credential.helper store' first\n\
                For SSH: Ensure SSH keys are set up with 'ssh -T git@github.com'",
                url,
                path.display()
            )
        })?;

        Ok(GitManager { repo })
    }

    /// Get the repository path
    #[cfg(test)]
    pub fn path(&self) -> PathBuf {
        self.repo
            .workdir()
            .unwrap_or_else(|| self.repo.path())
            .to_path_buf()
    }

    /// Check if a remote exists
    pub fn has_remote(&self, name: &str) -> bool {
        self.repo.find_remote(name).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_init_creates_new_repository() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Verify the repository path exists
        assert!(git_manager.path().exists());

        // Verify .git directory was created
        let git_dir = temp_dir.path().join(".git");
        assert!(git_dir.exists());
        assert!(git_dir.is_dir());
    }

    #[test]
    fn test_init_creates_parent_directories() {
        let temp_dir = TempDir::new().unwrap();
        let nested_path = temp_dir.path().join("parent").join("child").join("repo");

        let git_manager = GitManager::init(&nested_path).unwrap();

        assert!(git_manager.path().exists());
        assert!(nested_path.join(".git").exists());
    }

    #[test]
    fn test_init_can_be_called_multiple_times_on_same_path() {
        let temp_dir = TempDir::new().unwrap();

        // Initialize repository twice
        let git_manager1 = GitManager::init(temp_dir.path()).unwrap();
        let git_manager2 = GitManager::init(temp_dir.path()).unwrap();

        // Both should succeed and reference the same path
        assert_eq!(git_manager1.path(), git_manager2.path());
    }

    #[test]
    fn test_open_existing_repository() {
        let temp_dir = TempDir::new().unwrap();
        GitManager::init(temp_dir.path()).unwrap();

        // Open the same repository
        let git_manager = GitManager::open(temp_dir.path()).unwrap();
        assert_eq!(git_manager.path(), temp_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_open_nonexistent_repository_fails() {
        let temp_dir = TempDir::new().unwrap();
        let nonexistent = temp_dir.path().join("nonexistent");

        let result = GitManager::open(&nonexistent);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("Failed to open git repository"));
        }
    }

    #[test]
    fn test_open_non_git_directory_fails() {
        let temp_dir = TempDir::new().unwrap();
        fs::create_dir_all(temp_dir.path()).unwrap();

        // Directory exists but is not a git repo
        let result = GitManager::open(temp_dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_path_returns_working_directory() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        let path = git_manager.path();
        assert_eq!(path, temp_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_has_remote_returns_false_when_no_remotes() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        assert!(!git_manager.has_remote("origin"));
        assert!(!git_manager.has_remote("upstream"));
    }
}
