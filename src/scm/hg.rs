//! Mercurial (hg) CLI implementation of the Scm trait.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::Scm;

/// Mercurial SCM implementation using the `hg` CLI.
pub struct HgScm {
    path: PathBuf,
}

impl HgScm {
    /// Initialize a new Mercurial repository.
    pub fn init(path: &Path) -> Result<Self> {
        fs::create_dir_all(path)?;

        let output = Command::new("hg")
            .args(["init"])
            .current_dir(path)
            .output()
            .context("Failed to run 'hg init'")?;

        if !output.status.success() {
            bail!(
                "hg init failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Open an existing Mercurial repository.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.join(".hg").exists() {
            bail!("Not a Mercurial repository: {}", path.display());
        }
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Clone a repository from a URL.
    pub fn clone(url: &str, path: &Path) -> Result<Self> {
        let output = Command::new("hg")
            .args(["clone", url])
            .arg(path)
            .output()
            .context("Failed to run 'hg clone'")?;

        if !output.status.success() {
            bail!(
                "hg clone failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Run an hg command and return its output.
    fn run_hg(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("hg")
            .args(args)
            .current_dir(&self.path)
            .output()
            .with_context(|| format!("Failed to run 'hg {}'", args.join(" ")))?;

        if !output.status.success() {
            bail!(
                "hg {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Run an hg command and check if it succeeds.
    fn hg_succeeds(&self, args: &[&str]) -> bool {
        Command::new("hg")
            .args(args)
            .current_dir(&self.path)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Get path to .hg/hgrc config file.
    fn hgrc_path(&self) -> PathBuf {
        self.path.join(".hg").join("hgrc")
    }

    /// Read the hgrc file content.
    fn read_hgrc(&self) -> Result<String> {
        let path = self.hgrc_path();
        if path.exists() {
            fs::read_to_string(&path).context("Failed to read .hg/hgrc")
        } else {
            Ok(String::new())
        }
    }

    /// Write content to hgrc file.
    fn write_hgrc(&self, content: &str) -> Result<()> {
        let path = self.hgrc_path();
        fs::write(&path, content).context("Failed to write .hg/hgrc")
    }

    /// Parse paths from hgrc [paths] section.
    fn parse_paths(&self) -> Result<Vec<(String, String)>> {
        let content = self.read_hgrc()?;
        let mut paths = Vec::new();
        let mut in_paths_section = false;

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_paths_section = line == "[paths]";
                continue;
            }
            if in_paths_section && !line.is_empty() && !line.starts_with('#') {
                if let Some((name, url)) = line.split_once('=') {
                    paths.push((name.trim().to_string(), url.trim().to_string()));
                }
            }
        }

        Ok(paths)
    }

    /// Update a path in the [paths] section.
    fn update_path(&self, name: &str, url: Option<&str>) -> Result<()> {
        let content = self.read_hgrc()?;
        let mut new_content = String::new();
        let mut in_paths_section = false;
        let mut path_updated = false;
        let mut paths_section_exists = false;

        for line in content.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with('[') {
                // If we were in paths section and need to add, do it now
                if in_paths_section && !path_updated && url.is_some() {
                    new_content.push_str(&format!("{} = {}\n", name, url.unwrap()));
                    path_updated = true;
                }
                in_paths_section = trimmed == "[paths]";
                if in_paths_section {
                    paths_section_exists = true;
                }
                new_content.push_str(line);
                new_content.push('\n');
                continue;
            }

            if in_paths_section && !trimmed.is_empty() && !trimmed.starts_with('#') {
                if let Some((path_name, _)) = trimmed.split_once('=') {
                    if path_name.trim() == name {
                        // Found the path - update or remove
                        if let Some(new_url) = url {
                            new_content.push_str(&format!("{} = {}\n", name, new_url));
                        }
                        // If url is None, we skip (remove) this line
                        path_updated = true;
                        continue;
                    }
                }
            }

            new_content.push_str(line);
            new_content.push('\n');
        }

        // If we need to add and haven't yet
        if url.is_some() && !path_updated {
            if !paths_section_exists {
                new_content.push_str("\n[paths]\n");
            }
            new_content.push_str(&format!("{} = {}\n", name, url.unwrap()));
        }

        self.write_hgrc(&new_content)
    }
}

impl Scm for HgScm {
    fn current_branch(&self) -> Result<String> {
        self.run_hg(&["branch"])
    }

    fn current_commit_hash(&self) -> Result<String> {
        let hash = self.run_hg(&["id", "-i"])?;
        // Remove trailing '+' which indicates uncommitted changes
        Ok(hash.trim_end_matches('+').to_string())
    }

    fn stage_all(&self) -> Result<()> {
        // In Mercurial, addremove stages new and removed files
        self.run_hg(&["addremove"])?;
        Ok(())
    }

    fn commit(&self, message: &str) -> Result<()> {
        self.run_hg(&["commit", "-m", message])?;
        Ok(())
    }

    fn has_changes(&self) -> Result<bool> {
        let output = self.run_hg(&["status"])?;
        Ok(!output.is_empty())
    }

    fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        self.update_path(name, Some(url))
    }

    fn has_remote(&self, name: &str) -> bool {
        self.parse_paths()
            .map(|paths| paths.iter().any(|(n, _)| n == name))
            .unwrap_or(false)
    }

    fn get_remote_url(&self, name: &str) -> Result<String> {
        let paths = self.parse_paths()?;
        paths
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, url)| url)
            .ok_or_else(|| anyhow::anyhow!("Remote '{}' not found", name))
    }

    fn set_remote_url(&self, name: &str, url: &str) -> Result<()> {
        self.update_path(name, Some(url))
    }

    fn remove_remote(&self, name: &str) -> Result<()> {
        self.update_path(name, None)
    }

    fn list_remotes(&self) -> Result<Vec<String>> {
        let paths = self.parse_paths()?;
        Ok(paths.into_iter().map(|(name, _)| name).collect())
    }

    fn push(&self, remote: &str, _branch: &str) -> Result<()> {
        // Mercurial push uses path name, not remote + branch
        self.run_hg(&["push", remote])?;
        Ok(())
    }

    fn pull(&self, remote: &str, _branch: &str) -> Result<()> {
        // Pull and update
        self.run_hg(&["pull", "-u", remote])?;
        Ok(())
    }

    fn reset_soft(&self, commit: &str) -> Result<()> {
        // In Mercurial, we use 'update' to move to a revision
        // This is similar to a soft reset - working directory is updated
        self.run_hg(&["update", "-r", commit])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn hg_available() -> bool {
        Command::new("hg")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn test_hg_init_and_open() {
        if !hg_available() {
            eprintln!("Skipping: hg not installed");
            return;
        }

        let temp = TempDir::new().unwrap();
        let _scm = HgScm::init(temp.path()).unwrap();
        assert!(temp.path().join(".hg").exists());

        let _reopened = HgScm::open(temp.path()).unwrap();
    }

    #[test]
    fn test_hg_stage_commit() {
        if !hg_available() {
            eprintln!("Skipping: hg not installed");
            return;
        }

        let temp = TempDir::new().unwrap();
        let scm = HgScm::init(temp.path()).unwrap();

        assert!(!scm.has_changes().unwrap());

        fs::write(temp.path().join("test.txt"), "hello").unwrap();
        assert!(scm.has_changes().unwrap());

        scm.stage_all().unwrap();
        scm.commit("Initial commit").unwrap();

        assert!(!scm.has_changes().unwrap());
    }

    #[test]
    fn test_hg_branch() {
        if !hg_available() {
            eprintln!("Skipping: hg not installed");
            return;
        }

        let temp = TempDir::new().unwrap();
        let scm = HgScm::init(temp.path()).unwrap();

        // Need a commit first
        fs::write(temp.path().join("test.txt"), "content").unwrap();
        scm.stage_all().unwrap();
        scm.commit("Initial commit").unwrap();

        let branch = scm.current_branch().unwrap();
        assert_eq!(branch, "default");
    }

    #[test]
    fn test_hg_remote() {
        if !hg_available() {
            eprintln!("Skipping: hg not installed");
            return;
        }

        let temp = TempDir::new().unwrap();
        let scm = HgScm::init(temp.path()).unwrap();

        assert!(!scm.has_remote("origin"));

        scm.add_remote("origin", "https://example.com/repo").unwrap();
        assert!(scm.has_remote("origin"));

        let url = scm.get_remote_url("origin").unwrap();
        assert_eq!(url, "https://example.com/repo");

        scm.set_remote_url("origin", "https://example.com/new").unwrap();
        let new_url = scm.get_remote_url("origin").unwrap();
        assert_eq!(new_url, "https://example.com/new");

        scm.remove_remote("origin").unwrap();
        assert!(!scm.has_remote("origin"));
    }

    #[test]
    fn test_hg_list_remotes() {
        if !hg_available() {
            eprintln!("Skipping: hg not installed");
            return;
        }

        let temp = TempDir::new().unwrap();
        let scm = HgScm::init(temp.path()).unwrap();

        assert!(scm.list_remotes().unwrap().is_empty());

        scm.add_remote("origin", "https://example.com/origin").unwrap();
        scm.add_remote("upstream", "https://example.com/upstream").unwrap();

        let remotes = scm.list_remotes().unwrap();
        assert_eq!(remotes.len(), 2);
        assert!(remotes.contains(&"origin".to_string()));
        assert!(remotes.contains(&"upstream".to_string()));
    }
}
