//! The artifact paths this machine last synced, per sync repository.
//!
//! Without such a record, a deletion is indistinguishable from a file the
//! machine never had. With it, one rule covers both directions: a path this
//! machine received before and no longer has is a deletion, and is removed
//! from the other side.
//!
//! The record is per machine and never enters the repository. An absent or
//! unreadable record reads as empty, which propagates no deletions at all.

use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::repo_record::RepoRecord;

/// Repo-relative artifact paths, sorted.
pub type TrackedPaths = BTreeSet<String>;

/// File name under `~/.claude` holding the record for every sync repository.
const TRACKED_FILE_NAME: &str = ".claude-code-sync-tracked.json";

// The mechanical record wrappers — one source in `repo_record`.
crate::repo_record_wrappers!(TRACKED_FILE_NAME, TrackedPaths);

/// Whether the record files `repo_root` under a non-canonical (legacy raw)
/// spelling — the next save rewrites the file to retire it.
pub fn has_aliased_entry(claude_dir: &Path, repo_root: &Path) -> bool {
    RepoRecord::<TrackedPaths>::read(claude_dir, TRACKED_FILE_NAME).has_aliased_entry(repo_root)
}

/// Merge a delta into whatever the record holds NOW, under ONE lock —
/// see [`bases::save_delta`] for why the load, merge, and write must be
/// a single unit.
pub fn save_delta(
    claude_dir: &Path,
    repo_root: &Path,
    inserts: TrackedPaths,
    removals: Vec<String>,
) -> Result<()> {
    RepoRecord::<TrackedPaths>::merge_under_lock(
        claude_dir,
        TRACKED_FILE_NAME,
        repo_root,
        |current| {
            for rel in &removals {
                current.remove(rel);
            }
            for rel in inserts {
                current.insert(rel);
            }
        },
    )
}

/// Undo-side counterpart of `save`, with the same per-key contract as
/// [`bases::restore_keys`]: only the keys the undone pull touched go back
/// to their pre-pull state; entries a later push recorded keep their
/// owner. An entry that empties out is forgotten.
pub fn restore_keys(
    claude_dir: &Path,
    repo_root: &Path,
    pre_pull: &TrackedPaths,
    touched: &[String],
) -> Result<()> {
    RepoRecord::<TrackedPaths>::restore_keys_under_lock(
        claude_dir,
        TRACKED_FILE_NAME,
        repo_root,
        pre_pull,
        touched,
    )
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

    #[cfg(unix)]
    #[test]
    fn a_legacy_raw_spelling_entry_is_an_aliased_one() {
        let real = tempfile::tempdir().unwrap();
        let via = tempfile::tempdir().unwrap();
        let link = via.path().join("linked-repo");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        let claude = tempfile::tempdir().unwrap();

        // Modern save: keyed canonically, nothing to retire.
        save(claude.path(), &link, paths(&["a"])).unwrap();
        assert!(!has_aliased_entry(claude.path(), &link));

        // A pre-canonicalization version filed the repo under its raw
        // spelling: the next save rewrites the file to retire it.
        std::fs::write(
            record_path(claude.path()),
            format!(r#"{{"repos": {{ "{}": ["a"] }}}}"#, link.display()),
        )
        .unwrap();
        // Detected through the spelling family the machine actually uses
        // (that of the path plan_pull is given).
        assert!(has_aliased_entry(claude.path(), &link));
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
