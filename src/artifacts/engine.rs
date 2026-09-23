//! The artifact copy engine: registry-driven push/pull between `~/.claude`
//! and the sync repository. Every function takes explicit paths — the
//! `~/.claude` default is resolved by callers in `crate::sync` — so tests run
//! against temp directories with no environment coupling.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::filter::FilterConfig;
use crate::scm::Backend;

use super::denylist::{is_denied, is_unsafe_rel_path};
use super::memory_index::{is_memory_index, merge_memory_index};
use super::registry::{
    CategoryDescriptor, CategoryId, DestRoot, MergeStrategy, SourceSpec, ARTIFACTS_SUBDIR, REGISTRY,
};
use super::tokens::PathTokens;
use super::tracked::{self, TrackedPaths};
use super::union_jsonl::merge_history_lines;

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
        }
    }
}

/// How one artifact file changed during a push or pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactChangeKind {
    Added,
    Modified,
    Deleted,
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

    fn record(&mut self, category: CategoryId, kind: ArtifactChangeKind, path: &Path) {
        self.changes.push(ArtifactChange {
            category,
            kind,
            path: path.to_path_buf(),
        });
    }

    /// True when nothing was copied, merged, or even inspected.
    #[allow(dead_code)] // used via the library target; the bin compiles this module separately
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
            || self
                .counts
                .iter()
                .all(|c| c.added + c.modified + c.unchanged + c.skipped == 0)
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
fn collect(
    desc: &CategoryDescriptor,
    claude_dir: &Path,
    filter: &FilterConfig,
    skipped: &mut usize,
) -> Result<Vec<CollectedFile>> {
    let max_file_size = filter.max_file_size_bytes;
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
                if entry.metadata().map(|m| m.len()).unwrap_or(0) > max_file_size {
                    log::warn!("Skipping {} (exceeds max_file_size_bytes)", abs.display());
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
                    let project = crate::project_map::repo_dir_name(filter, encoded);
                    rel = Path::new(&project).join(parts.as_path());
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
    #[cfg(unix)]
    {
        if let Some(mode) = get_mode_for_copy(path, mode_source) {
            set_mode(tmp.path(), mode);
        }
    }
    tmp.persist(path)
        .with_context(|| format!("Failed to persist {}", path.display()))?;
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
) -> Result<ArtifactReport> {
    let mut report = ArtifactReport::default();
    let tokens = PathTokens::for_claude_dir(claude_dir);
    let tracked_before = tracked::load(claude_dir, repo_root);
    let mut tracked_now = TrackedPaths::new();

    for desc in active_categories(filter) {
        let mut counts = CategoryCounts::new(desc.id);

        let files = collect(desc, claude_dir, filter, &mut counts.skipped)?;
        let category_root = category_repo_root(desc, repo_root, filter);
        let mut pushed: TrackedPaths = TrackedPaths::new();

        for file in files {
            let dest = category_root.join(&file.rel);

            match desc.merge {
                MergeStrategy::UnionJsonl => {
                    let local_text = fs::read_to_string(&file.abs).unwrap_or_default();
                    let existed = dest.is_file();
                    let repo_text = if existed {
                        fs::read_to_string(&dest).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let (merged, new_lines) = merge_history_lines(&repo_text, &local_text);
                    counts.merged_entries += new_lines;
                    if !existed {
                        write_atomic(&dest, merged.as_bytes(), &file.abs)?;
                        counts.added += 1;
                        report.record(desc.id, ArtifactChangeKind::Added, &file.rel);
                    } else if merged != repo_text {
                        write_atomic(&dest, merged.as_bytes(), &file.abs)?;
                        counts.modified += 1;
                        report.record(desc.id, ArtifactChangeKind::Modified, &file.rel);
                    } else {
                        counts.unchanged += 1;
                    }
                }
                MergeStrategy::UnionMemoryIndex | MergeStrategy::RawOverwrite => {
                    let read_bytes = fs::read(&file.abs).with_context(|| {
                        format!("Failed to read artifact {}", file.abs.display())
                    })?;
                    let mut src_bytes = if desc.tokenize_paths {
                        tokens.to_repo(&read_bytes)
                    } else {
                        read_bytes
                    };
                    let existed = dest.is_file();
                    let unions_index =
                        desc.merge == MergeStrategy::UnionMemoryIndex && is_memory_index(&file.rel);
                    if existed && unions_index {
                        let (merged, new_entries) =
                            merge_memory_index(&fs::read(&dest)?, &src_bytes);
                        counts.merged_entries += new_entries;
                        src_bytes = merged;
                    }

                    if !existed {
                        write_atomic(&dest, &src_bytes, &file.abs)?;
                        counts.added += 1;
                        report.record(desc.id, ArtifactChangeKind::Added, &file.rel);
                    } else if fs::read(&dest)? != src_bytes {
                        write_atomic(&dest, &src_bytes, &file.abs)?;
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

            if let Some(rel) = repo_relative(repo_root, &dest) {
                pushed.insert(rel);
            }
        }

        if desc.mirror_deletes {
            if source_is_present(desc, claude_dir) {
                mark_category_synced(&category_root)?;
                let removed =
                    remove_from_repo(&tracked_before, &pushed, &category_root, repo_root)?;
                counts.deleted += removed.len();
                for path in &removed {
                    report.record(desc.id, ArtifactChangeKind::Deleted, path);
                }
                tracked_now.extend(pushed);
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
        }

        report.counts.push(counts);
    }

    if active_categories(filter).any(|desc| desc.mirror_deletes) {
        tracked::save(claude_dir, repo_root, tracked_now)?;
    }
    Ok(report)
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
/// has. Returns the removed paths, relative to the category root.
fn remove_from_repo(
    tracked_before: &TrackedPaths,
    pushed: &TrackedPaths,
    category_root: &Path,
    repo_root: &Path,
) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    let category_prefix = repo_relative(repo_root, category_root).unwrap_or_default();
    for gone in tracked_under(tracked_before, category_root, repo_root) {
        if pushed.contains(&gone) {
            continue;
        }
        let path = repo_root.join(&gone);
        if path.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("Failed to remove {}", path.display()))?;
            let category_relative = Path::new(&gone)
                .strip_prefix(&category_prefix)
                .map(Path::to_path_buf)
                .unwrap_or_default();
            removed.push(category_relative);
        }
    }
    Ok(removed)
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
    /// No local file yet: created, and recorded for deletion on undo.
    pub creates: Vec<PlannedWrite>,
    /// Union-merge targets whose local file would gain lines.
    pub unions: Vec<PlannedWrite>,
    /// Local files whose content already matches but whose executable bit does
    /// not: a `chmod +x` elsewhere, with nothing to rewrite.
    pub mode_fixes: Vec<PlannedWrite>,
    /// Local files this machine synced before that the repo no longer has.
    pub deletes: Vec<PlannedDelete>,
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
    /// The repo paths this machine will hold afterwards, for the next pull to
    /// tell a deletion from a file it never had.
    pub tracked_after: TrackedPaths,
    /// Whether any active category mirrors deletions. When none does, the
    /// record is left untouched rather than emptied.
    pub tracks_deletions: bool,
}

/// One planned local deletion during a pull.
#[derive(Debug, Clone)]
pub struct PlannedDelete {
    pub category: CategoryId,
    /// Absolute path under `~/.claude` to remove.
    pub local_path: PathBuf,
    /// Path relative to the category's root, as reported to the user.
    pub category_path: PathBuf,
}

impl PullPlan {
    /// True when applying the plan would write nothing.
    pub fn is_empty(&self) -> bool {
        self.overwrites.is_empty()
            && self.creates.is_empty()
            && self.unions.is_empty()
            && self.mode_fixes.is_empty()
            && self.deletes.is_empty()
    }

    /// Existing local files the caller must snapshot before applying
    /// (overwritten raw files, union-merged files and deletions — a snapshot is
    /// what makes `undo pull` able to bring a deleted file back).
    ///
    /// The record of what this machine last synced is included whenever the
    /// pull would rewrite it, so undoing a pull restores the files *and* the
    /// record that describes them.
    pub fn paths_to_snapshot(&self) -> Vec<PathBuf> {
        let record = tracked::record_path(&self.claude_dir);
        self.overwrites
            .iter()
            .chain(self.unions.iter())
            .map(|w| w.local_path.clone())
            .chain(self.deletes.iter().map(|d| d.local_path.clone()))
            .chain((self.tracks_deletions && record.is_file()).then_some(record))
            .collect()
    }

    /// Local paths this pull will create; recording them as a snapshot's
    /// `deleted_files` makes undo remove them again.
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
        tracks_deletions: active_categories(filter).any(|desc| desc.mirror_deletes),
        ..Default::default()
    };
    let tracked_before = tracked::load(claude_dir, repo_root);

    for desc in active_categories(filter) {
        let category_root = category_repo_root(desc, repo_root, filter);
        let mut present: TrackedPaths = TrackedPaths::new();

        for (repo_path, rel) in collect_repo_files(desc, repo_root, filter, &mut plan.skipped) {
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
                    let local_text = fs::read_to_string(&local_path).unwrap_or_default();
                    let repo_text = fs::read_to_string(&repo_path).unwrap_or_default();
                    let (merged, _) = merge_history_lines(&local_text, &repo_text);
                    if merged != local_text {
                        plan.unions.push(write);
                    } else {
                        plan.unchanged += 1;
                    }
                }
                MergeStrategy::UnionMemoryIndex if is_memory_index(&rel) => {
                    if !local_path.is_file() {
                        plan.creates.push(write);
                        continue;
                    }
                    let local_bytes = fs::read(&local_path)?;
                    let repo_bytes = machine_bytes(desc, &plan.tokens, &repo_path)?;
                    let (merged, _) = merge_memory_index(&local_bytes, &repo_bytes);
                    if merged != local_bytes {
                        plan.unions.push(write);
                    } else {
                        plan.unchanged += 1;
                    }
                }
                MergeStrategy::UnionMemoryIndex | MergeStrategy::RawOverwrite => {
                    if !local_path.is_file() {
                        plan.creates.push(write);
                    } else if fs::read(&local_path)?
                        != machine_bytes(desc, &plan.tokens, &repo_path)?
                    {
                        plan.overwrites.push(write);
                    } else {
                        let missing_executable_bit =
                            executable_bit_differs(&local_path, &repo_path);
                        if missing_executable_bit {
                            plan.mode_fixes.push(write);
                        } else {
                            plan.unchanged += 1;
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

        for gone in tracked_under(&tracked_before, &category_root, repo_root) {
            if present.contains(&gone) {
                continue;
            }
            let rel = Path::new(&gone)
                .strip_prefix(repo_relative(repo_root, &category_root).unwrap_or_default())
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
                plan.deletes.push(PlannedDelete {
                    category: desc.id,
                    local_path,
                    category_path: rel,
                });
            }
        }
        plan.tracked_after.extend(present);
    }

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

    let mut report = ArtifactReport::default();
    let prompt_overwrites = interactive && crate::interactive_conflict::is_interactive();

    for write in &plan.creates {
        let bytes = machine_bytes(descriptor(write.category), &plan.tokens, &write.repo_path)?;
        write_atomic(&write.local_path, &bytes, &write.repo_path)?;
        counts_for(&mut by_category, write.category).added += 1;
        report.record(
            write.category,
            ArtifactChangeKind::Added,
            &write.category_path,
        );
    }

    for write in &plan.overwrites {
        let bytes = machine_bytes(descriptor(write.category), &plan.tokens, &write.repo_path)?;
        let bytes = if prompt_overwrites {
            match crate::merge_tool::resolve_overwrite(&plan.merge_tool, &write.local_path, &bytes)?
            {
                Some(resolved) => resolved,
                None => {
                    counts_for(&mut by_category, write.category).skipped += 1;
                    continue;
                }
            }
        } else {
            bytes
        };
        write_atomic(&write.local_path, &bytes, &write.repo_path)?;
        counts_for(&mut by_category, write.category).modified += 1;
        report.record(
            write.category,
            ArtifactChangeKind::Modified,
            &write.category_path,
        );
    }

    for write in &plan.unions {
        let desc = descriptor(write.category);
        let repo_bytes = machine_bytes(desc, &plan.tokens, &write.repo_path)?;
        let local_bytes = fs::read(&write.local_path).unwrap_or_default();
        let (merged, new_entries) = match desc.merge {
            MergeStrategy::UnionMemoryIndex => merge_memory_index(&local_bytes, &repo_bytes),
            _ => {
                let (text, lines) = merge_history_lines(
                    &String::from_utf8_lossy(&local_bytes),
                    &String::from_utf8_lossy(&repo_bytes),
                );
                (text.into_bytes(), lines)
            }
        };
        write_atomic(&write.local_path, &merged, &write.repo_path)?;
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

    for delete in &plan.deletes {
        if prompt_overwrites && !confirm_deletion(&delete.local_path) {
            counts_for(&mut by_category, delete.category).skipped += 1;
            continue;
        }
        if delete.local_path.is_file() {
            fs::remove_file(&delete.local_path)
                .with_context(|| format!("Failed to remove {}", delete.local_path.display()))?;
        }
        counts_for(&mut by_category, delete.category).deleted += 1;
        report.record(
            delete.category,
            ArtifactChangeKind::Deleted,
            &delete.category_path,
        );
    }

    if plan.tracks_deletions {
        tracked::save(
            &plan.claude_dir,
            &plan.repo_root,
            plan.tracked_after.clone(),
        )?;
    }

    report.counts = by_category.into_values().collect();
    report.counts.sort_by_key(|c| c.category as usize);
    Ok(report)
}

/// Ask before making a local file runnable, since a hook runs on its own.
fn confirm_executable(local_path: &Path) -> bool {
    let file_name = local_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| local_path.display().to_string());
    inquire::Confirm::new(&format!(
        "'{file_name}' is executable on another machine. Make it executable here too?"
    ))
    .with_default(true)
    .with_help_message("Declining leaves the file as it is")
    .prompt()
    .unwrap_or(false)
}

/// Ask before removing a local file the sync repo no longer has.
fn confirm_deletion(local_path: &Path) -> bool {
    let file_name = local_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| local_path.display().to_string());
    inquire::Confirm::new(&format!(
        "'{file_name}' was deleted on another machine. Delete it here too?"
    ))
    .with_default(true)
    .with_help_message("Declining keeps the local file; it is pushed back on the next push")
    .prompt()
    .unwrap_or(false)
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
