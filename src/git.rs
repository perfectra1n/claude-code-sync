use anyhow::{anyhow, Context, Result};
use git2::{IndexAddOption, Repository, Signature};
use std::path::{Path, PathBuf};

/// Git repository manager for Claude Code history
pub struct GitManager {
    repo: Repository,
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
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directory: {}", parent.display()))?;
        }

        // Set up fetch options with credential helper
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(|_url, username_from_url, _allowed_types| {
            // Try credential helper first
            git2::Cred::credential_helper(&git2::Config::open_default()?, _url, username_from_url)
                // Fall back to SSH agent if credential helper fails
                .or_else(|_| git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git")))
        });

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

    /// Add a remote to the repository
    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        self.repo
            .remote(name, url)
            .with_context(|| format!("Failed to add remote '{}' with URL '{}'", name, url))?;
        Ok(())
    }

    /// Get the repository path
    pub fn path(&self) -> PathBuf {
        self.repo
            .workdir()
            .unwrap_or_else(|| self.repo.path())
            .to_path_buf()
    }

    /// Stage all changes in the working directory
    pub fn stage_all(&self) -> Result<()> {
        let mut index = self
            .repo
            .index()
            .context("Failed to get repository index")?;

        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .context("Failed to stage files")?;

        index.write().context("Failed to write index")?;

        Ok(())
    }

    /// Create a commit with all staged changes
    pub fn commit(&self, message: &str) -> Result<()> {
        let mut index = self
            .repo
            .index()
            .context("Failed to get repository index")?;

        let tree_oid = index.write_tree().context("Failed to write tree")?;

        let tree = self
            .repo
            .find_tree(tree_oid)
            .context("Failed to find tree")?;

        let signature = Signature::now("claude-sync", "noreply@claude-sync.local")
            .context("Failed to create signature")?;

        let parent_commit = match self.repo.head() {
            Ok(head) => {
                let oid = head.target().context("Failed to get HEAD target")?;
                Some(
                    self.repo
                        .find_commit(oid)
                        .context("Failed to find parent commit")?,
                )
            }
            Err(_) => None, // First commit
        };

        let parents: Vec<_> = parent_commit.iter().collect();

        self.repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &parents,
            )
            .context("Failed to create commit")?;

        Ok(())
    }

    /// Push to remote
    pub fn push(&self, remote_name: &str, branch_name: &str) -> Result<()> {
        let mut remote = self
            .repo
            .find_remote(remote_name)
            .with_context(|| format!("Failed to find remote '{}'", remote_name))?;

        let refspec = format!("refs/heads/{}:refs/heads/{}", branch_name, branch_name);

        // Set up callbacks for authentication
        let mut callbacks = git2::RemoteCallbacks::new();

        // Use git credential helper for authentication
        callbacks.credentials(|_url, username_from_url, _allowed_types| {
            git2::Cred::credential_helper(&self.repo.config()?, _url, username_from_url)
        });

        // Set up push options with callbacks
        let mut push_options = git2::PushOptions::new();
        push_options.remote_callbacks(callbacks);

        // Attempt push with detailed error handling
        match remote.push(&[&refspec], Some(&mut push_options)) {
            Ok(_) => Ok(()),
            Err(e) => {
                let remote_url = remote.url().unwrap_or("unknown");
                Err(anyhow!(
                    "Failed to push to remote '{}' at '{}': {}\n\
                    \n\
                    Possible causes:\n\
                    1. Authentication failed - ensure credentials are configured\n\
                    2. No permission to push to this repository\n\
                    3. Network connectivity issues\n\
                    4. Remote branch protection rules\n\
                    \n\
                    For HTTPS: Run 'git config --global credential.helper store' and try again\n\
                    For SSH: Ensure SSH keys are set up with 'ssh -T git@github.com'",
                    remote_name,
                    remote_url,
                    e
                ))
            }
        }
    }

    /// Fetch from remote
    pub fn fetch(&self, remote_name: &str) -> Result<()> {
        let mut remote = self
            .repo
            .find_remote(remote_name)
            .with_context(|| format!("Failed to find remote '{}'", remote_name))?;

        // Set up callbacks for authentication
        let mut callbacks = git2::RemoteCallbacks::new();

        // Use git credential helper for authentication
        callbacks.credentials(|_url, username_from_url, _allowed_types| {
            git2::Cred::credential_helper(&self.repo.config()?, _url, username_from_url)
        });

        // Set up fetch options with callbacks
        let mut fetch_options = git2::FetchOptions::new();
        fetch_options.remote_callbacks(callbacks);

        remote
            .fetch(&["main", "master"], Some(&mut fetch_options), None)
            .context("Failed to fetch from remote")?;

        Ok(())
    }

    /// Pull from remote (fetch + merge)
    pub fn pull(&self, remote_name: &str, branch_name: &str) -> Result<()> {
        self.fetch(remote_name)?;

        let fetch_head = self
            .repo
            .find_reference("FETCH_HEAD")
            .context("Failed to find FETCH_HEAD")?;

        let fetch_commit = self
            .repo
            .reference_to_annotated_commit(&fetch_head)
            .context("Failed to get fetch commit")?;

        // Perform the merge
        let analysis = self
            .repo
            .merge_analysis(&[&fetch_commit])
            .context("Failed to analyze merge")?;

        if analysis.0.is_up_to_date() {
            // Already up to date
            return Ok(());
        } else if analysis.0.is_fast_forward() {
            // Fast-forward merge
            let refname = format!("refs/heads/{}", branch_name);
            let mut reference = self
                .repo
                .find_reference(&refname)
                .context("Failed to find branch reference")?;

            reference
                .set_target(fetch_commit.id(), "Fast-forward merge")
                .context("Failed to set reference target")?;

            self.repo.set_head(&refname).context("Failed to set HEAD")?;

            self.repo
                .checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
                .context("Failed to checkout HEAD")?;
        } else {
            // Normal merge - we'll handle this in the sync module
            return Err(anyhow!("Merge required - conflicts may exist"));
        }

        Ok(())
    }

    /// Check if repository has uncommitted changes
    pub fn has_changes(&self) -> Result<bool> {
        let statuses = self
            .repo
            .statuses(None)
            .context("Failed to get repository status")?;

        Ok(!statuses.is_empty())
    }

    /// Get current branch name
    pub fn current_branch(&self) -> Result<String> {
        let head = self.repo.head().context("Failed to get HEAD reference")?;

        let branch_name = head
            .shorthand()
            .ok_or_else(|| anyhow!("Failed to get branch name"))?;

        Ok(branch_name.to_string())
    }

    /// Check if a remote exists
    pub fn has_remote(&self, name: &str) -> bool {
        self.repo.find_remote(name).is_ok()
    }

    /// Get the current commit hash (HEAD)
    ///
    /// Returns the full SHA-1 hash of the current HEAD commit.
    /// This is useful for creating snapshots that track git state.
    ///
    /// # Returns
    /// The commit hash as a 40-character hex string
    ///
    /// # Errors
    /// Returns error if HEAD doesn't exist or points to invalid commit
    pub fn current_commit_hash(&self) -> Result<String> {
        let head = self.repo.head().context("Failed to get HEAD")?;
        let commit = head
            .peel_to_commit()
            .context("Failed to get commit from HEAD")?;
        Ok(commit.id().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_init_repository() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();
        assert!(git_manager.path().exists());
    }

    #[test]
    fn test_commit_workflow() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test content").unwrap();

        // Stage and commit
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial commit").unwrap();

        // Verify commit was created
        assert!(!git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_current_commit_hash() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test content").unwrap();

        // Stage and commit
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial commit").unwrap();

        // Get the commit hash using the method
        let hash = git_manager.current_commit_hash().unwrap();

        // Verify it's a valid SHA-1 hash (40 hex characters)
        assert_eq!(hash.len(), 40);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        // Verify it matches the actual HEAD commit
        let repo = Repository::open(temp_dir.path()).unwrap();
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(hash, head_commit.id().to_string());
    }
}
