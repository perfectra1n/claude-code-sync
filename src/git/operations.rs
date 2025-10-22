use anyhow::{Context, Result};
use git2::{IndexAddOption, Signature};

use super::manager::GitManager;

impl GitManager {
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

    /// Check if repository has uncommitted changes
    ///
    /// This method checks for both staged and unstaged changes in tracked files,
    /// similar to running `git status --porcelain`.
    ///
    /// ## Behavior
    /// - Returns `true` if there are any staged or modified files
    /// - Excludes untracked files (same as `git status -uno`)
    /// - Excludes ignored files
    ///
    /// This matches the typical workflow expectation: we want to know if there
    /// are changes to commit, not whether there are random untracked files.
    pub fn has_changes(&self) -> Result<bool> {
        let mut opts = git2::StatusOptions::new();

        // Exclude ignored files to match git status behavior
        opts.include_ignored(false);

        // Exclude untracked files - we only care about changes to tracked files
        // This matches `git status -uno` behavior
        opts.include_untracked(false);

        // Exclude unmodified files for efficiency
        opts.include_unmodified(false);

        let statuses = self
            .repo
            .statuses(Some(&mut opts))
            .context("Failed to get repository status")?;

        // Check if there are any status entries
        // With our options, any entry means there's a real change to tracked files
        for entry in statuses.iter() {
            let status = entry.status();

            // If there's any status flag set, we have changes
            // This includes: INDEX_NEW, INDEX_MODIFIED, INDEX_DELETED,
            //                WT_MODIFIED, WT_DELETED, WT_TYPECHANGE, etc.
            if !status.is_empty() && !status.contains(git2::Status::IGNORED) {
                return Ok(true);
            }
        }

        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

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

        // Untracked files should NOT be counted as changes
        // This matches `git status -uno` behavior
        assert!(!git_manager.has_changes().unwrap());
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

    #[test]
    fn test_edge_case_empty_directory_not_tracked() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Create empty directory
        fs::create_dir_all(temp_dir.path().join("empty_dir")).unwrap();

        // Git doesn't track empty directories
        assert!(!git_manager.has_changes().unwrap());

        // Add file to directory - but it's untracked, so has_changes() returns false
        fs::write(
            temp_dir.path().join("empty_dir").join("file.txt"),
            "content",
        )
        .unwrap();
        // Untracked files don't count as changes (matches `git status -uno`)
        assert!(!git_manager.has_changes().unwrap());
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
}
