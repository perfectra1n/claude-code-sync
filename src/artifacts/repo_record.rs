//! Repo-scoped records under `~/.claude`: one JSON file, one entry per sync
//! repository. Shared by the tracked-paths and base-hashes records, which
//! differ only in what they store per repository.
//!
//! Entries are keyed by the repository's *canonical* path, so spelling
//! changes of the same checkout (`~/repo`, a symlink to it, a trailing
//! slash) keep sharing one entry instead of silently forking the record.
//! Lookups also honor the raw spelling, because a record written by an
//! older version may still be filed under it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Serializes read-modify-write cycles on the shared record files across
/// processes — without it, two syncs against different repositories would
/// each write the whole file and silently erase each other's entries
/// (last writer wins, protection included).
///
/// An OS file lock (`flock` / `LockFileEx`) on a marker file, held for
/// the duration of one load-merge-write: the kernel releases it when the
/// holder exits or crashes, so there is no staleness to judge, no pid to
/// inspect, and no break to race — the entire class of "is this lock
/// dead?" bugs cannot exist. A lock that cannot be taken within
/// `LOCK_GIVE_UP_AFTER` is an ERROR (see `acquire`): the callers degrade
/// their record save to a warning rather than write unprotected.
pub(super) struct RecordLock {
    file: Option<fs::File>,
}

/// A legitimate record write is sub-second; a holder still running after
/// this window is either wedged or contended, and waiting longer cannot
/// help — every failure is an error, never a proceed-unprotected.
const LOCK_GIVE_UP_AFTER: Duration = Duration::from_secs(15);

impl RecordLock {
    /// Acquire the lock, or explain why not. Every failure is an ERROR:
    /// writing the shared records unprotected would silently
    /// last-writer-win the holder's entries away — a record save that
    /// degrades to a warning for one round costs far less.
    fn acquire(claude_dir: &Path) -> Result<RecordLock> {
        // The marker file is never deleted: an OS lock does not care
        // about the file existing, only about who holds it.
        let path = claude_dir.join(".claude-code-sync-records.lock");
        let open = || -> Result<fs::File> {
            fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(&path)
                .with_context(|| format!("cannot open record lock {}", path.display()))
        };
        let file = match open() {
            Ok(file) => file,
            // A missing Claude directory is one create away — and if the
            // create fails, no amount of retrying will fix it (the same
            // goes for every other permanent open error: a component that
            // is a regular file, a read-only filesystem). Error in
            // milliseconds, never spin the give-up window.
            Err(_) => {
                fs::create_dir_all(claude_dir)
                    .with_context(|| format!("cannot create {}", claude_dir.display()))?;
                open().inspect_err(|_| {
                    log::warn!(
                        "the directory exists now, but the lock file {} still cannot be \
                         opened — check its own permissions",
                        path.display()
                    )
                })?
            }
        };
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(RecordLock { file: Some(file) }),
                // Held by someone else: wait, within the give-up window.
                Err(std::fs::TryLockError::WouldBlock) => {
                    if start.elapsed() > LOCK_GIVE_UP_AFTER {
                        return Err(anyhow::anyhow!(
                            "record lock {} not acquired in {:?} (holder still running); \
                             refusing to write the shared records unprotected",
                            path.display(),
                            LOCK_GIVE_UP_AFTER
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                // An I/O error is not contention (a filesystem without
                // flock support, say): retrying cannot help, and spinning
                // the give-up window on every record save would add 15
                // seconds per save on exactly the machines already
                // having trouble.
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(anyhow::anyhow!(
                        "record lock {} cannot be taken on this filesystem: {e}",
                        path.display()
                    ));
                }
            }
        }
    }
}

/// Take the inter-process record lock for the duration of the returned
/// guard's scope. Errors are part of the contract (see `acquire`): the
/// callers degrade their record save to a warning instead of writing
/// unprotected.
pub(super) fn record_lock(claude_dir: &Path) -> Result<RecordLock> {
    RecordLock::acquire(claude_dir)
}

/// Whether the record lock is SUPPORTED here — a plan-time probe that
/// never waits: one try_lock, and contention (WouldBlock) counts as
/// SUCCESS (the lock works; another sync just holds it). Only a hard
/// error — a filesystem without flock support, which fails every save —
/// comes back as Err, so the plan can announce the permanently-off
/// protections once, where its other record-health warnings live.
pub fn record_lock_probe(claude_dir: &Path) -> Result<()> {
    let path = claude_dir.join(".claude-code-sync-records.lock");
    // No `create`: the probe must not plant the marker file as a side
    // effect of a read-only command — an absent lock file means locking
    // trivially works (nothing to contend with, and the first save
    // creates it).
    let file = match std::fs::OpenOptions::new()
        .truncate(false)
        .write(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    match file.try_lock() {
        Ok(_guard) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => Ok(()),
        Err(std::fs::TryLockError::Error(e)) => Err(anyhow::anyhow!(
            "record lock {} cannot be taken on this filesystem: {e}",
            path.display()
        )),
    }
}

/// Whether `path` carries `file_name` under ANY directory, with no
/// Claude-dir anchoring. Only meaningful on paths already known to be
/// records (the snapshot's explicit declarations): a synced artifact can
/// share a record's name, which is precisely why undo never uses this to
/// DISCOVER records, only to pick a parser for a declared key. One shared
/// helper so the per-record copies cannot drift.
pub(super) fn is_record_file(path: &Path, file_name: &str) -> bool {
    path.file_name().is_some_and(|n| n == file_name)
}

impl Drop for RecordLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
        }
        // A crashed holder needs no cleanup: the kernel released the lock
        // when the process died.
    }
}

/// A record file mapping each sync repository to the value this machine
/// last recorded for it.
#[derive(Debug, Serialize, Deserialize)]
pub struct RepoRecord<V> {
    // An explicit path (not `#[serde(default)]`): the derive's bound
    // inference would then demand `V: Default`, which the value types have
    // no reason to implement.
    #[serde(default = "empty_repos")]
    repos: BTreeMap<String, V>,
}

fn empty_repos<V>() -> BTreeMap<String, V> {
    BTreeMap::new()
}

/// The per-entry shape `merge_under_lock` needs: a default (an absent
/// entry) and emptiness (an emptied entry is forgotten, not planted as
/// `"<repo>": {}`). Implemented by both record value types.
pub trait EntryValue: Default {
    fn is_empty(&self) -> bool;
    /// Restore one touched key to its PRE-PULL state from `pre_pull`:
    /// present there = set (to that value), absent = remove. One method
    /// so the shared restore cannot drift between the map- and set-shaped
    /// records.
    fn restore_key(&mut self, key: &str, pre_pull: &Self);
}

impl EntryValue for BTreeMap<String, String> {
    fn is_empty(&self) -> bool {
        BTreeMap::is_empty(self)
    }
    fn restore_key(&mut self, key: &str, pre_pull: &Self) {
        match pre_pull.get(key) {
            Some(hash) => {
                self.insert(key.to_string(), hash.clone());
            }
            None => {
                self.remove(key);
            }
        }
    }
}

impl EntryValue for BTreeSet<String> {
    fn is_empty(&self) -> bool {
        BTreeSet::is_empty(self)
    }
    fn restore_key(&mut self, key: &str, pre_pull: &Self) {
        if pre_pull.contains(key) {
            self.insert(key.to_string());
        } else {
            self.remove(key);
        }
    }
}

// Manual so an empty record needs nothing from `V` — a record read for a
// repo that has none must default even when `V` has no meaningful default.
impl<V> Default for RepoRecord<V> {
    fn default() -> Self {
        Self {
            repos: BTreeMap::new(),
        }
    }
}

/// The canonical spelling of a repository root. Falls back to the given
/// path when it cannot be resolved (a record may outlive its repository).
///
/// Known limit: a repository that is *moved* (canonicalize resolves, but
/// to a new path no old spelling matches) orphans its entries — they sit
/// in the shared file, protect nothing, and only a `forget` under an old
/// spelling (or hand removal) clears them. Accepted: the failure is
/// inert, and keying by anything but the path (a repo id file, say)
/// would put machine state inside the synced repository.
fn repo_key(repo_root: &Path) -> String {
    fs::canonicalize(repo_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| repo_root.to_string_lossy().into_owned())
}

/// Every spelling the repository may be filed under: the canonical key
/// first, then the raw path (an older version wrote that one).
fn repo_keys(repo_root: &Path) -> Vec<String> {
    let mut keys = vec![repo_key(repo_root)];
    let raw = repo_root.to_string_lossy().into_owned();
    if keys[0] != raw {
        keys.push(raw);
    }
    keys
}

/// Whether the record file on disk parses — with the CONCRETE value
/// type, not `serde_json::Value`: a record whose values have the wrong
/// shape (a hash where a set belongs, a bare string where a map does)
/// is corrupt for its loader even though it is valid JSON, and a
/// Value-typed check would wave it through while `load` silently reads
/// an empty record. An ABSENT record legitimately reads as empty (the
/// fresh-machine and upgrade path); a PRESENT but unreadable or corrupt
/// one is not that — callers warn, because silently reading it as empty
/// turns off everything the record protects.
pub fn intact<V: for<'de> serde::Deserialize<'de>>(record_path: &Path) -> bool {
    match fs::read_to_string(record_path) {
        Ok(text) => serde_json::from_str::<RepoRecord<V>>(&text).is_ok(),
        // ABSENT is the legitimate fresh-machine state — only a PRESENT
        // but unreadable file is corruption.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// The mechanical per-record wrappers every record module needs, so the
/// two cannot drift: path resolution, file-name recognition, entry
/// extraction from raw bytes, load, forget, and save — everything that
/// differs only in the file-name constant and the value type.
#[macro_export]
macro_rules! repo_record_wrappers {
    ($file_name:expr, $value:ty) => {
        /// Where the record lives for a given Claude directory.
        pub fn record_path(claude_dir: &Path) -> PathBuf {
            claude_dir.join($file_name)
        }

        /// Whether `path` carries this record's file name under ANY
        /// directory — only meaningful on paths already known to be
        /// records (the snapshot's explicit declarations): a synced
        /// artifact can share the name, which is precisely why undo
        /// never uses this to DISCOVER records, only to pick a parser
        /// for a declared key.
        pub fn is_record_path(path: &Path) -> bool {
            $crate::artifacts::repo_record::is_record_file(path, $file_name)
        }

        /// The entry `repo_root` had in a whole-record file's raw bytes
        /// (a pre-pull snapshot of the shared record, say). `None` when
        /// the bytes do not PARSE — a caller must treat that as corrupt
        /// (the plan-time `intact()` rule: a present-but-broken record
        /// turns off everything it protects and must be loud, never
        /// silently read as an empty entry); an entry-less file for a
        /// repo that had none still reads as `Some(empty)`.
        pub fn entry_from_record_bytes(bytes: &[u8], repo_root: &Path) -> Option<$value> {
            match RepoRecord::<$value>::from_bytes_strict(bytes) {
                Some(record) => Some(record.get(repo_root).unwrap_or_default()),
                None => None,
            }
        }

        /// What this machine last recorded for `repo_root`.
        pub fn load(claude_dir: &Path, repo_root: &Path) -> $value {
            RepoRecord::<$value>::read(claude_dir, $file_name)
                .get(repo_root)
                .unwrap_or_default()
        }

        /// Forget everything this machine recorded about `repo_root`.
        pub fn forget(claude_dir: &Path, repo_root: &Path) -> Result<()> {
            let _lock = $crate::artifacts::repo_record::record_lock(claude_dir)?;
            let mut record =
                RepoRecord::<$value>::try_read(claude_dir, $file_name)?.unwrap_or_default();
            if record.remove(repo_root) {
                record.write(claude_dir, $file_name)?;
            }
            Ok(())
        }

        /// Record the entry for `repo_root`, replacing the previous one.
        pub fn save(claude_dir: &Path, repo_root: &Path, entry: $value) -> Result<()> {
            let _lock = $crate::artifacts::repo_record::record_lock(claude_dir)?;
            let mut record =
                RepoRecord::<$value>::try_read(claude_dir, $file_name)?.unwrap_or_default();
            record.insert(repo_root, entry);
            record.write(claude_dir, $file_name)
        }
    };
}

impl<V: Clone + for<'de> Deserialize<'de> + Serialize> RepoRecord<V> {
    /// The record parsed from raw file bytes (a pre-pull snapshot of the
    /// shared file, say). Unparseable bytes read as empty — same contract
    /// as `read`.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        serde_json::from_slice(bytes).unwrap_or_default()
    }

    /// The record parsed from raw bytes, or `None` when they do not
    /// parse: callers that must DISTINGUISH corrupt from empty (the
    /// undo's record surgery) use this, so a truncated snapshot blob
    /// warns and pins instead of silently reading as an empty pre-pull
    /// entry and deleting live keys.
    pub fn from_bytes_strict(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }

    /// The record as stored under `claude_dir/file_name`. An absent or
    /// unreadable file reads as empty: a fresh machine, or one upgrading
    /// from a version without the record, loses nothing it had before.
    pub fn read(claude_dir: &Path, file_name: &str) -> Self {
        let Ok(text) = fs::read_to_string(claude_dir.join(file_name)) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// The record parsed from disk OR `None` when the bytes do not
    /// parse — every saving path (`save_delta`, `forget`) checks it
    /// before its locked write: writing anything over a corrupt document
    /// would erase every other repository's entries (the unwrap_to_default
    /// path makes the lock think the file is empty). The check is a
    /// fail-closed companion to `intact` (which only probes in plan_pull).
    pub fn try_read(claude_dir: &Path, file_name: &str) -> Result<Option<Self>> {
        let path = claude_dir.join(file_name);
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(record) => Ok(Some(record)),
                Err(e) => Err(anyhow::anyhow!(
                    "record {} is corrupt and cannot be merged into safely ({e}); \
                     fix or remove it before syncing again",
                    path.display()
                )),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow::anyhow!(
                "cannot read record {}: {e}",
                path.display()
            )),
        }
    }

    /// The value recorded for `repo_root`, under any spelling of it.
    pub fn get(&self, repo_root: &Path) -> Option<V> {
        repo_keys(repo_root)
            .iter()
            .find_map(|key| self.repos.get(key).cloned())
    }

    /// Record `value` for `repo_root` under its canonical key, retiring any
    /// raw-spelling alias an older version left behind.
    pub fn insert(&mut self, repo_root: &Path, value: V) {
        let mut keys = repo_keys(repo_root);
        // The canonical key comes first; the rest are legacy spellings.
        let canonical = keys.remove(0);
        for key in keys {
            self.repos.remove(&key);
        }
        self.repos.insert(canonical, value);
    }

    /// Forget everything recorded about `repo_root`, under any spelling.
    pub fn remove(&mut self, repo_root: &Path) -> bool {
        let mut removed = false;
        for key in repo_keys(repo_root) {
            removed |= self.repos.remove(&key).is_some();
        }
        removed
    }

    /// Whether `repo_root` is filed under a non-canonical spelling — an
    /// older version's raw key. The next `insert` retires it, which
    /// rewrites the file; writers that predict their writes need to know.
    pub fn has_aliased_entry(&self, repo_root: &Path) -> bool {
        let keys = repo_keys(repo_root);
        keys[1..].iter().any(|key| self.repos.contains_key(key))
    }

    /// The ONE locked read-merge-write every record mutation goes through:
    /// the lock covers the LOAD half too, so a same-repo writer landing
    /// between a caller's fresh read and its save cannot be silently
    /// overwritten (a load outside this function races every other
    /// writer — `save`'s own lock covers only its write). `merge` mutates
    /// this repository's entry in place; an entry that empties out is
    /// forgotten rather than planted as `"<repo>": {}`.
    pub fn merge_under_lock(
        claude_dir: &Path,
        file_name: &str,
        repo_root: &Path,
        merge: impl FnOnce(&mut V),
    ) -> Result<()>
    where
        V: EntryValue,
    {
        let _lock = record_lock(claude_dir)?;
        // Fail-closed on a corrupt record: see `try_read`'s doc — a
        // corrupt file reading as empty would erase every other
        // repository's entries on the first save.
        let mut record = Self::try_read(claude_dir, file_name)?.unwrap_or_default();
        let mut current: V = record.get(repo_root).unwrap_or_default();
        merge(&mut current);
        if current.is_empty() {
            if record.remove(repo_root) {
                return record.write(claude_dir, file_name);
            }
            return Ok(());
        }
        record.insert(repo_root, current);
        record.write(claude_dir, file_name)
    }

    /// The undo's per-key restore, shared by BOTH records: set each
    /// touched key back to its pre-pull value (absent pre-pull = remove),
    /// in one locked merge — a later push's entries keep their owner.
    /// One source, so the two wrappers cannot drift (one restoring
    /// per-key while the other goes wholesale would silently strand half
    /// the undo).
    pub fn restore_keys_under_lock(
        claude_dir: &Path,
        file_name: &str,
        repo_root: &Path,
        pre_pull: &V,
        touched: &[String],
    ) -> Result<()>
    where
        V: EntryValue,
    {
        Self::merge_under_lock(claude_dir, file_name, repo_root, |current| {
            for key in touched {
                current.restore_key(key, pre_pull);
            }
        })
    }

    /// Write the record back atomically (temp file + rename), skipping the
    /// write when the file already holds exactly this content — an unchanged
    /// sync must not churn the file (or its mtime) for nothing. A crash
    /// mid-write must not truncate the record: both files reading as empty
    /// is one unprotected, remote-wins cycle for every repository.
    pub fn write(&self, claude_dir: &Path, file_name: &str) -> Result<()> {
        let path: PathBuf = claude_dir.join(file_name);
        let text = serde_json::to_string_pretty(self)?;
        if let Ok(existing) = fs::read_to_string(&path) {
            if existing == text {
                return Ok(());
            }
        }
        fs::create_dir_all(claude_dir)?;
        let tmp = tempfile::NamedTempFile::new_in(claude_dir)
            .with_context(|| format!("Failed to stage {}", path.display()))?;
        fs::write(tmp.path(), &text)?;
        // Flush to the disk BEFORE the rename: with delayed allocation
        // (ext4/xfs), a rename landing before the data blocks turns a
        // power loss right after this write into a zero-length or
        // truncated record — the exact corruption the temp+rename exists
        // to prevent. A record reading as empty is one unprotected,
        // remote-wins cycle for every repository, so the fsync cost is
        // the cheap half of the invariant.
        tmp.as_file()
            .sync_all()
            .with_context(|| format!("Failed to flush {}", path.display()))?;
        tmp.persist(&path)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "record-test.json";

    #[test]
    fn an_absent_file_reads_as_an_empty_record() {
        let claude = tempfile::tempdir().unwrap();
        assert!(RepoRecord::<u8>::read(claude.path(), FILE)
            .get(Path::new("/repo"))
            .is_none());
    }

    #[test]
    fn a_corrupt_file_reads_as_an_empty_record() {
        let claude = tempfile::tempdir().unwrap();
        fs::write(claude.path().join(FILE), "{ not json").unwrap();
        assert!(RepoRecord::<u8>::read(claude.path(), FILE)
            .get(Path::new("/repo"))
            .is_none());
    }

    #[test]
    fn repositories_are_recorded_separately() {
        let claude = tempfile::tempdir().unwrap();
        let mut record = RepoRecord::read(claude.path(), FILE);
        record.insert(Path::new("/one"), 1);
        record.insert(Path::new("/two"), 2);
        record.write(claude.path(), FILE).unwrap();

        let record = RepoRecord::<u8>::read(claude.path(), FILE);
        assert_eq!(record.get(Path::new("/one")), Some(1));
        assert_eq!(record.get(Path::new("/two")), Some(2));
    }

    #[test]
    fn inserting_replaces_the_previous_value_for_that_repository() {
        let claude = tempfile::tempdir().unwrap();
        let mut record = RepoRecord::read(claude.path(), FILE);
        record.insert(Path::new("/one"), 1);
        record.insert(Path::new("/one"), 2);
        assert_eq!(record.get(Path::new("/one")), Some(2));
    }

    #[test]
    fn removing_one_repository_leaves_the_others_alone() {
        let claude = tempfile::tempdir().unwrap();
        let mut record = RepoRecord::read(claude.path(), FILE);
        record.insert(Path::new("/one"), 1);
        record.insert(Path::new("/two"), 2);
        assert!(record.remove(Path::new("/one")));
        assert!(record.get(Path::new("/one")).is_none());
        assert_eq!(record.get(Path::new("/two")), Some(2));

        assert!(!record.remove(Path::new("/never-synced")));
    }

    #[cfg(unix)]
    #[test]
    fn a_repository_reached_through_a_symlink_shares_one_entry() {
        let real = tempfile::tempdir().unwrap();
        let via = tempfile::tempdir().unwrap();
        let link = via.path().join("linked-repo");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let claude = tempfile::tempdir().unwrap();
        let mut record = RepoRecord::read(claude.path(), FILE);
        record.insert(&link, 1);
        record.write(claude.path(), FILE).unwrap();

        // Saved through the symlink, read through the real path — and back.
        let record = RepoRecord::<u8>::read(claude.path(), FILE);
        assert_eq!(record.get(real.path()), Some(1), "same checkout, one entry");
        assert_eq!(record.get(&link), Some(1));

        // Forgetting through either spelling clears the single entry.
        let mut record = RepoRecord::<u8>::read(claude.path(), FILE);
        assert!(record.remove(real.path()));
        assert!(record.get(&link).is_none());
    }

    #[test]
    fn a_record_written_by_an_older_version_is_still_found() {
        // Legacy layout: the raw (non-canonical) spelling as the key.
        let claude = tempfile::tempdir().unwrap();
        let raw = "{\"repos\": {\"/definitely/not/canonical\": 7}}";
        fs::write(claude.path().join(FILE), raw).unwrap();

        let record = RepoRecord::<u8>::read(claude.path(), FILE);
        assert_eq!(record.get(Path::new("/definitely/not/canonical")), Some(7));
    }

    #[test]
    fn writing_identical_content_leaves_the_file_untouched() {
        let claude = tempfile::tempdir().unwrap();
        let mut record = RepoRecord::read(claude.path(), FILE);
        record.insert(Path::new("/one"), 1);
        record.write(claude.path(), FILE).unwrap();
        let before = fs::metadata(claude.path().join(FILE))
            .unwrap()
            .modified()
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        record.write(claude.path(), FILE).unwrap();
        let after = fs::metadata(claude.path().join(FILE))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "an unchanged record is not rewritten");
    }

    #[test]
    fn a_held_lock_blocks_a_second_taker_until_released() {
        let claude = tempfile::tempdir().unwrap();
        let held = record_lock(claude.path()).unwrap();
        // The kernel owns the arbitration now: a second taker — even in
        // the same process, on its own handle — fails while held.
        let second = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(claude.path().join(".claude-code-sync-records.lock"))
            .unwrap();
        assert!(second.try_lock().is_err(), "the held lock blocks");
        drop(held);
        assert!(second.try_lock().is_ok(), "release reopens it");
        let _ = second.unlock();
    }

    #[test]
    fn a_released_lock_can_be_taken_again() {
        let claude = tempfile::tempdir().unwrap();
        {
            let _held = record_lock(claude.path()).unwrap();
        }
        // The marker file stays (an OS lock does not care), and the next
        // taker walks straight in — no staleness to judge, no break.
        let _again = record_lock(claude.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_directory_does_not_spin_the_give_up_window() {
        use std::os::unix::fs::PermissionsExt;

        let outer = tempfile::tempdir().unwrap();
        let claude = outer.path().join("claude");
        fs::create_dir(&claude).unwrap();
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o555)).unwrap();

        // Permission denied up front: the sentinel comes back in
        // milliseconds, not after the 15s give-up window.
        let start = std::time::Instant::now();
        assert!(record_lock(&claude).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(2));

        fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[cfg(test)]
mod intact_tests {
    use super::*;

    #[test]
    fn wrong_shape_values_count_as_corrupt_not_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record.json");

        // Valid JSON, wrong VALUE shape: a bare string where the bases
        // record's map belongs. A Value-typed check waves it through
        // while load() silently reads an empty record — protection off,
        // no warning.
        fs::write(&path, r#"{"repos": {"/repo": "oops"}}"#).unwrap();
        assert!(
            !intact::<std::collections::BTreeMap<String, String>>(&path),
            "a string where a map belongs is corruption"
        );
        assert!(
            intact::<String>(&path),
            "the same file IS intact for a record whose values are strings"
        );

        // The empty-file / fresh-machine case stays legitimate.
        assert!(intact::<String>(&dir.path().join("absent.json")));
    }
}
