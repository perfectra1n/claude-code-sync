use anyhow::{anyhow, Context, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::snapshot::Snapshot;

/// Resolve `path` to an absolute, symlink-free location **without creating
/// anything on disk**, so the caller can check it against an allowed base before
/// writing.
///
/// `Path::canonicalize` requires the path to already exist. The obvious way to
/// satisfy that is to create the file first and validate second — which is
/// exactly what this module used to do, meaning a snapshot naming a path outside
/// the home directory got its parent directories and an empty file created
/// before the traversal check rejected it, and the early return left them
/// behind.
///
/// Instead: canonicalize the deepest ancestor that *does* exist — which resolves
/// any symlinks in that prefix — then re-attach the remaining components
/// literally. Those components don't exist, so they can't be symlinks, but they
/// can still be `..`, and `canonicalize` is not there to collapse them for us
/// any more. So we reject `..` outright.
///
/// Existence is probed with `symlink_metadata`, not `exists`: `exists` follows
/// symlinks and so reports `false` for a *dangling* one. Treating a dangling
/// symlink as "not there" would let us re-attach its name lexically to a
/// canonical in-base prefix, pass the check, and then have `fs::write` follow it
/// straight out of the sandbox.
fn resolve_without_creating(path: &Path) -> Result<PathBuf> {
    // Reject `..` before doing anything else. Snapshot paths are absolute paths
    // captured from real files on disk, so a `..` component is never legitimate
    // — and now that we no longer call `canonicalize` on the whole path, nothing
    // is left to collapse them for us. (`.` needs no handling: `Path::components`
    // normalizes it away.)
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(anyhow!(
            "Security: Path traversal detected. Path {} contains '..'",
            path.display()
        ));
    }

    let mut existing = path;
    let mut tail: Vec<&OsStr> = Vec::new();

    while existing.symlink_metadata().is_err() {
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                existing = parent;
            }
            // Walked off the top without finding anything that exists: the path
            // is relative, or its root is gone. Either way we can't place it.
            _ => {
                return Err(anyhow!(
                    "Cannot resolve path for restore: {}",
                    path.display()
                ))
            }
        }
    }

    // Canonicalizing the existing prefix resolves any symlinks in it. The tail
    // components don't exist, so they cannot be symlinks, and we just proved they
    // aren't `..` — so re-attaching them literally is safe.
    let mut resolved = existing.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize {} while restoring {}",
            existing.display(),
            path.display()
        )
    })?;

    for name in tail.iter().rev() {
        resolved.push(name);
    }

    Ok(resolved)
}

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
                    fs::remove_file(&path)
                        .with_context(|| format!("Failed to delete file: {}", path.display()))?;
                }
            }
        }

        // Then restore all files from the reconstructed state.
        //
        // Every path is resolved and checked BEFORE anything is written. Doing it
        // the other way round -- creating the file so that `canonicalize` has
        // something to work with, then validating -- means a rejected path has
        // already had its parent directories and an empty file created by the
        // time we bail.
        for (path_str, content) in &all_files {
            let path = PathBuf::from(path_str);

            let target = resolve_without_creating(&path)?;

            if !target.starts_with(&allowed_base) {
                return Err(anyhow!(
                    "Security: Path traversal detected. Path {} is outside allowed directory {}",
                    path.display(),
                    allowed_base.display()
                ));
            }

            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
            }

            fs::write(&target, content)
                .with_context(|| format!("Failed to restore file: {}", target.display()))?;
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
    pub fn restore(&self) -> Result<()> {
        self.restore_with_base(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::OperationType;
    use crate::undo::test_support::{create_test_file, metadata_only_snapshot};
    use chrono::Duration;
    use tempfile::tempdir;
    use uuid::Uuid;

    #[test]
    fn test_snapshot_restore() {
        let temp_dir = tempdir().unwrap();
        let file1 = create_test_file(temp_dir.path(), "file1.txt", "original content");

        let snapshot = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();

        fs::write(&file1, "modified content").unwrap();
        assert_eq!(fs::read_to_string(&file1).unwrap(), "modified content");

        snapshot.restore_with_base(Some(temp_dir.path())).unwrap();

        assert_eq!(fs::read_to_string(&file1).unwrap(), "original content");
    }

    #[test]
    fn test_snapshot_restores_file_hierarchy() {
        let temp_dir = tempdir().unwrap();

        let nested_dir = temp_dir.path().join("dir1").join("dir2");
        fs::create_dir_all(&nested_dir).unwrap();
        let nested_file = nested_dir.join("deep.txt");
        fs::write(&nested_file, "deep content").unwrap();

        let snapshot = Snapshot::create(OperationType::Pull, vec![&nested_file], None).unwrap();

        // Blow away the whole tree, not just the file: restore has to recreate
        // the intermediate directories, not merely rewrite the leaf.
        fs::remove_dir_all(temp_dir.path().join("dir1")).unwrap();
        assert!(!nested_file.exists());

        snapshot.restore_with_base(Some(temp_dir.path())).unwrap();

        assert!(nested_file.exists());
        assert_eq!(fs::read_to_string(&nested_file).unwrap(), "deep content");
    }

    #[test]
    fn test_snapshot_path_traversal_protection() {
        // Note: this deliberately exercises the *real* home directory, because
        // that is the boundary `restore()` defends when no base dir is given.
        let mut malicious_snapshot = metadata_only_snapshot(
            &Uuid::new_v4().to_string(),
            OperationType::Pull,
            Duration::zero(),
        );

        // A path that escapes home via `..`; canonicalization should catch it.
        let home = dirs::home_dir().unwrap();
        let evil_path = home.join("..").join("..").join("etc").join("passwd");

        malicious_snapshot.files.insert(
            evil_path.to_string_lossy().to_string(),
            b"malicious content".to_vec(),
        );

        let result = malicious_snapshot.restore();

        if let Err(err) = result {
            let err_msg = err.to_string();
            assert!(
                err_msg.contains("Security") || err_msg.contains("outside home"),
                "Error message should indicate security issue: {err_msg}"
            );
        } else {
            // If it didn't error, at least verify nothing was written outside home.
            assert!(
                !PathBuf::from("/etc/passwd").exists()
                    || !fs::read_to_string("/etc/passwd")
                        .unwrap_or_default()
                        .contains("malicious")
            );
        }
    }

    #[test]
    fn test_differential_snapshot_restore_with_deletions() {
        let temp_dir = tempdir().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");
        let file1 = temp_dir.path().join("file1.txt");
        let file2 = temp_dir.path().join("file2.txt");

        fs::write(&file1, b"content1").unwrap();
        fs::write(&file2, b"content2").unwrap();

        let snapshot1 = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1, &file2],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();
        snapshot1.save_to_disk(Some(&snapshots_dir)).unwrap();

        fs::remove_file(&file2).unwrap();

        let snapshot2 = Snapshot::create_differential_with_dir(
            OperationType::Pull,
            vec![&file1],
            None,
            Some(&snapshots_dir),
        )
        .unwrap();
        snapshot2.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Recreate file2 so the restore has something to delete.
        fs::write(&file2, b"should_be_deleted").unwrap();

        snapshot2
            .restore_with_base_and_snapshots(Some(temp_dir.path()), Some(&snapshots_dir))
            .unwrap();

        assert!(file1.exists(), "file1 should exist after restore");
        assert!(!file2.exists(), "file2 should be deleted after restore");
    }

    #[test]
    fn test_restore_rejects_traversal_without_creating_anything() {
        // The regression this guards: validation used to happen *after* the file
        // was created, because `canonicalize` needs the path to exist. A rejected
        // path therefore still got its parent directories and an empty file
        // written before the error was returned.
        let temp_dir = tempdir().unwrap();
        let allowed = temp_dir.path().join("allowed");
        fs::create_dir_all(&allowed).unwrap();

        // Outside `allowed`, and does not exist.
        let outside = temp_dir
            .path()
            .join("outside")
            .join("deep")
            .join("evil.txt");

        let mut snapshot = metadata_only_snapshot(
            &Uuid::new_v4().to_string(),
            OperationType::Pull,
            Duration::zero(),
        );
        snapshot.files.insert(
            outside.to_string_lossy().to_string(),
            b"malicious content".to_vec(),
        );

        let err = snapshot
            .restore_with_base(Some(&allowed))
            .expect_err("restore must reject a path outside the allowed base")
            .to_string();
        assert!(
            err.contains("Security") || err.contains("outside"),
            "unexpected error: {err}"
        );

        assert!(!outside.exists(), "the rejected file must not be created");
        assert!(
            !outside.parent().unwrap().exists(),
            "the rejected file's parent directories must not be created either"
        );
    }

    #[test]
    fn test_restore_rejects_dotdot_in_a_path_that_does_not_exist() {
        // `..` in a *non-existent* tail is the case canonicalization can no
        // longer collapse for us, so it has to be rejected explicitly.
        let temp_dir = tempdir().unwrap();
        let allowed = temp_dir.path().join("allowed");
        fs::create_dir_all(&allowed).unwrap();

        let escape = allowed
            .join("nope")
            .join("..")
            .join("..")
            .join("escape.txt");

        let mut snapshot = metadata_only_snapshot(
            &Uuid::new_v4().to_string(),
            OperationType::Pull,
            Duration::zero(),
        );
        snapshot
            .files
            .insert(escape.to_string_lossy().to_string(), b"escaped".to_vec());

        let err = snapshot
            .restore_with_base(Some(&allowed))
            .expect_err("restore must reject '..' in an unresolvable tail")
            .to_string();
        assert!(err.contains("Security"), "unexpected error: {err}");

        assert!(!temp_dir.path().join("escape.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_restore_does_not_follow_a_dangling_symlink_out_of_the_base() {
        // A dangling symlink reports `exists() == false` but `symlink_metadata()`
        // still sees it. Probing with `exists` would treat it as a plain missing
        // file, re-attach its name to a canonical in-base prefix, pass the check,
        // and then let `fs::write` follow it out of the sandbox.
        use std::os::unix::fs::symlink;

        let temp_dir = tempdir().unwrap();
        let allowed = temp_dir.path().join("allowed");
        fs::create_dir_all(&allowed).unwrap();

        let target_outside = temp_dir.path().join("victim.txt");
        let link = allowed.join("innocent.txt");
        symlink(&target_outside, &link).unwrap();
        assert!(!link.exists(), "symlink should be dangling");
        assert!(link.symlink_metadata().is_ok(), "but it is still an entry");

        let mut snapshot = metadata_only_snapshot(
            &Uuid::new_v4().to_string(),
            OperationType::Pull,
            Duration::zero(),
        );
        snapshot
            .files
            .insert(link.to_string_lossy().to_string(), b"pwned".to_vec());

        // Either it errors, or it writes through the link -- the second is the bug.
        let _ = snapshot.restore_with_base(Some(&allowed));

        assert!(
            !target_outside.exists(),
            "restore must not write through a dangling symlink to {}",
            target_outside.display()
        );
    }
}
