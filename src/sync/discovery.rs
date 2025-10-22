use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::filter::FilterConfig;
use crate::parser::ConversationSession;

/// Threshold for warning about large conversation files (10 MB)
pub(crate) const LARGE_FILE_WARNING_THRESHOLD: u64 = 10 * 1024 * 1024;

/// Get the Claude Code projects directory
pub(crate) fn claude_projects_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to get home directory")?;
    Ok(home.join(".claude").join("projects"))
}

/// Discover all conversation sessions in Claude Code history
pub(crate) fn discover_sessions(
    base_path: &Path,
    filter: &FilterConfig,
) -> Result<Vec<ConversationSession>> {
    let mut sessions = Vec::new();

    for entry in WalkDir::new(base_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            if !filter.should_include(path) {
                continue;
            }

            match ConversationSession::from_file(path) {
                Ok(session) => sessions.push(session),
                Err(e) => {
                    log::warn!("Failed to parse {}: {}", path.display(), e);
                }
            }
        }
    }

    Ok(sessions)
}

/// Check for large conversation files and emit warnings
///
/// This helps users identify conversations that may be bloated with excessive
/// file history, token usage, or other data. Large conversations can slow down
/// sync operations and consume significant disk space.
///
/// # Arguments
/// * `file_paths` - Iterator of file paths to check
pub(crate) fn warn_large_files<P, I>(file_paths: I)
where
    P: AsRef<Path>,
    I: IntoIterator<Item = P>,
{
    for path in file_paths {
        let path = path.as_ref();

        if let Ok(metadata) = fs::metadata(path) {
            let size = metadata.len();

            if size >= LARGE_FILE_WARNING_THRESHOLD {
                let size_mb = size as f64 / (1024.0 * 1024.0);
                println!(
                    "  {} Large conversation file detected: {} ({:.1} MB)",
                    "⚠️ ".yellow().bold(),
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown"),
                    size_mb
                );
                println!(
                    "     {}",
                    "Consider archiving or cleaning up this conversation to improve sync performance"
                        .dimmed()
                );
            }
        }
    }
}
