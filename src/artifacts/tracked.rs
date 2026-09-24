//! The artifact paths this machine last synced, per sync repository.
//!
//! Without such a record, a deletion is indistinguishable from a file the
//! machine never had. With it, one rule covers both directions: a path this
//! machine received before and no longer has is a deletion, and is removed
//! from the other side.
//!
//! The record is per machine and never enters the repository. An absent or
//! unreadable record reads as empty, which propagates no deletions at all.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Repo-relative artifact paths, sorted.
pub type TrackedPaths = BTreeSet<String>;

/// File name under `~/.claude` holding the record for every sync repository.
const TRACKED_FILE_NAME: &str = ".claude-code-sync-tracked.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrackedRecord {
    #[serde(default)]
    repos: BTreeMap<String, TrackedPaths>,
}

/// Where the record lives for a given Claude directory.
pub fn record_path(claude_dir: &Path) -> PathBuf {
    claude_dir.join(TRACKED_FILE_NAME)
}

fn read_record(claude_dir: &Path) -> TrackedRecord {
    let Ok(text) = std::fs::read_to_string(record_path(claude_dir)) else {
        return TrackedRecord::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn repo_key(repo_root: &Path) -> String {
    repo_root.to_string_lossy().to_string()
}

/// The paths this machine last synced with `repo_root`.
pub fn load(claude_dir: &Path, repo_root: &Path) -> TrackedPaths {
    read_record(claude_dir)
        .repos
        .remove(&repo_key(repo_root))
        .unwrap_or_default()
}

/// Forget everything this machine recorded about `repo_root`.
///
/// Used when the repository is rewound under this machine's feet (`undo push`):
/// the record would claim paths the repo no longer has, and the next pull would
/// read that as a deletion. Forgetting restores the fresh-machine state, which
/// deletes nothing and re-learns on the next sync.
pub fn forget(claude_dir: &Path, repo_root: &Path) -> Result<()> {
    let mut record = read_record(claude_dir);
    if record.repos.remove(&repo_key(repo_root)).is_none() {
        return Ok(());
    }
    write_record(claude_dir, &record)
}

/// Record the paths this machine now holds in common with `repo_root`.
pub fn save(claude_dir: &Path, repo_root: &Path, paths: TrackedPaths) -> Result<()> {
    let mut record = read_record(claude_dir);
    record.repos.insert(repo_key(repo_root), paths);
    write_record(claude_dir, &record)
}

fn write_record(claude_dir: &Path, record: &TrackedRecord) -> Result<()> {
    let path = record_path(claude_dir);
    std::fs::create_dir_all(claude_dir)?;
    let text = serde_json::to_string_pretty(record)?;
    std::fs::write(&path, text).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(entries: &[&str]) -> TrackedPaths {
        entries.iter().map(|e| (*e).to_string()).collect()
    }

    #[test]
    fn an_absent_record_tracks_nothing() {
        let claude = tempfile::tempdir().unwrap();
        assert!(load(claude.path(), Path::new("/repo")).is_empty());
    }

    #[test]
    fn a_corrupt_record_tracks_nothing_instead_of_failing() {
        let claude = tempfile::tempdir().unwrap();
        std::fs::write(record_path(claude.path()), "{ not json").unwrap();
        assert!(load(claude.path(), Path::new("/repo")).is_empty());
    }

    #[test]
    fn each_repository_is_tracked_separately() {
        let claude = tempfile::tempdir().unwrap();
        save(
            claude.path(),
            Path::new("/one"),
            paths(&["artifacts/skills/a.md"]),
        )
        .unwrap();
        save(
            claude.path(),
            Path::new("/two"),
            paths(&["artifacts/rules/b.md"]),
        )
        .unwrap();

        assert_eq!(
            load(claude.path(), Path::new("/one")),
            paths(&["artifacts/skills/a.md"])
        );
        assert_eq!(
            load(claude.path(), Path::new("/two")),
            paths(&["artifacts/rules/b.md"])
        );
    }

    #[test]
    fn forgetting_one_repository_leaves_the_others_alone() {
        let claude = tempfile::tempdir().unwrap();
        save(claude.path(), Path::new("/one"), paths(&["a"])).unwrap();
        save(claude.path(), Path::new("/two"), paths(&["b"])).unwrap();

        forget(claude.path(), Path::new("/one")).unwrap();

        assert!(load(claude.path(), Path::new("/one")).is_empty());
        assert_eq!(load(claude.path(), Path::new("/two")), paths(&["b"]));
        forget(claude.path(), Path::new("/never-synced")).unwrap();
    }

    #[test]
    fn saving_replaces_the_previous_set_for_that_repository() {
        let claude = tempfile::tempdir().unwrap();
        save(claude.path(), Path::new("/one"), paths(&["a", "b"])).unwrap();
        save(claude.path(), Path::new("/one"), paths(&["b"])).unwrap();
        assert_eq!(load(claude.path(), Path::new("/one")), paths(&["b"]));
    }
}
