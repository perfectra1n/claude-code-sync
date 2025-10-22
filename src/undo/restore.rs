use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use super::snapshot::Snapshot;

impl Snapshot {
    /// Restore files from this snapshot
    ///
    /// This writes all files from the snapshot back to their original locations,
    /// overwriting any current content.
    ///
    /// For differential snapshots, this recursively loads the base snapshot chain
    /// and applies all changes in order to reconstruct the full state.
    ///
    /// # Security
    /// This method validates all paths to prevent path traversal attacks.
    /// By default, only paths within the home directory are allowed.
    /// For testing, you can pass a custom allowed_base_dir.
    ///
    /// # Arguments
    /// * `allowed_base_dir` - Optional base directory for path validation.
    ///   If None, defaults to home directory for security.
    /// * `snapshots_dir` - Optional snapshots directory (for testing with differential snapshots)
    pub fn restore_with_base_and_snapshots(
        &self,
        allowed_base_dir: Option<&Path>,
        snapshots_dir: Option<&Path>,
    ) -> Result<()> {
        // Determine the allowed base directory
        let allowed_base = if let Some(base) = allowed_base_dir {
            // For testing: use the provided base
            base.canonicalize().with_context(|| {
                format!("Failed to canonicalize base directory: {}", base.display())
            })?
        } else {
            // For production: use home directory
            let home_dir = dirs::home_dir().context("Failed to get home directory")?;
            home_dir
                .canonicalize()
                .context("Failed to canonicalize home directory")?
        };

        // Build the complete file state by walking the snapshot chain
        let all_files = self.reconstruct_full_state_with_dir(snapshots_dir)?;

        // First, handle file deletions from the snapshot
        for deleted_path in &self.deleted_files {
            let path = PathBuf::from(deleted_path);

            // Validate the path is within allowed directory
            if let Ok(canonical) = path.canonicalize() {
                if canonical.starts_with(&allowed_base) && path.exists() {
                    fs::remove_file(&path).with_context(|| {
                        format!("Failed to delete file: {}", path.display())
                    })?;
                }
            }
        }

        // Then restore all files from the reconstructed state
        for (path_str, content) in &all_files {
            let path = PathBuf::from(path_str);

            // Canonicalize the path to resolve any symlinks or .. components
            // First ensure parent directory exists for canonicalization to work
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
            }

            // Create the file if it doesn't exist for canonicalization
            if !path.exists() {
                fs::write(&path, b"").with_context(|| {
                    format!("Failed to create temporary file: {}", path.display())
                })?;
            }

            let canonical_path = path
                .canonicalize()
                .with_context(|| format!("Failed to canonicalize path: {}", path.display()))?;

            // Validate the canonical path is within the allowed base directory
            if !canonical_path.starts_with(&allowed_base) {
                return Err(anyhow!(
                    "Security: Path traversal detected. Path {} is outside allowed directory {}",
                    path.display(),
                    allowed_base.display()
                ));
            }

            // Now write the actual content
            fs::write(&canonical_path, content)
                .with_context(|| format!("Failed to restore file: {}", canonical_path.display()))?;
        }

        Ok(())
    }

    /// Restore files from this snapshot using default snapshots directory
    ///
    /// This is a wrapper around `restore_with_base_and_snapshots` for backwards compatibility.
    pub fn restore_with_base(&self, allowed_base_dir: Option<&Path>) -> Result<()> {
        self.restore_with_base_and_snapshots(allowed_base_dir, None)
    }

    /// Restore files from this snapshot
    ///
    /// This is a convenience wrapper that uses the home directory as the allowed base.
    #[allow(dead_code)]
    pub fn restore(&self) -> Result<()> {
        self.restore_with_base(None)
    }
}
