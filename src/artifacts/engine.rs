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
use super::registry::{
    CategoryDescriptor, CategoryId, DestRoot, MergeStrategy, SourceSpec, ARTIFACTS_SUBDIR,
    REGISTRY,
};
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
fn active_categories(filter: &FilterConfig) -> impl Iterator<Item = &'static CategoryDescriptor> + '_ {
    REGISTRY.iter().filter(|d| is_category_enabled(d, filter))
}

/// The sync-repo root directory for one category.
fn category_repo_root(desc: &CategoryDescriptor, repo_root: &Path, filter: &FilterConfig) -> PathBuf {
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
}

/// Outcome of one artifact push or pull across all enabled categories.
#[derive(Debug, Clone, Default)]
pub struct ArtifactReport {
    pub counts: Vec<CategoryCounts>,
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
    /// True when nothing was copied, merged, or even inspected.
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
                    log::warn!(
                        "Skipping {} (exceeds max_file_size_bytes)",
                        abs.display()
                    );
                    *skipped += 1;
                    continue;
                }
                if extension_excluded(desc, abs) {
                    continue;
                }
                let mut rel = abs.strip_prefix(&base).unwrap_or(abs).to_path_buf();
                // Attachments in name-only mode collapse the encoded project
                // dir to the bare project name, mirroring session layout.
                if desc.dest == DestRoot::SessionTree && filter.use_project_name_only {
                    let mut parts = rel.components();
                    let Some(encoded) = parts.next().and_then(|c| c.as_os_str().to_str()) else {
                        *skipped += 1;
                        continue;
                    };
                    let project = crate::sync::discovery::extract_project_name(encoded);
                    rel = Path::new(project).join(parts.as_path());
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
/// so a reader (or a crash) never sees a half-written file.
fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("No parent directory for {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    fs::write(tmp.path(), content)?;
    tmp.persist(path)
        .with_context(|| format!("Failed to persist {}", path.display()))?;
    Ok(())
}

/// Copy every enabled artifact category into `<repo_root>/artifacts/`,
/// classifying each file Added/Modified/Unchanged by byte comparison.
/// Prompt history is union-merged into the repo copy instead of overwritten.
pub fn push_artifacts(
    claude_dir: &Path,
    repo_root: &Path,
    filter: &FilterConfig,
) -> Result<ArtifactReport> {
    let mut report = ArtifactReport::default();

    for desc in active_categories(filter) {
        let mut counts = CategoryCounts {
            category: desc.id,
            added: 0,
            modified: 0,
            unchanged: 0,
            skipped: 0,
            merged_entries: 0,
        };

        let files = collect(desc, claude_dir, filter, &mut counts.skipped)?;
        let category_root = category_repo_root(desc, repo_root, filter);

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
                        write_atomic(&dest, merged.as_bytes())?;
                        counts.added += 1;
                    } else if merged != repo_text {
                        write_atomic(&dest, merged.as_bytes())?;
                        counts.modified += 1;
                    } else {
                        counts.unchanged += 1;
                    }
                }
                MergeStrategy::RawOverwrite => {
                    let src_bytes = fs::read(&file.abs).with_context(|| {
                        format!("Failed to read artifact {}", file.abs.display())
                    })?;
                    if !dest.is_file() {
                        write_atomic(&dest, &src_bytes)?;
                        counts.added += 1;
                    } else if fs::read(&dest)? != src_bytes {
                        write_atomic(&dest, &src_bytes)?;
                        counts.modified += 1;
                    } else {
                        counts.unchanged += 1;
                    }
                }
            }
        }

        report.counts.push(counts);
    }

    Ok(report)
}

/// One planned local write during a pull.
#[derive(Debug, Clone)]
pub struct PlannedWrite {
    pub category: CategoryId,
    /// Absolute destination under `~/.claude`.
    pub local_path: PathBuf,
    /// Absolute source inside the sync repository.
    pub repo_path: PathBuf,
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
    pub unchanged: usize,
    /// Repo files refused (denied names, unsafe paths).
    pub skipped: usize,
}

impl PullPlan {
    /// True when applying the plan would write nothing.
    pub fn is_empty(&self) -> bool {
        self.overwrites.is_empty() && self.creates.is_empty() && self.unions.is_empty()
    }

    /// Existing local files the caller must snapshot before applying
    /// (overwritten raw files and union-merged files).
    pub fn paths_to_snapshot(&self) -> Vec<PathBuf> {
        self.overwrites
            .iter()
            .chain(self.unions.iter())
            .map(|w| w.local_path.clone())
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

/// Map a category-relative repo path back to its absolute local destination
/// under `~/.claude`. Returns None when no destination can be determined
/// (name-only attachments whose project has no unambiguous local match).
fn local_destination(
    desc: &CategoryDescriptor,
    claude_dir: &Path,
    rel: &Path,
    filter: &FilterConfig,
) -> Option<PathBuf> {
    match desc.source {
        // File lists are stored flat in the repo; restore to the listed
        // location whose file name matches. An unlisted name has NO valid
        // destination — the allowlist must hold on pull as well as push, or a
        // poisoned repo could plant arbitrary files at the top of ~/.claude.
        SourceSpec::Files(list) => list
            .iter()
            .find(|entry| Path::new(entry).file_name() == rel.file_name())
            .map(|entry| claude_dir.join(entry)),
        SourceSpec::Dir(dir) => {
            if desc.dest == DestRoot::SessionTree && filter.use_project_name_only {
                // Repo path is <project-name>/<rest>; find the matching local
                // encoded project dir the way session pull does.
                let mut parts = rel.components();
                let name = parts.next()?.as_os_str().to_str()?.to_string();
                let projects_dir = claude_dir.join(dir);
                let local_project = crate::sync::discovery::find_local_project_by_name(
                    &projects_dir,
                    &name,
                )?;
                return Some(local_project.join(parts.as_path()));
            }
            Some(claude_dir.join(dir).join(rel))
        }
    }
}

/// Classify what a pull would write, without writing. Remote (repo) bytes win
/// for raw categories; union targets are compared against local ∪ remote.
pub fn plan_pull(
    claude_dir: &Path,
    repo_root: &Path,
    filter: &FilterConfig,
) -> Result<PullPlan> {
    let mut plan = PullPlan::default();

    for desc in active_categories(filter) {
        for (repo_path, rel) in collect_repo_files(desc, repo_root, filter, &mut plan.skipped) {
            let Some(local_path) = local_destination(desc, claude_dir, &rel, filter) else {
                log::warn!(
                    "Skipping {} (no unambiguous local project for name-only mode)",
                    repo_path.display()
                );
                plan.skipped += 1;
                continue;
            };
            let write = PlannedWrite {
                category: desc.id,
                local_path: local_path.clone(),
                repo_path: repo_path.clone(),
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
                MergeStrategy::RawOverwrite => {
                    if !local_path.is_file() {
                        plan.creates.push(write);
                    } else if fs::read(&local_path)? != fs::read(&repo_path)? {
                        plan.overwrites.push(write);
                    } else {
                        plan.unchanged += 1;
                    }
                }
            }
        }
    }

    Ok(plan)
}

/// Apply a pull plan: create missing files, overwrite differing ones
/// (remote wins), and union-merge prompt history. Under `interactive` in a
/// terminal, each overwrite asks for per-file confirmation; declined files
/// count as skipped.
pub fn apply_pull(plan: &PullPlan, interactive: bool) -> Result<ArtifactReport> {
    use std::collections::HashMap;

    let mut by_category: HashMap<CategoryId, CategoryCounts> = HashMap::new();
    fn counts_for(
        map: &mut HashMap<CategoryId, CategoryCounts>,
        id: CategoryId,
    ) -> &mut CategoryCounts {
        map.entry(id).or_insert_with(move || CategoryCounts {
            category: id,
            added: 0,
            modified: 0,
            unchanged: 0,
            skipped: 0,
            merged_entries: 0,
        })
    }

    let prompt_overwrites = interactive && crate::interactive_conflict::is_interactive();

    for write in &plan.creates {
        let bytes = fs::read(&write.repo_path)?;
        write_atomic(&write.local_path, &bytes)?;
        counts_for(&mut by_category, write.category).added += 1;
    }

    for write in &plan.overwrites {
        if prompt_overwrites {
            let file_name = write
                .local_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| write.local_path.display().to_string());
            let confirmed = inquire::Confirm::new(&format!(
                "Overwrite local '{file_name}' with the sync repo version?"
            ))
            .with_default(true)
            .with_help_message("Declined files keep their local content")
            .prompt()
            .unwrap_or(false);
            if !confirmed {
                counts_for(&mut by_category, write.category).skipped += 1;
                continue;
            }
        }
        let bytes = fs::read(&write.repo_path)?;
        write_atomic(&write.local_path, &bytes)?;
        counts_for(&mut by_category, write.category).modified += 1;
    }

    for write in &plan.unions {
        let local_text = fs::read_to_string(&write.local_path).unwrap_or_default();
        let repo_text = fs::read_to_string(&write.repo_path).unwrap_or_default();
        let (merged, new_lines) = merge_history_lines(&local_text, &repo_text);
        write_atomic(&write.local_path, merged.as_bytes())?;
        let counts = counts_for(&mut by_category, write.category);
        counts.modified += 1;
        counts.merged_entries += new_lines;
    }

    let mut counts: Vec<CategoryCounts> = by_category.into_values().collect();
    counts.sort_by_key(|c| c.category as usize);
    Ok(ArtifactReport { counts })
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
        let end = if existing[end..].starts_with('\n') { end + 1 } else { end };
        format!("{}{}{}", &existing[..start], block, &existing[end..])
    } else if existing.is_empty() {
        block
    } else {
        let sep = if existing.ends_with('\n') { "\n" } else { "\n\n" };
        format!("{existing}{sep}{block}")
    };

    if updated == existing {
        return Ok(false);
    }
    fs::write(&path, updated)?;
    Ok(true)
}
