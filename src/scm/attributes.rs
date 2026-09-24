//! `.gitattributes` in the sync repository.
//!
//! Every machine only ever appends to a transcript, to the prompt history and
//! to a memory index. Git cannot know that: when two machines both appended
//! between syncs, it reports a content conflict at the end of the file and the
//! pull stops with nothing merged. Git's own `union` merge driver keeps both
//! sides' lines instead, and claude-code-sync deduplicates and reorders them
//! on the next sync (see `artifacts::union_jsonl`).

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

const SYNC_ATTRIBUTE_RULES: &[&str] = &[
    "* text=auto eol=lf",
    "*.pdf binary",
    "*.bat text eol=crlf",
    "*.cmd text eol=crlf",
    "*.jsonl merge=union",
    "MEMORY.md merge=union",
];

/// Write the sync rules into the repository's `.gitattributes`.
///
/// Returns whether anything was added, so a caller can commit only a real
/// change.
pub fn ensure_sync_attributes(repo_path: &Path) -> Result<bool> {
    let rules: Vec<String> = SYNC_ATTRIBUTE_RULES.iter().map(|s| s.to_string()).collect();
    ensure_lines(&repo_path.join(".gitattributes"), &rules)
}

/// Append every line that the file does not already contain, creating it if
/// needed. Returns whether the file changed.
pub fn ensure_lines(path: &Path, lines: &[String]) -> Result<bool> {
    let mut content = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?
    } else {
        String::new()
    };

    let mut added = false;
    for line in lines {
        if content.lines().any(|existing| existing == line) {
            continue;
        }
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(line);
        content.push('\n');
        added = true;
    }

    if added {
        fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))?;
    }

    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn the_sync_rules_are_written_once() {
        let repo = TempDir::new().unwrap();

        assert!(ensure_sync_attributes(repo.path()).unwrap());
        let written = fs::read_to_string(repo.path().join(".gitattributes")).unwrap();
        assert!(written.contains("* text=auto eol=lf"));
        assert!(written.contains("*.jsonl merge=union"));
        assert!(written.contains("MEMORY.md merge=union"));

        assert!(
            !ensure_sync_attributes(repo.path()).unwrap(),
            "a second run has nothing to add"
        );
        assert_eq!(
            fs::read_to_string(repo.path().join(".gitattributes")).unwrap(),
            written
        );
    }

    #[test]
    fn rules_are_added_beside_what_is_already_there() {
        let repo = TempDir::new().unwrap();
        let attributes = repo.path().join(".gitattributes");
        fs::write(&attributes, "*.png filter=lfs diff=lfs merge=lfs -text").unwrap();

        assert!(ensure_sync_attributes(repo.path()).unwrap());

        let written = fs::read_to_string(&attributes).unwrap();
        assert!(written.starts_with("*.png filter=lfs"));
        assert!(written.contains("*.jsonl merge=union"));
    }
}
