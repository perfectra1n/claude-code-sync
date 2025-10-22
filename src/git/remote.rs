use anyhow::{anyhow, Context, Result};

use super::credentials;
use super::manager::GitManager;

impl GitManager {
    /// Add a remote to the repository
    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        self.repo
            .remote(name, url)
            .with_context(|| format!("Failed to add remote '{name}' with URL '{url}'"))?;
        Ok(())
    }

    /// Push to remote
    pub fn push(&self, remote_name: &str, branch_name: &str) -> Result<()> {
        let mut remote = self
            .repo
            .find_remote(remote_name)
            .with_context(|| format!("Failed to find remote '{remote_name}'"))?;

        let refspec = format!("refs/heads/{branch_name}:refs/heads/{branch_name}");

        // Set up callbacks for authentication
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(credentials::credential_callback);

        // Set up push options with callbacks
        let mut push_options = git2::PushOptions::new();
        push_options.remote_callbacks(callbacks);

        // Attempt push with detailed error handling
        match remote.push(&[&refspec], Some(&mut push_options)) {
            Ok(_) => Ok(()),
            Err(e) => {
                let remote_url = remote.url().unwrap_or("unknown");
                Err(anyhow!(
                    "Failed to push to remote '{remote_name}' at '{remote_url}': {e}\n\
                    \n\
                    Possible causes:\n\
                    1. Authentication failed - ensure credentials are configured\n\
                    2. No permission to push to this repository\n\
                    3. Network connectivity issues\n\
                    4. Remote branch protection rules\n\
                    \n\
                    For HTTPS: Run 'git config --global credential.helper store' and try again\n\
                    For SSH: Ensure SSH keys are set up with 'ssh -T git@github.com'"
                ))
            }
        }
    }

    /// Fetch from remote
    pub fn fetch(&self, remote_name: &str) -> Result<()> {
        let mut remote = self
            .repo
            .find_remote(remote_name)
            .with_context(|| format!("Failed to find remote '{remote_name}'"))?;

        // Set up callbacks for authentication
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(credentials::credential_callback);

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
            let refname = format!("refs/heads/{branch_name}");
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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

        git_manager
            .add_remote("origin", "https://github.com/test/repo.git")
            .unwrap();
        git_manager
            .add_remote("upstream", "https://github.com/test/upstream.git")
            .unwrap();
        git_manager
            .add_remote("backup", "https://gitlab.com/test/repo.git")
            .unwrap();

        assert!(git_manager.has_remote("origin"));
        assert!(git_manager.has_remote("upstream"));
        assert!(git_manager.has_remote("backup"));
    }

    #[test]
    fn test_add_remote_duplicate_name_fails() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        git_manager
            .add_remote("origin", "https://github.com/test/repo.git")
            .unwrap();

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

    #[test]
    fn test_has_remote_returns_true_after_adding() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        git_manager
            .add_remote("origin", "https://github.com/test/repo.git")
            .unwrap();

        assert!(git_manager.has_remote("origin"));
        assert!(!git_manager.has_remote("upstream"));
    }

    #[test]
    fn test_integration_multiple_remotes_workflow() {
        let temp_dir = TempDir::new().unwrap();
        let git_manager = GitManager::init(temp_dir.path()).unwrap();

        // Add multiple remotes
        git_manager
            .add_remote("origin", "https://github.com/user/repo.git")
            .unwrap();
        git_manager
            .add_remote("upstream", "https://github.com/org/repo.git")
            .unwrap();
        git_manager
            .add_remote("backup", "git@gitlab.com:user/repo.git")
            .unwrap();

        // Verify all remotes exist
        assert!(git_manager.has_remote("origin"));
        assert!(git_manager.has_remote("upstream"));
        assert!(git_manager.has_remote("backup"));
        assert!(!git_manager.has_remote("nonexistent"));

        // Create commit
        use std::fs;
        fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
        git_manager.stage_all().unwrap();
        git_manager.commit("Test").unwrap();

        // Remotes should still be available after commits
        assert!(git_manager.has_remote("origin"));
        assert!(git_manager.has_remote("upstream"));
        assert!(git_manager.has_remote("backup"));
    }
}
