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

use super::denylist::is_denied;
use super::registry::{
    enabled_categories, CategoryDescriptor, CategoryId, MergeStrategy, SourceSpec,
    ARTIFACTS_SUBDIR,
};
use super::union_jsonl::merge_history_lines;

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
    max_file_size: u64,
    skipped: &mut usize,
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
                let rel = abs.strip_prefix(&base).unwrap_or(abs).to_path_buf();
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
    let artifacts_root = repo_root.join(ARTIFACTS_SUBDIR);
    let mut report = ArtifactReport::default();

    for desc in enabled_categories(&filter.sync_artifacts) {
        let mut counts = CategoryCounts {
            category: desc.id,
            added: 0,
            modified: 0,
            unchanged: 0,
            skipped: 0,
            merged_entries: 0,
        };

        let files = collect(desc, claude_dir, filter.max_file_size_bytes, &mut counts.skipped)?;
        let category_root = artifacts_root.join(desc.repo_subdir);

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
