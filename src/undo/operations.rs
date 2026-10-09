use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use super::snapshot::Snapshot;
use crate::history::{OperationHistory, OperationType};
use crate::scm;

/// Restore ONE shared record: the effective-key intersection, the one
/// case analysis, the outcome dispatch, and the warning tail — everything
/// the bases and tracked blocks need, parameterized over the per-record
/// operations so the twins cannot drift. `record_entries_unrestored`
/// records that something restorable was actually lost (only then may
/// the snapshot be kept — informational notices pin nothing).
#[allow(clippy::too_many_arguments)]
fn restore_shared_record<V>(
    declared_touched: Option<&Vec<String>>,
    keys_written: Option<&[String]>,
    entry: Option<&V>,
    created: bool,
    label: &str,
    restore: impl Fn(&V, &[String]) -> Result<()>,
    save_whole: impl FnOnce(V) -> Result<()>,
    forget: impl FnOnce() -> Result<()>,
    empty: V,
    record_warnings: &mut Vec<String>,
    record_entries_unrestored: &mut bool,
) where
    V: Clone + crate::artifacts::repo_record::EntryValue,
{
    let effective: Vec<String> = match keys_written {
        Some(written) => {
            // HashSet lookup — Vec::contains is O(m) per key (O(n*m) overall).
            // A user with thousands of tracked files undoes a pull; without
            // a HashSet the surgery is the difference between a sub-
            // second undo and a multi-second hang.
            let written_set: std::collections::HashSet<&str> =
                written.iter().map(String::as_str).collect();
            declared_touched
                .into_iter()
                .flatten()
                .filter(|key| written_set.contains(key.as_str()))
                .cloned()
                .collect()
        }
        None => declared_touched.cloned().unwrap_or_default(),
    };
    // The entry exists only when a declared record file was actually READ
    // into the snapshot: a declared-but-vanished record reads as
    // carried=false, and restoring an empty entry would DELETE the
    // effective keys.
    let carried_bytes = entry.is_some();
    let carried_entry_nonempty = entry.is_some_and(|entry| !entry.is_empty());
    let plan = record_restore_plan(
        effective.len(),
        carried_bytes,
        declared_touched.is_none(),
        carried_entry_nonempty,
        created,
    );
    let outcome = match plan {
        RecordRestore::PerKey => restore(entry.unwrap_or(&empty), &effective),
        RecordRestore::Skip => {
            // The pure-Skip branch is silent for the normal case
            // (effective=0, nothing to undo), but suspect when the
            // apply moved keys (effective>0) and the snapshot had no
            // pre-pull entry for them — the apply wrote them, undo can
            // not roll them back without the pre-pull bytes, and the
            // caller is about to print "successfully undone". Warn and
            // pin so the kept-snapshot summary mentions this.
            // Also flip `record_entries_unrestored` so the kept-snapshot
            // summary's `sole_copy` template names "record entries the
            // warnings above say were not restored" rather than the
            // generic "pre-pull files" — the snapshot holds POST-pull
            // state here, not pre-pull files the user can recover.
            if !effective.is_empty() && !created {
                let warning = format!(
                    "WARNING: the apply updated {effective_len} {label} record key(s) but the \
                     snapshot carried no pre-pull entry for them; undo did not roll them back \
                     (the kept snapshot holds the post-pull state, not pre-pull). A push will \
                     re-record the true keys; until then the protected state is stale.",
                    effective_len = effective.len()
                );
                log::warn!("{warning}");
                record_warnings.push(warning);
                *record_entries_unrestored = true;
            }
            Ok(())
        }
        RecordRestore::LegacySave => save_whole(entry.cloned().unwrap_or(empty)),
        RecordRestore::LegacyForget => forget(),
    };
    if let Err(e) = outcome {
        // The files are already restored; failing the undo now would hide
        // the one state that DID change. But "success" must not swallow
        // it either: a stale record silently re-dirties every restored
        // file on the next pull. The warning is about the record entry
        // specifically ("the artifact {label} record entry was not"),
        // so the kept-snapshot summary's sole_copy template names
        // "record entries the warnings above say were not restored",
        // not the generic "pre-pull files" — same contract as the
        // F5 Skip-branch fix.
        let warning = format!(
            "WARNING: files were restored, but the artifact {label} record entry was not: {e}"
        );
        log::warn!("{warning}");
        record_warnings.push(warning);
        *record_entries_unrestored = true;
    }
}

/// What the surgery block computed for both shared records — named, so
/// the two restores below read as the same shape they are (see
/// `record_restore_plan`: they differ only in the functions they call).
struct RecordSurgery {
    claude_dir: PathBuf,
    repo_root: PathBuf,
    bases_entry: Option<crate::artifacts::bases::BaseHashes>,
    bases_created: bool,
    tracked_entry: Option<crate::artifacts::tracked::TrackedPaths>,
    tracked_created: bool,
}

/// What one shared record's undo must do. The ONE case analysis both
/// records follow — `undo_pull` computes it once per record and the two
/// blocks differ only in the functions they call.
enum RecordRestore {
    /// A current snapshot: restore the PRE-PULL value of exactly the
    /// touched keys (all of them when the pull created the record) and
    /// leave every other key to whatever recorded it after the pull —
    /// a push that ran in between would otherwise have its entries
    /// erased, and a file without its base is remote-wins material
    /// again, the exact loss the record exists to prevent.
    PerKey,
    /// The apply never wrote the record (every write failed, every
    /// prompt Abandoned), or the pull neither carried nor created it:
    /// nothing to undo. Restoring keys nobody moved would revert entries
    /// a later push legitimately recorded.
    Skip,
    /// A snapshot from before per-key declarations: the whole-entry
    /// restore it was written with. Current code writes the declarations
    /// and the touched lists together, so only snapshots produced by
    /// interim builds of this very PR (declarations without touched
    /// lists) can still land here — real on machines that ran them, dead
    /// for everyone else.
    LegacySave,
    /// Same provenance as [`RecordRestore::LegacySave`], with an empty or
    /// absent pre-pull entry: forget the repository's entry rather than
    /// plant a `"<repo>": {}` cruft key the real pre-pull file never had.
    LegacyForget,
}

/// The one case analysis for one shared record's undo (see
/// [`RecordRestore`]). `effective_keys` is how many keys the apply
/// actually moved (the snapshot's declared set intersected with the
/// operation record's exact list, or the full declared set for an older
/// record — the conservative superset); `carried_bytes` is whether the
/// snapshot holds this record's pre-pull bytes (a record the pull
/// CARRIED — it existed — whose bytes are missing was unreadable at
/// snapshot time, and restoring an empty entry would DELETE the
/// effective keys, the exact loss the surgery exists to prevent; a
/// record the pull CREATED legitimately has none, and its per-key
/// restore with an empty pre-pull entry undoes the creation);
/// `legacy_snapshot` marks a snapshot that predates per-key
/// declarations.
fn record_restore_plan(
    effective_keys: usize,
    carried_bytes: bool,
    legacy_snapshot: bool,
    carried_entry_nonempty: bool,
    created: bool,
) -> RecordRestore {
    if legacy_snapshot {
        if carried_entry_nonempty {
            RecordRestore::LegacySave
        } else if created {
            RecordRestore::LegacyForget
        } else {
            RecordRestore::Skip
        }
    } else if effective_keys > 0 && (carried_bytes || created) {
        RecordRestore::PerKey
    } else {
        RecordRestore::Skip
    }
}

/// Undo the last pull operation
///
/// This function:
/// 1. Loads the operation history
/// 2. Finds the most recent pull operation
/// 3. Loads the snapshot that was taken before that pull
/// 4. Restores all files to their pre-pull state
/// 5. Updates the operation history to mark the pull as undone
///
/// # Arguments
/// * `history_path` - Optional custom path for operation history (for testing)
/// * `allowed_base_dir` - Optional base directory for path validation (for testing)
///
/// # Returns
/// A summary message describing what was undone
pub fn undo_pull(history_path: Option<PathBuf>, allowed_base_dir: Option<&Path>) -> Result<String> {
    // Load operation history
    // Loaded ONCE and reused for the removal below: a second load could
    // observe a pull recorded in between (a concurrent sync's commit) and
    // remove THAT record instead of the one being undone — leaving the
    // undone record pointing at the snapshot file Step 3 deletes.
    let mut history = OperationHistory::from_path(history_path.clone())?;

    // Find the last pull an undo can act on — snapshotless pull records
    // (a kept-local-only sync changes no machine state, so none was
    // minted) are skipped: erroring on the newest of those would block
    // undo of the earlier snapshot-bearing pull beneath it.
    // An owned clone: the record must outlive the history's mutable
    // borrow in Step 1 (same load, no re-read).
    let last_pull = history.get_last_undoable_pull().cloned().ok_or_else(|| {
        if history
            .list_operations()
            .iter()
            .any(|op| op.operation_type == OperationType::Pull)
        {
            anyhow!(
                "No pull to undo: the recorded pulls changed no machine state \
                 (kept local only), so there is nothing to restore"
            )
        } else {
            anyhow!("No pull operation found in history to undo")
        }
    })?;

    // Get the snapshot path
    let snapshot_path = last_pull.snapshot_path.as_ref().ok_or_else(|| {
        anyhow!(
            "No snapshot found for last pull operation. \
                Cannot undo without a snapshot."
        )
    })?;

    // Verify snapshot exists
    if !snapshot_path.exists() {
        return Err(anyhow!(
            "Snapshot file not found: {}. \
            The snapshot may have been deleted.",
            snapshot_path.display()
        ));
    }

    // Load the snapshot
    let mut snapshot = Snapshot::load_from_disk(snapshot_path)?;

    // Verify this is indeed a pull snapshot
    if snapshot.operation_type != OperationType::Pull {
        return Err(anyhow!(
            "Snapshot type mismatch: expected pull, found {}",
            snapshot.operation_type.as_str()
        ));
    }

    // The artifact records (bases, tracked) are one file EACH, shared by
    // EVERY sync repository: a wholesale restore would clobber entries
    // other repositories recorded since this pull, and a wholesale delete
    // would discard them. Surgical instead — this repository's pre-pull
    // entry leaves the snapshot with the file itself taken out of the
    // generic restore, and is re-applied (or forgotten, when this pull is
    // what created the record) afterwards. The snapshot names its record
    // files explicitly (`record_files` / `created_record_files`), so a
    // synced artifact that merely shares a record's file name can never
    // hijack the surgery, and the pull-time spelling needs no
    // re-derivation. Snapshots written before those fields carry no
    // declarations: their record files are recognized below and routed
    // through the same surgery, never the generic restore.
    let mut record_warnings: Vec<String> = Vec::new();
    // Whether something RESTORABLE was lost: only that may keep and pin
    // the snapshot — informational notices (a record that was unreadable
    // at snapshot time, carrying nothing recoverable) pin nothing, or
    // every recurrence would accumulate a snapshot the cleanup can never
    // delete.
    //
    // `pin_on_warning` and `record_entries_unrestored` were collapsed
    // into one flag — the two were always set together at every
    // record-surgery warning site, and the kept-snapshot summary
    // template consults only this flag. `must_pin` also reads
    // `restore_failed` (a separate flag for the file-restoration
    // path) to decide whether to pin the snapshot.
    let mut record_entries_unrestored = false;
    // The declared record bytes, held aside for the surgical re-apply AND
    // for the pinned re-save below (a kept snapshot must still carry them
    // — it is the sole copy).
    let mut declared_bytes: Vec<(String, Vec<u8>)> = Vec::new();
    // Legacy snapshots (no declarations) get their record files
    // recognized and promoted by the shared rule — the same one
    // `preview_undo_pull` applies, so the preview and the undo agree.
    // A STRIPPED record blob (record-shaped but unverifiable placement)
    // was removed from the generic restore instead: it is the sole copy
    // of entries nothing could restore safely — warn and pin.
    if snapshot.promote_legacy_records() {
        let warning = "WARNING: the snapshot carries shared record file(s) that could \
                       not be safely classified; they were NOT restored — recover their \
                       entries by hand from the kept snapshot";
        log::warn!("{warning}");
        record_warnings.push(warning.to_string());
        record_entries_unrestored = true;
    }
    let records_surgery = if snapshot.record_files.is_empty()
        && snapshot.created_record_files.is_empty()
    {
        None
    } else {
        // The declared record keys leave the snapshot FIRST, whatever
        // happens below: the generic restore must never rewrite a shared
        // record file wholesale (other repositories hold newer entries in
        // it). The bytes are held aside for the surgical re-apply.
        //
        // Stripping `self.files` is COMPLETE: pull snapshots are always
        // full (Snapshot::create, never the differential chain), so the
        // restore's chain reconstruction cannot resurrect record bytes
        // from an ancestor. If differential pull snapshots ever appear,
        // the surgery must strip the RECONSTRUCTED state instead.
        for key in snapshot.record_files.iter() {
            if let Some(bytes) = snapshot.files.remove(key) {
                declared_bytes.push((key.clone(), bytes));
            }
        }
        // And the generic restore must never DELETE one either. Only the
        // DECLARED created records are exempted — a created artifact
        // sharing a record's file name is an ordinary file again (the
        // decoy test pins that contract), and record paths never ride a
        // deleted_files list from any release: records are not artifacts,
        // and created_paths() lists artifact files only.
        snapshot
            .deleted_files
            .retain(|p| !snapshot.created_record_files.iter().any(|c| c == p));
        // An operation record without a repo path (an interim build of
        // the record work) is not undo-stranding material: the declared
        // keys are already stripped above and the files still restore —
        // only the record entries cannot be scoped. Warn and skip the
        // surgery instead of failing the whole undo.
        let missing_repo_path = last_pull.repo_path.is_none();
        if missing_repo_path {
            let warning = "WARNING: the pull record predates repo-scoped record \
                           surgery; its artifact record entries were not restored"
                .to_string();
            log::warn!("{warning}");
            record_warnings.push(warning);
            record_entries_unrestored = true;
            // The snapshot carries the declared bytes — they are
            // recoverable by hand, so the file must survive the cleanup.
        }
        let repo_root = last_pull
            .repo_path
            .as_deref()
            .unwrap_or_else(|| Path::new(""));
        if missing_repo_path {
            None
        } else {
            // The Claude directory comes from the declared key's pull-time
            // spelling, not from undo-time resolution: the generic restore
            // writes files back to their pull-time paths, and the records
            // must land in the same place.
            let mut declared_keys = snapshot
                .record_files
                .iter()
                .chain(snapshot.created_record_files.iter());
            let record_key = declared_keys
                .next()
                .context("Record surgery with no declared record files")?;
            let claude_dir = Path::new(record_key)
                .parent()
                .context("A record path with no parent directory")?
                .to_path_buf();
            // The surgery issues EVERY record's restore against THIS one
            // directory: a snapshot declaring records under two different
            // directories (an interim format, say) would plant the second
            // record's entries in the first one's files. Warn and skip the
            // surgery instead of writing silently wrong record state.
            if declared_keys.any(|key| Path::new(key).parent() != Some(claude_dir.as_path())) {
                let warning = "WARNING: the snapshot declares record files under more \
                               than one directory; their entries were not restored"
                    .to_string();
                log::warn!("{warning}");
                record_warnings.push(warning);
                record_entries_unrestored = true;
                None
            } else {
                // The same boundary restore_with_base enforces on every other
                // snapshot-driven write: a tampered snapshot must not be able to
                // plant record files outside the allowed base directory
                // (RepoRecord::write creates directories on the way). A MISSING
                // Claude directory is not a security event — the generic restore
                // recreates it — the record entries simply cannot be re-applied,
                // which warns below.
                let allowed_base = super::allowed_base::allowed_base(allowed_base_dir)?;
                let surgery_ok = match claude_dir.canonicalize() {
                    Ok(dir) if dir.starts_with(&allowed_base) => true,
                    Ok(_) => {
                        anyhow::bail!(
                            "Refusing to restore artifact records outside {}: {}",
                            allowed_base.display(),
                            claude_dir.display()
                        )
                    }
                    Err(_) => {
                        let warning = format!(
                            "WARNING: the Claude directory {} no longer exists; artifact record entries were not restored",
                            claude_dir.display()
                        );
                        log::warn!("{warning}");
                        record_warnings.push(warning);
                        record_entries_unrestored = true;
                        // The snapshot still carries the declared bytes —
                        // recoverable by hand once the directory exists, so
                        // the file must survive the cleanup.
                        false
                    }
                };
                if !surgery_ok {
                    None
                } else {
                    let mut bases_entry = None;
                    let mut tracked_entry = None;
                    for (key, bytes) in &declared_bytes {
                        // The declaration says which keys are records; the file
                        // name only picks the parser. A blob that does not
                        // parse is CORRUPT, not empty: reading it as an empty
                        // pre-pull entry would send the per-key restore on to
                        // DELETE this repository's live keys (every restored
                        // file then reads as never-synced) — the exact loss the
                        // record exists to prevent. Warn, treat as not carried,
                        // and pin the snapshot (its bytes are the corrupt sole
                        // copy a hand recovery starts from).
                        if crate::artifacts::bases::is_record_path(Path::new(key)) {
                            match crate::artifacts::bases::entry_from_record_bytes(bytes, repo_root)
                            {
                                Some(entry) => bases_entry = Some(entry),
                                None => {
                                    let warning = format!(
                                        "WARNING: the snapshot's copy of {} is corrupt \
                                     (unparseable); this repository's record entries \
                                     were not restored — recover them by hand from \
                                     the kept snapshot",
                                        key
                                    );
                                    log::warn!("{warning}");
                                    record_warnings.push(warning);
                                    record_entries_unrestored = true;
                                }
                            }
                        } else if crate::artifacts::tracked::is_record_path(Path::new(key)) {
                            match crate::artifacts::tracked::entry_from_record_bytes(
                                bytes, repo_root,
                            ) {
                                Some(entry) => tracked_entry = Some(entry),
                                None => {
                                    let warning = format!(
                                        "WARNING: the snapshot's copy of {} is corrupt \
                                     (unparseable); this repository's record entries \
                                     were not restored — recover them by hand from \
                                     the kept snapshot",
                                        key
                                    );
                                    log::warn!("{warning}");
                                    record_warnings.push(warning);
                                    record_entries_unrestored = true;
                                }
                            }
                        }
                    }
                    if declared_bytes.len() < snapshot.record_files.len() {
                        // Declared but never snapshotted (deleted between plan and
                        // snapshot): say so — a silent skip leaves the post-pull
                        // entry describing an undone pull.
                        let missing = snapshot.record_files.len() - declared_bytes.len();
                        let warning = format!(
                            "WARNING: {missing} declared record file(s) carry no snapshot \
                         bytes; their entries were not checked for restore"
                        );
                        log::warn!("{warning}");
                        record_warnings.push(warning);
                        record_entries_unrestored = true;
                        // Sole-copy contract: the warning above tells the user
                        // to recover the missing entries by hand from the
                        // kept snapshot — so the snapshot must be kept.
                    }
                    let mut bases_created = false;
                    let mut tracked_created = false;
                    for key in &snapshot.created_record_files {
                        if crate::artifacts::bases::is_record_path(Path::new(key)) {
                            bases_created = true;
                        } else if crate::artifacts::tracked::is_record_path(Path::new(key)) {
                            tracked_created = true;
                        }
                    }
                    Some(RecordSurgery {
                        claude_dir,
                        repo_root: repo_root.to_path_buf(),
                        bases_entry,
                        bases_created,
                        tracked_entry,
                        tracked_created,
                    })
                }
            }
        }
    };

    // Get the list of files that will be restored
    let restored_files: Vec<String> = snapshot.files.keys().cloned().collect();
    let file_count = restored_files.len();

    // TRANSACTION-LIKE ORDERING: Update history FIRST, then restore files.
    // This ensures that if file restoration fails, the history is still consistent
    // and accurately reflects that we've attempted the undo. The snapshot file
    // remains on disk until we successfully complete the restoration.

    // Step 1: Remove the pull operation from history — the SAME record
    // the undo acts on (the newest pull WITH a snapshot), not the newest
    // pull of any kind: snapshotless kept-local records sit on top, and
    // removing one of those would leave the just-undone record behind,
    // pointing at the snapshot file Step 3 deletes.
    history
        .remove_last_undoable_pull(history_path.clone())
        .context("Failed to remove pull operation from history")?;

    // Step 2: Restore the snapshot files
    // If this fails, the history is already updated (which is safer than having
    // an inconsistent history state)
    // A mid-restore failure must not strand the undo silently: the
    // history is already rewritten (step 1), so returning Err would leave
    // a partially-restored machine with no warning, no kept snapshot, and
    // a cleanup timer on the only copy of the pre-pull files. Degrade to
    // a warning like every other failure here — the pin below keeps the
    // snapshot.
    let mut restore_failed = false;
    if let Err(e) = snapshot.restore_with_base(allowed_base_dir) {
        let warning = format!(
            "WARNING: file restoration failed partway ({e}); the pre-pull files \
             were NOT fully restored — the snapshot was KEPT (pinned) with them, \
             restore the failing ones by hand once the error is fixed"
        );
        log::warn!("{warning}");
        record_warnings.push(warning);
        restore_failed = true;
    }

    // Re-apply this repository's entries now that the generic restore is
    // done and the shared files are untouched. Failures here cannot fail
    // the undo (the files are already restored) but must reach the user.
    // A PARTLY FAILED restoration changes what the records should say:
    // files after the failing one hold post-pull bytes, and re-applying
    // pre-pull record entries would make them read as local edits —
    // the next pull's "push to publish" would push the remote bytes
    // right back, silently redoing the pull just undone. The records
    // stay post-pull (honest about where most files are); the kept
    // snapshot carries the pre-pull entries for hand recovery.
    if restore_failed {
        if records_surgery.is_some() {
            let warning = "WARNING: file restoration failed partway, so the artifact \
                           record entries were NOT restored either — the records still \
                           describe the post-pull state; recover the pre-pull entries \
                           from the kept snapshot if needed";
            log::warn!("{warning}");
            record_warnings.push(warning.to_string());
            record_entries_unrestored = true;
        }
    } else if let Some(RecordSurgery {
        claude_dir,
        repo_root,
        bases_entry,
        bases_created,
        tracked_entry,
        tracked_created,
    }) = records_surgery
    {
        // The ONE case analysis both shared records follow — the blocks
        // below differ only in the functions they call, never in the
        // cases (see `record_restore_plan`). The effective set is the
        // snapshot's declared keys intersected with what the apply
        // ACTUALLY moved (its own report, carried on the operation
        // record): a later push's entries keep their owner even when the
        // plan declared a superset of no-op re-writes.
        // The ONE shared runner for both records — see
        // `restore_shared_record`; the calls differ only in the
        // operations they pass.
        restore_shared_record(
            snapshot.record_touched_bases.as_ref(),
            last_pull.bases_keys_written.as_deref(),
            bases_entry.as_ref(),
            bases_created,
            "base",
            |entry, effective| {
                crate::artifacts::bases::restore_keys(&claude_dir, &repo_root, entry, effective)
            },
            |entry| crate::artifacts::bases::save(&claude_dir, &repo_root, entry),
            || crate::artifacts::bases::forget(&claude_dir, &repo_root),
            crate::artifacts::bases::BaseHashes::new(),
            &mut record_warnings,
            &mut record_entries_unrestored,
        );
        restore_shared_record(
            snapshot.record_touched_tracked.as_ref(),
            last_pull.tracked_keys_written.as_deref(),
            tracked_entry.as_ref(),
            tracked_created,
            "tracked",
            |entry, effective| {
                crate::artifacts::tracked::restore_keys(&claude_dir, &repo_root, entry, effective)
            },
            |entry| crate::artifacts::tracked::save(&claude_dir, &repo_root, entry),
            || crate::artifacts::tracked::forget(&claude_dir, &repo_root),
            crate::artifacts::tracked::TrackedPaths::new(),
            &mut record_warnings,
            &mut record_entries_unrestored,
        );
    }

    // Step 3: Clean up the snapshot file (only after successful
    // restoration) — but keep it when the record surgery only warned:
    // the snapshot holds the sole copy of the pre-pull entry the warning
    // says was not restored, and deleting it would make that warning
    // unrecoverable.
    let must_pin = record_entries_unrestored || restore_failed;
    let mut kept_snapshot = false;
    let mut pin_failed = false;
    if !must_pin {
        // Informational warnings alone pin nothing — the snapshot holds
        // nothing restorable, and keeping it would only accumulate files
        // the cleanup can never delete.
        if let Err(e) = fs::remove_file(snapshot_path) {
            eprintln!(
                "Warning: Failed to remove snapshot file {}: {}",
                snapshot_path.display(),
                e
            );
        }
    } else {
        kept_snapshot = true;
        // Pin it: the summary below promises this file holds the only
        // copy of the unrestored record entries, and the regular cleanup
        // (keep 5 per type / 7 days, every pull) must not delete it from
        // under the user.
        snapshot.pinned = true;
        // The record bytes were stripped from `files` for the surgical
        // re-apply — put them BACK before re-saving, or the pinned copy
        // would overwrite the original without the very entries it was
        // kept to preserve.
        for (key, bytes) in declared_bytes.drain(..) {
            snapshot.files.insert(key, bytes);
        }
        // Stripped record blobs too: the kept file must still carry the
        // very bytes the warning says are recoverable from it.
        for (key, bytes) in snapshot.stripped_blobs.drain(..) {
            snapshot.files.insert(key, bytes);
        }
        if let Some(dir) = snapshot_path.parent() {
            if let Err(e) = snapshot.save_to_disk(Some(dir)) {
                // The file on disk still says pinned:false — promising a
                // pin the cleanup will not honor would send the user to a
                // file that vanishes. Say the opposite, urgently.
                pin_failed = true;
                eprintln!(
                    "Warning: Failed to pin snapshot file {}: {}",
                    snapshot_path.display(),
                    e
                );
            }
        }
    }

    // The headline is what a skimming user takes away: it must not
    // claim a restore that failed partway.
    let mut summary = if restore_failed {
        format!(
            "Undone last pull operation WITH WARNINGS — file restoration \
             failed partway, see below.\n\
            Snapshot taken at: {}",
            snapshot.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
        )
    } else {
        format!(
            "Successfully undone last pull operation.\n\
            Restored {} files to their pre-pull state.\n\
            Snapshot taken at: {}",
            file_count,
            snapshot.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
        )
    };
    if kept_snapshot && pin_failed {
        // The snapshot holds its declared record bytes only when the
        // record surgery flagged unrestored entries (declared bytes
        // were stripped then re-inserted before the failed re-save).
        // When the surgery was clean, the on-disk file's record bytes
        // come from the snapshot being full (pull snapshots are always
        // full, see Snapshot::create), not from the surgery's drain —
        // but the user's recovery interest is still the pre-pull
        // FILES, the only thing the surgery did not produce. Branch the
        // wording so the pin-failed message points at the right
        // recovery content.
        let pin_sole_copy = if record_entries_unrestored && !restore_failed {
            "the record entries the warnings above say were not restored"
        } else {
            "the pre-pull files that the file-restoration failure left \
             hiding (the record surgery was clean, so no record entries \
             are pending recovery in this snapshot)"
        };
        summary.push_str(&format!(
            "\n\nThe snapshot at {} could NOT be pinned (see the warning \
             above): the regular snapshot cleanup WILL delete it. Copy it \
             somewhere safe NOW if you need {pin_sole_copy} \u{2014} no \
             command can retry this once the pull has left the history.",
            snapshot_path.display()
        ));
    } else if kept_snapshot {
        // Name what the snapshot is actually the sole copy OF: a pin from
        // the record surgery holds record entries, a pin from a failed
        // file restoration holds the pre-pull FILES — pointing the reader
        // at restored-fine record entries sends their recovery nowhere.
        //
        // When BOTH `restore_failed` and `record_entries_unrestored`
        // are true, the file restoration failed partway AND the record
        // surgery left entries unrestored. The pre-pull FILES are the
        // more actionable recovery content (the records were preserved
        // in post-pull state by design — the surgery flag is about
        // the per-key restore not the entries themselves). Prefer the
        // file-restore wording.
        let sole_copy = if record_entries_unrestored && !restore_failed {
            "the record entries the warnings above say were not restored"
        } else {
            "the pre-pull files the warning above says were not fully \
             restored"
        };
        summary.push_str(&format!(
            "\n\nThe snapshot was KEPT ({}) — it holds the only copy of \
             {sole_copy}. Fix the underlying error (permissions, disk), \
             then recover by hand from that file: no command can retry \
             this once the pull has left the history. The snapshot is \
             pinned against the regular snapshot cleanup.",
            snapshot_path.display()
        ));
    }
    for warning in &record_warnings {
        summary.push_str(&format!("\n\n{warning}"));
    }
    Ok(summary)
}

/// Undo the last push operation
///
/// This function:
/// 1. Loads the operation history
/// 2. Finds the most recent push operation
/// 3. Gets the commit hash from the operation record (no snapshot needed!)
/// 4. Uses SCM abstraction to reset the repository to the previous commit
/// 5. Updates the operation history to mark the push as undone
/// 6. Warns the user if they need to force push to the remote
///
/// Note: Push operations no longer create file snapshots. Git/Mercurial already
/// tracks history, so we just store the commit hash and use `reset` to undo.
///
/// # Arguments
/// * `repo_path` - Path to the SCM repository
/// * `history_path` - Optional custom path for operation history (for testing)
///
/// # Returns
/// A summary message describing what was undone and any required follow-up actions
pub fn undo_push(repo_path: &Path, history_path: Option<PathBuf>) -> Result<String> {
    // Load operation history
    let history = OperationHistory::from_path(history_path.clone())?;

    // Find the last push operation
    let last_push = history
        .get_last_operation_by_type(OperationType::Push)
        .ok_or_else(|| anyhow!("No push operation found in history to undo"))?;

    // Get the commit hash to reset to
    // New operations store this in commit_hash field directly
    // Legacy operations may have it in a snapshot file
    let target_commit = if let Some(ref hash) = last_push.commit_hash {
        hash.clone()
    } else if let Some(ref snapshot_path) = last_push.snapshot_path {
        // Legacy: load from snapshot file
        if !snapshot_path.exists() {
            return Err(anyhow!(
                "No commit hash in operation record and snapshot file not found: {}",
                snapshot_path.display()
            ));
        }
        let snapshot = Snapshot::load_from_disk(snapshot_path)?;
        snapshot
            .git_commit_hash
            .ok_or_else(|| anyhow!("No commit hash found in snapshot"))?
    } else {
        return Err(anyhow!(
            "No commit hash found for last push operation. Cannot undo."
        ));
    };

    // Open the SCM repository
    let repo = scm::open(repo_path)
        .with_context(|| format!("Failed to open repository at {}", repo_path.display()))?;

    // Check if we need to warn about remote (before reset)
    let branch_name = last_push.branch.as_deref().unwrap_or("unknown");
    let needs_force_push = repo.has_remote("origin");

    // TRANSACTION-LIKE ORDERING: Update history FIRST, then perform reset.
    // This ensures that if the reset fails, the history is still consistent.

    // Step 1: Remove the push operation from history
    let mut history = OperationHistory::from_path(history_path.clone())?;
    history
        .remove_last_operation_by_type(OperationType::Push, history_path.clone())
        .context("Failed to remove push operation from history")?;

    // Step 2: Perform the reset
    repo.reset_soft(&target_commit)
        .context("Failed to reset repository to previous commit")?;

    // The repository now holds less than this machine's record of what it
    // synced, and the next pull would read the difference as deletions made
    // elsewhere. Forgetting the record puts this machine where a fresh clone
    // is: it deletes nothing and re-learns on the next sync.
    if let Ok(claude_dir) = crate::sync::discovery::claude_home_dir() {
        if let Err(e) = crate::artifacts::tracked::forget(&claude_dir, repo_path) {
            log::warn!("Failed to reset the artifact sync record: {e}");
        }
        // The BASE record is deliberately kept: its entries describe the
        // bytes this machine last synced — the pushed ones — which is what
        // keeps any post-push local edit protected against the rewound
        // repository. Forgetting an entry reads as "never synced", and the
        // next pull would remote-wins straight over the edit — the exact
        // data loss the record exists to prevent.
    }

    // Step 3: Clean up legacy snapshot file if it exists
    if let Some(ref snapshot_path) = last_push.snapshot_path {
        if snapshot_path.exists() {
            if let Err(e) = fs::remove_file(snapshot_path) {
                eprintln!(
                    "Warning: Failed to remove snapshot file {}: {}",
                    snapshot_path.display(),
                    e
                );
            }
        }
    }

    let short_commit = if target_commit.len() >= 8 {
        &target_commit[..8]
    } else {
        &target_commit
    };

    let mut summary = format!(
        "Successfully undone last push operation.\n\
        Reset repository to commit: {}\n\
        Branch: {}\n\
        Operation was at: {}",
        short_commit,
        branch_name,
        last_push.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
    );

    if needs_force_push {
        summary.push_str(&format!(
            "\n\n\
            WARNING: The remote repository was updated by the push.\n\
            You will need to force push to update the remote:\n\
            (For Git: git push --force origin {branch_name})"
        ));
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::undo::test_support::{
        create_test_file, metadata_only_snapshot, operation_count, operation_types,
        setup_test_repo, HistoryBuilder,
    };
    use chrono::Duration;
    use tempfile::tempdir;
    use uuid::Uuid;

    #[test]
    fn test_undo_pull_no_history() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");

        let result = undo_pull(Some(history_path), Some(temp_dir.path()));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No pull operation found"));
    }

    #[test]
    fn a_legacy_snapshot_never_rewrites_the_shared_record_wholesale() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");
        let repo_root = temp_dir.path().join("sync-repo");
        let other_repo = temp_dir.path().join("other-repo");

        // An artifact file the pull overwrote: the generic restore must
        // put its pre-pull bytes back.
        fs::create_dir_all(temp_dir.path().join("projects/x")).unwrap();
        let artifact = create_test_file(temp_dir.path(), "projects/x/fact.md", "pre-pull");

        // A LEGACY snapshot (written before the record_files
        // declarations): the shared tracked record rides among `files`
        // with no declarations at all, carrying pre-pull entries for TWO
        // repositories.
        let record_path = temp_dir.path().join(".claude-code-sync-tracked.json");
        // Built with serde_json so a Windows path's backslashes are escaped.
        let key = |path: &Path| path.to_string_lossy().into_owned();
        let pre_pull_record = serde_json::json!({ "repos": {
            key(&repo_root): ["projects/x/fact.md"],
            key(&other_repo): ["other/file.md"],
        }})
        .to_string();
        let mut snapshot = Snapshot::create(OperationType::Pull, vec![&artifact], None).unwrap();
        snapshot.files.insert(
            record_path.to_string_lossy().to_string(),
            pre_pull_record.into_bytes(),
        );
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        HistoryBuilder::new(&history_path)
            .push_on_repo(
                OperationType::Pull,
                "main",
                Some(&snapshot_path),
                Some(&repo_root),
            )
            .save();

        // The pull "ran" (the artifact changed) and the OTHER repository
        // recorded an entry of its own in the meantime.
        fs::write(&artifact, "post-pull").unwrap();
        let post_pull_record =
            serde_json::json!({ "repos": { key(&other_repo): ["later/entry.md"] } }).to_string();
        fs::write(&record_path, post_pull_record).unwrap();

        let summary = undo_pull(Some(history_path.clone()), Some(temp_dir.path())).unwrap();
        assert!(summary.contains("Successfully undone"));

        // The artifact restored...
        assert_eq!(fs::read_to_string(&artifact).unwrap(), "pre-pull");
        // ...the OTHER repository's entry survived untouched (a wholesale
        // restore would have wiped it back to the pre-pull file)...
        let now = fs::read_to_string(&record_path).unwrap();
        assert!(
            now.contains("later/entry.md"),
            "another repository's entries must survive the undo: {now}"
        );
        // ...and this repository's pre-pull entry came back surgically.
        assert!(
            now.contains("projects/x/fact.md"),
            "this repository's pre-pull entry is restored: {now}"
        );
    }

    #[test]
    fn test_undo_pull_success() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        let file1 = create_test_file(temp_dir.path(), "conversation.jsonl", "original");

        let snapshot = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        HistoryBuilder::new(&history_path)
            .push(OperationType::Pull, "main", Some(&snapshot_path))
            .save();

        fs::write(&file1, "modified by pull").unwrap();

        let result = undo_pull(Some(history_path), Some(temp_dir.path())).unwrap();
        assert!(result.contains("Successfully undone"));

        assert_eq!(fs::read_to_string(&file1).unwrap(), "original");
        assert!(!snapshot_path.exists(), "snapshot should be cleaned up");
    }

    #[test]
    fn test_undo_pull_missing_snapshot() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");

        HistoryBuilder::new(&history_path)
            .push(
                OperationType::Pull,
                "main",
                Some(Path::new("/nonexistent/snapshot.json")),
            )
            .save();

        let result = undo_pull(Some(history_path), Some(temp_dir.path()));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Snapshot file not found"));
    }

    #[test]
    #[serial_test::serial]
    fn test_undo_pull_ignores_an_artifact_sharing_a_records_file_name() {
        use crate::artifacts::engine::{apply_pull, plan_pull, push_artifacts};
        use crate::artifacts::registry::ArtifactToggles;
        use crate::filter::FilterConfig;

        let home = tempdir().unwrap();
        let claude = home.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        let repo = tempdir().unwrap();
        let snapshots_dir = home.path().join("snapshots");
        let history_path = home.path().join("history.json");
        let filter = FilterConfig {
            sync_artifacts: ArtifactToggles::all_enabled(),
            ..Default::default()
        };

        let prev = std::env::var("CLAUDE_CODE_SYNC_CLAUDE_DIR");
        std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", &claude);

        // A synced skill ships a decoy with the shared record's file name.
        let machine_a = tempdir().unwrap();
        let skill = machine_a
            .path()
            .join("skills/docs/.claude-code-sync-bases.json");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, "definitely not a record").unwrap();
        push_artifacts(
            machine_a.path(),
            repo.path(),
            &filter,
            &std::collections::HashSet::new(),
        )
        .unwrap();

        let plan = plan_pull(&claude, repo.path(), &filter).unwrap();
        let mut snapshot = Snapshot::create(
            OperationType::Pull,
            plan.paths_to_snapshot(false).iter(),
            None,
        )
        .unwrap();
        snapshot.deleted_files = plan.created_paths();
        snapshot.attach_record_bookkeeping(&plan, &claude, false);
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
        let decoy = claude.join("skills/docs/.claude-code-sync-bases.json");
        apply_pull(&plan, false).unwrap();
        assert!(decoy.is_file(), "the decoy was pulled like any file");

        HistoryBuilder::new(&history_path)
            .push_on_repo(
                OperationType::Pull,
                "main",
                Some(&snapshot_path),
                Some(repo.path()),
            )
            .save();

        // Restore the env before asserting: a failed unwrap or assert
        // must not leak the override into the serial suite.
        let result = undo_pull(Some(history_path), Some(home.path()));
        match prev {
            Ok(v) => std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", v),
            Err(_) => std::env::remove_var("CLAUDE_CODE_SYNC_CLAUDE_DIR"),
        }
        let result = result.unwrap();
        assert!(result.contains("Successfully undone"));
        assert!(
            !decoy.is_file(),
            "the decoy is an ordinary created file: undo removes it again"
        );
        // And the real record was never hijacked: no repo entry was parsed
        // out of "definitely not a record".
        assert!(
            crate::artifacts::bases::load(&claude, repo.path()).is_empty(),
            "the decoy never fed the record surgery"
        );
    }

    #[test]
    #[serial_test::serial]
    #[cfg(unix)] // The respelling is a symlink; Windows cannot fake one unprivileged.
    fn test_undo_pull_surgery_survives_a_claude_dir_respelling() {
        use crate::artifacts::engine::{apply_pull, plan_pull, push_artifacts};
        use crate::artifacts::registry::ArtifactToggles;
        use crate::filter::FilterConfig;

        let home = tempdir().unwrap();
        let claude = home.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        // A different spelling of the same Claude directory, resolved only
        // at undo time: exact-path snapshot matching would miss it.
        let alias_home = tempdir().unwrap();
        let alias_claude = alias_home.path().join("claude-alias");
        std::os::unix::fs::symlink(&claude, &alias_claude).unwrap();

        let repo_y = tempdir().unwrap();
        let repo_x = tempdir().unwrap();
        let snapshots_dir = home.path().join("snapshots");
        let history_path = home.path().join("history.json");
        let filter = FilterConfig {
            sync_artifacts: ArtifactToggles::all_enabled(),
            ..Default::default()
        };

        let prev = std::env::var("CLAUDE_CODE_SYNC_CLAUDE_DIR");
        std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", &claude);

        let machine_a = tempdir().unwrap();
        let skill = machine_a.path().join("skills/my-skill/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, "v1\n").unwrap();
        push_artifacts(
            machine_a.path(),
            repo_y.path(),
            &filter,
            &std::collections::HashSet::new(),
        )
        .unwrap();

        let plan = plan_pull(&claude, repo_y.path(), &filter).unwrap();
        let mut snapshot = Snapshot::create(
            OperationType::Pull,
            plan.paths_to_snapshot(false).iter(),
            None,
        )
        .unwrap();
        snapshot.deleted_files = plan.created_paths();
        snapshot.attach_record_bookkeeping(&plan, &claude, false);
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
        apply_pull(&plan, false).unwrap();

        HistoryBuilder::new(&history_path)
            .push_on_repo(
                OperationType::Pull,
                "main",
                Some(&snapshot_path),
                Some(repo_y.path()),
            )
            .save();

        push_artifacts(
            &claude,
            repo_x.path(),
            &filter,
            &std::collections::HashSet::new(),
        )
        .unwrap();
        let x_entry = crate::artifacts::bases::load(&claude, repo_x.path());
        assert!(!x_entry.is_empty(), "repo X recorded its entry");

        // Undo resolves the Claude directory through the alias: the
        // surgery must still find the record entries, or the generic
        // restore would rewind the shared files wholesale. The allowed
        // base stays the real home — the alias only repoints the record
        // resolution, not the sandbox.
        std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", &alias_claude);
        // Restore the env before asserting: a failed unwrap or assert
        // must not leak the override into the serial suite.
        let result = undo_pull(Some(history_path), Some(home.path()));
        match prev {
            Ok(v) => std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", v),
            Err(_) => std::env::remove_var("CLAUDE_CODE_SYNC_CLAUDE_DIR"),
        }
        let result = result.unwrap();
        assert!(result.contains("Successfully undone"));
        assert_eq!(
            crate::artifacts::bases::load(&claude, repo_x.path()),
            x_entry,
            "other repositories' entries survive the respelled undo untouched"
        );
        assert!(
            crate::artifacts::bases::load(&claude, repo_y.path()).is_empty(),
            "the undone repository's entry is forgotten"
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_undo_pull_restores_only_that_repositorys_base_entry() {
        use crate::artifacts::engine::{apply_pull, plan_pull, push_artifacts};
        use crate::artifacts::registry::ArtifactToggles;
        use crate::filter::FilterConfig;

        let home = tempdir().unwrap();
        let claude = home.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        let repo_y = tempdir().unwrap();
        let repo_x = tempdir().unwrap();
        let snapshots_dir = home.path().join("snapshots");
        let history_path = home.path().join("history.json");

        let filter = FilterConfig {
            sync_artifacts: ArtifactToggles::all_enabled(),
            ..Default::default()
        };

        let prev = std::env::var("CLAUDE_CODE_SYNC_CLAUDE_DIR");
        std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", &claude);

        // A pull of repo Y on a machine with no base record yet — the pull
        // creates the file, so the snapshot carries the created-record
        // signal the same way pull_history records it.
        let machine_a = tempdir().unwrap();
        let skill = machine_a.path().join("skills/my-skill/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, "v1\n").unwrap();
        push_artifacts(
            machine_a.path(),
            repo_y.path(),
            &filter,
            &std::collections::HashSet::new(),
        )
        .unwrap();

        let plan = plan_pull(&claude, repo_y.path(), &filter).unwrap();
        let mut snapshot = Snapshot::create(
            OperationType::Pull,
            plan.paths_to_snapshot(false).iter(),
            None,
        )
        .unwrap();
        snapshot.deleted_files = plan.created_paths();
        // The record declarations, exactly as pull_history records them.
        snapshot.attach_record_bookkeeping(&plan, &claude, false);
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
        let pulled_file = claude.join("skills/my-skill/SKILL.md");
        apply_pull(&plan, false).unwrap();
        assert!(pulled_file.is_file(), "the pull created the file");

        HistoryBuilder::new(&history_path)
            .push_on_repo(
                OperationType::Pull,
                "main",
                Some(&snapshot_path),
                Some(repo_y.path()),
            )
            .save();

        // A push of repo X afterwards records X's entries into the SAME
        // shared files.
        push_artifacts(
            &claude,
            repo_x.path(),
            &filter,
            &std::collections::HashSet::new(),
        )
        .unwrap();
        assert!(!crate::artifacts::bases::load(&claude, repo_y.path()).is_empty());
        let x_bases_entry = crate::artifacts::bases::load(&claude, repo_x.path());
        assert!(!x_bases_entry.is_empty(), "repo X recorded its bases entry");
        let x_tracked_entry = crate::artifacts::tracked::load(&claude, repo_x.path());
        assert!(
            !x_tracked_entry.is_empty(),
            "repo X recorded its tracked entry"
        );

        // Undoing Y's pull must forget Y's entries and leave X's alone —
        // never delete or rewind the shared files wholesale.
        // Restore the env before asserting: a failed unwrap or assert must
        // not leak the override into the rest of the serial suite.
        let result = undo_pull(Some(history_path), Some(home.path()));
        match prev {
            Ok(v) => std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", v),
            Err(_) => std::env::remove_var("CLAUDE_CODE_SYNC_CLAUDE_DIR"),
        }
        let result = result.unwrap();
        assert!(result.contains("Successfully undone"));
        assert!(
            crate::artifacts::bases::load(&claude, repo_y.path()).is_empty(),
            "the undone repository's bases entry is forgotten"
        );
        assert_eq!(
            crate::artifacts::bases::load(&claude, repo_x.path()),
            x_bases_entry,
            "the other repository's bases entries survive the undo untouched"
        );
        assert!(
            crate::artifacts::tracked::load(&claude, repo_y.path()).is_empty(),
            "the undone repository's tracked entry is forgotten"
        );
        assert_eq!(
            crate::artifacts::tracked::load(&claude, repo_x.path()),
            x_tracked_entry,
            "the other repository's tracked entries survive the undo untouched"
        );
        assert!(
            crate::artifacts::bases::record_path(&claude).is_file(),
            "the shared file itself survives"
        );
        assert!(
            crate::artifacts::tracked::record_path(&claude).is_file(),
            "the tracked file itself survives"
        );
        assert!(!pulled_file.is_file(), "the pulled file is removed again");
    }

    #[test]
    #[serial_test::serial]
    fn test_undo_push_forgets_tracked_but_keeps_the_base_record() {
        let (temp_dir, repo) = setup_test_repo();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        let initial_hash = repo.current_commit_hash().unwrap();
        let new_file = create_test_file(temp_dir.path(), "new.txt", "new content");
        repo.stage_all().unwrap();
        repo.commit("Second commit").unwrap();

        let mut snapshot =
            Snapshot::create(OperationType::Push, vec![&new_file], Some(&initial_hash)).unwrap();
        snapshot.git_commit_hash = Some(initial_hash.clone());
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        HistoryBuilder::new(&history_path)
            .push(OperationType::Push, "master", Some(&snapshot_path))
            .save();

        // The tracked record describes the repo as it was before the
        // rewind and must be forgotten, or the next pull would act on
        // state the repository no longer reflects. The base record must
        // survive — see the assertions below.
        let claude = tempdir().unwrap();
        let prev = std::env::var("CLAUDE_CODE_SYNC_CLAUDE_DIR");
        std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", claude.path());
        crate::artifacts::bases::save(
            claude.path(),
            temp_dir.path(),
            [("a".to_string(), "aa".to_string())].into_iter().collect(),
        )
        .unwrap();
        crate::artifacts::tracked::save(
            claude.path(),
            temp_dir.path(),
            ["a".to_string()].into_iter().collect(),
        )
        .unwrap();

        let result = undo_push(temp_dir.path(), Some(history_path)).unwrap();

        match prev {
            Ok(v) => std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", v),
            Err(_) => std::env::remove_var("CLAUDE_CODE_SYNC_CLAUDE_DIR"),
        }
        assert!(result.contains("Successfully undone"));
        // The tracked record is forgotten (it drives deletion planning and
        // would read the rewind as deletions made elsewhere)...
        assert!(
            crate::artifacts::tracked::load(claude.path(), temp_dir.path()).is_empty(),
            "the tracked record is forgotten"
        );
        // ...but the base record is KEPT: its entries describe the pushed
        // bytes, which is what keeps post-push local edits protected
        // against the rewound repository.
        assert_eq!(
            crate::artifacts::bases::load(claude.path(), temp_dir.path()),
            [("a".to_string(), "aa".to_string())].into_iter().collect(),
            "the base record survives the undo push"
        );
    }

    #[test]
    fn test_undo_push_success() {
        let (temp_dir, repo) = setup_test_repo();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        let initial_hash = repo.current_commit_hash().unwrap();

        // Commit something on top, simulating the push we're about to undo.
        let new_file = temp_dir.path().join("new.txt");
        fs::write(&new_file, "new content").unwrap();
        repo.stage_all().unwrap();
        repo.commit("Second commit").unwrap();

        let mut snapshot =
            Snapshot::create(OperationType::Push, vec![&new_file], Some(&initial_hash)).unwrap();
        snapshot.git_commit_hash = Some(initial_hash.clone());
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        HistoryBuilder::new(&history_path)
            .push(OperationType::Push, "master", Some(&snapshot_path))
            .save();

        let result = undo_push(temp_dir.path(), Some(history_path)).unwrap();
        assert!(result.contains("Successfully undone"));
        assert!(result.contains(&initial_hash[..8]));

        let repo_check = scm::open(temp_dir.path()).unwrap();
        assert_eq!(repo_check.current_commit_hash().unwrap(), initial_hash);
    }

    #[test]
    fn test_undo_push_no_history() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");

        let result = undo_push(temp_dir.path(), Some(history_path));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No push operation found"));
    }

    #[test]
    fn test_undo_push_missing_commit_hash() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        // A push snapshot with no commit hash: there is nothing to reset to.
        let mut snapshot = metadata_only_snapshot(
            &Uuid::new_v4().to_string(),
            OperationType::Push,
            Duration::zero(),
        );
        snapshot.branch = Some("main".to_string());
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        HistoryBuilder::new(&history_path)
            .push(OperationType::Push, "main", Some(&snapshot_path))
            .save();

        let repo = scm::init(temp_dir.path()).unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test").unwrap();
        repo.stage_all().unwrap();
        repo.commit("Initial commit").unwrap();

        let result = undo_push(temp_dir.path(), Some(history_path));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No commit hash found"));
    }

    #[test]
    fn test_undo_pull_preserves_other_operations() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        let file1 = create_test_file(temp_dir.path(), "conversation.jsonl", "original");

        let snapshot1 = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();
        let snapshot_path1 = snapshot1.save_to_disk(Some(&snapshots_dir)).unwrap();
        let snapshot2 = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();
        let snapshot_path2 = snapshot2.save_to_disk(Some(&snapshots_dir)).unwrap();

        // Pull, then push, then pull. Undoing should take only the last pull.
        HistoryBuilder::new(&history_path)
            .push(OperationType::Pull, "main", Some(&snapshot_path1))
            .push(OperationType::Push, "main", None)
            .push(OperationType::Pull, "main", Some(&snapshot_path2))
            .save();

        assert_eq!(operation_count(&history_path), 3);

        let result = undo_pull(Some(history_path.clone()), Some(temp_dir.path())).unwrap();
        assert!(result.contains("Successfully undone"));

        // Most recent first: the push, then the earlier pull. Only the newest
        // pull was consumed.
        assert_eq!(
            operation_types(&history_path),
            vec![OperationType::Push, OperationType::Pull],
            "the earlier pull and the push must survive"
        );
    }

    #[test]
    fn test_undo_push_preserves_other_operations() {
        let (temp_dir, repo) = setup_test_repo();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        let initial_hash = repo.current_commit_hash().unwrap();

        let new_file = temp_dir.path().join("new.txt");
        fs::write(&new_file, "new content").unwrap();
        repo.stage_all().unwrap();
        repo.commit("Second commit").unwrap();

        let mut snapshot =
            Snapshot::create(OperationType::Push, vec![&new_file], Some(&initial_hash)).unwrap();
        snapshot.git_commit_hash = Some(initial_hash.clone());
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        HistoryBuilder::new(&history_path)
            .push(OperationType::Pull, "master", None)
            .push(OperationType::Push, "master", Some(&snapshot_path))
            .save();

        assert_eq!(operation_count(&history_path), 2);

        let result = undo_push(temp_dir.path(), Some(history_path.clone())).unwrap();
        assert!(result.contains("Successfully undone"));

        assert_eq!(
            operation_types(&history_path),
            vec![OperationType::Pull],
            "the pull must survive"
        );
    }

    #[test]
    fn test_undo_pull_transaction_safety() {
        // History is updated BEFORE files are restored, so that a failure to
        // restore can't leave a consumed snapshot still listed as undoable.
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        let file1 = create_test_file(temp_dir.path(), "conversation.jsonl", "original");

        let snapshot = Snapshot::create(OperationType::Pull, vec![&file1], None).unwrap();
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        HistoryBuilder::new(&history_path)
            .push(OperationType::Pull, "main", Some(&snapshot_path))
            .save();
        assert_eq!(operation_count(&history_path), 1);

        fs::write(&file1, "modified by pull").unwrap();

        // Make restoration as likely to fail as we portably can.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&file1).unwrap().permissions();
            perms.set_mode(0o444); // read-only
            fs::set_permissions(&file1, perms).unwrap();
        }

        let result = undo_pull(Some(history_path.clone()), Some(temp_dir.path()));

        // Whether or not restoration succeeded, history must already be updated.
        assert_eq!(
            operation_count(&history_path),
            0,
            "History should be updated even if file restoration fails"
        );

        // A failed restoration now degrades to a warning instead of an
        // Err (the history is already rewritten — an Err would strand a
        // partially-restored machine silently): the undo succeeds, says
        // so, and KEEPS the snapshot, pinned, as the only copy of the
        // pre-pull files. Only Unix can make the restore fail (above), so
        // only there is the kept snapshot asserted.
        let summary = result.unwrap();
        #[cfg(not(unix))]
        let _ = summary;
        #[cfg(unix)]
        assert!(
            summary.contains("KEPT"),
            "the summary announces the kept snapshot"
        );
        #[cfg(unix)]
        {
            assert!(
                snapshot_path.exists(),
                "a failed restore keeps the snapshot file"
            );
            let reloaded = Snapshot::load_from_disk(&snapshot_path).unwrap();
            assert!(
                reloaded.pinned,
                "the kept snapshot is pinned against cleanup"
            );
            assert!(
                reloaded
                    .files
                    .contains_key(&file1.to_string_lossy().to_string()),
                "the kept snapshot still carries the pre-pull files"
            );
        }

        // Restore write permission so the TempDir can be removed.
        #[cfg(unix)]
        if file1.exists() {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&file1).unwrap().permissions();
            perms.set_mode(0o644);
            let _ = fs::set_permissions(&file1, perms);
        }
    }

    #[test]
    fn test_undo_push_transaction_safety() {
        // Same ordering guarantee as the pull case: history first, reset second.
        let (temp_dir, repo) = setup_test_repo();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");

        let initial_hash = repo.current_commit_hash().unwrap();

        let new_file = temp_dir.path().join("new.txt");
        fs::write(&new_file, "new content").unwrap();
        repo.stage_all().unwrap();
        repo.commit("Second commit").unwrap();

        let mut snapshot =
            Snapshot::create(OperationType::Push, vec![&new_file], Some(&initial_hash)).unwrap();
        snapshot.git_commit_hash = Some(initial_hash.clone());
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        HistoryBuilder::new(&history_path)
            .push(OperationType::Push, "master", Some(&snapshot_path))
            .save();
        assert_eq!(operation_count(&history_path), 1);

        let result = undo_push(temp_dir.path(), Some(history_path.clone()));

        assert_eq!(
            operation_count(&history_path),
            0,
            "History should be updated even if git reset fails"
        );

        if result.is_ok() {
            let repo_check = scm::open(temp_dir.path()).unwrap();
            assert_eq!(repo_check.current_commit_hash().unwrap(), initial_hash);
            assert!(
                !snapshot_path.exists(),
                "Snapshot should be cleaned up on success"
            );
        }
    }
    #[test]
    fn a_corrupt_record_blob_is_not_read_as_an_empty_entry() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");
        let repo_root = temp_dir.path().join("sync-repo");

        fs::create_dir_all(temp_dir.path().join("projects/x")).unwrap();
        let artifact = create_test_file(temp_dir.path(), "projects/x/fact.md", "pre-pull");

        // A snapshot whose bases blob is TRUNCATED (a partial write): the
        // per-key restore must treat it as corrupt — warn, keep, pin —
        // never read it as an empty pre-pull entry and DELETE the live
        // keys the pull moved.
        let record_path = temp_dir.path().join(".claude-code-sync-bases.json");
        let mut snapshot = Snapshot::create(OperationType::Pull, vec![&artifact], None).unwrap();
        snapshot.files.insert(
            record_path.to_string_lossy().to_string(),
            br#"{"repos": {"/trunc"#.to_vec(),
        );
        snapshot.record_files = vec![record_path.to_string_lossy().to_string()];
        snapshot.record_touched_bases = Some(vec!["projects/x/fact.md".to_string()]);
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        HistoryBuilder::new(&history_path)
            .push_on_repo(
                OperationType::Pull,
                "main",
                Some(&snapshot_path),
                Some(&repo_root),
            )
            .save();
        // The pull wrote a bases entry for this repo; the undo must NOT
        // delete it (the corrupt blob carries nothing restorable).
        crate::artifacts::bases::save(
            temp_dir.path(),
            &repo_root,
            [("projects/x/fact.md".to_string(), "hash".to_string())]
                .into_iter()
                .collect(),
        )
        .unwrap();

        let summary = undo_pull(Some(history_path), Some(temp_dir.path())).unwrap();
        assert!(
            summary.contains("KEPT"),
            "a corrupt blob pins the snapshot: {summary}"
        );
        assert!(
            !crate::artifacts::bases::load(temp_dir.path(), &repo_root).is_empty(),
            "the live entry survived the corrupt blob"
        );
    }
    #[test]
    fn a_partly_failed_restore_leaves_the_records_post_pull() {
        let temp_dir = tempdir().unwrap();
        let history_path = temp_dir.path().join("history.json");
        let snapshots_dir = temp_dir.path().join("snapshots");
        let repo_root = temp_dir.path().join("sync-repo");

        fs::create_dir_all(temp_dir.path().join("projects/x")).unwrap();
        let artifact = create_test_file(temp_dir.path(), "projects/x/fact.md", "pre-pull");

        let mut snapshot = Snapshot::create(OperationType::Pull, vec![&artifact], None).unwrap();
        snapshot.record_touched_bases = Some(vec!["projects/x/fact.md".to_string()]);
        snapshot.record_files = vec![crate::artifacts::bases::record_path(temp_dir.path())
            .to_string_lossy()
            .to_string()];
        let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

        HistoryBuilder::new(&history_path)
            .push_on_repo(
                OperationType::Pull,
                "main",
                Some(&snapshot_path),
                Some(&repo_root),
            )
            .save();

        // The pull ran; restoring the file will FAIL (read-only), as in
        // the transaction-safety test.
        fs::write(&artifact, "post-pull").unwrap();
        crate::artifacts::bases::save(
            temp_dir.path(),
            &repo_root,
            [("projects/x/fact.md".to_string(), "postpull".to_string())]
                .into_iter()
                .collect(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&artifact).unwrap().permissions();
            perms.set_mode(0o444);
            fs::set_permissions(&artifact, perms).unwrap();
        }

        let summary = undo_pull(Some(history_path), Some(temp_dir.path())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&artifact).unwrap().permissions();
            perms.set_mode(0o644);
            fs::set_permissions(&artifact, perms).unwrap();
        }
        assert!(
            summary.contains("KEPT"),
            "the snapshot is pinned: {summary}"
        );
        // The record entry was NOT rewound to pre-pull: the files are
        // mostly post-pull, and a pre-pull record would read them as
        // local edits and push the pull right back.
        assert_eq!(
            crate::artifacts::bases::load(temp_dir.path(), &repo_root)
                .get("projects/x/fact.md")
                .map(String::as_str),
            Some("postpull"),
            "a partly failed restore leaves the records post-pull"
        );
    }
}
