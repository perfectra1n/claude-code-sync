//! The artifact copy engine: registry-driven push/pull between `~/.claude`
//! and the sync repository. Every function takes explicit paths — the
//! `~/.claude` default is resolved by callers in `crate::sync` — so tests run
//! against temp directories with no environment coupling.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::filter::FilterConfig;
use crate::scm::Backend;

use super::bases::{self, BaseHashes};
use super::denylist::{is_denied, is_unsafe_rel_path};
use super::memory_index::{is_memory_index, merge_memory_index};
use super::registry::{
    CategoryDescriptor, CategoryId, DestRoot, MergeStrategy, SourceSpec, ARTIFACTS_SUBDIR, REGISTRY,
};
use super::tokens::PathTokens;
use super::tracked::{self, TrackedPaths};
use super::union_jsonl::merge_history_lines;
use crate::later_timestamps::keep_later_timestamps;

/// Whether one category participates for this configuration: toggles for the
/// regular categories, the (inverted) attachments flag for ProjectAttachments.
pub fn is_category_enabled(desc: &CategoryDescriptor, filter: &FilterConfig) -> bool {
    match desc.id {
        CategoryId::ProjectAttachments => !filter.exclude_attachments,
        _ => filter.sync_artifacts.is_enabled(desc.id),
    }
}

/// All registry rows active under this configuration.
fn active_categories(
    filter: &FilterConfig,
) -> impl Iterator<Item = &'static CategoryDescriptor> + '_ {
    REGISTRY.iter().filter(|d| is_category_enabled(d, filter))
}

/// Whether a file under this merge strategy records a synced base — the
/// ONE rule, consulted by push and pull.
///
/// All user-editable files need a base: the local-edit-protection
/// contract (issue #103) requires a recorded base so the next pull
/// can detect a local edit and route to `kept_local` instead of
/// silently clobbering the user's changes. For union-merge strategies
/// the base is the local snapshot at push time, and the apply
/// re-records after every merge so the next pull sees local == base
/// and skips the merge when nothing new arrived. Prompt history
/// (`UnionJsonl`) is append-only and does not need per-line
/// protection.
pub(crate) fn records_base(strategy: MergeStrategy, _rel: &Path) -> bool {
    match strategy {
        MergeStrategy::RawOverwrite => true,
        MergeStrategy::UnionJsonl => false,
        MergeStrategy::UnionMemoryIndex => true,
    }
}

/// The plan's one size-gate triage: outside the limit, or unreadable — a
/// stat failure must never read as size 0 and wave an oversized (or
/// broken) file past the guard. Shared by the overwrite arm and the
/// gone-scan delete arm so the fail-closed semantics cannot drift apart.
fn outside_size_limit(local_path: &Path, filter: &FilterConfig) -> bool {
    metadata_outside_limit(fs::metadata(local_path), filter.max_file_size_bytes)
}

/// The triage's core, shared with the repo-side scan (which already
/// holds the metadata): fail closed — a stat failure must never read as
/// size 0.
fn metadata_outside_limit<E>(metadata: Result<fs::Metadata, E>, max: u64) -> bool {
    metadata.map_or(true, |m| m.len() > max)
}

/// What a fresh read of a local file says about its edit status relative
/// to the recorded base — the ONE triage, shared by the plan's delete
/// arm and both apply arms (overwrites and deletes).
#[derive(Debug, PartialEq, Eq)]
enum FreshRead {
    /// Matches the base, or no base recorded (legacy remote-wins): safe
    /// to overwrite or delete. No base means no read at all.
    Clean,
    /// Edited here since the last sync: must be kept.
    Dirty,
    /// Gone from disk since the plan: proceed as if absent.
    Vanished,
    /// Present but unreadable: keep and report, never guess and never
    /// fail the whole sync over one bad file.
    Unreadable,
}

fn dirty_status(recorded: Option<&String>, path: &Path) -> FreshRead {
    let Some(base) = recorded else {
        return FreshRead::Clean;
    };
    match fs::read(path) {
        Ok(bytes) if bases::is_dirty(Some(base), &bytes) => FreshRead::Dirty,
        Ok(_) => FreshRead::Clean,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FreshRead::Vanished,
        Err(e) => {
            log::warn!(
                "Keeping {} (unreadable, edited-status unknown): {e}",
                path.display()
            );
            FreshRead::Unreadable
        }
    }
}

/// F3-class synthetic-Dirty guard: a missing base (None recorded,
/// concurrent sync pruned it) is treated as "potentially edited"
/// — silently destroying the file would discard a possibly-edited
/// user's edit, the exact loss the protect-local-edits contract
/// exists to prevent. Two arms (kept_local_deletes, deletes)
/// share this exact predicate; this helper centralises it.
fn is_potentially_dirty(recorded: Option<&String>, path: &Path) -> bool {
    recorded.is_none() || dirty_status(recorded, path) == FreshRead::Dirty
}

/// Plan-time F3-class signal: a file with no recorded base AND a
/// tracked entry is "we synced this before, the base got pruned
/// out from under us" — a potentially-edited file. Fresh installs
/// (no tracked entry) must still remote-win.
///
/// This is the plan-time analogue of `is_potentially_dirty` for
/// arms that operate on already-read bytes (overwrites): it
/// substitutes the tracked_before set for the file-read step,
/// because the file content is in scope at the call site rather
/// than on disk in a known state.
fn f3_class_potentially_dirty(
    base_hashes: &BaseHashes,
    tracked_before: &TrackedPaths,
    rel: &str,
) -> bool {
    base_hashes.get(rel).is_none() && tracked_before.contains(rel)
}

/// Whether the repository still holds the version this machine last synced,
/// so a local difference is this machine's change alone and the push that
/// follows publishes it. The protection sentinel records no real version and
/// never matches, and neither does a held base (see [`bases::held_base`]).
fn repo_unchanged_since_sync(recorded: Option<&String>, repo_bytes: &[u8]) -> bool {
    recorded.is_some_and(|base| {
        bases::is_common_ancestor(base) && *base == bases::hash_bytes(repo_bytes)
    })
}

/// Whether a missing local file is a deletion made here: this machine synced
/// it, the repository still holds that version, and the category mirrors
/// deletions. Such a file is left deleted for the push to remove from the
/// repository, instead of being recreated. A whole missing category folder is
/// a machine that never had it, not a deletion.
fn deleted_here_since_sync(
    desc: &CategoryDescriptor,
    claude_dir: &Path,
    tokens: &PathTokens,
    base_hashes: &BaseHashes,
    repo_root: &Path,
    repo_path: &Path,
) -> bool {
    if !desc.mirror_deletes || !source_is_present(desc, claude_dir) {
        return false;
    }
    let recorded = repo_relative(repo_root, repo_path).and_then(|rel| base_hashes.get(&rel));
    recorded.is_some()
        && machine_bytes(desc, tokens, repo_path)
            .is_ok_and(|repo_bytes| repo_unchanged_since_sync(recorded, &repo_bytes))
}

/// The sync-repo root directory for one category.
fn category_repo_root(
    desc: &CategoryDescriptor,
    repo_root: &Path,
    filter: &FilterConfig,
) -> PathBuf {
    match desc.dest {
        DestRoot::Artifacts => repo_root.join(ARTIFACTS_SUBDIR).join(desc.repo_subdir),
        DestRoot::SessionTree => repo_root.join(&filter.sync_subdirectory),
    }
}

/// True when a file extension is excluded for this category.
fn extension_excluded(desc: &CategoryDescriptor, path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| {
            desc.exclude_extensions
                .iter()
                .any(|x| ext.eq_ignore_ascii_case(x))
        })
        .unwrap_or(false)
}

/// Per-category outcome counts for one push or pull.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CategoryCounts {
    pub category: CategoryId,
    #[serde(default)]
    pub added: usize,
    #[serde(default)]
    pub modified: usize,
    #[serde(default)]
    pub unchanged: usize,
    /// Files skipped (size cap, denied names).
    #[serde(default)]
    pub skipped: usize,
    /// New lines contributed by a union merge (prompt history).
    #[serde(default)]
    pub merged_entries: usize,
    /// Files removed because this machine synced them before and no longer has
    /// them (only for categories that mirror deletions).
    #[serde(default)]
    pub deleted: usize,
    /// Files a pull left untouched because they were edited locally since the
    /// last sync — a `push` is what publishes them.
    #[serde(default)]
    pub kept_local: usize,
    /// Files a pull left as they are because only this machine changed (or
    /// deleted) them since the last sync — the push that follows publishes
    /// them. Unlike `kept_local`, nothing is held back.
    #[serde(default)]
    pub pending_push: usize,
    /// Files the push refused to re-publish because the remote lost them
    /// since the last sync (adversarial review 2026-10-09, 5ff1d62 hole).
    /// The local copy is preserved on disk. There is no CLI override
    /// by design — re-introducing a file the remote intentionally lost
    /// is the dangerous operation, and the safe default is to honor
    /// the deletion. The escape hatch requires a `pull` after the local
    /// `rm` (the pull sees local and repo both gone, clears the
    /// tracked entry; the next push on a freshly created local file
    /// then publishes as `Added`).
    #[serde(default)]
    pub held_back_remote_lost: usize,
}

impl CategoryCounts {
    fn new(category: CategoryId) -> Self {
        CategoryCounts {
            category,
            added: 0,
            modified: 0,
            unchanged: 0,
            skipped: 0,
            merged_entries: 0,
            deleted: 0,
            kept_local: 0,
            pending_push: 0,
            held_back_remote_lost: 0,
        }
    }
}

/// How one artifact file changed during a push or pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactChangeKind {
    Added,
    Modified,
    Deleted,
    /// Left untouched because it was edited locally since the last sync;
    /// a `push` is what publishes it.
    KeptLocal,
    /// The local file is gone (vanished mid-pull, or user-confirmed
    /// during the prompt) and the repo's deletion is being honored.
    /// A `push` must NOT recreate the file in the repo: the
    /// removal has been accepted, and the next pull's
    /// gone-from-both-sides pass is what clears the record entry.
    KeptLocalDelete,
    /// The push refused to re-publish a file the remote lost since the
    /// last sync (adversarial review 2026-10-09, 5ff1d62 hole). The
    /// local copy is preserved on disk; the file path is in
    /// `ArtifactChange.path` (repo-relative). The user-facing escape
    /// hatch is `claude-code-sync push --resurrect <path>` (see
    /// `engine::prepare_resurrection`).
    HeldBackRemoteLost,
    /// A pull planned to write this file but did not (it appeared,
    /// vanished or became unreadable while the pull ran, had no backup,
    /// or a write failed). The local side is then not a decision about
    /// the repository version, so the push that follows must leave the
    /// path alone — publishing it, or mirroring its absence as a
    /// deletion, could overwrite a change another machine made. Retried
    /// by the next pull.
    Unfinished,
}

/// One file a push or pull actually added, modified or deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactChange {
    pub category: CategoryId,
    pub kind: ArtifactChangeKind,
    /// Path relative to the category's root, e.g. `my-skill/SKILL.md`.
    pub path: PathBuf,
}

/// Outcome of one artifact push or pull across all enabled categories.
#[derive(Debug, Clone, Default)]
pub struct ArtifactReport {
    pub counts: Vec<CategoryCounts>,
    /// Every file written or removed, in the order it happened.
    pub changes: Vec<ArtifactChange>,
    /// The repo-relative keys the apply actually MOVED in each shared
    /// record — wrote, removed, or durably declined. The operation record
    /// carries them on, and `undo pull` restores exactly these keys: a
    /// superset would revert entries a LATER push legitimately recorded
    /// (the plan's declared-touched list includes no-op re-writes, e.g.
    /// unchanged files whose base already matched).
    pub bases_keys_written: Vec<String>,
    pub tracked_keys_written: Vec<String>,
    /// The creates the apply actually EXECUTED (absolute paths). The
    /// pull's snapshot narrows its undo-delete list to these: a
    /// user-written file that appeared mid-pull is deliberately skipped
    /// (a push publishes it) and must survive the undo.
    pub created_abs_paths: Vec<PathBuf>,
    /// Of the kept-local files, how many matched their base at keep time
    /// — a write that failed, an edit reverted while the pull waited.
    /// They hold the sync gate (fail-safe) but have NOTHING to publish:
    /// the "push to publish" advice would publish their stale bytes, and
    /// the hint must not give it for them.
    pub kept_local_clean: usize,
}

impl ArtifactReport {
    pub fn total_added(&self) -> usize {
        self.counts.iter().map(|c| c.added).sum()
    }
    pub fn total_modified(&self) -> usize {
        self.counts.iter().map(|c| c.modified).sum()
    }
    pub fn total_unchanged(&self) -> usize {
        self.counts.iter().map(|c| c.unchanged).sum()
    }
    pub fn total_deleted(&self) -> usize {
        self.counts.iter().map(|c| c.deleted).sum()
    }
    /// The paths the push right after this pull must leave alone: files the
    /// pull kept (both machines changed them) or did not finish. `sync`
    /// passes these to its push, so the push never publishes — or deletes
    /// from the repository — a version this machine has not reconciled
    /// with the other machine's.
    pub fn paths_the_push_must_skip(&self) -> HashSet<(CategoryId, PathBuf)> {
        self.changes
            .iter()
            .filter(|c| {
                matches!(
                    c.kind,
                    ArtifactChangeKind::KeptLocal
                        | ArtifactChangeKind::KeptLocalDelete
                        | ArtifactChangeKind::Unfinished
                )
            })
            .map(|c| (c.category, c.path.clone()))
            .collect()
    }

    pub fn total_kept_local(&self) -> usize {
        self.counts.iter().map(|c| c.kept_local).sum()
    }
    pub fn total_skipped(&self) -> usize {
        self.counts.iter().map(|c| c.skipped).sum()
    }
    /// KeptLocalDelete is counted under `deleted` in CategoryCounts; the
    /// per-kind total lives here, off the changes list.
    pub fn total_kept_local_deletes(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| c.kind == ArtifactChangeKind::KeptLocalDelete)
            .count()
    }

    /// Files the push refused to re-publish because the remote lost
    /// them since the last sync (adversarial review 2026-10-09, 5ff1d62
    /// hole). Surfaced in the push summary and the history so the
    /// user can see that a deletion they made (or that the remote
    /// received) was preserved.
    pub fn total_held_back_remote_lost(&self) -> usize {
        self.counts.iter().map(|c| c.held_back_remote_lost).sum()
    }

    /// The repo-relative paths of every file the 5ff1d62 push guard
    /// refused, in push-iteration order. Each entry is paired with the
    /// category whose push would have published the file. The push summary
    /// lists these so the user can see exactly which files were held back
    /// (the `status --held-back` command lists the same set without
    /// performing a push).
    pub fn held_back_paths(
        &self,
    ) -> impl Iterator<Item = (&CategoryId, &PathBuf, ArtifactChangeKind)> {
        self.changes.iter().filter_map(|c| {
            if c.kind == ArtifactChangeKind::HeldBackRemoteLost {
                Some((&c.category, &c.path, c.kind))
            } else {
                None
            }
        })
    }

    fn record(&mut self, category: CategoryId, kind: ArtifactChangeKind, path: &Path) {
        self.changes.push(ArtifactChange {
            category,
            kind,
            path: path.to_path_buf(),
        });
    }
}

/// One collected artifact file: its absolute source under `~/.claude` and its
/// destination path relative to the category's repo subdirectory.
struct CollectedFile {
    abs: PathBuf,
    rel: PathBuf,
}

/// Enumerate a category's files on disk. Missing sources yield an empty list;
/// denied paths and oversized files are skipped (the latter counted).
///
/// An oversized file still exists on this machine, so its category-relative
/// path goes into `held_back`: deletion mirroring must not read "not pushed
/// this time" as "deleted here" and remove every machine's copy.
fn collect(
    desc: &CategoryDescriptor,
    claude_dir: &Path,
    filter: &FilterConfig,
    skipped: &mut usize,
    held_back: &mut Vec<PathBuf>,
) -> Result<Vec<CollectedFile>> {
    let mut files = Vec::new();

    match desc.source {
        SourceSpec::Files(list) => {
            for entry in list {
                let claude_rel = Path::new(entry);
                if is_denied(claude_rel) {
                    *skipped += 1;
                    continue;
                }
                let abs = claude_dir.join(entry);
                if !abs.is_file() {
                    continue;
                }
                // The one shared fail-closed triage: the highest-impact
                // config files must not be the one place the limit sleeps.
                if outside_size_limit(&abs, filter) {
                    log::warn!("Skipping {} (exceeds max_file_size_bytes)", abs.display());
                    *skipped += 1;
                    held_back.push(
                        claude_rel
                            .file_name()
                            .map(PathBuf::from)
                            .unwrap_or_else(|| claude_rel.to_path_buf()),
                    );
                    continue;
                }
                let rel = claude_rel
                    .file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| claude_rel.to_path_buf());
                files.push(CollectedFile { abs, rel });
            }
        }
        SourceSpec::Dir(dir) => {
            let base = claude_dir.join(dir);
            if !base.is_dir() {
                return Ok(files);
            }
            // Resolved once per project: in name-only mode it reads a transcript.
            let mut project_names: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for entry in walkdir::WalkDir::new(&base)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let abs = entry.path();
                // Deny rules match against the ~/.claude-relative path so a
                // component like `cache/` is caught wherever it appears.
                let claude_rel = abs.strip_prefix(claude_dir).unwrap_or(abs);
                if is_denied(claude_rel) {
                    *skipped += 1;
                    continue;
                }
                if extension_excluded(desc, abs) {
                    continue;
                }
                let mut rel = abs.strip_prefix(&base).unwrap_or(abs).to_path_buf();
                // Attachments take the project's repo-side directory name,
                // mirroring session layout.
                if desc.dest == DestRoot::SessionTree {
                    let mut parts = rel.components();
                    let Some(encoded) = parts.next().and_then(|c| c.as_os_str().to_str()) else {
                        *skipped += 1;
                        continue;
                    };
                    let project = project_names
                        .entry(encoded.to_string())
                        .or_insert_with(|| {
                            crate::project_map::repo_dir_name_in(filter, &base, encoded)
                        })
                        .clone();
                    rel = Path::new(&project).join(parts.as_path());
                }
                // The one shared fail-closed triage, like the Files branch.
                if outside_size_limit(abs, filter) {
                    log::warn!("Skipping {} (exceeds max_file_size_bytes)", abs.display());
                    *skipped += 1;
                    held_back.push(rel);
                    continue;
                }
                files.push(CollectedFile {
                    abs: abs.to_path_buf(),
                    rel,
                });
            }
        }
    }

    Ok(files)
}

/// Write `content` to `path` via a same-directory temp file + atomic rename,
/// so a reader (or a crash) never sees a half-written file. `mode_source` is
/// the file whose executable bit the result adopts.
#[cfg_attr(not(unix), allow(unused_variables))]
fn write_atomic(path: &Path, content: &[u8], mode_source: &Path) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("No parent directory for {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    fs::write(tmp.path(), content)?;
    // Durability: without fsync, `rename(2)` is not durable across power
    // loss — the data can be in the page cache but not on disk when the
    // machine loses power. The file fsync flushes the data; the directory
    // fsync (Unix only) commits the rename entry itself. Both required
    // for crash-and-power-loss-safe writes.
    tmp.as_file().sync_all()?;
    #[cfg(unix)]
    {
        if let Some(mode) = get_mode_for_copy(path, mode_source) {
            set_mode(tmp.path(), mode);
        }
    }
    tmp.persist(path)
        .with_context(|| format!("Failed to persist {}", path.display()))?;
    #[cfg(unix)]
    {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

/// The mode a copy of `mode_source` should have at `destination`: what the
/// destination already has, plus the owner's executable bit when the source is
/// executable. Granting only: a repository written before this bit was synced
/// holds every file non-executable, and a pull from it must not disarm the
/// scripts on this machine.
#[cfg(unix)]
fn get_mode_for_copy(destination: &Path, mode_source: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;

    let source_mode = fs::metadata(mode_source).ok()?.permissions().mode();
    let base = match fs::metadata(destination) {
        Ok(existing) => existing.permissions().mode() & 0o777,
        Err(_) => 0o600,
    };
    if source_mode & 0o111 == 0 {
        return Some(base);
    }
    Some(base | 0o100)
}

/// Apply `mode` and report whether the file actually carries it afterwards. A
/// filesystem without permission bits (exFAT, some CIFS mounts) either refuses
/// the call or ignores it; either way the sync continues and the file counts as
/// unchanged, instead of being offered again on every run.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> bool {
    use std::os::unix::fs::PermissionsExt;

    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
        log::warn!("Could not set permissions on {}: {error}", path.display());
        return false;
    }
    let applied = fs::metadata(path).map(|m| m.permissions().mode() & 0o777);
    applied.is_ok_and(|applied| applied == mode)
}

/// Whether `path` is missing an executable bit that `mode_source` has. Content
/// comparison alone never notices a `chmod +x`, which would leave the bit stuck
/// at whatever it was when the file was first copied.
#[cfg(unix)]
fn executable_bit_differs(path: &Path, mode_source: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    if is_symlink(path) {
        return false;
    }
    let Some(wanted) = get_mode_for_copy(path, mode_source) else {
        return false;
    };
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    metadata.permissions().mode() & 0o777 != wanted
}

#[cfg(not(unix))]
fn executable_bit_differs(_path: &Path, _mode_source: &Path) -> bool {
    false
}

/// A chmod follows symlinks, so it would reach a file outside `~/.claude` that
/// a write never touches: `write_atomic` replaces the link itself.
#[cfg(unix)]
fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

/// Bring `path`'s executable bit in line with `mode_source`, reporting whether
/// the file changed.
#[cfg(unix)]
fn align_executable_bit(path: &Path, mode_source: &Path) -> bool {
    if !executable_bit_differs(path, mode_source) {
        return false;
    }
    let Some(mode) = get_mode_for_copy(path, mode_source) else {
        return false;
    };
    set_mode(path, mode)
}

#[cfg(not(unix))]
fn align_executable_bit(_path: &Path, _mode_source: &Path) -> bool {
    false
}

/// Copy every enabled artifact category into `<repo_root>/artifacts/`,
/// classifying each file Added/Modified/Unchanged by byte comparison.
/// Prompt history and memory indexes are union-merged into the repo copy
/// instead of overwritten, config files are path-tokenized, and a file this
/// machine previously synced and has since deleted is removed from the repo.
pub fn push_artifacts(
    claude_dir: &Path,
    repo_root: &Path,
    filter: &FilterConfig,
    skip_artifact_paths: &HashSet<(CategoryId, PathBuf)>,
) -> Result<ArtifactReport> {
    let mut report = ArtifactReport::default();
    let tokens = PathTokens::for_claude_dir(claude_dir);
    let tracked_before = tracked::load(claude_dir, repo_root);
    // The deltas this push owns. Both records are saved as deltas merged
    // into a fresh load, never as a start-of-push map: a same-repo pull
    // can record entries while this push runs, and a whole-entry save
    // would erase them (the push-side twin of the apply's delta merge).
    let mut deltas = RecordDeltas::default();
    let mut tracked_now = TrackedPaths::new();

    for desc in active_categories(filter) {
        let mut counts = CategoryCounts::new(desc.id);

        let mut held_back = Vec::new();
        let files = collect(
            desc,
            claude_dir,
            filter,
            &mut counts.skipped,
            &mut held_back,
        )?;
        let category_root = category_repo_root(desc, repo_root, filter);
        // The tracked record is the durable "this machine has seen this
        // file" set. The push consults it to refuse resurrecting a file
        // the remote lost between syncs (adversarial review 2026-10-09,
        // 5ff1d62 hole): a fresh-looking "Added" on the push report
        // would otherwise silently rewrite the remote's deletion. The
        // local copy is preserved on disk. Re-introducing the file
        // requires a `pull` after the local `rm` — the pull sees local
        // and repo both gone and clears the tracked entry, and a
        // subsequent push on a freshly created local file then lands
        // as a fresh `Added`. The push itself does not clear tracked
        // entries; the only way out is the pull. Read once, at the top of
        // the push (`tracked_before`): the push only saves its record at
        // the end, so a per-category re-read would see the same set.
        let mut pushed: TrackedPaths = TrackedPaths::new();
        // Files kept back for their size are still here: count them as
        // present so the repo copy (if any) is neither deleted nor forgotten.
        for rel in held_back {
            if let Some(rel) = repo_relative(repo_root, &category_root.join(rel)) {
                pushed.insert(rel);
            }
        }

        for file in files {
            // Per-file skip: a `kept_local` (or `kept_local_delete`)
            // decision made by the pull must not be reversed by the
            // next push. Without this, a clean-keep file (local
            // matches the base) would still differ from the repo
            // (the repo has the newer version) and a RawOverwrite
            // write would overwrite the repo's newer bytes with the
            // local base bytes — the exact data loss the protection
            // exists to prevent. A Union* file is also skipped, but
            // the union merge is grow-only and the local lines
            // survive even if the merge runs.
            // The is_empty fast path matters: a bare `push` passes an
            // empty set, and cloning every file's rel just to probe an
            // empty HashSet is pure waste on large trees.
            if !skip_artifact_paths.is_empty()
                && skip_artifact_paths.contains(&(desc.id, file.rel.clone()))
            {
                counts.unchanged += 1;
                continue;
            }
            let dest = category_root.join(&file.rel);
            // The LOCAL bytes of a raw-overwrite file, hashed while they are
            // still at hand; consumed after the match.
            let mut base_hash: Option<String> = None;

            match desc.merge {
                MergeStrategy::UnionJsonl => {
                    // Never fatal, like the other arms: an unreadable file
                    // is held back and warned, not read as empty — reading
                    // a broken local file as empty would promise a merge
                    // that silently forgets its lines.
                    let local_text = match fs::read_to_string(&file.abs) {
                        Ok(text) => text,
                        Err(e) => {
                            hold_back(&mut counts, &file.abs, "unreadable", &e);
                            continue;
                        }
                    };
                    let existed = dest.is_file();
                    let repo_text = if existed {
                        match fs::read_to_string(&dest) {
                            Ok(text) => text,
                            Err(e) => {
                                hold_back(&mut counts, &file.abs, "repo copy unreadable", &e);
                                continue;
                            }
                        }
                    } else {
                        String::new()
                    };
                    let (merged, new_lines) = merge_history_lines(&repo_text, &local_text);
                    if !existed {
                        if let Err(e) = write_atomic(&dest, merged.as_bytes(), &file.abs) {
                            hold_back(&mut counts, &file.abs, "repo write failed", &e);
                            continue;
                        }
                        // Counted only once the write landed: a held-back
                        // write must not report its lines as merged.
                        counts.merged_entries += new_lines;
                        counts.added += 1;
                        report.record(desc.id, ArtifactChangeKind::Added, &file.rel);
                    } else if merged != repo_text {
                        if let Err(e) = write_atomic(&dest, merged.as_bytes(), &file.abs) {
                            hold_back(&mut counts, &file.abs, "repo write failed", &e);
                            continue;
                        }
                        // Same landed-write rule as the creation branch
                        // (and the memory-index twin).
                        counts.merged_entries += new_lines;
                        counts.modified += 1;
                        report.record(desc.id, ArtifactChangeKind::Modified, &file.rel);
                    } else {
                        counts.unchanged += 1;
                    }
                }
                MergeStrategy::UnionMemoryIndex | MergeStrategy::RawOverwrite => {
                    // Never fatal, matching the pull side: an unreadable
                    // local file is held back and warned, so a bad file
                    // cannot wedge every later sync behind one error.
                    let read_bytes = match fs::read(&file.abs) {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            hold_back(&mut counts, &file.abs, "unreadable", &e);
                            continue;
                        }
                    };
                    let existed = dest.is_file();
                    let unions_index =
                        desc.merge == MergeStrategy::UnionMemoryIndex && is_memory_index(&file.rel);
                    // Hashed before `read_bytes` moves into `src_bytes`: the
                    // base record stores the LOCAL bytes as pushed, per the
                    // one shared rule (see `records_base`).
                    base_hash =
                        records_base(desc.merge, &file.rel).then(|| bases::hash_bytes(&read_bytes));
                    let mut src_bytes = if desc.tokenize_paths {
                        tokens.to_repo(&read_bytes)
                    } else {
                        read_bytes
                    };
                    // Repo-side IO is never fatal either: one bad repo
                    // file must not wedge every later push.
                    // Merged lines count only once a write lands (see the
                    // jsonl arm): `pending_entries` rides to the write.
                    let mut pending_entries = 0usize;
                    if existed && unions_index {
                        match fs::read(&dest) {
                            Ok(dest_bytes) => {
                                let (merged, new_entries) =
                                    merge_memory_index(&dest_bytes, &src_bytes);
                                pending_entries = new_entries;
                                src_bytes = merged;
                            }
                            Err(e) => {
                                hold_back(&mut counts, &file.abs, "repo copy unreadable", &e);
                                continue;
                            }
                        }
                    }

                    if !existed {
                        // If this machine has a `tracked` entry for the
                        // file, the remote lost it since the last sync
                        // (or since this machine last fetched it). The
                        // local copy is preserved on disk; refusing to
                        // re-publish it is the safe default — the
                        // user's intent on the other side was a real
                        // deletion, and resurrecting it silently
                        // rewrites the remote history. The
                        // higher-level `sync` command exposes an
                        // explicit way to override; here we just
                        // hold back, count, and warn.
                        let repo_rel = repo_relative(repo_root, &category_root.join(&file.rel));
                        if repo_rel
                            .as_ref()
                            .map(|r| tracked_before.contains(r))
                            .unwrap_or(false)
                        {
                            log::warn!(
                                "Not re-publishing {}: the remote lost this file since the last sync; the local copy is preserved",
                                file.rel.display()
                            );
                            counts.held_back_remote_lost += 1;
                            // Record the file identity (repo-relative) so the
                            // push summary can list which files were held back
                            // and `push --resurrect <path>` can target them.
                            // The escape hatch lives in
                            // `engine::prepare_resurrection`.
                            report.record(
                                desc.id,
                                ArtifactChangeKind::HeldBackRemoteLost,
                                &file.rel,
                            );
                            continue;
                        }
                        if let Err(e) = write_atomic(&dest, &src_bytes, &file.abs) {
                            hold_back(&mut counts, &file.abs, "repo write failed", &e);
                            continue;
                        }
                        counts.merged_entries += pending_entries;
                        counts.added += 1;
                        report.record(desc.id, ArtifactChangeKind::Added, &file.rel);
                    } else {
                        let dest_bytes = match fs::read(&dest) {
                            Ok(bytes) => bytes,
                            Err(e) => {
                                hold_back(&mut counts, &file.abs, "repo copy unreadable", &e);
                                continue;
                            }
                        };
                        if dest_bytes != src_bytes {
                            if let Err(e) = write_atomic(&dest, &src_bytes, &file.abs) {
                                hold_back(&mut counts, &file.abs, "repo write failed", &e);
                                continue;
                            }
                            counts.merged_entries += pending_entries;
                            counts.modified += 1;
                            report.record(desc.id, ArtifactChangeKind::Modified, &file.rel);
                        } else {
                            let realigned = align_executable_bit(&dest, &file.abs);
                            if realigned {
                                counts.modified += 1;
                                report.record(desc.id, ArtifactChangeKind::Modified, &file.rel);
                            } else {
                                counts.unchanged += 1;
                            }
                        }
                    }
                }
            }

            if let Some(rel) = repo_relative(repo_root, &dest) {
                if let Some(hash) = &base_hash {
                    deltas.bases_inserts.insert(rel.clone(), hash.clone());
                }
                pushed.insert(rel);
            }
        }

        if desc.mirror_deletes {
            if source_is_present(desc, claude_dir) {
                // The marker only keeps an emptied directory alive; failing
                // to write it must not abort a push that already wrote
                // files — warn and let the removals run.
                if let Err(e) = mark_category_synced(&category_root) {
                    log::warn!("Failed to mark {} synced: {e}", category_root.display());
                }
                let (removed, retained) = remove_from_repo(
                    desc,
                    claude_dir,
                    filter,
                    &tracked_before,
                    &pushed,
                    &category_root,
                    repo_root,
                    &mut deltas,
                    skip_artifact_paths,
                );
                counts.deleted += removed.len();
                for path in &removed {
                    report.record(desc.id, ArtifactChangeKind::Deleted, path);
                }
                tracked_now.extend(pushed);
                tracked_now.extend(retained);
            } else {
                // A category this machine does not have says nothing about
                // what the others hold: leave the repo copy and the record.
                log::info!(
                    "Category {} is not present under {}; its repo copy is left untouched",
                    desc.name,
                    claude_dir.display()
                );
                tracked_now.extend(tracked_under(&tracked_before, &category_root, repo_root));
            }
        } else {
            // mirror_deletes:false categories still need the pushed paths
            // recorded in the `tracked` set — the push consults it to
            // refuse resurrecting a file the remote lost since this
            // machine last fetched it (adversarial review 2026-10-09,
            // 5ff1d62 hole). The gone-scan skips these categories
            // anyway, so recording here cannot trigger a future delete.
            tracked_now.extend(pushed);
        }

        report.counts.push(counts);
    }

    // ONE locked delta merge (see `tracked::save_delta`), unconditional:
    // a same-repo pull that rewrites the record while this push runs
    // keeps its entries, and so do the entries of categories this push
    // did not scan (disabled ones) — their files still exist, and
    // forgetting them would silently stop their deletion mirroring.
    // Unconditional also covers mirror_deletes:false categories, whose
    // pushed paths need to land in the tracked set so the 5ff1d62
    // guard (refuse to resurrect a remote-lost file) is effective
    // even when the filter enables only those categories. A
    // record-save failure must not discard the report and strand a
    // half-applied push.
    if let Err(e) = tracked::save_delta(claude_dir, repo_root, tracked_now, deltas.tracked_removals)
    {
        log::warn!("Tracked record NOT saved (files are already pushed): {e}");
    }
    // Gated on what this push OWNS (its computed inserts and removals),
    // not on `bases_changed`'s start-of-push movement comparison: a
    // concurrent writer that pruned an entry between the lock-free load
    // and this save would make the movement test read false and skip the
    // save entirely — the pushed file silently losing its base entry
    // (its next local edit would read as never-synced). The save itself
    // is a locked delta merge on a FRESH load, and `RepoRecord::write`
    // skips identical content, so an ownership-gated no-op save is
    // churn-free.
    if !deltas.bases_inserts.is_empty() || !deltas.bases_removals.is_empty() {
        if let Err(e) = bases::save_delta(
            claude_dir,
            repo_root,
            deltas.bases_inserts,
            deltas.bases_removals,
        ) {
            log::warn!("Base record NOT saved (files are already pushed): {e}");
        }
    }
    Ok(report)
}

/// The push's one hold-back tail: name the file, say why, count it — and
/// never abort the push over it. Every arm calls this so the wording and
/// the counting cannot drift apart.
fn hold_back(counts: &mut CategoryCounts, abs: &Path, why: &str, e: &dyn std::fmt::Display) {
    log::warn!("Holding back {} ({}): {e}", abs.display(), why);
    counts.skipped += 1;
}

/// The record deltas one push owns: entries its own writes add or remove,
/// merged into a fresh load at save time (see `push_artifacts`).
#[derive(Default)]
struct RecordDeltas {
    bases_inserts: BaseHashes,
    bases_removals: Vec<String>,
    tracked_removals: Vec<String>,
}

/// Marker file that keeps a category's repo directory present once its last
/// real file is deleted.
///
/// Git does not track directories, so an emptied category would disappear and
/// be read as "this repo has no such category", which pull must not delete
/// for. The marker distinguishes an empty category from an absent one.
pub const CATEGORY_MARKER: &str = ".synced";

/// Keep the category's repo directory alive across an emptying push.
fn mark_category_synced(category_root: &Path) -> Result<()> {
    let marker = category_root.join(CATEGORY_MARKER);
    if marker.is_file() {
        return Ok(());
    }
    fs::create_dir_all(category_root)?;
    fs::write(&marker, b"").with_context(|| format!("Failed to write {}", marker.display()))?;
    Ok(())
}

/// A repo path as a `/`-separated string relative to the repository root.
fn repo_relative(repo_root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(repo_root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// Whether this machine has the source a category copies from. A `Files`
/// category is always "present": its individual files are optional.
fn source_is_present(desc: &CategoryDescriptor, claude_dir: &Path) -> bool {
    match desc.source {
        SourceSpec::Files(_) => true,
        SourceSpec::Dir(dir) => claude_dir.join(dir).is_dir(),
    }
}

/// The tracked paths that belong to one category's repo directory.
fn tracked_under(tracked: &TrackedPaths, category_root: &Path, repo_root: &Path) -> Vec<String> {
    let Some(prefix) = repo_relative(repo_root, category_root) else {
        return Vec::new();
    };
    let prefix = format!("{prefix}/");
    tracked
        .iter()
        .filter(|path| path.starts_with(&prefix))
        .cloned()
        .collect()
}

/// Delete the repo copies of files this machine synced before and no longer
/// has, pruning their base entries so the record stops describing files that
/// no longer exist on either side.
///
/// "No longer has" is verified against the LOCAL side, not inferred from the
/// push: a file the push held back (unreadable, over the size limit, failed
/// write) never entered `pushed`, and treating it as a deletion would delete
/// the repo copy — and every other machine's file with it. Such files are
/// returned as `retained` so the caller keeps them tracked and the next push
/// retries them. Nothing here is fatal: a removal that cannot happen is left
/// for the next push, like a held-back file.
#[allow(clippy::too_many_arguments)]
fn remove_from_repo(
    desc: &CategoryDescriptor,
    claude_dir: &Path,
    filter: &FilterConfig,
    tracked_before: &TrackedPaths,
    pushed: &TrackedPaths,
    category_root: &Path,
    repo_root: &Path,
    deltas: &mut RecordDeltas,
    skip_artifact_paths: &HashSet<(CategoryId, PathBuf)>,
) -> (Vec<PathBuf>, TrackedPaths) {
    let mut removed = Vec::new();
    let mut retained = TrackedPaths::new();
    let category_prefix = repo_relative(repo_root, category_root).unwrap_or_default();
    for gone in tracked_under(tracked_before, category_root, repo_root) {
        if pushed.contains(&gone) {
            continue;
        }
        let category_relative = Path::new(&gone)
            .strip_prefix(&category_prefix)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        // The same refusal the pull gone-scan applies: a hand-edited or
        // corrupted record entry must never point a repo deletion outside
        // its category (".." components resolve straight out of the
        // repository otherwise).
        if is_unsafe_rel_path(&category_relative) || is_denied(&category_relative) {
            // The entry is FORGOTTEN, not retained: every later push would
            // refuse it again, and no command could ever clear the loop
            // short of hand-editing the tracked record. The repo copy
            // stays (the refusal stands), the file simply leaves sync's
            // books — loudly, once.
            log::warn!(
                "Refusing denied/unsafe tracked path {gone} — forgetting the tracked \
                 entry (the repo copy is kept; the file is no longer synced)"
            );
            // The bases entry must go with it: the pull-side gone-scan
            // (the only other pruner) iterates tracked entries, so a
            // bases key left behind here could never be pruned by any
            // command — and a stale hash for a path this tool refuses
            // to touch describes nothing.
            deltas.bases_removals.push(gone.clone());
            deltas.tracked_removals.push(gone);
            continue;
        }
        // The local copy still existing means this is NOT a deletion: the
        // push held the file back. Keep the repo copy, the base entry, and
        // the tracked entry. A destination that cannot be resolved is
        // unverifiable, and an unverifiable file is never delete material.
        let local_deleted = match local_destination(desc, claude_dir, &category_relative, filter) {
            Ok(local) => !local.is_file(),
            Err(_) => {
                log::warn!("Keeping repo copy of {gone} (no local destination)");
                false
            }
        };
        // A path the pull just held or left unfinished is not this
        // machine's deletion to publish: the absence may be a deletion
        // made while the pull waited, set against a change the other
        // machine made.
        if !local_deleted || skip_artifact_paths.contains(&(desc.id, category_relative.clone())) {
            retained.insert(gone);
            continue;
        }
        let path = repo_root.join(&gone);
        if path.is_file() {
            if let Err(e) = fs::remove_file(&path) {
                log::warn!("Failed to remove {}: {e}", path.display());
                retained.insert(gone);
                continue;
            }
            removed.push(category_relative);
        }
        // Neither side has this file anymore — the entries must go whether
        // or not the repo copy was still there to remove (it may already
        // have been deleted outside this tool).
        deltas.bases_removals.push(gone.clone());
        deltas.tracked_removals.push(gone);
    }
    (removed, retained)
}

/// One planned local write during a pull.
#[derive(Debug, Clone)]
pub struct PlannedWrite {
    pub category: CategoryId,
    /// Absolute destination under `~/.claude`.
    pub local_path: PathBuf,
    /// Absolute source inside the sync repository.
    pub repo_path: PathBuf,
    /// Path relative to the category's root, as reported to the user.
    pub category_path: PathBuf,
}

/// Read-only classification of an artifact pull, computed BEFORE any write so
/// the caller can snapshot the exact set of files that will change.
#[derive(Debug, Default)]
pub struct PullPlan {
    /// Local file exists and repo bytes differ: remote wins after snapshot.
    pub overwrites: Vec<PlannedWrite>,
    /// Local files edited since the last sync: kept as they are, because
    /// overwriting them would destroy edits only this machine holds. A
    /// `push` is what publishes them.
    pub kept_local: Vec<PlannedWrite>,
    /// Local files edited here since the last sync while the repository still
    /// holds that sync's version: nothing to take. Left as they are and, unlike
    /// `kept_local`, not held back — the push that follows publishes them.
    pub local_only: Vec<PlannedWrite>,
    /// Files this machine synced and has since deleted while the repository
    /// still holds that sync's version: left deleted, and the push removes the
    /// repository copy.
    pub deleted_here: Vec<PlannedWrite>,
    /// Files both sides changed since the last sync whose two versions differ
    /// only in date-times (Claude Code rewrites timestamps in its plugin and
    /// skill manifests on every machine): settled on apply by keeping the later
    /// dates, which the push then publishes.
    pub date_settles: Vec<PlannedWrite>,
    /// No local file yet: created, and recorded for deletion on undo.
    pub creates: Vec<PlannedWrite>,
    /// Union-merge targets whose local file would gain lines.
    pub unions: Vec<PlannedWrite>,
    /// Local files whose content already matches but whose executable bit does
    /// not: a `chmod +x` elsewhere, with nothing to rewrite.
    pub mode_fixes: Vec<PlannedWrite>,
    /// Local files this machine synced before that the repo no longer has.
    pub deletes: Vec<PlannedDelete>,
    /// Tracked files the repo no longer has that were edited locally since
    /// the last sync: kept instead of deleted, for the same reason as
    /// `kept_local` — a `push` is what publishes the decision to drop them.
    pub kept_local_deletes: Vec<PlannedDelete>,
    pub unchanged: usize,
    /// Repo files refused (denied names, unsafe paths).
    pub skipped: usize,
    /// Repo files whose project this machine has no destination for, grouped
    /// by the project directory they came from, so the caller can warn once
    /// per project instead of once per file.
    pub unmapped_projects: crate::project_map::SkippedByProject,
    /// This machine's path tokens, so applying renders repo bytes the same way
    /// planning compared them.
    pub tokens: PathTokens,
    /// Where to record what this machine holds once the plan is applied.
    pub claude_dir: PathBuf,
    pub repo_root: PathBuf,
    /// The configured external merge tool, offered when a file differs.
    pub merge_tool: String,
    /// The tracked paths as they were at PLAN time. The apply's save is a
    /// delta (this set → [`Self::tracked_after`]) merged into a fresh
    /// load, so a same-repo push that records paths while a (possibly
    /// minutes-long interactive) apply waits keeps them. A
    /// Default-constructed plan leaves it empty, which only NARROWS the
    /// delta — and its `tracks_deletions` is false, so the save gate
    /// never opens for it anyway.
    pub tracked_before: TrackedPaths,
    /// Whether the overwrite prompt starts on the merge tool.
    pub prefer_merge_tool: bool,
    /// The repo paths this machine will hold afterwards, for the next pull to
    /// tell a deletion from a file it never had.
    pub tracked_after: TrackedPaths,
    /// Files the plan would modify whose pre-pull SNAPSHOT failed
    /// (unreadable at snapshot time). The caller fills this between the
    /// snapshot and the apply; every modifying arm refuses these paths —
    /// nothing is modified without a backup, the pre-PR contract,
    /// surviving the never-fatal snapshot. Not serialized: the field is
    /// apply-time plumbing, not part of the plan's own prediction.
    pub unsnapshotted: Vec<std::path::PathBuf>,
    /// The repo-relative keys this pull's apply writes or drops in the
    /// TRACKED record: creates join it, the gone-from-repo paths leave it
    /// (or, re-armed, stay), and repo files entering the record for the
    /// first time are writes this pull owns too. Everything NOT in this
    /// list keeps its owner — above all the entries of categories the
    /// plan did not scan because they are disabled. Computed ONCE at
    /// plan time; the prediction, the snapshot declaration, and the
    /// apply's save loop read this field instead of rebuilding it.
    pub touched_tracked_keys: Vec<String>,
    /// Whether any active category mirrors deletions. When none does, the
    /// record is left untouched rather than emptied.
    pub tracks_deletions: bool,
    /// (repo-relative path, hash) pairs for files whose bytes the plan
    /// verified as matching the repository — including mode-fix targets,
    /// whose content matches too. Recorded as this machine's last-synced
    /// base on apply, so a later local edit is detectable. Hashed at plan
    /// time: the apply must record what the plan verified, not whatever
    /// the files hold by the time it runs.
    pub base_hashes: Vec<(String, String)>,
    /// Whether the apply will write the bases record, predicted at plan
    /// time against the record on disk (creation, differing entries, or
    /// forgets). Deliberately EXCLUDES interactive kept-local writes
    /// (take-remote records, confirmed-delete forgets): those are
    /// possible only under an interactive apply, which
    /// [`Self::changes_machine_state`] covers via its `interactive` term —
    /// do not consume this field alone. Feeds
    /// [`PullPlan::changes_machine_state`].
    pub rewrites_bases: bool,
    /// Whether the apply will write the tracked record — it is saved
    /// whenever deletions are tracked and either does not exist yet or
    /// would change. Feeds [`PullPlan::changes_machine_state`].
    pub rewrites_tracked: bool,
    /// Repo-relative paths gone from BOTH sides (repo copy removed
    /// elsewhere, local copy deleted by hand): their base entries are
    /// pruned on apply, since no later operation can prune them.
    pub base_prunes: Vec<String>,
    /// Whether the bases record file exists on disk at plan time. The
    /// `carries_*` / `may_create_*` predicates read this instead of
    /// re-statting per call.
    pub bases_record_exists: bool,
    /// Whether the tracked record file exists on disk at plan time.
    pub tracked_record_exists: bool,
    /// Whether the tracked record files this repo under a legacy raw
    /// spelling — probed once at plan time and reused by the apply's
    /// save gate, which would otherwise re-read and re-parse the whole
    /// record (the plan ran moments earlier).
    pub tracked_alias_present: bool,
}

/// One planned local deletion during a pull.
#[derive(Debug, Clone)]
pub struct PlannedDelete {
    pub category: CategoryId,
    /// Absolute path under `~/.claude` to remove.
    pub local_path: PathBuf,
    /// Path relative to the category's root, as reported to the user.
    pub category_path: PathBuf,
    /// Path relative to the sync repository, as filed in the base record.
    pub repo_rel: String,
}

impl PullPlan {
    /// True when applying the plan would write no artifact file by
    /// default. Kept-local entries count despite writing nothing
    /// non-interactively, because an interactive apply may take the
    /// repository copy over them. Records are excluded: recording a base
    /// is machine-state change, not an artifact write — see
    /// [`Self::changes_machine_state`].
    pub fn is_empty(&self) -> bool {
        self.overwrites.is_empty()
            && self.date_settles.is_empty()
            && self.creates.is_empty()
            && self.unions.is_empty()
            && self.mode_fixes.is_empty()
            && self.deletes.is_empty()
            && self.kept_local.is_empty()
            && self.kept_local_deletes.is_empty()
    }

    /// True when applying the plan writes anything on this machine:
    /// artifact files, or the shared records (which `rewrites_bases` /
    /// `rewrites_tracked`, computed at plan time against what is on disk,
    /// predict). This is what the snapshot gate consults — a pull whose
    /// only effect is creating a record still needs a snapshot for its
    /// undo, while a fully no-op pull must not churn one.
    ///
    /// Kept-local lists count only under `interactive`: a non-interactive
    /// apply provably never writes them, and an unresolved edit must not
    /// mint a snapshot on every daily sync.
    pub fn changes_machine_state(&self, interactive: bool) -> bool {
        // Stated directly, not as a nested double negation: some arm writes
        // a file, or an interactive apply can (the kept-local lists).
        // mode_fixes are deliberately absent: a chmod cannot be restored
        // from a bytes snapshot, so a mode-fix-only pull must not mint an
        // empty one — the base entry the fix records still gates the
        // snapshot through `rewrites_bases`.
        let file_writes = !(self.overwrites.is_empty()
            && self.date_settles.is_empty()
            && self.creates.is_empty()
            && self.unions.is_empty()
            && self.deletes.is_empty())
            || (interactive
                && (!self.kept_local.is_empty() || !self.kept_local_deletes.is_empty()));
        file_writes || self.rewrites_bases || self.rewrites_tracked
    }

    /// The repo-relative keys this pull's apply writes or prunes in the
    /// BASES record — every arm that records, forgets, or durably declines.
    /// `kept_local` included: an interactive take-remote records the repo
    /// bytes as the new base, and undo must be able to restore that key
    /// per-key instead of falling back to a whole-entry rewind. Undo
    /// restores the PRE-PULL value of exactly these keys; entries a push
    /// recorded after the pull keep their owner. A superset is safe:
    /// restoring a key the apply never moved is a no-op.
    pub fn touched_base_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .creates
            .iter()
            .chain(self.overwrites.iter())
            .chain(self.mode_fixes.iter())
            .chain(self.kept_local.iter())
            // A union-merged memory index records the merged bytes.
            .chain(self.unions.iter())
            .filter_map(|w| repo_relative(&self.repo_root, &w.repo_path))
            .collect();
        keys.extend(self.base_hashes.iter().map(|(rel, _)| rel.clone()));
        keys.extend(self.deletes.iter().map(|d| d.repo_rel.clone()));
        keys.extend(self.kept_local_deletes.iter().map(|d| d.repo_rel.clone()));
        keys.extend(self.base_prunes.iter().cloned());
        keys
    }

    /// Whether the snapshot must carry the bases record: it exists on disk
    /// and the apply can write it — the non-interactive writes
    /// `rewrites_bases` predicts, plus the interactive kept-local
    /// resolutions (a take-remote records the repo bytes as the new
    /// base) that `rewrites_bases` deliberately excludes; the snapshot
    /// gate's own `interactive` term covers their file-write side. The
    /// ONE rule — `paths_to_snapshot` and
    /// `Snapshot::attach_record_bookkeeping` both call these, so the
    /// carrier and the declaration can never drift apart.
    pub fn carries_bases_record(&self, interactive: bool) -> bool {
        self.bases_record_exists && self.can_write_bases(interactive)
    }

    /// Whether the snapshot must carry the tracked record: it exists and
    /// the apply will actually rewrite it (`rewrites_tracked` — tracked
    /// writes are fully predictable without the interactive term, unlike
    /// bases: no apply arm touches the tracked record only because a
    /// prompt was answered). Carrying it otherwise would declare record
    /// surgery — and snapshot the shared file — for a record the apply
    /// provably never writes.
    pub fn carries_tracked_record(&self) -> bool {
        self.tracked_record_exists && self.rewrites_tracked
    }

    /// Whether the apply may create the bases record (it does not exist
    /// yet and this plan can write it — same rule as
    /// [`Self::carries_bases_record`]).
    pub fn may_create_bases_record(&self, interactive: bool) -> bool {
        !self.bases_record_exists && self.can_write_bases(interactive)
    }

    /// Whether the apply can write the bases record under the given
    /// interactivity — see [`Self::carries_bases_record`]. Public for the
    /// pull's concurrent-writer warning, which must mirror the snapshot
    /// gate exactly instead of recomposing the existence split.
    pub fn can_write_bases(&self, interactive: bool) -> bool {
        self.rewrites_bases
            || (interactive && (!self.kept_local.is_empty() || !self.kept_local_deletes.is_empty()))
    }

    /// Whether the apply may create the tracked record: it does not exist
    /// and there is something to record — the same rule as
    /// [`Self::rewrites_tracked`], whose absent-file branch this is.
    pub fn may_create_tracked_record(&self) -> bool {
        !self.tracked_record_exists && self.rewrites_tracked
    }

    /// Existing local files the caller must snapshot before applying
    /// (overwritten raw files, union-merged files, deletions, and — only
    /// under `interactive` — kept-local files: an interactive apply may
    /// take the repository copy over the local edit, and a snapshot is
    /// what makes `undo pull` able to bring the edit back; the
    /// non-interactive apply provably never writes them).
    ///
    /// The shared records are included as carriers only, per the
    /// `carries_*_record` rules above: `undo pull` reads this
    /// repository's pre-pull entry from the snapshot bytes and re-applies
    /// it surgically — the files hold every sync repository's state and
    /// must not be restored wholesale.
    pub fn paths_to_snapshot(&self, interactive: bool) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self
            .overwrites
            .iter()
            .chain(self.date_settles.iter())
            .chain(self.unions.iter())
            .map(|w| w.local_path.clone())
            .chain(self.deletes.iter().map(|d| d.local_path.clone()))
            .collect();
        if interactive {
            paths.extend(self.kept_local.iter().map(|w| w.local_path.clone()));
            // An interactive confirm can also DELETE a kept-local file —
            // same undo requirement as a take-remote.
            paths.extend(self.kept_local_deletes.iter().map(|d| d.local_path.clone()));
        }
        if self.carries_tracked_record() {
            paths.push(tracked::record_path(&self.claude_dir));
        }
        if self.carries_bases_record(interactive) {
            paths.push(bases::record_path(&self.claude_dir));
        }
        paths
    }

    /// Local paths this pull will create; recording them as a snapshot's
    /// `deleted_files` makes undo remove them again.
    ///
    /// The base record is deliberately absent: it is one file shared by
    /// every sync repository, and `undo pull` restores this repository's
    /// entry from the snapshot instead of deleting the file.
    pub fn created_paths(&self) -> Vec<String> {
        self.creates
            .iter()
            .map(|w| w.local_path.to_string_lossy().to_string())
            .collect()
    }
}

/// Enumerate one category's files as stored in the sync repository, returning
/// (absolute repo path, path relative to the category subdir). Denied and
/// unsafe paths are refused here, so nothing below ever sees them.
fn collect_repo_files(
    desc: &CategoryDescriptor,
    repo_root: &Path,
    filter: &FilterConfig,
    skipped: &mut usize,
    held_back: &mut Vec<PathBuf>,
) -> Vec<(PathBuf, PathBuf)> {
    let category_root = category_repo_root(desc, repo_root, filter);
    if !category_root.is_dir() {
        return Vec::new();
    }

    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(&category_root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path();
        let rel = abs
            .strip_prefix(&category_root)
            .unwrap_or(abs)
            .to_path_buf();
        if extension_excluded(desc, &rel) {
            continue;
        }
        // The marker exists to keep the directory, and belongs to no machine.
        if rel == Path::new(CATEGORY_MARKER) {
            continue;
        }
        if is_unsafe_rel_path(&rel) || is_denied(&rel) {
            log::warn!(
                "Refusing denied/unsafe artifact from sync repo: {}",
                abs.display()
            );
            *skipped += 1;
            continue;
        }
        // The size limit applies to the repo side too — the mirror of the
        // push-side guard: another machine with a higher limit may have
        // pushed a file this one must not write. Held back, NOT gone:
        // deletion mirroring must never read a not-pulled file as
        // deleted-here (the pull-side twin of push's held_back).
        // The one shared fail-closed triage (metadata form — the entry
        // already holds it).
        if metadata_outside_limit(entry.metadata(), filter.max_file_size_bytes) {
            log::warn!("Skipping {} (exceeds max_file_size_bytes)", abs.display());
            *skipped += 1;
            held_back.push(abs.to_path_buf());
            continue;
        }
        files.push((abs.to_path_buf(), rel));
    }
    files
}

/// Why a repo file has no local destination. The caller reports the two cases
/// differently: an unmapped project is one warning for all of its files, an
/// unrestorable path is a refusal worth its own line.
enum SkipReason {
    /// This machine has no directory for the named sync-repo project.
    UnmappedProject(String),
    /// Nothing in the category can receive this path.
    NotRestorable,
}

/// Map a category-relative repo path back to its absolute local destination
/// under `~/.claude`, or the reason it has none.
fn local_destination(
    desc: &CategoryDescriptor,
    claude_dir: &Path,
    rel: &Path,
    filter: &FilterConfig,
) -> Result<PathBuf, SkipReason> {
    match desc.source {
        // File lists are stored flat in the repo; restore to the listed
        // location whose file name matches. An unlisted name has NO valid
        // destination — the allowlist must hold on pull as well as push, or a
        // poisoned repo could plant arbitrary files at the top of ~/.claude.
        SourceSpec::Files(list) => list
            .iter()
            .find(|entry| Path::new(entry).file_name() == rel.file_name())
            .map(|entry| claude_dir.join(entry))
            .ok_or(SkipReason::NotRestorable),
        SourceSpec::Dir(dir) => {
            if desc.dest == DestRoot::SessionTree {
                // Repo path is <project-dir>/<rest>; resolve the leading
                // component the way session pull does.
                let split = crate::project_map::split_project_path(rel);
                let (project, inside_project) = split.ok_or(SkipReason::NotRestorable)?;
                let projects_dir = claude_dir.join(dir);
                let local_project =
                    crate::project_map::local_project_dir(filter, &projects_dir, project)
                        .ok_or_else(|| SkipReason::UnmappedProject(project.to_string()))?;
                return Ok(local_project.join(inside_project));
            }
            Ok(claude_dir.join(dir).join(rel))
        }
    }
}

/// The bytes a repo file becomes on this machine before any merge: rendered
/// back from tokens for config categories, verbatim otherwise.
fn machine_bytes(
    desc: &CategoryDescriptor,
    tokens: &PathTokens,
    repo_path: &Path,
) -> Result<Vec<u8>> {
    let bytes = fs::read(repo_path)
        .with_context(|| format!("Failed to read artifact {}", repo_path.display()))?;
    if desc.tokenize_paths {
        return Ok(tokens.to_machine(&bytes));
    }
    Ok(bytes)
}

/// The registry row for one category.
pub(crate) fn descriptor(id: CategoryId) -> &'static CategoryDescriptor {
    REGISTRY
        .iter()
        .find(|d| d.id == id)
        .expect("every CategoryId has a registry row")
}

/// Classify what a pull would write, without writing. Remote (repo) bytes win
/// for raw categories; union targets are compared against local ∪ remote; a
/// file this machine synced before and the repo no longer has is a deletion.
pub fn plan_pull(claude_dir: &Path, repo_root: &Path, filter: &FilterConfig) -> Result<PullPlan> {
    let mut plan = PullPlan {
        tokens: PathTokens::for_claude_dir(claude_dir),
        claude_dir: claude_dir.to_path_buf(),
        repo_root: repo_root.to_path_buf(),
        merge_tool: filter.merge_tool.clone(),
        prefer_merge_tool: filter.prefer_merge_tool,
        tracks_deletions: active_categories(filter).any(|desc| desc.mirror_deletes),
        ..Default::default()
    };
    // A PRESENT but broken record is not the fresh-machine state: say
    // so, loudly — a corrupt bases record silently turns the protection
    // off (every differing file reads as a clean overwrite), a corrupt
    // tracked record silently stops deletion mirroring.
    if !super::repo_record::intact::<BaseHashes>(&bases::record_path(claude_dir)) {
        log::warn!(
            "the base record is unreadable or corrupt — local-edit protection is OFF this round; \
             fix or remove {}",
            bases::record_path(claude_dir).display()
        );
    }
    if !super::repo_record::intact::<TrackedPaths>(&tracked::record_path(claude_dir)) {
        log::warn!(
            "the tracked record is unreadable or corrupt — deletion mirroring is OFF this round; \
             fix or remove {}",
            tracked::record_path(claude_dir).display()
        );
    }
    // The records save under an OS file lock; on a filesystem that does
    // not support it (old NFS, some SMB/fuse mounts) every save fails and
    // both protections are permanently off — say so at plan time, where
    // the corrupt-record warnings live, instead of leaving it to per-save
    // warns scattered through the apply. An ABSENT Claude directory is
    // the fresh-machine state (nothing to protect yet; the saves create
    // everything on first write) — probing it would make a read-only
    // `status` create directories, so the probe runs only when the
    // directory exists.
    if claude_dir.is_dir() {
        if let Err(e) = super::repo_record::record_lock_probe(claude_dir) {
            log::warn!(
                "record locking is unavailable — local-edit protection and deletion mirroring \
                 cannot be recorded this round ({e})"
            );
        }
    }
    let tracked_before = tracked::load(claude_dir, repo_root);
    plan.tracked_before = tracked_before.clone();
    let base_hashes: BaseHashes = bases::load(claude_dir, repo_root);
    // A machine that has synced this repository before but holds no base
    // for a differing file is upgrading from a version that kept no bases
    // (or lost its record): the difference may be an edit made here, and
    // with no base nothing tells it apart from a stale copy. Hold it rather
    // than remote-win over it. Only a machine new to this repository —
    // nothing tracked, nothing recorded — takes the repository's version.
    let synced_this_repo_before = !tracked_before.is_empty() || !base_hashes.is_empty();

    for desc in active_categories(filter) {
        let category_root = category_repo_root(desc, repo_root, filter);
        let mut present: TrackedPaths = TrackedPaths::new();

        let mut repo_held_back = Vec::new();
        let collected = collect_repo_files(
            desc,
            repo_root,
            filter,
            &mut plan.skipped,
            &mut repo_held_back,
        );
        // Size-held-back repo files still exist in the repository: they
        // count as present so the deletion-mirror arm never reads them
        // as gone and deletes a clean local copy. But they are NOT
        // tracked: this machine never received the file, and a tracked
        // path with no local copy is exactly what the next push's
        // remove_from_repo reads as a local deletion — deleting the repo
        // copy for every machine.
        let mut repo_held_back_rels: TrackedPaths = TrackedPaths::new();
        for abs in &repo_held_back {
            if let Some(rel) = repo_relative(repo_root, abs) {
                repo_held_back_rels.insert(rel.clone());
                present.insert(rel);
            }
        }
        for (repo_path, rel) in collected {
            let local_path = match local_destination(desc, claude_dir, &rel, filter) {
                Ok(local_path) => local_path,
                Err(SkipReason::UnmappedProject(project)) => {
                    plan.skipped += 1;
                    plan.unmapped_projects
                        .entry(project)
                        .or_default()
                        .push(repo_path);
                    continue;
                }
                Err(SkipReason::NotRestorable) => {
                    plan.skipped += 1;
                    log::warn!(
                        "Skipping {} (not a file the {} category restores)",
                        repo_path.display(),
                        desc.name
                    );
                    continue;
                }
            };
            if desc.mirror_deletes {
                if let Some(tracked_path) = repo_relative(repo_root, &repo_path) {
                    present.insert(tracked_path);
                }
            }
            let write = PlannedWrite {
                category: desc.id,
                local_path: local_path.clone(),
                repo_path: repo_path.clone(),
                category_path: rel.clone(),
            };

            match desc.merge {
                MergeStrategy::UnionJsonl => {
                    if !local_path.is_file() {
                        plan.creates.push(write);
                        continue;
                    }
                    // An unreadable file is skipped this round, never fatal:
                    // one bad file must not block every other category and
                    // every session merge (status included). Reading it as
                    // empty would instead promise a union write the apply
                    // would have to refuse.
                    let local_text = match fs::read_to_string(&local_path) {
                        Ok(text) => text,
                        Err(e) => {
                            log::warn!("Skipping {} (unreadable): {e}", local_path.display());
                            plan.skipped += 1;
                            continue;
                        }
                    };
                    // Never fatal here either (see the local read above):
                    // reading a broken repo copy as empty would promise a
                    // union write of "no remote lines" — silently dropping
                    // the repository's prompts while status calls it
                    // in sync. Skip and count instead.
                    let repo_text = match fs::read_to_string(&repo_path) {
                        Ok(text) => text,
                        Err(e) => {
                            log::warn!(
                                "Skipping {} (repo copy unreadable): {e}",
                                repo_path.display()
                            );
                            plan.skipped += 1;
                            continue;
                        }
                    };
                    let (merged, _) = merge_history_lines(&local_text, &repo_text);
                    if merged != local_text {
                        plan.unions.push(write);
                    } else {
                        plan.unchanged += 1;
                    }
                }
                MergeStrategy::UnionMemoryIndex if is_memory_index(&rel) => {
                    if !local_path.is_file() {
                        if deleted_here_since_sync(
                            desc,
                            claude_dir,
                            &plan.tokens,
                            &base_hashes,
                            repo_root,
                            &repo_path,
                        ) {
                            plan.deleted_here.push(write);
                        } else {
                            plan.creates.push(write);
                        }
                        continue;
                    }
                    // Unreadable is never fatal (one bad file must not
                    // block every category): skipped this round.
                    let Ok(local_bytes) = fs::read(&local_path) else {
                        log::warn!("Skipping {} (unreadable)", local_path.display());
                        plan.skipped += 1;
                        continue;
                    };
                    // The union-merge contract is "incoming wins per
                    // entry" (memory_index.rs): a user-curated bullet
                    // at a target key the incoming (repo) also has is
                    // treated as a match and overwritten. That is
                    // silent clobber for issue #103. The same F3 + base
                    // dirty guards as the overwrites arm apply: if we
                    // have no recorded base but the file is in
                    // `tracked_before`, or the recorded base does not
                    // match the local bytes, the user edited this
                    // since the last sync and the next pull must not
                    // union-merge over it.
                    // Never fatal (see the unions arm below): one bad
                    // repo file must not block every category and session.
                    let Ok(repo_bytes) = machine_bytes(desc, &plan.tokens, &repo_path) else {
                        log::warn!("Skipping {} (repo copy unreadable)", repo_path.display());
                        plan.skipped += 1;
                        continue;
                    };
                    let repo_rel = repo_relative(repo_root, &repo_path);
                    // Both sides already hold the same index: in sync,
                    // whatever the record says. Recorded as the base, so a
                    // machine with no base (an upgrade) or a held one
                    // settles here instead of being held on every sync.
                    if local_bytes == repo_bytes {
                        if let Some(rel) = repo_rel {
                            plan.base_hashes
                                .push((rel, bases::hash_bytes(&local_bytes)));
                        }
                        plan.unchanged += 1;
                        continue;
                    }
                    let recorded = repo_rel.as_ref().and_then(|r| base_hashes.get(r));
                    let f3 = repo_rel
                        .as_ref()
                        .map(|r| f3_class_potentially_dirty(&base_hashes, &plan.tracked_before, r))
                        .unwrap_or(false)
                        || (recorded.is_none() && synced_this_repo_before);
                    if f3 || bases::is_dirty(recorded, &local_bytes) {
                        // Edited here. If the repository still holds the
                        // last synced version, only this machine moved on:
                        // the push publishes the edit. Otherwise both did,
                        // and the edit is kept and held back.
                        if repo_unchanged_since_sync(recorded, &repo_bytes) {
                            plan.local_only.push(write);
                        } else {
                            plan.kept_local.push(write);
                        }
                        continue;
                    }
                    let (merged, _) = merge_memory_index(&local_bytes, &repo_bytes);
                    if merged != local_bytes {
                        plan.unions.push(write);
                    } else {
                        plan.unchanged += 1;
                    }
                }
                MergeStrategy::UnionMemoryIndex | MergeStrategy::RawOverwrite => {
                    if !local_path.is_file() {
                        if deleted_here_since_sync(
                            desc,
                            claude_dir,
                            &plan.tokens,
                            &base_hashes,
                            repo_root,
                            &repo_path,
                        ) {
                            plan.deleted_here.push(write);
                        } else {
                            plan.creates.push(write);
                        }
                    } else {
                        // A file past the size limit is outside artifact
                        // sync: the push refuses it, so "kept local" could
                        // never be published and would deadlock the sync
                        // gate on it forever. Skipped, like the push side
                        // skips it — the local file is left as it is.
                        if outside_size_limit(&local_path, filter) {
                            log::warn!(
                                "Skipping {} (unreadable or exceeds max_file_size_bytes)",
                                local_path.display()
                            );
                            plan.skipped += 1;
                            continue;
                        }
                        // Same never-fatal rule as the union arms.
                        let Ok(local_bytes) = fs::read(&local_path) else {
                            log::warn!("Skipping {} (unreadable)", local_path.display());
                            plan.skipped += 1;
                            continue;
                        };
                        // Never fatal, like every other read: skip and
                        // count, do not abort the whole plan.
                        let repo_bytes = match machine_bytes(desc, &plan.tokens, &repo_path) {
                            Ok(bytes) => bytes,
                            Err(e) => {
                                log::warn!(
                                    "Skipping {} (repo copy unreadable): {e}",
                                    repo_path.display()
                                );
                                plan.skipped += 1;
                                continue;
                            }
                        };
                        if local_bytes != repo_bytes {
                            // Bytes differ. If they were ALSO edited here since
                            // the last sync, the repository holds nothing this
                            // machine lacks: keep the local file and let a
                            // push publish it, instead of overwriting it.
                            //
                            // Two signals drive the decision:
                            //
                            // - `recorded` (the BASE record): the local
                            //   bytes vs the bytes last synced. Empty =
                            //   we have never recorded this file's synced
                            //   state (fresh install or a concurrent
                            //   prune). The apply's `is_dirty(None, _) =
                            //   false` would let this slip through to
                            //   `plan.overwrites` and silently overwrite
                            //   a local edit.
                            //
                            // - `plan.tracked_before`: the set of files
                            //   this machine has EVER synced. A file in
                            //   this set is "we have a contract for it" —
                            //   the file existed on this machine at some
                            //   point. For an overwrites candidate, this
                            //   is the F3-class signal: tracked_before
                            //   says "we synced this before" and a
                            //   concurrent sync has pruned the base.
                            //   Fresh install / upgrade never recorded
                            //   this file in tracked_before; that case
                            //   must still remote-win (an_unknown_base_
                            //   keeps_the_remote_wins_behavior).
                            //
                            // The fix: an overwrites candidate with a
                            // missing base AND a tracked entry is
                            // treated as the F3-class synthetic-Dirty
                            // (None-base) case and routed to kept_local.
                            // The existing base-based dirty check still
                            // catches the conventional "edited since the
                            // last sync" case.
                            let rel = repo_relative(repo_root, &repo_path);
                            let recorded = rel.as_ref().and_then(|r| base_hashes.get(r));
                            // Two ways this file has a real local edit:
                            //   1. F3-class: a concurrent sync pruned the
                            //      base between the last pull and this one
                            //      (no recorded hash) but we have a
                            //      tracked entry — a possibly-edited file
                            //      that must be kept.
                            //   2. Conventional dirty: a recorded base and
                            //      the local bytes differ from it.
                            // A file outside the repo root (rel is None)
                            // can only be #2 (no tracked entry applies).
                            let f3 = rel
                                .as_ref()
                                .map(|r| {
                                    f3_class_potentially_dirty(
                                        &base_hashes,
                                        &plan.tracked_before,
                                        r,
                                    )
                                })
                                .unwrap_or(false)
                                || (recorded.is_none() && synced_this_repo_before);
                            if repo_unchanged_since_sync(recorded, &repo_bytes) {
                                // Only this machine moved on since the last
                                // sync (local differs from a repository that
                                // still holds the base): nothing to take, and
                                // the push that follows publishes the edit.
                                plan.local_only.push(write);
                            } else if f3 || bases::is_dirty(recorded, &local_bytes) {
                                // Both sides moved on. Two versions apart only
                                // in their date-times settle on the later
                                // ones; anything else is kept and held back.
                                if keep_later_timestamps(&local_bytes, &repo_bytes).is_some() {
                                    plan.date_settles.push(write);
                                } else {
                                    plan.kept_local.push(write);
                                }
                            } else {
                                plan.overwrites.push(write);
                            }
                        } else {
                            // The verified bytes are the synced state, hashed
                            // now — at plan time — so the apply records what
                            // this plan actually saw, not whatever the file
                            // holds by the time the apply runs (an edit made
                            // while the pull waits must NOT be recorded as
                            // synced).
                            if let Some(rel) = repo_relative(repo_root, &repo_path) {
                                plan.base_hashes
                                    .push((rel, bases::hash_bytes(&local_bytes)));
                            }
                            if executable_bit_differs(&local_path, &repo_path) {
                                plan.mode_fixes.push(write);
                            } else {
                                plan.unchanged += 1;
                            }
                        }
                    }
                }
            }
        }

        if !desc.mirror_deletes {
            continue;
        }

        // Mirror of the push-side guard: a category the repo does not have —
        // an older or rewound branch — is not "everything was deleted".
        if !category_root.is_dir() {
            log::info!(
                "Category {} is not present in {}; local files are left alone",
                desc.name,
                repo_root.display()
            );
            plan.tracked_after
                .extend(tracked_under(&tracked_before, &category_root, repo_root));
            continue;
        }

        // Loop-invariant, hoisted: the category prefix every tracked path
        // is stripped against (one string rebuild per category, not per
        // file).
        let category_prefix = repo_relative(repo_root, &category_root).unwrap_or_default();
        for gone in tracked_under(&tracked_before, &category_root, repo_root) {
            if present.contains(&gone) {
                continue;
            }
            let rel = Path::new(&gone)
                .strip_prefix(&category_prefix)
                .map(Path::to_path_buf)
                .unwrap_or_default();
            // The same refusal `collect_repo_files` applies: a record must
            // never point a deletion outside its category.
            if is_unsafe_rel_path(&rel) || is_denied(&rel) {
                log::warn!("Refusing denied/unsafe tracked path: {gone}");
                plan.skipped += 1;
                continue;
            }
            let Ok(local_path) = local_destination(desc, claude_dir, &rel, filter) else {
                continue;
            };
            if local_path.is_file() {
                // The same size guard as the overwrite arm (same shared
                // triage): an oversized file can never be published by a
                // push, so kept-local-delete would deadlock the sync gate
                // on it forever. The file is left as it is.
                if outside_size_limit(&local_path, filter) {
                    log::warn!(
                        "Skipping {} (unreadable or exceeds max_file_size_bytes)",
                        local_path.display()
                    );
                    plan.skipped += 1;
                    continue;
                }
                // Same refusal as an overwrite: a file edited here since the
                // last sync holds edits only this machine has, and a repo
                // deletion is not license to destroy them. The file is kept,
                // and a `push` publishes the decision to drop it.
                //
                // The shared triage (see `dirty_status`): no base is
                // predetermined remote-wins; vanished is nothing to keep
                // or delete — the next pull's gone-scan re-decides, and
                // the tracked entry rides along until then (the apply's
                // record delta only touches keys this plan declared);
                // unreadable is UNDECIDED, not a keep — a keep would
                // re-arm forever and deadlock the sync gate on a file
                // neither a push (read fails, held back) nor a prompt
                // could resolve. Skipped and re-tracked: the decision is
                // retried once the file is readable again.
                //
                // The F3-class signal is the same one the overwrites
                // arm uses: a None base + a tracked entry is a
                // possibly-edited file, even though `dirty_status`
                // cannot see the bytes (no base to compare).
                // `dirty_status` short-circuits on `recorded == None`
                // and returns Clean, so the F3 arm is keyed on Clean
                // (not Dirty / Unreadable — those can never coexist
                // with f3 == true). Without this, the deletes arm
                // would route an F3-class file to `plan.deletes` (the
                // plain "no local edit" path) and silently destroy a
                // possibly-edited local file.
                let status = dirty_status(base_hashes.get(&gone), &local_path);
                let f3 = f3_class_potentially_dirty(&base_hashes, &tracked_before, &gone);
                let planned = PlannedDelete {
                    category: desc.id,
                    local_path,
                    category_path: rel,
                    repo_rel: gone.clone(),
                };
                match status {
                    FreshRead::Clean if f3 => {
                        // None base + tracked entry. The local file
                        // could be edited — we have no base to compare
                        // against — and silently deleting it would
                        // discard the user's work. Route to
                        // kept_local_deletes and re-arm the tracked
                        // entry so the next pull replans it.
                        plan.kept_local_deletes.push(planned);
                        plan.tracked_after.insert(gone.clone());
                    }
                    FreshRead::Dirty => {
                        plan.kept_local_deletes.push(planned);
                        // RE-ARM: the file stays in tracked_after, so every
                        // later pull replans it. Dropping it here (the plain
                        // `deletes` fate) made the protection last exactly one
                        // round: the next pull would see kept_local = 0, the
                        // sync gate would open, and the full push would
                        // silently resurrect the repo-deleted file with this
                        // machine's edit. A confirmed interactive delete lags
                        // one round instead (the entry leaves the record on
                        // the next pull's gone-from-both-sides pass) — inert,
                        // since the repo copy is already gone.
                        plan.tracked_after.insert(gone.clone());
                    }
                    FreshRead::Clean => plan.deletes.push(planned),
                    FreshRead::Vanished => continue,
                    FreshRead::Unreadable => {
                        log::warn!(
                            "Skipping {} (unreadable; the deletion is retried once it is readable)",
                            planned.local_path.display()
                        );
                        plan.skipped += 1;
                        plan.tracked_after.insert(gone);
                    }
                }
            } else {
                // Gone from BOTH sides (repo copy removed outside the
                // tool, local copy deleted by hand): nothing to delete,
                // but the base entry must still go — no later operation
                // can prune it, and a stale entry describes a file that
                // exists nowhere.
                plan.base_prunes.push(gone.clone());
            }
        }
        plan.tracked_after.extend(present);
        // The held-back repo files were folded into `present` only for
        // the gone-scan guard above — they must leave before the record
        // is built.
        for rel in &repo_held_back_rels {
            plan.tracked_after.remove(rel);
        }
    }

    // Predict, against what is on disk, whether the apply will write the
    // shared records — the snapshot gate needs it, and a fully no-op pull
    // must not read as machine-state change. Approximation is one-sided:
    // an interactive decline can make the apply write less, never more.
    // The existence flags are cached on the plan: every later predicate
    // (carries_* / may_create_*) reuses them instead of re-statting.
    plan.bases_record_exists = bases::record_path(claude_dir).is_file();
    let bases_record_exists = plan.bases_record_exists;
    plan.tracked_record_exists = tracked::record_path(claude_dir).is_file();
    // Every write term runs through the ONE shared rule (`records_base`),
    // like the apply does: union files never record a base, and counting
    // them would embed the shared record in snapshots that provably
    // never write it. And like the APPLY's `record_base` (which skips a
    // value that is already recorded), a create/overwrite/mode-fix only
    // counts when the value it would record actually MOVES: a mode fix
    // over converged hashes changes no bytes, records nothing, and a
    // prediction that counted it would mint a snapshot whose undo
    // restores zero files while burying the substantive pull beneath it.
    // The exists-branch's deletes term only counts deletions with an
    // entry to remove; the absent-record branch drops it entirely (no
    // record, no entries).
    let planned_base_hashes: BaseHashes = plan.base_hashes.iter().cloned().collect();
    let records_base_for =
        |w: &PlannedWrite| records_base(descriptor(w.category).merge, &w.category_path);
    // Creates and overwrites record the REPOSITORY bytes, which their
    // classification already proves differ from the base (or that there
    // is no base entry): membership is movement. A mode fix records the
    // file's OWN bytes — the same hash the plan just verified — so it
    // moves nothing when the base already holds that value (the apply's
    // `record_base` would skip it; predicting it anyway mints a snapshot
    // whose undo restores zero files).
    // A union merge records the merged bytes, which differ from the local
    // ones the base described (the plan only lists a union that changes
    // the file): membership is movement too.
    let moves_base = plan
        .creates
        .iter()
        .chain(&plan.overwrites)
        .chain(&plan.unions)
        .any(records_base_for)
        || plan.mode_fixes.iter().any(|w| {
            records_base_for(w)
                && repo_relative(repo_root, &w.repo_path)
                    .is_some_and(|rel| planned_base_hashes.get(&rel) != base_hashes.get(&rel))
        });
    plan.rewrites_bases = if bases_record_exists {
        moves_base
            || !plan.base_prunes.is_empty()
            || plan
                .deletes
                .iter()
                .any(|d| base_hashes.contains_key(&d.repo_rel))
            || plan
                .base_hashes
                .iter()
                .any(|(rel, hash)| base_hashes.get(rel) != Some(hash))
    } else {
        moves_base
            || plan
                .base_hashes
                .iter()
                .any(|(rel, hash)| base_hashes.get(rel) != Some(hash))
    };
    // The apply saves a TOUCHED-KEY delta; the prediction uses the same
    // semantics, not a whole-entry compare: an entry the pull's scan does
    // not own (a disabled category's, say) would otherwise read as a
    // pending rewrite forever, minting a no-op snapshot and history entry
    // on every pull. Alias retirement still counts — the apply's save
    // retires it. The key set is computed once here (see the field).
    let mut touched_tracked: Vec<String> = plan
        .creates
        .iter()
        .filter_map(|w| repo_relative(repo_root, &w.repo_path))
        .collect();
    touched_tracked.extend(plan.deletes.iter().map(|d| d.repo_rel.clone()));
    touched_tracked.extend(plan.kept_local_deletes.iter().map(|d| d.repo_rel.clone()));
    touched_tracked.extend(plan.base_prunes.iter().cloned());
    for key in &plan.tracked_after {
        if !plan.tracked_before.contains(key) {
            touched_tracked.push(key.clone());
        }
    }
    plan.touched_tracked_keys = touched_tracked;
    let tracked_membership_flips = plan
        .touched_tracked_keys
        .iter()
        .any(|key| plan.tracked_before.contains(key) != plan.tracked_after.contains(key));
    plan.tracked_alias_present = tracked::has_aliased_entry(claude_dir, repo_root);
    plan.rewrites_tracked =
        plan.tracks_deletions && (tracked_membership_flips || plan.tracked_alias_present);

    Ok(plan)
}

/// Apply a pull plan: create missing files, overwrite differing ones (remote
/// wins), union-merge prompt history and memory indexes, and remove files the
/// repo no longer has. Under `interactive` in a terminal, each overwrite asks
/// for per-file confirmation — with the configured merge tool as an option —
/// and each deletion asks for confirmation; declined files count as skipped.
pub fn apply_pull(plan: &PullPlan, interactive: bool) -> Result<ArtifactReport> {
    use std::collections::HashMap;

    let mut by_category: HashMap<CategoryId, CategoryCounts> = HashMap::new();
    fn counts_for(
        map: &mut HashMap<CategoryId, CategoryCounts>,
        id: CategoryId,
    ) -> &mut CategoryCounts {
        map.entry(id)
            .or_insert_with(move || CategoryCounts::new(id))
    }

    /// Count and name one kept-local file — the shared tail of every arm
    /// that decides to keep one. `nothing_to_publish` is decided AT the
    /// keep site, where the arm knows WHY it kept: a file the arm knows
    /// still matches its base (an overwrite or take whose write failed)
    /// holds the gate but has nothing a push would publish, and the
    /// "push to publish" hint must not be given for it; a declined
    /// deletion is the opposite — the file exists only here, and the
    /// next push republishes it.
    fn report_kept(
        map: &mut HashMap<CategoryId, CategoryCounts>,
        report: &mut ArtifactReport,
        category: CategoryId,
        path: &Path,
        nothing_to_publish: bool,
    ) {
        counts_for(map, category).kept_local += 1;
        if nothing_to_publish {
            report.kept_local_clean += 1;
        }
        report.record(category, ArtifactChangeKind::KeptLocal, path);
    }

    /// A planned write the apply did not carry out: counted skipped, and
    /// reported [`ArtifactChangeKind::Unfinished`] so `sync`'s push leaves
    /// the path alone too. The caller logs why.
    fn skip_unfinished_write(
        map: &mut HashMap<CategoryId, CategoryCounts>,
        report: &mut ArtifactReport,
        write: &PlannedWrite,
    ) {
        counts_for(map, write.category).skipped += 1;
        report.record(
            write.category,
            ArtifactChangeKind::Unfinished,
            &write.category_path,
        );
    }

    /// Refuse a planned write because the snapshot could not read its
    /// destination (plan.unsnapshotted). The deletes arm still inlines its
    /// variant because it also pushes to `deletes_to_retry`.
    fn skip_unsnapshotted_write(
        map: &mut HashMap<CategoryId, CategoryCounts>,
        report: &mut ArtifactReport,
        write: &PlannedWrite,
    ) {
        log::warn!(
            "Skipping {} (no pre-pull backup could be taken; retried next pull)",
            write.local_path.display()
        );
        skip_unfinished_write(map, report, write);
    }

    let mut report = ArtifactReport::default();
    let prompt_overwrites = interactive && crate::interactive_conflict::is_interactive();

    // The last-synced byte hashes of every file this pull leaves identical on
    // both sides, so the next pull can tell a local edit from a stale file.
    // Interactive declines and kept-local files are deliberately absent.
    // Kept-local: their local bytes must keep reading as unsynced. A
    // declined overwrite records the REPO bytes as the new base (the
    // KeepLocal branch) — the local file then reads dirty
    // against the recorded repo bytes, so the next pull keeps the file
    // instead of remote-winning the user's decline away. That is the
    // intended persistence: declining is not publishing, and the
    // recorded repo bytes are the marker the next pull consults.
    let mut synced_bases = bases::load(&plan.claude_dir, &plan.repo_root);
    let mut bases_changed = false;
    // Deltas, not a whole-entry replacement: a concurrent same-repo push
    // during this (possibly minutes-long interactive) apply must not have
    // its entries erased by our save — the final write merges these into
    // whatever is on disk THEN.
    let mut bases_inserts: BaseHashes = BaseHashes::new();
    let mut bases_removals: Vec<String> = Vec::new();
    // Base entries the apply recorded as PROTECTION (the mid-pull
    // create-skip: the user's file must read dirty or the next pull
    // overwrites it). They move the record but are not pull state: the
    // undo must not "restore" them away, or it disarms exactly the
    // protection whose loss ends in the user's file being overwritten.
    let mut protection_keys: Vec<String> = Vec::new();
    fn record_base(
        map: &mut BaseHashes,
        inserts: &mut BaseHashes,
        repo_root: &Path,
        repo_path: &Path,
        bytes: &[u8],
    ) -> bool {
        match repo_relative(repo_root, repo_path) {
            Some(rel) => {
                let hash = bases::hash_bytes(bytes);
                // A base that already holds this value moved nothing: no
                // insert, no `bases_changed`, and — through the caller — no
                // key in `bases_keys_written` for an undo to "restore".
                if map.get(&rel) == Some(&hash) {
                    return false;
                }
                map.insert(rel.clone(), hash.clone());
                inserts.insert(rel, hash);
                true
            }
            None => false,
        }
    }
    /// `record_base` for a version this machine deliberately holds apart
    /// from the repository's (see [`bases::held_base`]): the repository
    /// bytes are named, never recorded as a synced version.
    fn record_held_base(
        map: &mut BaseHashes,
        inserts: &mut BaseHashes,
        repo_root: &Path,
        repo_path: &Path,
        repo_bytes: &[u8],
    ) -> bool {
        let Some(rel) = repo_relative(repo_root, repo_path) else {
            return false;
        };
        let held = bases::held_base(repo_bytes);
        if map.get(&rel) == Some(&held) {
            return false;
        }
        map.insert(rel.clone(), held.clone());
        inserts.insert(rel, held);
        true
    }
    /// The PROTECTION_SENTINEL twin of `record_base`: both mid-pull
    /// create-skip sites record the sentinel (never a real hash — see
    /// the constant's doc) through the same movement rule, so the
    /// no-churn guard lives in one place. Returns whether the record
    /// moved (caller flips `bases_changed`); the caller also owns the
    /// `protection_keys` push that keeps the sentinel out of the undo
    /// surgery's allowlist.
    fn record_protection_base(map: &mut BaseHashes, inserts: &mut BaseHashes, rel: &str) -> bool {
        let hash = bases::PROTECTION_SENTINEL.to_string();
        let prev = map.insert(rel.to_string(), hash.clone());
        if prev.as_deref() != Some(bases::PROTECTION_SENTINEL) {
            inserts.insert(rel.to_string(), hash);
            return true;
        }
        false
    }

    // Creates that did not EXECUTE (repo copy unreadable, write failed):
    // their keys must not enter the tracked record — the local file does
    // not exist, and the next push would read that as a local DELETION,
    // removing the repo copy for every machine. Skipped and retried, like
    // the deletes arm's non-executions.
    let mut creates_to_retry: Vec<String> = Vec::new();
    for write in &plan.creates {
        // The arm calls descriptor(write.category) 3-4 times per iteration;
        // the registry slice is short but linear — cache once per loop.
        let desc = descriptor(write.category);
        // The same mid-pull window every other arm re-checks: a file that
        // APPEARED at the destination since the plan is local content the
        // repository knows nothing about — overwriting it with the repo
        // bytes would destroy it unrecoverably (a create is not in the
        // snapshot; undo would only remove the file). Skip, and make the
        // skip DURABLE the way a decline is: recording the repo bytes as
        // the base leaves the user's file reading dirty, so the next
        // pull keeps it too instead of fast-forwarding the repo copy
        // over it (which a baseless skip would let happen).
        if write.local_path.is_file() {
            log::warn!(
                "Skipping {} (created while the pull ran; held until a pull or an \
                 explicit push settles it against the repository copy)",
                write.local_path.display()
            );
            skip_unfinished_write(&mut by_category, &mut report, write);
            // The same file runs every iteration of this arm — lift the
            // repo-relative key once instead of repeating 2-4 times below.
            let rel = repo_relative(&plan.repo_root, &write.repo_path);
            creates_to_retry.push(rel.clone().unwrap_or_default());
            if records_base(desc.merge, &write.category_path) {
                match machine_bytes(desc, &plan.tokens, &write.repo_path) {
                    Ok(repo_bytes) => {
                        bases_changed |= record_held_base(
                            &mut synced_bases,
                            &mut bases_inserts,
                            &plan.repo_root,
                            &write.repo_path,
                            &repo_bytes,
                        );
                        if let Some(rel) = rel.as_ref() {
                            protection_keys.push(rel.clone());
                        }
                    }
                    Err(e) => {
                        // The repo copy was readable at plan time and is not
                        // anymore — recording NOTHING would let the next pull
                        // (copy readable again, no base entry) fast-forward
                        // the repository copy straight over the user's
                        // mid-pull file. Record the hash of EMPTY bytes
                        // instead: the user's file reads dirty against it,
                        // so the next pull keeps it too. The one cost is a
                        // kept-local round that a push (which records the
                        // true base) clears.
                        log::warn!(
                            "Recording a protection base for {} (repo copy unreadable: {e}); \
                             its mid-pull local file stays protected",
                            write.repo_path.display()
                        );
                        if let Some(rel) = rel.as_ref() {
                            bases_changed |=
                                record_protection_base(&mut synced_bases, &mut bases_inserts, rel);
                            protection_keys.push(rel.clone());
                        }
                    }
                }
            }
            continue;
        }
        let bytes = match machine_bytes(desc, &plan.tokens, &write.repo_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                log::warn!(
                    "Skipping {} (repo copy unreadable): {e}",
                    write.repo_path.display()
                );
                skip_unfinished_write(&mut by_category, &mut report, write);
                creates_to_retry
                    .push(repo_relative(&plan.repo_root, &write.repo_path).unwrap_or_default());
                continue;
            }
        };
        // Last-chance re-read closes the is_file→read→write TOCTOU: a user
        // file that appeared between the snapshot and now would be
        // overwritten by the unconditional persist. Convert it into a
        // durable skip (same as the upstream is_file guard) — the user's
        // file survives and is protected next pull.
        if write.local_path.is_file() {
            log::warn!(
                "Skipping {} (user file appeared just before the write)",
                write.local_path.display()
            );
            skip_unfinished_write(&mut by_category, &mut report, write);
            // Same durable protection as the upstream is_file guard: a
            // user file whose content we cannot read is recorded as a
            // PROTECTION base so the next pull reads it dirty instead of
            // treating the now-existing local file as 'never synced'
            // (which would overwrite it on the next non-interactive pull).
            if records_base(desc.merge, &write.category_path) {
                if let Some(rel) = repo_relative(&plan.repo_root, &write.repo_path) {
                    bases_changed |=
                        record_protection_base(&mut synced_bases, &mut bases_inserts, &rel);
                    // Mirror the upstream is_file guard: the protection
                    // entry must NOT flow into `bases_keys_written` (the
                    // undo surgery's allowlist) — see the `retain` that
                    // filters protection keys, or an `undo_pull` would
                    // strip the sentinel and disarm the protection.
                    protection_keys.push(rel);
                }
            }
            creates_to_retry
                .push(repo_relative(&plan.repo_root, &write.repo_path).unwrap_or_default());
            continue;
        }
        if let Err(e) = write_atomic(&write.local_path, &bytes, &write.repo_path) {
            log::warn!(
                "Skipping {} (write failed): {e}",
                write.local_path.display()
            );
            skip_unfinished_write(&mut by_category, &mut report, write);
            creates_to_retry
                .push(repo_relative(&plan.repo_root, &write.repo_path).unwrap_or_default());
            continue;
        }
        // The one shared rule (see `records_base`): union-merged files grow
        // on every later merge while no push ever re-records them; every
        // other file raw-overwrites and keeps its base.
        if records_base(desc.merge, &write.category_path) {
            bases_changed |= record_base(
                &mut synced_bases,
                &mut bases_inserts,
                &plan.repo_root,
                &write.repo_path,
                &bytes,
            );
        }
        counts_for(&mut by_category, write.category).added += 1;
        report.record(
            write.category,
            ArtifactChangeKind::Added,
            &write.category_path,
        );
        report.created_abs_paths.push(write.local_path.clone());
    }

    for write in &plan.kept_local {
        // No backup, no modification (see `unsnapshotted`): a kept-
        // local file the snapshot couldn't read for an env reason must
        // not be reported kept without a backup (the take-remote path
        // would recreate it; the kept path would hold the sync gate
        // indefinitely). The overwrites, unions, and deletes arms all
        // gate the same way via `skip_unsnapshotted_write`; kept_local
        // diverged from the established pattern.
        if plan.unsnapshotted.contains(&write.local_path) {
            skip_unsnapshotted_write(&mut by_category, &mut report, write);
            continue;
        }
        // The same mid-pull window the other arms re-check: a file deleted
        // while the pull waited is neither kept nor prompted over — the
        // deletion stands (counting a phantom keep would hold the sync
        // gate on a file that no longer exists, and a take-remote would
        // recreate it).
        if !write.local_path.is_file() {
            log::warn!(
                "Skipping {} (deleted while the pull ran)",
                write.local_path.display()
            );
            skip_unfinished_write(&mut by_category, &mut report, write);
            continue;
        }
        // The fall-through's FreshRead drives both the report_kept's
        // `nothing_to_publish` (only Clean — local matches the base —
        // means nothing to publish) and the Vanished skip. None-recorded
        // maps to Dirty below, NOT Clean: a missing base must never
        // inflate `kept_local_clean` and bias the hint toward a push
        // that would republish older bytes.
        let recorded =
            repo_relative(&plan.repo_root, &write.repo_path).and_then(|rel| synced_bases.get(&rel));
        // Deliberately NO bytes-level re-check here (unlike the deletes
        // arms): a kept-local-only pull is predicted to write nothing, is
        // snapshotless, and an unpredicted file+base write from this arm
        // would be un-undoable and mislead the concurrent-sync warning. An
        // edit reverted while the pull waited is therefore held exactly one
        // round — the next plan sees local == base and fast-forwards
        // through the predicted path. An unreadable target is kept, not
        // guessed at: the protection stands until the file is readable.
        //
        // Keeping the local file is the protection and the default. But an
        // interactive pull is exactly where the user should get to say
        // otherwise: offer the same take-remote / keep / merge-tool choice
        // an overwrite gets, so "take the repository copy" does not require
        // pushing first (which would clobber the remote change). The
        // top-of-loop unsnapshotted guard already returned
        // early on unbacked files — the file IS backed here.
        if prompt_overwrites {
            // The file IS backed — we proceed into the
            // interactive take-remote/merge-tool path, exactly like a
            // backed file in the overwrites arm.
            let repo_bytes =
                match machine_bytes(descriptor(write.category), &plan.tokens, &write.repo_path) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        log::warn!(
                            "Keeping {} (repo copy unreadable): {e}",
                            write.local_path.display()
                        );
                        // The REPO copy is what failed: the advised push
                        // reads the same repo copy for comparison and holds
                        // the file back too — "push to publish" cannot
                        // resolve this keep right now. Held for the next
                        // pull to retry (a recoverable transient, NOT a
                        // "nothing to publish" keep — the local bytes ARE
                        // dirty; passing nothing_to_publish=true would
                        // inflate kept_local_clean and bias the hint).
                        report_kept(
                            &mut by_category,
                            &mut report,
                            write.category,
                            &write.category_path,
                            false,
                        );
                        continue;
                    }
                };
            let resolution = crate::merge_tool::resolve_overwrite(
                &plan.merge_tool,
                plan.prefer_merge_tool,
                &write.local_path,
                &repo_bytes,
            );
            let resolution = match resolution {
                Ok(resolution) => resolution,
                Err(e) => {
                    log::warn!(
                        "Keeping {} (merge tool failed): {e}",
                        write.local_path.display()
                    );
                    report_kept(
                        &mut by_category,
                        &mut report,
                        write.category,
                        &write.category_path,
                        false,
                    );
                    continue;
                }
            };
            // KEEP_BOTH: write the remote bytes to a sibling conflict
            // copy (Syncthing naming) and leave the local file untouched.
            // PROMPT→ANSWER re-check still applies (the file could have
            // been deleted during the prompt) and the write failure path
            // is the same.
            if let crate::merge_tool::ResolutionOutcome::ConflictCopy { rename } = resolution {
                if !write.local_path.is_file() {
                    log::warn!(
                        "Skipping conflict copy for {} (deleted during prompt; held for next pull)",
                        write.local_path.display()
                    );
                    skip_unfinished_write(&mut by_category, &mut report, write);
                    continue;
                }
                if let Err(e) = write_atomic(&rename, &repo_bytes, &write.repo_path) {
                    log::warn!(
                        "Could not save repo version of {} to {}: {e}",
                        write.local_path.display(),
                        rename.display()
                    );
                    report_kept(
                        &mut by_category,
                        &mut report,
                        write.category,
                        &write.category_path,
                        false,
                    );
                    continue;
                }
                // Keeping both is a resolution like a merge: the user saw the
                // repository version (now saved beside the file) and kept
                // theirs. Recorded the same way, so the next sync publishes it.
                bases_changed |= record_base(
                    &mut synced_bases,
                    &mut bases_inserts,
                    &plan.repo_root,
                    &write.repo_path,
                    &repo_bytes,
                );
                log::info!(
                    "Saved repo version of '{}' as {} (conflicting local edit)",
                    write.local_path.display(),
                    rename.display()
                );
                counts_for(&mut by_category, write.category).modified += 1;
                report.record(
                    write.category,
                    ArtifactChangeKind::Modified,
                    &write.category_path,
                );
                continue;
            }
            if let crate::merge_tool::ResolutionOutcome::Write(bytes) = resolution {
                // PROMPT→ANSWER re-check: a file deleted during the
                // multi-second merge-tool session must not be
                // silently resurrected by the TakeRemote path. The
                // initial is_file check was before the
                // prompt; the deletes arm catches the same scenario
                // via FreshRead::Vanished at apply time. Mirror it
                // here so kept_local does not diverge.
                if !write.local_path.is_file() {
                    log::warn!(
                        "Skipping {} (deleted during prompt; held for next pull)",
                        write.local_path.display()
                    );
                    skip_unfinished_write(&mut by_category, &mut report, write);
                    continue;
                }
                if let Err(e) = write_atomic(&write.local_path, &bytes, &write.repo_path) {
                    log::warn!("Keeping {} (write failed): {e}", write.local_path.display());
                    report_kept(
                        &mut by_category,
                        &mut report,
                        write.category,
                        &write.category_path,
                        false,
                    );
                    continue;
                }
                // The base is the repository version the user resolved
                // against — for a take, exactly what was written; for a
                // merge-tool result, the version the merge already holds.
                // Not the merged bytes: those would read as synced, and the
                // next non-interactive pull would overwrite the resolution
                // with any repository change. With the repository version
                // as base, the next sync sees "only this machine changed
                // it" and publishes the resolution — unless the other
                // machine changed the file again meanwhile, which holds it.
                bases_changed |= record_base(
                    &mut synced_bases,
                    &mut bases_inserts,
                    &plan.repo_root,
                    &write.repo_path,
                    &repo_bytes,
                );
                counts_for(&mut by_category, write.category).modified += 1;
                report.record(
                    write.category,
                    ArtifactChangeKind::Modified,
                    &write.category_path,
                );
                continue;
            }
        }
        // PROMPT→ANSWER window (the kept arm): the per-write flag
        // captured at the top of the iteration is stale once the prompt
        // has been answered — a user can revert or further edit the file
        // while the resolver is running (the minutes-long merge-tool
        // session especially). Refresh against the current disk bytes.
        //
        // A file that vanished mid-pull (deleted by the user or a
        // background process between the initial is_file check and the
        // re-read here) must NOT count as kept_local: the file is gone,
        // and counting it as kept_local would inflate the hint metric
        // AND the kept_local count, holding the sync gate open on a
        // phantom file. The overwrites arm catches the same scenario at
        // (Vanished branch) — kept_local diverged from the established pattern.
        //
        // None-recorded files (a concurrent sync pruned the base
        // between plan and apply) skip the dirty_status read (the base
        // is gone; there is nothing to compare against). An explicit
        // is_file check catches the same scenario as above for this case.
        // `recorded` is already `Option<&String>` (from the
        // synced_bases.get above), so `.as_ref()` would produce
        // `Option<&&String>` — the cleaner pattern (matching the
        // overwrites arm) passes the binding directly.
        let fresh = match recorded {
            Some(b) => dirty_status(Some(b), &write.local_path),
            None => {
                if write.local_path.is_file() {
                    FreshRead::Dirty
                } else {
                    FreshRead::Vanished
                }
            }
        };
        match fresh {
            FreshRead::Vanished => {
                log::warn!(
                    "Skipping {} (deleted between plan and apply; held for next pull)",
                    write.local_path.display()
                );
                skip_unfinished_write(&mut by_category, &mut report, write);
                continue;
            }
            FreshRead::Clean => report_kept(
                &mut by_category,
                &mut report,
                write.category,
                &write.category_path,
                true,
            ),
            FreshRead::Dirty | FreshRead::Unreadable => report_kept(
                &mut by_category,
                &mut report,
                write.category,
                &write.category_path,
                false,
            ),
        }
    }
    // Changed or deleted here only: nothing to take and, unlike a keep,
    // nothing held back. Counted so the summary says the push publishes them.
    for write in plan.local_only.iter().chain(&plan.deleted_here) {
        counts_for(&mut by_category, write.category).pending_push += 1;
    }

    for write in &plan.date_settles {
        // No backup, no modification (see `unsnapshotted`).
        if plan.unsnapshotted.contains(&write.local_path) {
            skip_unsnapshotted_write(&mut by_category, &mut report, write);
            continue;
        }
        let Ok(local_bytes) = fs::read(&write.local_path) else {
            log::warn!(
                "Skipping {} (unreadable or deleted while the pull ran)",
                write.local_path.display()
            );
            skip_unfinished_write(&mut by_category, &mut report, write);
            continue;
        };
        // Settled against the bytes on disk NOW: an edit saved while the
        // pull waited can make the two versions differ in more than dates,
        // and then the file is a keep like any other both-sides change.
        let merged = machine_bytes(descriptor(write.category), &plan.tokens, &write.repo_path)
            .ok()
            .and_then(|repo_bytes| keep_later_timestamps(&local_bytes, &repo_bytes));
        let Some(merged) = merged else {
            report_kept(
                &mut by_category,
                &mut report,
                write.category,
                &write.category_path,
                false,
            );
            continue;
        };
        if merged == local_bytes {
            // This machine already holds every later date.
            counts_for(&mut by_category, write.category).pending_push += 1;
            continue;
        }
        // No base is recorded: the merge holds dates the repository lacks,
        // so until a push publishes it the next pull still sees both sides
        // changed, and settles the same way.
        if let Err(e) = write_atomic(&write.local_path, &merged, &write.repo_path) {
            log::warn!("Keeping {} (write failed): {e}", write.local_path.display());
            report_kept(
                &mut by_category,
                &mut report,
                write.category,
                &write.category_path,
                false,
            );
            continue;
        }
        counts_for(&mut by_category, write.category).modified += 1;
        report.record(
            write.category,
            ArtifactChangeKind::Modified,
            &write.category_path,
        );
    }

    for delete in &plan.kept_local_deletes {
        // The same mid-pull window the kept_local arm re-checks: a file
        // deleted while the pull waited has already accomplished what the
        // repository asked — no prompt about a file that isn't there, no
        // phantom keep holding the sync gate.
        //
        // The base entry is NOT pruned here: the plan-time prediction
        // cannot see this arm, and an unpredicted record write from a
        // snapshotless pull misleads the undo hint and the concurrent-
        // sync warning. The re-armed tracked entry hands the prune to
        // the next pull's gone-from-both-sides pass (one round, inert —
        // the file exists nowhere).
        if !delete.local_path.is_file() {
            counts_for(&mut by_category, delete.category).deleted += 1;
            report.record(
                delete.category,
                ArtifactChangeKind::KeptLocalDelete,
                &delete.category_path,
            );
            continue;
        }
        // Keeping is the default (the protection), but an interactive pull
        // must be able to CLEAR the keep — `sync` points here — so offer
        // the explicit delete, defaulting to keep. An UNBACKED file (see
        // `unsnapshotted`) is never offered the delete: removing it would
        // destroy the only copy of bytes nothing could restore.
        if prompt_overwrites
            && !plan.unsnapshotted.contains(&delete.local_path)
            && confirm_kept_local_deletion(&delete.local_path)
        {
            // PROMPT→ANSWER: same window the plain deletes arm closes —
            // an edit saved during the confirmation is kept (the snapshot
            // holds only pre-pull bytes; undo cannot restore the edit).
            // A missing base (concurrent sync pruned it between plan and
            // apply) is treated as a synthetic Dirty: silently deleting on
            // a destroyed base would destroy a possibly-edited file, the
            // exact loss the protect-local-edits contract exists to
            // prevent. Mirrors the deletes arm's None-base guard.
            let recorded = synced_bases.get(&delete.repo_rel);
            if is_potentially_dirty(recorded, &delete.local_path) {
                log::warn!(
                    "Keeping {} (edited while the prompt was open, or its base was cleared mid-pull)",
                    delete.local_path.display()
                );
                report_kept(
                    &mut by_category,
                    &mut report,
                    delete.category,
                    &delete.category_path,
                    false,
                );
                continue;
            }
            if delete.local_path.is_file() {
                if let Err(e) = fs::remove_file(&delete.local_path) {
                    // Same never-strand rule as the plain deletes arm.
                    log::warn!(
                        "Keeping {} (remove failed): {e}",
                        delete.local_path.display()
                    );
                    report_kept(
                        &mut by_category,
                        &mut report,
                        delete.category,
                        &delete.category_path,
                        false,
                    );
                    continue;
                }
            }
            if synced_bases.remove(&delete.repo_rel).is_some() {
                bases_changed = true;
                bases_removals.push(delete.repo_rel.clone());
            }
            counts_for(&mut by_category, delete.category).deleted += 1;
            report.record(
                delete.category,
                ArtifactChangeKind::KeptLocalDelete,
                &delete.category_path,
            );
            continue;
        }
        report_kept(
            &mut by_category,
            &mut report,
            delete.category,
            &delete.category_path,
            false,
        );
    }

    for write in &plan.overwrites {
        // No backup, no modification (see `unsnapshotted`).
        if plan.unsnapshotted.contains(&write.local_path) {
            skip_unsnapshotted_write(&mut by_category, &mut report, write);
            continue;
        }
        let repo_bytes =
            match machine_bytes(descriptor(write.category), &plan.tokens, &write.repo_path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    log::warn!(
                        "Keeping {} (repo copy unreadable): {e}",
                        write.local_path.display()
                    );
                    report_kept(
                        &mut by_category,
                        &mut report,
                        write.category,
                        &write.category_path,
                        true,
                    );
                    continue;
                }
            };
        // Re-check dirty status against the bytes that are on disk NOW,
        // not the ones the plan saw: an edit saved while the pull waited
        // on a confirmation must convert this write into a keep, not be
        // overwritten — the same window the plan-time base hashing closes
        // from the other side. The shared triage (`dirty_status`) skips
        // the read entirely without a base entry, treats a vanished file
        // as a plain recreate, and keeps an unreadable one instead of
        // aborting a half-applied pull.
        let rel_now = repo_relative(&plan.repo_root, &write.repo_path);
        let recorded_now = rel_now.as_ref().and_then(|rel| synced_bases.get(rel));
        match dirty_status(recorded_now, &write.local_path) {
            FreshRead::Dirty => {
                // An edit saved while the pull waited: publishable — a
                // push is exactly what publishes it.
                report_kept(
                    &mut by_category,
                    &mut report,
                    write.category,
                    &write.category_path,
                    false,
                );
                continue;
            }
            // UNDECIDED, not a keep — the deletes arm's rule: nothing was
            // edited, the "push to publish" advice would publish nothing
            // (the push holds an unreadable file back), and the gate must
            // not stall a round over a file nothing touched. Skipped and
            // retried once readable.
            FreshRead::Unreadable => {
                log::warn!(
                    "Skipping {} (unreadable; retried once it is readable)",
                    write.local_path.display()
                );
                skip_unfinished_write(&mut by_category, &mut report, write);
                continue;
            }
            // Deleted while the pull ran: the user's deletion stands —
            // the same window the unions arm refuses (writing the repo
            // bytes back would silently resurrect the file). A push
            // publishes the decision.
            FreshRead::Vanished => {
                log::warn!(
                    "Skipping {} (deleted while the pull ran)",
                    write.local_path.display()
                );
                skip_unfinished_write(&mut by_category, &mut report, write);
                continue;
            }
            FreshRead::Clean => {}
        }
        let resolved = if prompt_overwrites {
            match crate::merge_tool::resolve_overwrite(
                &plan.merge_tool,
                plan.prefer_merge_tool,
                &write.local_path,
                &repo_bytes,
            ) {
                Err(e) => {
                    log::warn!(
                        "Keeping {} (merge tool failed): {e}",
                        write.local_path.display()
                    );
                    report_kept(
                        &mut by_category,
                        &mut report,
                        write.category,
                        &write.category_path,
                        true,
                    );
                    continue;
                }
                Ok(crate::merge_tool::ResolutionOutcome::ConflictCopy { rename }) => {
                    // KEEP_BOTH: write the remote bytes to a sibling
                    // conflict copy and leave the local file untouched.
                    // The change is reported as
                    // Modified against the original local path so the
                    // summary is intelligible (the conflict copy is a
                    // side effect, not a separate file the user
                    // tracks).
                    if let Err(e) = write_atomic(&rename, &repo_bytes, &write.repo_path) {
                        log::warn!(
                            "Could not save repo version of {} to {}: {e}",
                            write.local_path.display(),
                            rename.display()
                        );
                        report_kept(
                            &mut by_category,
                            &mut report,
                            write.category,
                            &write.category_path,
                            true,
                        );
                        continue;
                    }
                    // Same as the kept-local arm's keep-both.
                    bases_changed |= record_base(
                        &mut synced_bases,
                        &mut bases_inserts,
                        &plan.repo_root,
                        &write.repo_path,
                        &repo_bytes,
                    );
                    log::info!(
                        "Saved repo version of '{}' as {} (conflicting local edit)",
                        write.local_path.display(),
                        rename.display()
                    );
                    counts_for(&mut by_category, write.category).modified += 1;
                    report.record(
                        write.category,
                        ArtifactChangeKind::Modified,
                        &write.category_path,
                    );
                    continue;
                }
                Ok(crate::merge_tool::ResolutionOutcome::Write(resolved)) => Some(resolved),
                Ok(crate::merge_tool::ResolutionOutcome::KeepLocal) => {
                    // A decline IS a keep: counted kept-local (not
                    // skipped), so sync's gate sees it and holds the
                    // artifact push back instead of publishing the stale
                    // local bytes over the repository's newer version.
                    // And it is made DURABLE: recording the declined repo
                    // bytes as the base leaves the local file reading
                    // dirty, so tomorrow's non-interactive sync keeps it
                    // too instead of fast-forwarding the user's choice
                    // away. A push or an accepted take clears it.
                    bases_changed |= record_held_base(
                        &mut synced_bases,
                        &mut bases_inserts,
                        &plan.repo_root,
                        &write.repo_path,
                        &repo_bytes,
                    );
                    // The decline recorded the repo bytes as base, so the
                    // local file reads dirty: a push publishes it.
                    report_kept(
                        &mut by_category,
                        &mut report,
                        write.category,
                        &write.category_path,
                        false,
                    );
                    continue;
                }
                Ok(crate::merge_tool::ResolutionOutcome::Abandoned) => {
                    // No decision was made: keep for THIS pull only, record
                    // nothing — an abandoned merge must not durably mark
                    // the file dirty.
                    report_kept(
                        &mut by_category,
                        &mut report,
                        write.category,
                        &write.category_path,
                        true,
                    );
                    continue;
                }
            }
        } else {
            None
        };
        // `None` means the pure repository bytes (the non-interactive
        // default): borrowed, not cloned; only a resolution allocates.
        let bytes: &[u8] = resolved.as_deref().unwrap_or(&repo_bytes);
        if let Err(e) = write_atomic(&write.local_path, bytes, &write.repo_path) {
            log::warn!("Keeping {} (write failed): {e}", write.local_path.display());
            report_kept(
                &mut by_category,
                &mut report,
                write.category,
                &write.category_path,
                true,
            );
            continue;
        }
        // A take records the repository bytes it wrote; a merge-tool
        // result records the repository version it resolved against (see
        // the kept-local arm) — the same value either way.
        bases_changed |= record_base(
            &mut synced_bases,
            &mut bases_inserts,
            &plan.repo_root,
            &write.repo_path,
            &repo_bytes,
        );
        counts_for(&mut by_category, write.category).modified += 1;
        report.record(
            write.category,
            ArtifactChangeKind::Modified,
            &write.category_path,
        );
    }

    for write in &plan.unions {
        // No backup, no modification (see `unsnapshotted`).
        if plan.unsnapshotted.contains(&write.local_path) {
            skip_unsnapshotted_write(&mut by_category, &mut report, write);
            continue;
        }
        let desc = descriptor(write.category);
        let repo_bytes = match machine_bytes(desc, &plan.tokens, &write.repo_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                log::warn!(
                    "Skipping {} (repo copy unreadable): {e}",
                    write.repo_path.display()
                );
                skip_unfinished_write(&mut by_category, &mut report, write);
                continue;
            }
        };
        // Never fatal, like every other arm: aborting here would strand a
        // half-applied pull with no undo record. An unreadable file is
        // skipped (reading it as empty would write the repo's lines over
        // the local-only ones); a file deleted while the pull waited is
        // left deleted — a union target existed at plan time by
        // construction, so a vanished one is a mid-pull deletion, and
        // recreating it with the repo's lines would be the same window the
        // overwrites and deletes arms re-check for.
        let local_bytes = match fs::read(&write.local_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::warn!(
                    "Skipping {} (deleted while the pull ran)",
                    write.local_path.display()
                );
                skip_unfinished_write(&mut by_category, &mut report, write);
                continue;
            }
            Err(e) => {
                log::warn!(
                    "Skipping {} (unreadable, cannot union-merge): {e}",
                    write.local_path.display()
                );
                skip_unfinished_write(&mut by_category, &mut report, write);
                continue;
            }
        };
        let (merged, new_entries) = match desc.merge {
            MergeStrategy::UnionMemoryIndex => merge_memory_index(&local_bytes, &repo_bytes),
            _ => {
                // Strict UTF-8 decode — `from_utf8_lossy` would silently
                // substitute invalid sequences with U+FFFD, corrupting
                // binary or partial-encoding prompt-history content on
                // every pull/push. An unreadable prompt-history must
                // error out so the user can hand-recover, not silently
                // rewrite the file with replacement characters.
                let local_text = match std::str::from_utf8(&local_bytes) {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!(
                            "Skipping {} (local bytes not valid UTF-8; cannot union-merge): {e}",
                            write.local_path.display()
                        );
                        skip_unfinished_write(&mut by_category, &mut report, write);
                        continue;
                    }
                };
                let repo_text = match std::str::from_utf8(&repo_bytes) {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!(
                            "Skipping {} (repo bytes not valid UTF-8; cannot union-merge): {e}",
                            write.local_path.display()
                        );
                        skip_unfinished_write(&mut by_category, &mut report, write);
                        continue;
                    }
                };
                let (text, lines) = merge_history_lines(local_text, repo_text);
                (text.into_bytes(), lines)
            }
        };
        if let Err(e) = write_atomic(&write.local_path, &merged, &write.repo_path) {
            log::warn!(
                "Skipping {} (write failed): {e}",
                write.local_path.display()
            );
            skip_unfinished_write(&mut by_category, &mut report, write);
            continue;
        }
        // Record the MERGED bytes (not the pre-merge local bytes) as the
        // base — the apply just put them there and the next non-interactive
        // pull must see local == base, else it reads as a local change
        // the user never made. `UnionMemoryIndex` is now in
        // `records_base = true` so a future local edit will surface
        // as a real dirty read on the next pull (issue #103 contract);
        // `UnionJsonl` stays out because per-line protection is not
        // what prompt-history union-merge guarantees. The gate is the
        // shared per-file rule kept so a future strategy that belongs
        // here inherits the correct behavior automatically.
        if records_base(desc.merge, &write.category_path)
            && record_base(
                &mut synced_bases,
                &mut bases_inserts,
                &plan.repo_root,
                &write.repo_path,
                &merged,
            )
        {
            bases_changed = true;
        }
        let counts = counts_for(&mut by_category, write.category);
        counts.modified += 1;
        counts.merged_entries += new_entries;
        report.record(
            write.category,
            ArtifactChangeKind::Modified,
            &write.category_path,
        );
    }

    for write in &plan.mode_fixes {
        // The content already matches the repository; only the mode can move,
        // so the file is synced either way. Its base hash was recorded at
        // plan time (from the verified bytes), nothing to re-read here.
        if prompt_overwrites && !confirm_executable(&write.local_path) {
            counts_for(&mut by_category, write.category).skipped += 1;
            continue;
        }
        let realigned = align_executable_bit(&write.local_path, &write.repo_path);
        if realigned {
            counts_for(&mut by_category, write.category).modified += 1;
            report.record(
                write.category,
                ArtifactChangeKind::Modified,
                &write.category_path,
            );
        }
    }

    // Deletes that did not EXECUTE (unreadable at apply time, remove
    // failed): their tracked entries must stay, or the deletion would
    // never be retried and the next push would resurrect the repo copy
    // the other machine deleted. A DECLINE deliberately un-tracks
    // instead — the keep was the user's decision, and publishing it on
    // the next push is the documented contract.
    let mut deletes_to_retry: Vec<String> = Vec::new();
    for delete in &plan.deletes {
        // No backup, no deletion (see `unsnapshotted`).
        if plan.unsnapshotted.contains(&delete.local_path) {
            log::warn!(
                "Skipping delete of {} (no pre-pull backup could be taken; retried next pull)",
                delete.local_path.display()
            );
            counts_for(&mut by_category, delete.category).skipped += 1;
            // Keep the tracked entry alive: the skip is a transient
            // (the snapshot couldn't be taken for an env reason, not
            // a deliberate user choice), so the next pull must
            // re-classify the file as a delete, not as never-synced.
            // Mirrors the post-prompt None-base/Dirty branch below.
            //
            // This fix only preserves the tracked entry across one
            // round. Multi-round scenarios where A re-pushes the file
            // after B's F3-keep are handled by the overwrites arm's
            // plan-time `tracked_before` second signal: a None-base
            // with `tracked_before.contains(rel)` falls into
            // `kept_local`, not `overwrites`. Fresh installs (no
            // `tracked_before` entry) still remote-win.
            deletes_to_retry.push(delete.repo_rel.clone());
            continue;
        }
        // Vanished while the pull waited: the deletion is already done.
        // No prompt about a file that isn't there (a decline would count
        // a phantom keep AND strand the base entry where no later prune
        // can reach it, the key having left the tracked record).
        if !delete.local_path.is_file() {
            if synced_bases.remove(&delete.repo_rel).is_some() {
                bases_changed = true;
                bases_removals.push(delete.repo_rel.clone());
            }
            counts_for(&mut by_category, delete.category).deleted += 1;
            report.record(
                delete.category,
                ArtifactChangeKind::KeptLocalDelete,
                &delete.category_path,
            );
            continue;
        }
        // Re-check FIRST, like the overwrites arm: an edit saved while the
        // pull waited converts the deletion into a keep BEFORE any prompt —
        // otherwise a confirmed deletion would be silently overridden by
        // the re-check below. An unreadable file at apply time is
        // UNDECIDED, not a keep (a keep would stall the sync gate on a
        // file nothing can resolve): the deletion simply does not run
        // this round. (The vanished early-continue above already
        // established the file exists — no is_file wrapper needed.)
        let recorded_now = synced_bases.get(&delete.repo_rel);
        match dirty_status(recorded_now, &delete.local_path) {
            FreshRead::Dirty => {
                // An edit saved while the pull waited: keep the file
                // AND its tracked entry — dropping the entry here would
                // open the sync gate next round and the full push would
                // republish the edit, silently reverting the deletion
                // (the apply-time twin of the plan-time re-arm).
                report_kept(
                    &mut by_category,
                    &mut report,
                    delete.category,
                    &delete.category_path,
                    false,
                );
                deletes_to_retry.push(delete.repo_rel.clone());
                continue;
            }
            FreshRead::Unreadable => {
                log::warn!(
                    "Skipping delete of {} (unreadable; retried once it is readable)",
                    delete.local_path.display()
                );
                counts_for(&mut by_category, delete.category).skipped += 1;
                deletes_to_retry.push(delete.repo_rel.clone());
                continue;
            }
            _ => {}
        }
        // ONE prompt whose answer governs the outcome (interactively).
        // Non-interactive: no prompt, proceed (the apply-time re-check
        // above already gates the user-edit windows the prompt would
        // cover). The PROMPT→ANSWER re-check below closes the window
        // between the answer and the delete.
        let confirmed = if prompt_overwrites {
            confirm_deletion(&delete.local_path)
        } else {
            true
        };
        if !confirmed {
            // Declined: the file stays, so its base entry must stay too —
            // dropping it would let a later repo copy of the file overwrite
            // the local one the user just chose to keep. And a decline IS
            // a keep: counted kept-local so sync's gate holds the push.
            report_kept(
                &mut by_category,
                &mut report,
                delete.category,
                &delete.category_path,
                false,
            );
            continue;
        }
        // PROMPT→ANSWER + None-base guard: dirty_status returns Clean when
        // recorded_now is None, but a missing base entry between plan
        // and apply (a concurrent sync pruned it, or it was never there)
        // is NOT evidence the file is clean — a destroyed base would
        // silently let an edited file be deleted, which the protect-
        // local-edits contract exists to prevent. Treat missing-base as
        // a synthetic Dirty: skip the delete, retry next round when
        // the base returns or the file is re-classified. The guard
        // applies to BOTH interactive and non-interactive pulls. The
        // re-read only pays when a prompt actually ran (the answer
        // window is the race); non-interactive reaching here already
        // read Clean at the pre-prompt match, so only the None-base
        // half can still fire.
        let post_prompt_dirty = recorded_now.is_none()
            || (prompt_overwrites
                && dirty_status(recorded_now, &delete.local_path) == FreshRead::Dirty);
        if post_prompt_dirty {
            // Edit saved during the prompt, or no recorded base: an
            // edited file is kept, never destroyed (the snapshot holds
            // only pre-pull bytes). A missing base treats the file as
            // potentially edited until the records can be re-classified.
            log::warn!(
                "Keeping {} (edited while the prompt was open, or its base was cleared mid-pull)",
                delete.local_path.display()
            );
            report_kept(
                &mut by_category,
                &mut report,
                delete.category,
                &delete.category_path,
                false,
            );
            // The tracked entry must survive this skip, mirroring the
            // pre-prompt Dirty/Unreadable branches: a transient
            // keep that the next round will re-classify. Without the
            // push, the tracked-delta loop drops the entry, and the
            // next pull sees the locally-edited file as never-synced
            // and silently overwrites it (the data-loss the original
            // F3-class fix was meant to prevent).
            deletes_to_retry.push(delete.repo_rel.clone());
            continue;
        }
        if delete.local_path.is_file() {
            if let Err(e) = fs::remove_file(&delete.local_path) {
                log::warn!(
                    "Skipping delete of {} (remove failed): {e}",
                    delete.local_path.display()
                );
                counts_for(&mut by_category, delete.category).skipped += 1;
                deletes_to_retry.push(delete.repo_rel.clone());
                continue;
            }
        }
        // The removal key counts only when an entry actually existed:
        // keys_written feeds the undo, and a no-op removal would let it
        // delete a base a LATER push legitimately recorded.
        if synced_bases.remove(&delete.repo_rel).is_some() {
            bases_changed = true;
            bases_removals.push(delete.repo_rel.clone());
        }
        counts_for(&mut by_category, delete.category).deleted += 1;
        report.record(
            delete.category,
            ArtifactChangeKind::Deleted,
            &delete.category_path,
        );
    }

    // Base hashes verified at plan time: recorded as-is, without re-reading
    // the files — an edit made while the pull waited must NOT end up
    // recorded as the synced state. Only a value that actually moves
    // becomes an insert: an unchanged entry is a no-op the apply does not
    // own (and the undo must not "restore").
    for (rel, hash) in &plan.base_hashes {
        // `insert` returns the previous value in one BTreeMap descent.
        if synced_bases.insert(rel.clone(), hash.clone()) != Some(hash.clone()) {
            bases_changed = true;
            bases_inserts.insert(rel.clone(), hash.clone());
        }
    }
    // Files that exist nowhere anymore: their entries go too (no counts —
    // nothing visible happened to any file).
    for rel in &plan.base_prunes {
        if synced_bases.remove(rel).is_some() {
            bases_changed = true;
            bases_removals.push(rel.clone());
        }
    }

    // Decided at apply time from the delta itself — the same touched-key
    // semantics the plan's `rewrites_tracked` predicts (a whole-entry
    // compare would see disabled categories' entries as pending
    // rewrites forever). The files are already written: a record-save
    // failure must not turn the whole pull into an Err — pull_history
    // would skip the operation record and strand the snapshot, making
    // the overwritten files unrecoverable. Degrade to a warning
    // instead: a stale record costs one extra kept-local round, a lost
    // undo costs the files.
    let mut removals = Vec::new();
    let mut inserts = TrackedPaths::new();
    // Hash lookups: the retry lists can grow with the pull.
    let deletes_to_retry: std::collections::HashSet<&String> = deletes_to_retry.iter().collect();
    let creates_to_retry: std::collections::HashSet<&String> = creates_to_retry.iter().collect();
    for key in &plan.touched_tracked_keys {
        if deletes_to_retry.contains(key) || creates_to_retry.contains(key) {
            // The deletion (or creation) did not execute: the entry must
            // stay exactly as it was so the decision is retried, exactly
            // like the plan-time unreadable path re-arms it.
            continue;
        }
        match (
            plan.tracked_before.contains(key),
            plan.tracked_after.contains(key),
        ) {
            (false, true) => {
                inserts.insert(key.clone());
            }
            (true, false) => removals.push(key.clone()),
            _ => {}
        }
    }
    let tracked_save_gate = plan.tracks_deletions
        && ((!inserts.is_empty() || !removals.is_empty()) || plan.tracked_alias_present);
    let mut tracked_keys_written: Vec<String> = Vec::new();
    if tracked_save_gate {
        // The delta this plan OWNS — exactly the keys its scan decided
        // about — merged into whatever is on disk NOW in ONE locked unit
        // (see `tracked::save_delta`): a same-repo push that records
        // paths while this (possibly minutes-long interactive) apply ran
        // keeps them, and so do the entries of categories this pull did
        // not scan — their files still exist, and dropping them would
        // silently stop their deletion mirroring. The write skips
        // identical content, so a delta that changes nothing does not
        // churn the file.
        tracked_keys_written.extend(inserts.iter().cloned());
        tracked_keys_written.extend(removals.iter().cloned());
        match tracked::save_delta(&plan.claude_dir, &plan.repo_root, inserts, removals) {
            Ok(()) => {}
            Err(e) => {
                log::warn!("Tracked record NOT saved (files are already applied): {e}");
                // Same contract as the bases save below: a failed save moved
                // nothing, and reported keys would let a later undo "restore"
                // entries a concurrent writer legitimately recorded.
                tracked_keys_written.clear();
            }
        }
    }
    let mut bases_keys_written: Vec<String> = Vec::new();
    if bases_changed {
        // One locked load-merge-write (see `bases::save_delta`): a
        // concurrent same-repo push's entries are preserved, and the lock
        // covers the READ half too. Keys reported only on a successful
        // save, like the tracked save above: a failed one moved nothing,
        // and the undo would otherwise "restore" keys a later push
        // legitimately recorded.
        bases_keys_written.extend(bases_removals.iter().cloned());
        bases_keys_written.extend(bases_inserts.keys().cloned());
        // Protection entries are NOT pull state (see `protection_keys`).
        // HashSet lookup is O(n+m) — Vec::contains was O(n*m).
        let protection_set: std::collections::HashSet<&str> =
            protection_keys.iter().map(String::as_str).collect();
        bases_keys_written.retain(|key| !protection_set.contains(key.as_str()));
        match bases::save_delta(
            &plan.claude_dir,
            &plan.repo_root,
            bases_inserts,
            bases_removals,
        ) {
            Ok(()) => {}
            Err(e) => {
                log::warn!("Base record NOT saved (files are already applied): {e}");
                bases_keys_written.clear();
            }
        }
    }

    // What the caller's operation record needs to know: exactly the keys
    // this apply moved in each shared record (see the fields). Collected
    // from the deltas, not the plan — the plan's lists include no-ops a
    // later push must not have "restored".
    report.bases_keys_written = bases_keys_written;
    report.tracked_keys_written = tracked_keys_written;

    report.counts = by_category.into_values().collect();
    report.counts.sort_by_key(|c| c.category as usize);
    Ok(report)
}

/// Ask a yes/no question about one file: the shared prompt shape (file
/// name extraction, Confirm, default, help), so the arms cannot drift in
/// style. EOF or a rendering failure reads as the default-breaking answer.
fn confirm_file_action(prompt: &str, default: bool, help: &str) -> bool {
    inquire::Confirm::new(prompt)
        .with_default(default)
        .with_help_message(help)
        .prompt()
        .unwrap_or(false)
}

/// The display name of a file in prompts: its base name, or the whole
/// path when it has none. Shared with the merge-tool prompt — one
/// prompt-shape rule.
pub(crate) fn prompt_file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Ask before making a local file runnable, since a hook runs on its own.
fn confirm_executable(local_path: &Path) -> bool {
    confirm_file_action(
        &format!(
            "'{}' is executable on another machine. Make it executable here too?",
            prompt_file_name(local_path)
        ),
        true,
        "Declining leaves the file as it is",
    )
}

/// Ask before removing a local file the sync repo no longer has.
fn confirm_deletion(local_path: &Path) -> bool {
    confirm_file_action(
        &format!(
            "'{}' was deleted on another machine. Delete it here too?",
            prompt_file_name(local_path)
        ),
        true,
        "Declining keeps the file and holds THIS sync's artifacts back; \
         the NEXT sync's push republishes it (or run `claude-code-sync push` now)",
    )
}

/// Ask before dropping a file that was deleted in the repository but edited
/// here since the last sync — the one decision the protection defers to the
/// user. Default NO: keeping the edit is the whole point.
fn confirm_kept_local_deletion(local_path: &Path) -> bool {
    confirm_file_action(
        &format!(
            "'{}' was deleted on another machine, but edited here. Delete it (discarding the edit)?",
            prompt_file_name(local_path)
        ),
        false,
        "Declining keeps the local edit; a push is what publishes it. \
         Only this prompt can clear a kept-local deletion besides pushing. \
         (Files whose base got pruned mid-pull are held regardless — the \
         prompt can decline them but cannot drop them.)",
    )
}

/// Globs for the managed ignore block: defense-in-depth behind the code-level
/// denylist, in case files land in the repo by hand or via other tools.
const IGNORE_GLOBS: &[&str] = &[
    ".credentials.json",
    "settings.local.json",
    ".claude.json",
    "*.pem",
    "*.key",
    ".env*",
    "daemon*",
    "stats-cache.json",
    ".last-update-result.json",
    "mcp-needs-auth-cache.json",
    "shell-snapshots/",
    "session-env/",
    "file-history/",
    "paste-cache/",
    "statsig/",
    "backups/",
    "sessions/",
    "**/cache/",
    "**/debug/",
];

const IGNORE_BLOCK_START: &str = "# >>> claude-code-sync managed block — do not edit inside";
const IGNORE_BLOCK_END: &str = "# <<< claude-code-sync managed block";

/// Build the full managed block for one backend.
fn ignore_block(backend: Backend) -> String {
    let mut block = String::new();
    block.push_str(IGNORE_BLOCK_START);
    block.push('\n');
    if backend == Backend::Mercurial {
        block.push_str("syntax: glob\n");
    }
    for glob in IGNORE_GLOBS {
        block.push_str(glob);
        block.push('\n');
    }
    block.push_str(IGNORE_BLOCK_END);
    block.push('\n');
    block
}

/// Write the managed never-sync ignore block into the sync repository's
/// ignore file for the given backend. Idempotent; preserves user content
/// outside the block. Returns whether the file changed.
pub fn ensure_ignore_files(repo_root: &Path, backend: Backend) -> Result<bool> {
    let file_name = match backend {
        Backend::Git => ".gitignore",
        Backend::Mercurial => ".hgignore",
    };
    let path = repo_root.join(file_name);
    let existing = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };

    let block = ignore_block(backend);

    let updated = if let (Some(start), Some(end)) = (
        existing.find(IGNORE_BLOCK_START),
        existing.find(IGNORE_BLOCK_END),
    ) {
        // Replace the existing block in place.
        let end = end + IGNORE_BLOCK_END.len();
        // Include the trailing newline of the old block if present.
        let end = if existing[end..].starts_with('\n') {
            end + 1
        } else {
            end
        };
        format!("{}{}{}", &existing[..start], block, &existing[end..])
    } else if existing.is_empty() {
        block
    } else {
        let sep = if existing.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        format!("{existing}{sep}{block}")
    };

    if updated == existing {
        return Ok(false);
    }
    fs::write(&path, updated)?;
    Ok(true)
}

/// Remove a list of repo-relative paths from the per-machine tracked record.
///
/// The 5ff1d62 push guard (engine.rs:711-759) refuses to re-publish a file
/// the remote lost when the file is in the tracked record. The user-facing
/// escape hatch `claude-code-sync push --resurrect <path>` calls this helper
/// to drop the path from the record first; the next push then sees no entry
/// and publishes the file as a fresh `Added`.
///
/// Defense in depth: returns `Err` if any path is NOT in the current tracked
/// record. The CLI flag should have validated against the held-back set, but
/// if it didn't, refuse rather than silently no-op. The exception is the
/// initial install case (record empty): an empty tracked record is exactly
/// the state this helper is meant to set, so a no-op is correct there.
pub fn prepare_resurrection(claude_dir: &Path, repo_root: &Path, paths: &[String]) -> Result<()> {
    let current = tracked::load(claude_dir, repo_root);
    let unknown: Vec<&str> = paths
        .iter()
        .filter(|p| !current.contains(*p))
        .map(|p| p.as_str())
        .collect();
    if !unknown.is_empty() {
        anyhow::bail!(
            "cannot resurrect paths that are not in the tracked record: {}",
            unknown.join(", ")
        );
    }
    if paths.is_empty() {
        return Ok(());
    }
    // `save_delta` is a locked load-merge-write: a concurrent same-repo
    // pull that records entries while this runs keeps them. The empty
    // insert set is the no-op case for the inserts half; only the removals
    // half matters.
    let empty: TrackedPaths = TrackedPaths::new();
    tracked::save_delta(claude_dir, repo_root, empty, paths.to_vec())
}

/// Plan which files the 5ff1d62 push guard would refuse for a given filter,
/// without performing any writes. Drives `status --held-back` without
/// running a full push (which is the only place the guard normally fires).
///
/// Returns `(category, repo-relative-path)` pairs in push-iteration order.
/// The category is the one whose push would have refused the file.
pub fn plan_held_back_remote_lost(
    claude_dir: &Path,
    repo_root: &Path,
    filter: &FilterConfig,
) -> Result<Vec<(CategoryId, PathBuf)>> {
    let mut held_back: Vec<(CategoryId, PathBuf)> = Vec::new();
    let tracked_seen = tracked::load(claude_dir, repo_root);
    for desc in active_categories(filter) {
        let category_root = category_repo_root(desc, repo_root, filter);
        let mut counts = CategoryCounts::new(desc.id);
        let mut held_back_local: Vec<PathBuf> = Vec::new();
        let files = collect(
            desc,
            claude_dir,
            filter,
            &mut counts.skipped,
            &mut held_back_local,
        )?;
        for file in files {
            let dest = category_root.join(&file.rel);
            // Mirror the push guard's "is the remote missing this file" test.
            if !dest.is_file() {
                if let Some(rel) = repo_relative(repo_root, &dest) {
                    if tracked_seen.contains(&rel) {
                        held_back.push((desc.id, PathBuf::from(rel)));
                    }
                }
            }
        }
    }
    Ok(held_back)
}
