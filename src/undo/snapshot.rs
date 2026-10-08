use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::history::OperationType;

/// Represents a snapshot of conversation files at a point in time
///
/// Snapshots are created before each sync operation to enable undo functionality.
/// They capture the complete state of all conversation files that might be affected.
///
/// ## Differential Snapshots
///
/// To save disk space, snapshots can be differential - only storing files that changed
/// since the previous snapshot. This is controlled by the `base_snapshot_id` field:
/// - `None`: Full snapshot containing all files
/// - `Some(id)`: Differential snapshot containing only changes since base snapshot
///
/// When restoring a differential snapshot, we recursively load the chain of base
/// snapshots to reconstruct the full state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unique identifier for this snapshot
    pub snapshot_id: String,

    /// When this snapshot was created
    pub timestamp: chrono::DateTime<chrono::Utc>,

    /// Type of operation this snapshot was created for
    pub operation_type: OperationType,

    /// Git commit hash before the operation (for push operations)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit_hash: Option<String>,

    /// Mapping of file paths (relative to Claude projects dir) to their content
    ///
    /// We store the raw bytes to preserve exact file state including encoding.
    /// The HashMap key is a string path for JSON serialization compatibility.
    ///
    /// For differential snapshots, only contains files that changed/were added.
    #[serde(with = "base64_map")]
    pub files: HashMap<String, Vec<u8>>,

    /// Git branch name at the time of snapshot
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// Base snapshot ID for differential snapshots
    ///
    /// If present, this snapshot only contains changes relative to the base.
    /// The full state can be reconstructed by loading the chain of snapshots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_snapshot_id: Option<String>,

    /// Files that were deleted since the base snapshot
    ///
    /// Only populated for differential snapshots. Lists file paths that existed
    /// in the base snapshot but should be removed when restoring this snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted_files: Vec<String>,

    /// Artifact record files (bases, tracked) this snapshot carries, at
    /// their pull-time spellings. They hold one entry per sync repository
    /// and are restored surgically — this repository's entry only, never
    /// the whole file — so undo needs to know exactly which keys are
    /// records; matching by file name alone could be hijacked by a synced
    /// artifact that happens to share the name. Absent on snapshots
    /// written before the field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub record_files: Vec<String>,

    /// Artifact record files the apply of this snapshot's pull may create.
    /// Undo forgets this repository's entry instead of deleting the shared
    /// file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_record_files: Vec<String>,

    /// Repo-relative keys the pull's apply writes or prunes in the BASES
    /// record, as the plan computed them. Undo restores the PRE-PULL value
    /// of exactly these keys — never the whole entry, which would erase
    /// entries a push recorded between the pull and its undo. `None` on
    /// snapshots written before the field existed (undo then falls back
    /// to the whole-entry restore those snapshots were written with);
    /// `Some(empty)` is a CURRENT snapshot whose pull owned no key — the
    /// restore is then a no-op, never a rewind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_touched_bases: Option<Vec<String>>,

    /// The TRACKED record's counterpart of `record_touched_bases`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_touched_tracked: Option<Vec<String>>,

    /// Files the pull intended to modify whose pre-pull backup FAILED
    /// (unreadable at snapshot time). The apply reads this and skips
    /// them: the pre-PR contract — no file is modified without a backup
    /// — must survive the never-fatal snapshot, or a transient read
    /// error at snapshot time that recovers before the apply turns into
    /// a modification undo cannot restore.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable_files: Vec<String>,

    /// Record-shaped blobs `promote_legacy_records` stripped from
    /// `files` (unverifiable placement). NOT serialized: the on-disk
    /// snapshot keeps them inside `files` untouched — this stash exists
    /// only so the undo's pinned re-save can put them BACK before
    /// overwriting the sole copy (the warning tells the user to recover
    /// them by hand from that file; persisting the stripped map would
    /// destroy what it points at).
    #[serde(skip, default)]
    pub stripped_blobs: Vec<(String, Vec<u8>)>,

    /// Set when an undo KEPT this snapshot (record-surgery warnings): it
    /// holds the sole copy of the unrestored record entries, and the
    /// regular snapshot cleanup must never delete it from under the
    /// user. Cleared by hand once the entries are recovered.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
}

/// serde helper for [`Snapshot::pinned`].
fn is_false(value: &bool) -> bool {
    !*value
}

/// Custom serialization for `HashMap<String, Vec<u8>>` using base64 encoding
///
/// This is necessary because JSON doesn't natively support binary data,
/// so we encode file contents as base64 strings for storage.
mod base64_map {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    pub fn serialize<S>(map: &HashMap<String, Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let base64_map: HashMap<String, String> = map
            .iter()
            .map(|(k, v)| (k.clone(), STANDARD.encode(v)))
            .collect();
        base64_map.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<String, Vec<u8>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let base64_map: HashMap<String, String> = HashMap::deserialize(deserializer)?;
        base64_map
            .into_iter()
            .map(|(k, v)| {
                STANDARD
                    .decode(&v)
                    .map(|bytes| (k, bytes))
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
}

impl Snapshot {
    /// Declare the shared record files a LEGACY snapshot (written before
    /// the declaration fields, e.g. by a previous release still inside
    /// the retention window) carries among `files` without declaring
    /// them — so both undo and its preview treat them surgically instead
    /// of the whole-file generic restore, which would clobber every
    /// other repository's entries.
    ///
    /// Recognition must not be by file name alone: a synced artifact can
    /// share the name (the decoy test pins that contract). A real record
    /// sits at the CLAUDE ROOT with every other snapshotted file under
    /// that same root, and it PARSES — a JSON object keyed by repository
    /// roots. A name-matching key counts as a legacy record only when
    /// its parent directory is an ancestor of every other snapshot key
    /// AND its bytes parse as a record. A decoy under skills/docs/ fails
    /// the first (nothing else lives under it), a text decoy at the root
    /// fails the second, and a snapshot of records alone (no artifacts
    /// to hijack) passes trivially.
    /// Returns whether anything was STRIPPED (a record-looking blob that
    /// could not be safely classified): the caller must warn and pin —
    /// the stripped bytes are the sole copy and are recoverable by hand.
    pub fn promote_legacy_records(&mut self) -> bool {
        if !self.record_files.is_empty() || !self.created_record_files.is_empty() {
            return false;
        }
        let is_record_name = |key: &str| {
            let path = Path::new(key);
            crate::artifacts::bases::is_record_path(path)
                || crate::artifacts::tracked::is_record_path(path)
        };
        let parses_as_record = |key: &str| {
            use crate::artifacts::repo_record::RepoRecord;
            let bytes = &self.files[key];
            RepoRecord::<crate::artifacts::tracked::TrackedPaths>::from_bytes_strict(bytes)
                .is_some()
                || RepoRecord::<crate::artifacts::bases::BaseHashes>::from_bytes_strict(bytes)
                    .is_some()
        };
        let others: Vec<&String> = self
            .files
            .keys()
            .filter(|key| !is_record_name(key))
            .collect();
        let mut legacy_records = Vec::new();
        let mut stripped = Vec::new();
        for key in self.files.keys() {
            if !is_record_name(key) || !parses_as_record(key) {
                // Not a record at all (a same-named artifact decoy stays
                // an ordinary file), or one whose shape disqualifies it.
                continue;
            }
            let ancestor_of_all = Path::new(key).parent().is_some_and(|parent| {
                others
                    .iter()
                    .all(|other| Path::new(other.as_str()).starts_with(parent))
            });
            if ancestor_of_all {
                legacy_records.push(key.clone());
            } else {
                // Record-shaped but unverifiable placement: restoring it
                // WHOLESALE would clobber every other repository's entries
                // (the exact loss the surgery exists to prevent), and
                // surgically is impossible without a trusted directory.
                // Strip it from the generic restore entirely — the bytes
                // are recoverable by hand from the kept file, and stashed
                // here so the pinned re-save puts them back.
                if let Some(bytes) = self.files.get(key) {
                    self.stripped_blobs.push((key.clone(), bytes.clone()));
                }
                stripped.push(key.clone());
            }
        }
        if !legacy_records.is_empty() {
            log::warn!(
                "Legacy snapshot carries {} undeclared shared record file(s); \
                 restoring their entries surgically",
                legacy_records.len()
            );
            self.record_files.extend(legacy_records);
        }
        let stripped_anything = !stripped.is_empty();
        for key in stripped {
            self.files.remove(&key);
        }
        stripped_anything
    }

    /// Declare the artifact record files a pull snapshot carries (or may
    /// create), so undo can restore this repository's entries surgically
    /// instead of the whole shared files. The rules live on the plan
    /// (`carries_*_record` / `may_create_*_record`) — the same ones
    /// `paths_to_snapshot` uses, so carrier and declaration cannot drift.
    pub fn attach_record_bookkeeping(
        &mut self,
        plan: &crate::artifacts::engine::PullPlan,
        claude_dir: &Path,
        interactive: bool,
    ) {
        let bases_record = crate::artifacts::bases::record_path(claude_dir);
        if plan.carries_bases_record(interactive) {
            self.record_files
                .push(bases_record.to_string_lossy().to_string());
        } else if plan.may_create_bases_record(interactive) {
            self.created_record_files
                .push(bases_record.to_string_lossy().to_string());
        }
        let tracked_record = crate::artifacts::tracked::record_path(claude_dir);
        if plan.carries_tracked_record() {
            self.record_files
                .push(tracked_record.to_string_lossy().to_string());
        } else if plan.may_create_tracked_record() {
            self.created_record_files
                .push(tracked_record.to_string_lossy().to_string());
        }
        // Which keys the apply owns in each record: the undo restores the
        // pre-pull value of exactly these, so entries a later push recorded
        // survive it (see `record_touched_bases`). Always set here — even
        // empty — so the undo can tell a CURRENT snapshot (per-key restore,
        // possibly of zero keys) from a pre-field one (whole-entry restore).
        self.record_touched_bases = Some(plan.touched_base_keys());
        self.record_touched_tracked = Some(plan.touched_tracked_keys.clone());
    }

    /// Create a new snapshot from a set of file paths
    ///
    /// # Arguments
    /// * `operation_type` - Type of operation this snapshot is for
    /// * `file_paths` - Iterator of file paths to include in snapshot
    /// * `commit_hash` - Optional git commit hash to store in the snapshot
    ///
    /// # Returns
    /// A new Snapshot instance with all file contents captured
    pub fn create<P, I>(
        operation_type: OperationType,
        file_paths: I,
        commit_hash: Option<&str>,
    ) -> Result<Self>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = P>,
    {
        let snapshot_id = Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now();
        let mut files = HashMap::new();
        let mut unreadable_files = Vec::new();

        // Capture current state of all specified files
        for path in file_paths {
            let path = path.as_ref();

            // Use direct read instead of checking existence first to avoid TOCTOU
            match fs::read(path) {
                Ok(content) => {
                    // Store with path as string for JSON compatibility
                    files.insert(path.to_string_lossy().to_string(), content);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // File doesn't exist - this is expected in some cases (e.g., deleted files)
                    // Just skip it without error
                    continue;
                }
                Err(e) => {
                    // Never fatal — the same rule every pull arm follows:
                    // one unreadable file must not block the whole sync.
                    // But the file is RECORDED: the apply consults this
                    // list and refuses to modify a file it could not
                    // back up (the pre-PR guarantee — nothing is modified
                    // without a pre-pull copy — survives the never-fatal
                    // snapshot; a transient permission error that recovers
                    // before the apply must not become an unbackupable
                    // modification).
                    log::warn!(
                        "Not snapshotting {} (unreadable: {e}); the pull will not modify it",
                        path.display()
                    );
                    unreadable_files.push(path.to_string_lossy().to_string());
                    continue;
                }
            }
        }

        Ok(Snapshot {
            snapshot_id,
            timestamp,
            operation_type,
            git_commit_hash: commit_hash.map(|s| s.to_string()),
            files,
            branch: None,
            base_snapshot_id: None,
            deleted_files: Vec::new(),
            record_files: Vec::new(),
            created_record_files: Vec::new(),
            record_touched_bases: None,
            record_touched_tracked: None,
            unreadable_files,
            stripped_blobs: Vec::new(),
            pinned: false,
        })
    }

    /// Save this snapshot to disk
    ///
    /// Snapshots are saved to `~/.claude-code-sync/snapshots/{snapshot_id}.json`
    ///
    /// # Arguments
    /// * `custom_path` - Optional custom directory to save snapshot (for testing)
    pub fn save_to_disk(&self, custom_path: Option<&Path>) -> Result<PathBuf> {
        let snapshot_dir = if let Some(path) = custom_path {
            path.to_path_buf()
        } else {
            Self::snapshots_dir()?
        };

        // Ensure snapshots directory exists
        fs::create_dir_all(&snapshot_dir).with_context(|| {
            format!(
                "Failed to create snapshots directory: {}",
                snapshot_dir.display()
            )
        })?;

        let snapshot_path = snapshot_dir.join(format!("{}.json", self.snapshot_id));

        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize snapshot to JSON")?;

        // Same-directory temp + atomic rename (see the artifacts engine's
        // `write_atomic`): a re-save can overwrite the ONLY pre-pull backup
        // long after the files it describes are already gone (the undo-delete
        // narrowing after an apply, a pin during an undo). A truncate-in-place
        // write that fails midway would corrupt that sole copy in place, and a
        // crash mid-write leaves the same corruption — the temp file keeps
        // the original intact on every failure path.
        let tmp = tempfile::NamedTempFile::new_in(&snapshot_dir).with_context(|| {
            format!(
                "Failed to create temp file in snapshots directory: {}",
                snapshot_dir.display()
            )
        })?;
        fs::write(tmp.path(), &json).with_context(|| {
            format!(
                "Failed to write snapshot to disk: {}",
                snapshot_path.display()
            )
        })?;
        // Flush before the rename (see RepoRecord::write): a pinned
        // snapshot is the SOLE copy of record entries a warned undo could
        // not restore — a rename landing before the data blocks would
        // turn a power loss into an empty sole copy.
        tmp.as_file().sync_all().with_context(|| {
            format!(
                "Failed to flush snapshot to disk: {}",
                snapshot_path.display()
            )
        })?;
        tmp.persist(&snapshot_path).with_context(|| {
            format!(
                "Failed to persist snapshot to disk: {}",
                snapshot_path.display()
            )
        })?;

        // Log snapshot size information (to file only, UI output is handled by caller)
        let size_mb = json.len() as f64 / (1024.0 * 1024.0);
        let snapshot_type = if self.base_snapshot_id.is_some() {
            "differential"
        } else {
            "full"
        };

        log::debug!(
            "Created {} snapshot: {} ({:.1} MB, {} files)",
            snapshot_type,
            self.snapshot_id,
            size_mb,
            self.files.len()
        );

        if size_mb > 100.0 {
            log::debug!("Large snapshot size - consider cleaning up old conversation files");
        }

        Ok(snapshot_path)
    }

    /// Load a snapshot from disk
    ///
    /// # Arguments
    /// * `snapshot_path` - Path to the snapshot JSON file
    ///
    /// # Behavior
    /// - Logs snapshot size information
    /// - Warns if snapshot is unusually large (>100MB) but still loads it
    /// - Snapshots are critical for undo functionality and should never be skipped
    ///
    /// # Errors
    /// Returns error only if file cannot be read or parsed
    pub fn load_from_disk<P: AsRef<Path>>(snapshot_path: P) -> Result<Self> {
        let snapshot_path = snapshot_path.as_ref();

        // Get file size for logging
        let metadata = fs::metadata(snapshot_path).with_context(|| {
            format!(
                "Failed to read snapshot file metadata: {}",
                snapshot_path.display()
            )
        })?;

        let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);

        // Log size information - always show this for visibility
        if size_mb > 100.0 {
            println!(
                "    {} Loading large snapshot: {} ({:.1} MB) - This may take a moment...",
                "⚠".yellow(),
                snapshot_path.file_name().unwrap().to_string_lossy().cyan(),
                size_mb
            );
        } else {
            println!(
                "    {} snapshot: {} ({:.1} MB)",
                "Loading".dimmed(),
                snapshot_path.file_name().unwrap().to_string_lossy().cyan(),
                size_mb
            );
        }

        let content = fs::read_to_string(snapshot_path).with_context(|| {
            format!("Failed to read snapshot file: {}", snapshot_path.display())
        })?;

        let snapshot: Snapshot = serde_json::from_str(&content).with_context(|| {
            format!("Failed to parse snapshot JSON: {}", snapshot_path.display())
        })?;

        // Log file count information
        println!("    {} {} files", "Contains".dimmed(), snapshot.files.len());

        // Warn if unusually large number of files
        if snapshot.files.len() > 1000 {
            println!(
                "    {} Large number of files - this is a full (non-differential) snapshot",
                "Note:".yellow()
            );
        }

        Ok(snapshot)
    }

    /// Get the default snapshots directory
    pub(crate) fn snapshots_dir() -> Result<PathBuf> {
        crate::config::ConfigManager::snapshots_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::undo::test_support::{create_test_file, setup_test_repo};
    use tempfile::tempdir;

    #[test]
    #[cfg(unix)]
    fn an_unreadable_file_is_skipped_not_fatal() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempdir().unwrap();
        let readable = create_test_file(temp_dir.path(), "readable.jsonl", "x");
        let locked = create_test_file(temp_dir.path(), "locked.jsonl", "y");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        // The pull's only Snapshot::create caller must not fail the whole
        // sync over one bad file — the apply's kept arm keeps it anyway.
        let snapshot =
            Snapshot::create(OperationType::Pull, vec![&readable, &locked], None).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(snapshot
            .files
            .contains_key(&readable.to_string_lossy().to_string()));
        assert!(
            !snapshot
                .files
                .contains_key(&locked.to_string_lossy().to_string()),
            "the unreadable file is left out, not fatal"
        );
    }

    #[test]
    fn test_snapshot_create_and_save() {
        let temp_dir = tempdir().unwrap();
        let file1 = create_test_file(temp_dir.path(), "file1.txt", "content 1");
        let file2 = create_test_file(temp_dir.path(), "file2.txt", "content 2");

        let snapshot = Snapshot::create(OperationType::Pull, vec![&file1, &file2], None).unwrap();

        assert_eq!(snapshot.operation_type, OperationType::Pull);
        assert_eq!(snapshot.files.len(), 2);
        assert!(snapshot.git_commit_hash.is_none());

        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
        assert!(snapshot_path.exists());
    }

    #[test]
    fn test_snapshot_with_commit_hash() {
        let (temp_dir, repo) = setup_test_repo();
        let file1 = temp_dir.path().join("test.txt");
        let commit_hash = repo.current_commit_hash().unwrap();

        let snapshot =
            Snapshot::create(OperationType::Push, vec![&file1], Some(&commit_hash)).unwrap();

        assert_eq!(snapshot.operation_type, OperationType::Push);
        let stored_hash = snapshot.git_commit_hash.unwrap();
        assert_eq!(stored_hash.len(), 40); // Git SHA-1 hash length
        assert_eq!(stored_hash, commit_hash);
    }

    #[test]
    fn test_snapshot_save_and_load() {
        let temp_dir = tempdir().unwrap();
        let file1 = create_test_file(temp_dir.path(), "file1.txt", "test content");

        let original = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();

        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = original.save_to_disk(Some(&snapshots_dir)).unwrap();

        let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();

        assert_eq!(loaded.snapshot_id, original.snapshot_id);
        assert_eq!(loaded.operation_type, original.operation_type);
        assert_eq!(loaded.files.len(), original.files.len());
    }

    #[test]
    fn test_snapshot_handles_binary_files() {
        let temp_dir = tempdir().unwrap();
        let binary_file = temp_dir.path().join("binary.dat");

        let binary_content: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0x01, 0x02, 0x03];
        fs::write(&binary_file, &binary_content).unwrap();

        let snapshot = Snapshot::create(OperationType::Pull, vec![&binary_file], None).unwrap();

        let key = binary_file.to_string_lossy().to_string();
        assert_eq!(snapshot.files.get(&key).unwrap(), &binary_content);

        // The base64 serde shim has to survive a disk round-trip, not just
        // hold the bytes in memory.
        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();
        assert_eq!(loaded.files.get(&key).unwrap(), &binary_content);
    }

    #[test]
    fn test_snapshot_serialization_with_special_characters() {
        let temp_dir = tempdir().unwrap();
        let file_with_unicode = temp_dir.path().join("日本語.txt");
        fs::write(&file_with_unicode, "Hello 世界").unwrap();

        let snapshot =
            Snapshot::create(OperationType::Pull, vec![&file_with_unicode], None).unwrap();

        let snapshots_dir = temp_dir.path().join("snapshots");
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();

        let content = loaded.files.values().next().unwrap();
        assert_eq!(String::from_utf8_lossy(content), "Hello 世界");
    }

    #[test]
    fn test_base64_encoding_for_binary_data() {
        let temp_dir = tempdir().unwrap();

        // Every possible byte value, including those that are not valid UTF-8.
        let binary_file = temp_dir.path().join("binary.dat");
        let binary_data: Vec<u8> = (0..=255).collect();
        fs::write(&binary_file, &binary_data).unwrap();

        let snapshot = Snapshot::create(OperationType::Pull, vec![&binary_file], None).unwrap();

        let json = serde_json::to_string(&snapshot).unwrap();
        let _parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        let deserialized: Snapshot = serde_json::from_str(&json).unwrap();

        let key = binary_file.to_string_lossy().to_string();
        assert_eq!(
            snapshot.files.get(&key).unwrap(),
            deserialized.files.get(&key).unwrap()
        );
        assert_eq!(deserialized.files.get(&key).unwrap(), &binary_data);
    }

    #[test]
    fn test_empty_snapshot() {
        let snapshot = Snapshot::create::<PathBuf, _>(OperationType::Pull, vec![], None).unwrap();

        assert_eq!(snapshot.files.len(), 0);
        assert!(snapshot.git_commit_hash.is_none());

        let temp_dir = tempdir().unwrap();
        let snapshot_path = snapshot.save_to_disk(Some(temp_dir.path())).unwrap();

        let loaded = Snapshot::load_from_disk(&snapshot_path).unwrap();
        assert_eq!(loaded.files.len(), 0);

        // Restoring nothing must be a no-op, not an error.
        loaded.restore().unwrap();
    }

    #[test]
    fn test_snapshot_create_handles_missing_files() {
        let temp_dir = tempdir().unwrap();

        let existing_file = create_test_file(temp_dir.path(), "exists.txt", "content");
        let missing_file = temp_dir.path().join("does_not_exist.txt");

        // A path that has already been deleted is skipped, not an error.
        let snapshot = Snapshot::create(
            OperationType::Pull,
            vec![&existing_file, &missing_file],
            None,
        )
        .unwrap();

        assert_eq!(snapshot.files.len(), 1);
        assert!(snapshot
            .files
            .contains_key(&existing_file.to_string_lossy().to_string()));
        assert!(!snapshot
            .files
            .contains_key(&missing_file.to_string_lossy().to_string()));
    }
}
