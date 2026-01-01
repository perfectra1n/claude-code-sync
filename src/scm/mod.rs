//! SCM (Source Control Management) abstraction layer.
//!
//! Provides a unified interface for Git using CLI commands.
//! Designed to support multiple backends (Git, Mercurial) via the `Backend` enum.

mod git;
pub mod lfs;

use anyhow::{anyhow, Result};
use std::path::Path;

pub use git::GitScm;

/// SCM backend types.
///
/// Used for parameterized testing and explicit backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Git version control
    Git,
    // Mercurial,  // Future: uncomment when hg.rs is added
}

impl Backend {
    /// Check if this backend's binary is available on the system.
    pub fn is_available(&self) -> bool {
        let binary = match self {
            Backend::Git => "git",
        };
        std::process::Command::new(binary)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Get the marker directory for this backend (.git, .hg, etc).
    pub fn marker(&self) -> &'static str {
        match self {
            Backend::Git => ".git",
        }
    }
}

/// Trait for source control management operations.
pub trait Scm: Send + Sync {
    /// Get the current branch name.
    fn current_branch(&self) -> Result<String>;

    /// Get the current commit hash.
    fn current_commit_hash(&self) -> Result<String>;

    /// Stage all changes (add and remove).
    fn stage_all(&self) -> Result<()>;

    /// Commit staged changes with a message.
    fn commit(&self, message: &str) -> Result<()>;

    /// Check if there are uncommitted changes.
    fn has_changes(&self) -> Result<bool>;

    /// Add a remote repository.
    fn add_remote(&self, name: &str, url: &str) -> Result<()>;

    /// Check if a remote exists.
    fn has_remote(&self, name: &str) -> bool;

    /// Get the URL for a remote.
    fn get_remote_url(&self, name: &str) -> Result<String>;

    /// Set or update the URL for a remote.
    fn set_remote_url(&self, name: &str, url: &str) -> Result<()>;

    /// Remove a remote.
    fn remove_remote(&self, name: &str) -> Result<()>;

    /// List all remote names.
    fn list_remotes(&self) -> Result<Vec<String>>;

    /// Push to a remote repository.
    fn push(&self, remote: &str, branch: &str) -> Result<()>;

    /// Pull from a remote repository (fetch + merge/update).
    fn pull(&self, remote: &str, branch: &str) -> Result<()>;

    /// Reset to a specific commit (soft reset - keeps working directory).
    fn reset_soft(&self, commit: &str) -> Result<()>;
}

/// Check if a directory is a Git repository.
pub fn is_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

/// Open an existing Git repository.
pub fn open(path: &Path) -> Result<Box<dyn Scm>> {
    if is_repo(path) {
        Ok(Box::new(GitScm::open(path)?))
    } else {
        Err(anyhow!(
            "No Git repository found at '{}'. Expected .git directory.",
            path.display()
        ))
    }
}

/// Initialize a new Git repository.
pub fn init(path: &Path) -> Result<Box<dyn Scm>> {
    Ok(Box::new(GitScm::init(path)?))
}

/// Clone a repository from a URL.
pub fn clone(url: &str, path: &Path) -> Result<Box<dyn Scm>> {
    Ok(Box::new(GitScm::clone(url, path)?))
}

/// Initialize a new repository with the specified backend.
///
/// This is useful for parameterized testing where you want to test
/// the same operations against different SCM backends.
pub fn init_with_backend(path: &Path, backend: Backend) -> Result<Box<dyn Scm>> {
    match backend {
        Backend::Git => Ok(Box::new(GitScm::init(path)?)),
    }
}

/// Detect which backend a repository uses.
///
/// Returns `None` if the path is not a repository.
pub fn detect_backend(path: &Path) -> Option<Backend> {
    if path.join(".git").exists() {
        Some(Backend::Git)
    // } else if path.join(".hg").exists() {
    //     Some(Backend::Mercurial)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_is_repo() {
        let temp = TempDir::new().unwrap();
        assert!(!is_repo(temp.path()));

        std::fs::create_dir(temp.path().join(".git")).unwrap();
        assert!(is_repo(temp.path()));
    }

    #[test]
    fn test_open_non_repo_fails() {
        let temp = TempDir::new().unwrap();
        assert!(open(temp.path()).is_err());
    }
}
