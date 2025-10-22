use anyhow::{anyhow, Context, Result};

use super::manager::GitManager;

impl GitManager {
    /// Get current branch name
    pub fn current_branch(&self) -> Result<String> {
        let head = self.repo.head().context("Failed to get HEAD reference")?;

        let branch_name = head
            .shorthand()
            .ok_or_else(|| anyhow!("Failed to get branch name"))?;

        Ok(branch_name.to_string())
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
    use git2::Repository;
    use std::fs;
    use tempfile::TempDir;

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
        git_manager
            .repo
            .branch("feature-branch", &commit, false)
            .unwrap();
        git_manager
            .repo
            .set_head("refs/heads/feature-branch")
            .unwrap();

        let branch = git_manager.current_branch().unwrap();
        assert_eq!(branch, "feature-branch");
    }

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

    #[test]
    fn test_init_empty_repository_has_no_commits() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Getting current commit hash should fail on empty repo
        assert!(git_manager.current_commit_hash().is_err());
    }
}
