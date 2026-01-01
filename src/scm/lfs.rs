//! Git LFS support for large conversation files.
//!
//! This module provides helpers for configuring Git LFS. Since we use
//! the git CLI, LFS operations (clean/smudge) happen automatically
//! once configured. This module handles the setup.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Check if git-lfs is installed on the system.
pub fn is_installed() -> bool {
    Command::new("git-lfs")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Initialize LFS in a repository.
///
/// This runs `git lfs install --local` to configure the repository for LFS.
pub fn init(repo_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["lfs", "install", "--local"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run 'git lfs install'")?;

    if !output.status.success() {
        bail!(
            "git lfs install failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Configure .gitattributes for LFS patterns.
///
/// Writes a .gitattributes file that tells git to use LFS for the
/// specified file patterns.
pub fn configure_gitattributes(repo_path: &Path, patterns: &[String]) -> Result<()> {
    let gitattributes_path = repo_path.join(".gitattributes");

    let mut content = String::new();

    // Read existing content if file exists
    if gitattributes_path.exists() {
        content = fs::read_to_string(&gitattributes_path)
            .context("Failed to read existing .gitattributes")?;
    }

    // Add LFS patterns that aren't already present
    for pattern in patterns {
        let lfs_line = format!("{} filter=lfs diff=lfs merge=lfs -text", pattern);
        if !content.contains(&lfs_line) {
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(&lfs_line);
            content.push('\n');
        }
    }

    fs::write(&gitattributes_path, content).context("Failed to write .gitattributes")?;

    Ok(())
}

/// Set up LFS for a repository with the given patterns.
///
/// This is a convenience function that:
/// 1. Checks if git-lfs is installed
/// 2. Initializes LFS in the repository
/// 3. Configures .gitattributes for the patterns
pub fn setup(repo_path: &Path, patterns: &[String]) -> Result<()> {
    if !is_installed() {
        bail!(
            "git-lfs is not installed.\n\
            Install it with:\n  \
            - macOS: brew install git-lfs\n  \
            - Ubuntu/Debian: apt install git-lfs\n  \
            - Windows: https://git-lfs.github.com"
        );
    }

    init(repo_path)?;
    configure_gitattributes(repo_path, patterns)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_configure_gitattributes_new_file() {
        let temp_dir = TempDir::new().unwrap();
        let patterns = vec!["*.jsonl".to_string()];

        configure_gitattributes(temp_dir.path(), &patterns).unwrap();

        let content = fs::read_to_string(temp_dir.path().join(".gitattributes")).unwrap();
        assert!(content.contains("*.jsonl filter=lfs diff=lfs merge=lfs -text"));
    }

    #[test]
    fn test_configure_gitattributes_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let gitattributes = temp_dir.path().join(".gitattributes");

        // Write existing content
        fs::write(&gitattributes, "*.txt text\n").unwrap();

        let patterns = vec!["*.jsonl".to_string()];
        configure_gitattributes(temp_dir.path(), &patterns).unwrap();

        let content = fs::read_to_string(&gitattributes).unwrap();
        assert!(content.contains("*.txt text"));
        assert!(content.contains("*.jsonl filter=lfs diff=lfs merge=lfs -text"));
    }

    #[test]
    fn test_configure_gitattributes_no_duplicates() {
        let temp_dir = TempDir::new().unwrap();
        let patterns = vec!["*.jsonl".to_string()];

        // Configure twice
        configure_gitattributes(temp_dir.path(), &patterns).unwrap();
        configure_gitattributes(temp_dir.path(), &patterns).unwrap();

        let content = fs::read_to_string(temp_dir.path().join(".gitattributes")).unwrap();
        // Should only appear once
        assert_eq!(content.matches("*.jsonl filter=lfs").count(), 1);
    }

    #[test]
    fn test_is_installed() {
        // Just verify the function doesn't panic
        let _ = is_installed();
    }

    #[test]
    fn test_setup_configures_gitattributes() {
        let temp_dir = TempDir::new().unwrap();
        let patterns = vec!["*.jsonl".to_string(), "*.png".to_string()];

        // Initialize a git repo first (setup requires a git repo)
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .unwrap();

        // Setup may fail if git-lfs is not installed, which is OK for this test
        // We just want to verify it configures .gitattributes when it can
        let result = setup(temp_dir.path(), &patterns);

        if is_installed() {
            // If LFS is installed, setup should succeed
            assert!(result.is_ok(), "setup failed: {:?}", result.err());

            // Verify .gitattributes was configured
            let content = fs::read_to_string(temp_dir.path().join(".gitattributes")).unwrap();
            assert!(content.contains("*.jsonl filter=lfs"));
            assert!(content.contains("*.png filter=lfs"));
        } else {
            // If LFS is not installed, setup should fail with helpful message
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("git-lfs is not installed"));
        }
    }
}
