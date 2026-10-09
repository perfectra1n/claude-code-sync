//! The artifact contents this machine last synced, per sync repository.
//!
//! Without such a record, a pull cannot tell a locally-modified file from a
//! stale one: any local copy that differs from the repository is "remote
//! wins" material. With it, one rule closes the data-loss window: a local
//! file whose bytes differ from BOTH the repository and the record was
//! edited here since the last sync, and a pull must keep it (a `push` is
//! what publishes it) instead of overwriting it.
//!
//! The record is per machine and never enters the repository. An absent or
//! unreadable record reads as empty, which restores the pre-record behavior
//! (remote wins) — a fresh machine, or one upgrading from a version without
//! the record, loses nothing it had before.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::repo_record::RepoRecord;

/// Repo-relative artifact path to the SHA-256 of the local bytes at the last
/// sync, sorted.
pub type BaseHashes = BTreeMap<String, String>;

/// File name under `~/.claude` holding the record for every sync repository.
const BASES_FILE_NAME: &str = ".claude-code-sync-bases.json";

/// The stable identity of some local bytes: what the record stores.
pub fn hash_bytes(bytes: &[u8]) -> String {
    crate::digest::sha256_hex(bytes)
}

// The mechanical record wrappers (path, name recognition, entry
// extraction, load, forget, save) — one source in `repo_record`.
//
// bases-specific note on `forget`: `undo push` deliberately does NOT
// call it — an entry describing the pushed bytes keeps post-push local
// edits protected against the rewound repository (a forgotten entry
// reads as "never synced", and the next pull remote-wins straight over
// the edit).
crate::repo_record_wrappers!(BASES_FILE_NAME, BaseHashes);

/// Merge a delta into whatever the record holds NOW — one locked
/// load-merge-write unit (see `RepoRecord::merge_under_lock`).
pub fn save_delta(
    claude_dir: &Path,
    repo_root: &Path,
    inserts: BaseHashes,
    removals: Vec<String>,
) -> Result<()> {
    RepoRecord::<BaseHashes>::merge_under_lock(claude_dir, BASES_FILE_NAME, repo_root, |current| {
        for rel in &removals {
            current.remove(rel);
        }
        for (rel, hash) in inserts {
            current.insert(rel, hash);
        }
    })
}

/// Undo-side counterpart of `save`: restore the PRE-PULL value of exactly
/// the keys the undone pull touched, leaving every other key to whatever
/// recorded it later — a push that ran after the pull owns those entries
/// now, and a wholesale save would erase them (dropping a file's base
/// makes its next post-push edit remote-wins material, the exact loss the
/// record exists to prevent).
pub fn restore_keys(
    claude_dir: &Path,
    repo_root: &Path,
    pre_pull: &BaseHashes,
    touched: &[String],
) -> Result<()> {
    RepoRecord::<BaseHashes>::restore_keys_under_lock(
        claude_dir,
        BASES_FILE_NAME,
        repo_root,
        pre_pull,
        touched,
    )
}

/// The base value recorded when a mid-pull create-skip could not read
/// the repo copy: the user's file must read dirty or the next pull
/// overwrites it. NOT a hash of any bytes — a real hash (even of empty
/// bytes, which a legitimately pushed empty file records) would collide
/// with that file's true base and read it as clean, remote-wins
/// material.
pub const PROTECTION_SENTINEL: &str = "mid-pull-create-protection";

/// Prefix of a [`held_base`]: a hex digest never starts with it.
const HELD_PREFIX: &str = "held:";

/// The base recorded when this machine deliberately holds a local version
/// apart from the repository's — a declined overwrite, or a file created
/// while a pull waited. It is NOT a common ancestor: it only names the
/// repository version that was set aside, so it never equals a content hash.
/// The local file therefore keeps reading edited (the next pull keeps it),
/// and the repository never reads "unchanged since the last sync" — which
/// would let the push publish the held file over the other machine's version.
pub fn held_base(repo_bytes: &[u8]) -> String {
    format!("{HELD_PREFIX}{}", hash_bytes(repo_bytes))
}

/// Whether a recorded base is a real synced version both sides once held —
/// not a [`held_base`] and not the [`PROTECTION_SENTINEL`].
pub fn is_common_ancestor(base: &str) -> bool {
    base != PROTECTION_SENTINEL && !base.starts_with(HELD_PREFIX)
}

/// Whether `local_bytes` were edited since the last sync.
///
/// Only a recorded file can be dirty: an unknown path keeps the legacy
/// remote-wins behavior, so upgrading installs and fresh clones see no
/// behavior change on their first sync.
pub fn is_dirty(recorded: Option<&String>, local_bytes: &[u8]) -> bool {
    let Some(base) = recorded else {
        return false;
    };
    // The sentinel always reads dirty: its whole job is protection, and
    // no genuine hash can equal it (see the constant's doc).
    base == PROTECTION_SENTINEL || base != &hash_bytes(local_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashes(entries: &[(&str, &str)]) -> BaseHashes {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn an_absent_record_hashes_nothing() {
        let claude = tempfile::tempdir().unwrap();
        assert!(load(claude.path(), Path::new("/repo")).is_empty());
    }

    #[test]
    fn a_corrupt_record_hashes_nothing_instead_of_failing() {
        let claude = tempfile::tempdir().unwrap();
        std::fs::write(record_path(claude.path()), "{ not json").unwrap();
        assert!(load(claude.path(), Path::new("/repo")).is_empty());
    }

    #[test]
    fn each_repository_is_recorded_separately() {
        let claude = tempfile::tempdir().unwrap();
        save(
            claude.path(),
            Path::new("/one"),
            hashes(&[("artifacts/skills/a.md", "aa")]),
        )
        .unwrap();
        save(
            claude.path(),
            Path::new("/two"),
            hashes(&[("artifacts/rules/b.md", "bb")]),
        )
        .unwrap();

        assert_eq!(
            load(claude.path(), Path::new("/one")),
            hashes(&[("artifacts/skills/a.md", "aa")])
        );
        assert_eq!(
            load(claude.path(), Path::new("/two")),
            hashes(&[("artifacts/rules/b.md", "bb")])
        );
    }

    #[test]
    fn forgetting_one_repository_leaves_the_others_alone() {
        let claude = tempfile::tempdir().unwrap();
        save(claude.path(), Path::new("/one"), hashes(&[("a", "aa")])).unwrap();
        save(claude.path(), Path::new("/two"), hashes(&[("b", "bb")])).unwrap();

        forget(claude.path(), Path::new("/one")).unwrap();

        assert!(load(claude.path(), Path::new("/one")).is_empty());
        assert_eq!(
            load(claude.path(), Path::new("/two")),
            hashes(&[("b", "bb")])
        );
        forget(claude.path(), Path::new("/never-synced")).unwrap();
    }

    #[test]
    fn saving_replaces_the_previous_set_for_that_repository() {
        let claude = tempfile::tempdir().unwrap();
        save(
            claude.path(),
            Path::new("/one"),
            hashes(&[("a", "aa"), ("b", "bb")]),
        )
        .unwrap();
        save(claude.path(), Path::new("/one"), hashes(&[("b", "bb")])).unwrap();
        assert_eq!(
            load(claude.path(), Path::new("/one")),
            hashes(&[("b", "bb")])
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_repository_reached_through_a_symlink_shares_one_record() {
        let real = tempfile::tempdir().unwrap();
        let via = tempfile::tempdir().unwrap();
        let link = via.path().join("linked-repo");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let claude = tempfile::tempdir().unwrap();
        save(claude.path(), &link, hashes(&[("a", "aa")])).unwrap();

        assert_eq!(load(claude.path(), real.path()), hashes(&[("a", "aa")]));
        assert_eq!(load(claude.path(), &link), hashes(&[("a", "aa")]));

        forget(claude.path(), &link).unwrap();
        assert!(load(claude.path(), real.path()).is_empty());
    }

    #[test]
    fn restoring_keys_keeps_entries_a_later_operation_recorded() {
        let claude = tempfile::tempdir().unwrap();
        let repo = Path::new("/repo");
        save(
            claude.path(),
            repo,
            hashes(&[("pulled", "old"), ("kept", "kk")]),
        )
        .unwrap();

        // The pull owns "pulled" (pre-pull: "old", post-pull: "new"); a
        // later push recorded "pushed". Undo restores only the touched
        // key — a whole-entry restore would erase the push's entry.
        let mut current = hashes(&[("pulled", "new"), ("pushed", "pp")]);
        current.insert("kept".to_string(), "kk".to_string());
        save(claude.path(), repo, current).unwrap();

        restore_keys(
            claude.path(),
            repo,
            &hashes(&[("pulled", "old")]),
            &["pulled".to_string()],
        )
        .unwrap();

        let after = load(claude.path(), repo);
        assert_eq!(after.get("pulled"), Some(&"old".to_string()));
        assert_eq!(
            after.get("pushed"),
            Some(&"pp".to_string()),
            "the later push's entry survives the undo"
        );
        assert_eq!(after.get("kept"), Some(&"kk".to_string()));
    }

    #[test]
    fn restoring_keys_a_key_the_pull_created_removes_it() {
        let claude = tempfile::tempdir().unwrap();
        let repo = Path::new("/repo");
        // Pre-pull: no entry. The pull recorded two keys; undo owns only
        // the one it touched — the other keeps its owner.
        save(
            claude.path(),
            repo,
            hashes(&[("pulled", "new"), ("pushed", "pp")]),
        )
        .unwrap();

        restore_keys(
            claude.path(),
            repo,
            &BaseHashes::new(),
            &["pulled".to_string()],
        )
        .unwrap();

        let after = load(claude.path(), repo);
        assert!(!after.contains_key("pulled"));
        assert_eq!(after.get("pushed"), Some(&"pp".to_string()));
    }

    #[test]
    fn restoring_away_the_last_entry_forgets_it() {
        let claude = tempfile::tempdir().unwrap();
        let repo = Path::new("/repo");
        save(claude.path(), repo, hashes(&[("pulled", "new")])).unwrap();

        restore_keys(
            claude.path(),
            repo,
            &BaseHashes::new(),
            &["pulled".to_string()],
        )
        .unwrap();

        assert!(load(claude.path(), repo).is_empty());
    }

    #[test]
    fn dirty_means_edited_since_the_recorded_sync() {
        let bytes = b"contents\n";
        let base = hash_bytes(bytes);

        // Unknown base: never dirty (legacy remote-wins behavior).
        assert!(!is_dirty(None, bytes));
        assert!(!is_dirty(None, b"edited\n"));

        // Recorded base: dirty exactly when the bytes moved on.
        assert!(!is_dirty(Some(&base), bytes));
        assert!(is_dirty(Some(&base), b"edited\n"));
    }
}
