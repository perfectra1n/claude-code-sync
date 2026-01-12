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

/// Extract project name from Claude's encoded project directory name.
///
/// Claude encodes project paths by replacing '/' with '-', so a project at
/// `/Users/abc/Documents/GitHub/myproject` becomes `-Users-abc-Documents-GitHub-myproject`.
/// This function extracts the last segment (the actual project name).
///
/// # Examples
/// - Input: `-Users-abc-Documents-GitHub-myproject` -> Output: `myproject`
/// - Input: `myproject` -> Output: `myproject`
/// - Input: `-root-projects-test` -> Output: `test`
pub fn extract_project_name(encoded_path: &str) -> &str {
    // The encoded path uses '-' as separator (from path encoding)
    // Take the last non-empty segment
    encoded_path
        .rsplit('-')
        .find(|s| !s.is_empty())
        .unwrap_or(encoded_path)
}

/// Find a local Claude project directory that ends with the given project name.
///
/// Scans `~/.claude/projects/` for directories whose encoded name ends with
/// the specified project name. Returns the path if exactly one match is found.
///
/// # Returns
/// - `Some(PathBuf)` if exactly one matching project directory is found
/// - `None` if no match found or multiple matches (ambiguous)
pub fn find_local_project_by_name(claude_projects_dir: &Path, project_name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(claude_projects_dir).ok()?;

    let matches: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|name| extract_project_name(name) == project_name)
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();

    // Return only if exactly one match to avoid ambiguity
    if matches.len() == 1 {
        Some(matches.into_iter().next().unwrap())
    } else {
        None
    }
}

/// Get all project directories in Claude's projects folder that would map to the same project name.
/// Used for collision detection when `use_project_name_only` is enabled.
pub fn find_colliding_projects(
    claude_projects_dir: &Path,
) -> std::collections::HashMap<String, Vec<PathBuf>> {
    use std::collections::HashMap;

    let mut collisions: HashMap<String, Vec<PathBuf>> = HashMap::new();

    if let Ok(entries) = std::fs::read_dir(claude_projects_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
                    let project_name = extract_project_name(dir_name).to_string();
                    collisions.entry(project_name).or_default().push(path);
                }
            }
        }
    }

    // Only keep entries with more than one project (actual collisions)
    collisions.retain(|_, paths| paths.len() > 1);
    collisions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_extract_project_name_basic() {
        // Standard encoded path
        assert_eq!(
            extract_project_name("-Users-abc-Documents-GitHub-myproject"),
            "myproject"
        );
    }

    #[test]
    fn test_extract_project_name_simple() {
        // Already just a project name
        assert_eq!(extract_project_name("myproject"), "myproject");
    }

    #[test]
    fn test_extract_project_name_short_path() {
        // Short encoded path
        assert_eq!(extract_project_name("-root-project"), "project");
    }

    #[test]
    fn test_extract_project_name_empty() {
        // Empty string edge case
        assert_eq!(extract_project_name(""), "");
    }

    #[test]
    fn test_extract_project_name_single_segment() {
        // Path with trailing dash
        assert_eq!(extract_project_name("-myproject"), "myproject");
    }

    #[test]
    fn test_find_local_project_by_name_single_match() {
        let temp_dir = tempdir().unwrap();
        let projects_dir = temp_dir.path();

        // Create a project directory
        fs::create_dir(projects_dir.join("-Users-abc-Documents-myproject")).unwrap();

        let result = find_local_project_by_name(projects_dir, "myproject");
        assert!(result.is_some());
        assert!(result.unwrap().ends_with("-Users-abc-Documents-myproject"));
    }

    #[test]
    fn test_find_local_project_by_name_no_match() {
        let temp_dir = tempdir().unwrap();
        let projects_dir = temp_dir.path();

        // Create a project directory with different name
        fs::create_dir(projects_dir.join("-Users-abc-Documents-otherproject")).unwrap();

        let result = find_local_project_by_name(projects_dir, "myproject");
        assert!(result.is_none());
    }

    #[test]
    fn test_find_local_project_by_name_multiple_matches() {
        let temp_dir = tempdir().unwrap();
        let projects_dir = temp_dir.path();

        // Create two project directories with same project name
        fs::create_dir(projects_dir.join("-Users-abc-work-myproject")).unwrap();
        fs::create_dir(projects_dir.join("-Users-abc-personal-myproject")).unwrap();

        // Should return None for ambiguous matches
        let result = find_local_project_by_name(projects_dir, "myproject");
        assert!(result.is_none());
    }

    #[test]
    fn test_find_colliding_projects_no_collisions() {
        let temp_dir = tempdir().unwrap();
        let projects_dir = temp_dir.path();

        // Create directories with unique project names
        fs::create_dir(projects_dir.join("-Users-abc-project1")).unwrap();
        fs::create_dir(projects_dir.join("-Users-abc-project2")).unwrap();

        let collisions = find_colliding_projects(projects_dir);
        assert!(collisions.is_empty());
    }

    #[test]
    fn test_find_colliding_projects_with_collisions() {
        let temp_dir = tempdir().unwrap();
        let projects_dir = temp_dir.path();

        // Create directories that map to the same project name
        fs::create_dir(projects_dir.join("-Users-abc-work-myapp")).unwrap();
        fs::create_dir(projects_dir.join("-Users-abc-personal-myapp")).unwrap();
        fs::create_dir(projects_dir.join("-Users-abc-unique")).unwrap();

        let collisions = find_colliding_projects(projects_dir);
        assert_eq!(collisions.len(), 1);
        assert!(collisions.contains_key("myapp"));
        assert_eq!(collisions.get("myapp").unwrap().len(), 2);
    }
}
