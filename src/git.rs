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

        let signature = Signature::now("claude-code-sync", "noreply@claude-code-sync.local")
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

    // =========================================================================
    // init() tests
    // =========================================================================

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
    fn test_init_empty_repository_has_no_commits() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Getting current commit hash should fail on empty repo
        assert!(git_manager.current_commit_hash().is_err());
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

    // =========================================================================
    // open() tests
    // =========================================================================

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

    // =========================================================================
    // path() tests
    // =========================================================================

    #[test]
    fn test_path_returns_working_directory() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        let path = git_manager.path();
        assert_eq!(path, temp_dir.path().canonicalize().unwrap());
    }

    // =========================================================================
    // add_remote() tests
    // =========================================================================

    #[test]
    fn test_add_remote_origin() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        let result = git_manager.add_remote("origin", "https://github.com/test/repo.git");
        assert!(result.is_ok());

        // Verify remote was added
        assert!(git_manager.has_remote("origin"));
    }

    #[test]
    fn test_add_remote_custom_name() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        let result = git_manager.add_remote("upstream", "https://github.com/test/upstream.git");
        assert!(result.is_ok());
        assert!(git_manager.has_remote("upstream"));
    }

    #[test]
    fn test_add_multiple_remotes() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        git_manager.add_remote("origin", "https://github.com/test/repo.git").unwrap();
        git_manager.add_remote("upstream", "https://github.com/test/upstream.git").unwrap();
        git_manager.add_remote("backup", "https://gitlab.com/test/repo.git").unwrap();

        assert!(git_manager.has_remote("origin"));
        assert!(git_manager.has_remote("upstream"));
        assert!(git_manager.has_remote("backup"));
    }

    #[test]
    fn test_add_remote_duplicate_name_fails() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        git_manager.add_remote("origin", "https://github.com/test/repo.git").unwrap();

        // Adding same remote name should fail
        let result = git_manager.add_remote("origin", "https://github.com/test/other.git");
        assert!(result.is_err());
    }

    #[test]
    fn test_add_remote_ssh_url() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        let result = git_manager.add_remote("origin", "git@github.com:test/repo.git");
        assert!(result.is_ok());
        assert!(git_manager.has_remote("origin"));
    }

    // =========================================================================
    // has_remote() tests
    // =========================================================================

    #[test]
    fn test_has_remote_returns_false_when_no_remotes() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        assert!(!git_manager.has_remote("origin"));
        assert!(!git_manager.has_remote("upstream"));
    }

    #[test]
    fn test_has_remote_returns_true_after_adding() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        git_manager.add_remote("origin", "https://github.com/test/repo.git").unwrap();

        assert!(git_manager.has_remote("origin"));
        assert!(!git_manager.has_remote("upstream"));
    }

    // =========================================================================
    // stage_all() tests
    // =========================================================================

    #[test]
    fn test_stage_all_empty_repository() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Staging in empty repo should succeed
        let result = git_manager.stage_all();
        assert!(result.is_ok());
    }

    #[test]
    fn test_stage_all_single_file() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create a file
        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();

        git_manager.stage_all().unwrap();

        // Verify file is staged (has changes before commit)
        assert!(git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_stage_all_multiple_files() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create multiple files
        fs::write(temp_dir.path().join("file1.txt"), "content1").unwrap();
        fs::write(temp_dir.path().join("file2.txt"), "content2").unwrap();
        fs::write(temp_dir.path().join("file3.txt"), "content3").unwrap();

        git_manager.stage_all().unwrap();
        assert!(git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_stage_all_nested_directories() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create nested directory structure
        let nested_dir = temp_dir.path().join("dir1").join("dir2").join("dir3");
        fs::create_dir_all(&nested_dir).unwrap();

        fs::write(temp_dir.path().join("root.txt"), "root").unwrap();
        fs::write(temp_dir.path().join("dir1").join("level1.txt"), "level1").unwrap();
        fs::write(nested_dir.join("deep.txt"), "deep").unwrap();

        git_manager.stage_all().unwrap();
        assert!(git_manager.has_changes().unwrap());

        // Commit to verify all files were staged
        git_manager.commit("Test nested files").unwrap();
        assert!(!git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_stage_all_with_modifications() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create and commit initial file
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "original").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial").unwrap();

        // Modify the file
        fs::write(&file_path, "modified").unwrap();

        // Stage modifications
        git_manager.stage_all().unwrap();
        assert!(git_manager.has_changes().unwrap());
    }

    // =========================================================================
    // commit() tests
    // =========================================================================

    #[test]
    fn test_commit_first_commit() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create and stage a file
        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();

        let result = git_manager.commit("First commit");
        assert!(result.is_ok());

        // Verify no uncommitted changes
        assert!(!git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_commit_subsequent_commits() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // First commit
        fs::write(temp_dir.path().join("file1.txt"), "content1").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("First commit").unwrap();

        // Second commit
        fs::write(temp_dir.path().join("file2.txt"), "content2").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Second commit").unwrap();

        // Third commit
        fs::write(temp_dir.path().join("file3.txt"), "content3").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Third commit").unwrap();

        assert!(!git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_commit_with_empty_message() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();

        // Empty message should still work
        let result = git_manager.commit("");
        assert!(result.is_ok());
    }

    #[test]
    fn test_commit_with_multiline_message() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();

        let message = "Title line\n\nDetailed description\nwith multiple lines";
        let result = git_manager.commit(message);
        assert!(result.is_ok());
    }

    #[test]
    fn test_commit_without_staged_files() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Try to commit without staging anything - this should succeed
        // (creates an empty tree commit)
        let result = git_manager.commit("Empty commit");
        assert!(result.is_ok());
    }

    #[test]
    fn test_commit_updates_current_commit_hash() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // First commit
        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("First").unwrap();
        let hash1 = git_manager.current_commit_hash().unwrap();

        // Second commit
        fs::write(temp_dir.path().join("test.txt"), "modified").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Second").unwrap();
        let hash2 = git_manager.current_commit_hash().unwrap();

        // Hashes should be different
        assert_ne!(hash1, hash2);
    }

    // =========================================================================
    // has_changes() tests
    // =========================================================================

    #[test]
    fn test_has_changes_clean_repository() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Empty repo has no changes
        assert!(!git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_has_changes_with_untracked_file() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create untracked file
        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();

        assert!(git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_has_changes_with_staged_file() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();

        assert!(git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_has_changes_after_commit() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Commit").unwrap();

        // After commit, no changes should remain
        assert!(!git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_has_changes_with_modified_file() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Commit initial file
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "original").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial").unwrap();

        // Modify the file
        fs::write(&file_path, "modified").unwrap();

        assert!(git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_has_changes_with_deleted_file() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Commit initial file
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial").unwrap();

        // Delete the file
        fs::remove_file(&file_path).unwrap();

        assert!(git_manager.has_changes().unwrap());
    }

    // =========================================================================
    // current_branch() tests
    // =========================================================================

    #[test]
    fn test_current_branch_default_is_master() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create initial commit so HEAD points to a branch
        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial").unwrap();

        let branch = git_manager.current_branch().unwrap();
        // Default branch is usually "master" in git2
        assert!(branch == "master" || branch == "main");
    }

    #[test]
    fn test_current_branch_fails_on_empty_repo() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Empty repo without commits has no HEAD
        let result = git_manager.current_branch();
        assert!(result.is_err());
    }

    #[test]
    fn test_current_branch_after_creating_branch() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create initial commit
        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial").unwrap();

        // Create and checkout new branch using git2
        let head = git_manager.repo.head().unwrap();
        let commit = head.peel_to_commit().unwrap();
        git_manager.repo.branch("feature-branch", &commit, false).unwrap();
        git_manager.repo.set_head("refs/heads/feature-branch").unwrap();

        let branch = git_manager.current_branch().unwrap();
        assert_eq!(branch, "feature-branch");
    }

    // =========================================================================
    // current_commit_hash() tests
    // =========================================================================

    #[test]
    fn test_current_commit_hash_format() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Commit").unwrap();

        let hash = git_manager.current_commit_hash().unwrap();

        // Verify it's a valid SHA-1 hash (40 hex characters)
        assert_eq!(hash.len(), 40);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_current_commit_hash_fails_on_empty_repo() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Empty repo has no commits
        let result = git_manager.current_commit_hash();
        assert!(result.is_err());
    }

    #[test]
    fn test_current_commit_hash_matches_git2_api() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Commit").unwrap();

        let hash = git_manager.current_commit_hash().unwrap();

        // Verify it matches the actual HEAD commit from git2
        let repo = Repository::open(temp_dir.path()).unwrap();
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(hash, head_commit.id().to_string());
    }

    #[test]
    fn test_current_commit_hash_different_for_different_commits() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // First commit
        fs::write(temp_dir.path().join("file1.txt"), "content1").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("First").unwrap();
        let hash1 = git_manager.current_commit_hash().unwrap();

        // Second commit
        fs::write(temp_dir.path().join("file2.txt"), "content2").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Second").unwrap();
        let hash2 = git_manager.current_commit_hash().unwrap();

        // Third commit
        fs::write(temp_dir.path().join("file3.txt"), "content3").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Third").unwrap();
        let hash3 = git_manager.current_commit_hash().unwrap();

        // All hashes should be different
        assert_ne!(hash1, hash2);
        assert_ne!(hash2, hash3);
        assert_ne!(hash1, hash3);
    }

    // =========================================================================
    // Integration tests - combining multiple operations
    // =========================================================================

    #[test]
    fn test_integration_full_workflow() {
        let temp_dir = TempDir::new().unwrap();

        // Initialize repository
        let git_manager = GitManager::init(temp_dir.path()).unwrap();
        assert!(git_manager.path().exists());

        // Add remote
        git_manager.add_remote("origin", "https://github.com/test/repo.git").unwrap();
        assert!(git_manager.has_remote("origin"));

        // Create files
        fs::write(temp_dir.path().join("file1.txt"), "content1").unwrap();
        fs::write(temp_dir.path().join("file2.txt"), "content2").unwrap();

        // Stage and commit
        assert!(git_manager.has_changes().unwrap());
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial commit").unwrap();
        assert!(!git_manager.has_changes().unwrap());

        // Verify commit hash
        let hash = git_manager.current_commit_hash().unwrap();
        assert_eq!(hash.len(), 40);

        // Verify branch name
        let branch = git_manager.current_branch().unwrap();
        assert!(branch == "master" || branch == "main");
    }

    #[test]
    fn test_integration_multiple_commits_with_modifications() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // First commit
        fs::write(temp_dir.path().join("test.txt"), "v1").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Version 1").unwrap();
        let hash1 = git_manager.current_commit_hash().unwrap();

        // Second commit - modify file
        fs::write(temp_dir.path().join("test.txt"), "v2").unwrap();
        assert!(git_manager.has_changes().unwrap());
        git_manager.stage_all().unwrap();
        git_manager.commit("Version 2").unwrap();
        let hash2 = git_manager.current_commit_hash().unwrap();

        // Third commit - add new file
        fs::write(temp_dir.path().join("new.txt"), "new content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Version 3").unwrap();
        let hash3 = git_manager.current_commit_hash().unwrap();

        // Verify all commits have different hashes
        assert_ne!(hash1, hash2);
        assert_ne!(hash2, hash3);
        assert_ne!(hash1, hash3);

        // No uncommitted changes
        assert!(!git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_integration_init_and_reopen() {
        let temp_dir = TempDir::new().unwrap();

        // Initialize and create a commit
        let git_manager1 = GitManager::init(temp_dir.path()).unwrap();
        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager1.stage_all().unwrap();
        git_manager1.commit("Initial").unwrap();
        let hash1 = git_manager1.current_commit_hash().unwrap();

        // Drop the first manager and reopen
        drop(git_manager1);
        let git_manager2 = GitManager::open(temp_dir.path()).unwrap();

        // Should be able to read the same commit hash
        let hash2 = git_manager2.current_commit_hash().unwrap();
        assert_eq!(hash1, hash2);

        // Should have no changes
        assert!(!git_manager2.has_changes().unwrap());
    }

    #[test]
    fn test_integration_nested_directory_workflow() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create complex directory structure
        let src_dir = temp_dir.path().join("src");
        let tests_dir = temp_dir.path().join("tests");
        let docs_dir = temp_dir.path().join("docs").join("api");

        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&tests_dir).unwrap();
        fs::create_dir_all(&docs_dir).unwrap();

        // Create files in different directories
        fs::write(temp_dir.path().join("README.md"), "# Project").unwrap();
        fs::write(src_dir.join("main.rs"), "fn main() {}").unwrap();
        fs::write(src_dir.join("lib.rs"), "pub fn test() {}").unwrap();
        fs::write(tests_dir.join("test.rs"), "#[test]").unwrap();
        fs::write(docs_dir.join("index.md"), "# API Docs").unwrap();

        // Stage and commit all
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial project structure").unwrap();

        // Verify clean state
        assert!(!git_manager.has_changes().unwrap());

        // Modify files in different directories
        fs::write(src_dir.join("main.rs"), "fn main() { println!(\"hello\"); }").unwrap();
        fs::write(docs_dir.join("changelog.md"), "# Changelog").unwrap();

        // Should detect changes
        assert!(git_manager.has_changes().unwrap());

        // Commit changes
        git_manager.stage_all().unwrap();
        git_manager.commit("Update files").unwrap();
        assert!(!git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_integration_multiple_remotes_workflow() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Add multiple remotes
        git_manager.add_remote("origin", "https://github.com/user/repo.git").unwrap();
        git_manager.add_remote("upstream", "https://github.com/org/repo.git").unwrap();
        git_manager.add_remote("backup", "git@gitlab.com:user/repo.git").unwrap();

        // Verify all remotes exist
        assert!(git_manager.has_remote("origin"));
        assert!(git_manager.has_remote("upstream"));
        assert!(git_manager.has_remote("backup"));
        assert!(!git_manager.has_remote("nonexistent"));

        // Create commit
        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Test").unwrap();

        // Remotes should still be available after commits
        assert!(git_manager.has_remote("origin"));
        assert!(git_manager.has_remote("upstream"));
        assert!(git_manager.has_remote("backup"));
    }

    // =========================================================================
    // Edge case tests
    // =========================================================================

    #[test]
    fn test_edge_case_commit_same_content_multiple_times() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        let file_path = temp_dir.path().join("test.txt");

        // First commit
        fs::write(&file_path, "same content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("First").unwrap();
        let hash1 = git_manager.current_commit_hash().unwrap();

        // Modify and restore to same content
        fs::write(&file_path, "different").unwrap();
        fs::write(&file_path, "same content").unwrap();

        // Should have no changes (content is identical)
        assert!(!git_manager.has_changes().unwrap());

        // Change to actually different content
        fs::write(&file_path, "actually different").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Second").unwrap();
        let hash2 = git_manager.current_commit_hash().unwrap();

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_edge_case_empty_directory_not_tracked() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create empty directory
        fs::create_dir_all(temp_dir.path().join("empty_dir")).unwrap();

        // Git doesn't track empty directories
        assert!(!git_manager.has_changes().unwrap());

        // Add file to directory
        fs::write(temp_dir.path().join("empty_dir").join("file.txt"), "content").unwrap();
        assert!(git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_edge_case_stage_after_delete() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create and commit file
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Initial").unwrap();

        // Delete file
        fs::remove_file(&file_path).unwrap();
        assert!(git_manager.has_changes().unwrap());

        // Stage deletion
        git_manager.stage_all().unwrap();
        git_manager.commit("Delete file").unwrap();

        assert!(!git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_edge_case_large_number_of_files() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create 100 files
        for i in 0..100 {
            fs::write(temp_dir.path().join(format!("file_{}.txt", i)), format!("content {}", i)).unwrap();
        }

        assert!(git_manager.has_changes().unwrap());
        git_manager.stage_all().unwrap();
        git_manager.commit("Add 100 files").unwrap();

        assert!(!git_manager.has_changes().unwrap());

        let hash = git_manager.current_commit_hash().unwrap();
        assert_eq!(hash.len(), 40);
    }

    #[test]
    fn test_edge_case_unicode_filenames() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create files with unicode names
        fs::write(temp_dir.path().join("test_日本語.txt"), "Japanese").unwrap();
        fs::write(temp_dir.path().join("test_émojis_🚀.txt"), "Emoji").unwrap();
        fs::write(temp_dir.path().join("test_Ελληνικά.txt"), "Greek").unwrap();

        git_manager.stage_all().unwrap();
        git_manager.commit("Unicode files").unwrap();

        assert!(!git_manager.has_changes().unwrap());
    }

    #[test]
    fn test_edge_case_very_long_commit_message() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();

        // Very long commit message
        let long_message = "A".repeat(10000);
        let result = git_manager.commit(&long_message);
        assert!(result.is_ok());
    }

    #[test]
    fn test_edge_case_special_characters_in_commit_message() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();

        let message = "Special chars: !@#$%^&*()[]{}|\\:;\"'<>,.?/~`±§";
        let result = git_manager.commit(message);
        assert!(result.is_ok());
    }

    #[test]
    fn test_edge_case_rapid_sequential_commits() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        let mut hashes = Vec::new();

        // Create 10 commits rapidly
        for i in 0..10 {
            fs::write(temp_dir.path().join(format!("file{}.txt", i)), format!("content{}", i)).unwrap();
            git_manager.stage_all().unwrap();
            git_manager.commit(&format!("Commit {}", i)).unwrap();
            hashes.push(git_manager.current_commit_hash().unwrap());
        }

        // All hashes should be unique
        for i in 0..hashes.len() {
            for j in (i + 1)..hashes.len() {
                assert_ne!(hashes[i], hashes[j]);
            }
        }
    }
}
