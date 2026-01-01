//! Git SCM backend using CLI commands.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::Scm;

/// Git SCM implementation using the git CLI.
pub struct GitScm {
    workdir: PathBuf,
}

impl GitScm {
    /// Open an existing Git repository.
    pub fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        if !path.join(".git").exists() {
            return Err(anyhow!(
                "Not a git repository: '{}' (no .git directory)",
                path.display()
            ));
        }

        Ok(Self { workdir: path })
    }

    /// Initialize a new Git repository.
    pub fn init(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("Failed to create directory '{}'", path.display()))?;

        let output = Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .context("Failed to run 'git init'")?;

        if !output.status.success() {
            return Err(anyhow!(
                "git init failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        // Configure user name and email if not set
        let _ = Command::new("git")
            .args(["config", "user.name", "Claude Code Sync"])
            .current_dir(path)
            .output();
        let _ = Command::new("git")
            .args(["config", "user.email", "claude-code-sync@local"])
            .current_dir(path)
            .output();

        Self::open(path)
    }

    /// Clone a remote repository.
    pub fn clone(url: &str, path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directory for '{}'", path.display()))?;
        }

        let output = Command::new("git")
            .args(["clone", url, &path.to_string_lossy()])
            .output()
            .context("Failed to run 'git clone'")?;

        if !output.status.success() {
            return Err(anyhow!(
                "git clone failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        Self::open(path)
    }

    /// Run a git command and return stdout as a string.
    fn run_git(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.workdir)
            .output()
            .with_context(|| format!("Failed to run 'git {}'", args.join(" ")))?;

        if !output.status.success() {
            return Err(anyhow!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Run a git command, returning Ok if it succeeds (ignoring stdout).
    fn run_git_ok(&self, args: &[&str]) -> Result<()> {
        self.run_git(args)?;
        Ok(())
    }

    /// Check if a git command succeeds (exit code 0).
    fn git_succeeds(&self, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(&self.workdir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

impl Scm for GitScm {
    fn workdir(&self) -> &Path {
        &self.workdir
    }

    fn current_branch(&self) -> Result<String> {
        self.run_git(&["branch", "--show-current"])
    }

    fn current_commit_hash(&self) -> Result<String> {
        self.run_git(&["rev-parse", "HEAD"])
    }

    fn stage_all(&self) -> Result<()> {
        self.run_git_ok(&["add", "-A"])
    }

    fn commit(&self, message: &str) -> Result<()> {
        self.run_git_ok(&["commit", "-m", message])
    }

    fn has_changes(&self) -> Result<bool> {
        let output = self.run_git(&["status", "--porcelain"])?;
        Ok(!output.is_empty())
    }

    fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        self.run_git_ok(&["remote", "add", name, url])
    }

    fn has_remote(&self, name: &str) -> bool {
        self.git_succeeds(&["remote", "get-url", name])
    }

    fn get_remote_url(&self, name: &str) -> Result<String> {
        self.run_git(&["remote", "get-url", name])
    }

    fn set_remote_url(&self, name: &str, url: &str) -> Result<()> {
        self.run_git_ok(&["remote", "set-url", name, url])
    }

    fn remove_remote(&self, name: &str) -> Result<()> {
        self.run_git_ok(&["remote", "remove", name])
    }

    fn list_remotes(&self) -> Result<Vec<String>> {
        let output = self.run_git(&["remote"])?;
        if output.is_empty() {
            Ok(Vec::new())
        } else {
            Ok(output.lines().map(|s| s.to_string()).collect())
        }
    }

    fn push(&self, remote: &str, branch: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["push", remote, branch])
            .current_dir(&self.workdir)
            .output()
            .context("Failed to run 'git push'")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "Failed to push to remote '{}': {}\n\n\
                Possible causes:\n\
                1. Authentication failed - ensure credentials are configured\n\
                2. No permission to push to this repository\n\
                3. Network connectivity issues\n\
                4. Remote branch protection rules\n\n\
                For HTTPS: Run 'git config --global credential.helper store' and try again\n\
                For SSH: Ensure SSH keys are set up with 'ssh -T git@github.com'",
                remote, stderr
            ));
        }

        Ok(())
    }

    fn fetch(&self, remote: &str) -> Result<()> {
        self.run_git_ok(&["fetch", remote])
    }

    fn pull(&self, remote: &str, branch: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["pull", remote, branch])
            .current_dir(&self.workdir)
            .output()
            .context("Failed to run 'git pull'")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "Failed to pull from remote '{}': {}",
                remote, stderr
            ));
        }

        Ok(())
    }

    fn reset_soft(&self, commit: &str) -> Result<()> {
        self.run_git_ok(&["reset", "--soft", commit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_git_init_and_open() {
        let temp = TempDir::new().unwrap();
        let scm = GitScm::init(temp.path()).unwrap();

        assert!(temp.path().join(".git").exists());
        assert_eq!(scm.workdir(), temp.path().canonicalize().unwrap());
    }

    #[test]
    fn test_git_stage_commit() {
        let temp = TempDir::new().unwrap();
        let scm = GitScm::init(temp.path()).unwrap();

        // Initially no changes
        assert!(!scm.has_changes().unwrap());

        // Create a file
        std::fs::write(temp.path().join("test.txt"), "hello").unwrap();
        assert!(scm.has_changes().unwrap());

        // Stage and commit
        scm.stage_all().unwrap();
        scm.commit("Initial commit").unwrap();
        assert!(!scm.has_changes().unwrap());

        // Verify commit hash
        let hash = scm.current_commit_hash().unwrap();
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 40); // Full SHA
    }

    #[test]
    fn test_git_branch() {
        let temp = TempDir::new().unwrap();
        let scm = GitScm::init(temp.path()).unwrap();

        // Create initial commit (needed for branch to exist)
        std::fs::write(temp.path().join("test.txt"), "hello").unwrap();
        scm.stage_all().unwrap();
        scm.commit("Initial commit").unwrap();

        // Check branch (default is master or main depending on git config)
        let branch = scm.current_branch().unwrap();
        assert!(!branch.is_empty());
    }

    #[test]
    fn test_git_remote() {
        let temp = TempDir::new().unwrap();
        let scm = GitScm::init(temp.path()).unwrap();

        assert!(!scm.has_remote("origin"));

        scm.add_remote("origin", "https://github.com/test/repo.git").unwrap();
        assert!(scm.has_remote("origin"));
        assert!(!scm.has_remote("upstream"));
    }
}
