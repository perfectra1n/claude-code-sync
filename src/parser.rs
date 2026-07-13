use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// Represents a single line/entry in the JSONL conversation file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationEntry {
    /// The type of this entry (e.g., "user", "assistant", "file-history-snapshot")
    ///
    /// This field identifies what kind of entry this is in the conversation.
    /// Common types include user messages, assistant responses, and system events.
    #[serde(rename = "type")]
    pub entry_type: String,

    /// Unique identifier for this conversation entry
    ///
    /// Each entry may have its own UUID to uniquely identify it within the conversation.
    /// Not all entry types require a UUID, hence this is optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,

    /// UUID of the parent entry in the conversation thread
    ///
    /// This links entries together in a conversation tree, allowing for branching
    /// and threading of messages. If present, it references the UUID of the entry
    /// that this entry is responding to or following from.
    #[serde(rename = "parentUuid", skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,

    /// Session identifier grouping related conversation entries together
    ///
    /// All entries within a single conversation session share the same session ID.
    /// This is used to associate entries across multiple files or to reconstruct
    /// conversation context. If not present in the entry, the filename may be used.
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// ISO 8601 timestamp indicating when this entry was created
    ///
    /// Format is typically "YYYY-MM-DDTHH:MM:SS.sssZ" (e.g., "2025-01-01T00:00:00.000Z").
    /// Used for sorting entries chronologically and determining the latest activity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,

    /// The actual message content as a JSON value
    ///
    /// Contains the text and structured data of the user or assistant message.
    /// Stored as a generic JSON value to accommodate different message formats
    /// and structures without strict schema requirements.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,

    /// Current working directory at the time this entry was created
    ///
    /// Stores the filesystem path of the working directory, providing context
    /// about where the conversation or command was executed. Useful for
    /// reproducing environments and understanding file references.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Version string of the Claude Code CLI that created this entry
    ///
    /// Records which version of the tool generated this conversation entry,
    /// helpful for debugging compatibility issues and tracking feature support.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Git branch name active when this entry was created
    ///
    /// Captures the current git branch context, allowing conversation entries
    /// to be associated with specific branches in version control. Useful for
    /// tracking which branch work was performed on.
    #[serde(rename = "gitBranch", skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,

    /// Catch-all field for additional JSON properties not explicitly defined
    ///
    /// Preserves any extra fields in the JSON that aren't part of the explicit schema.
    /// This allows forward compatibility - newer versions can add fields without breaking
    /// older parsers. The flattened serde attribute merges these fields at the same level
    /// as the named fields when serializing/deserializing.
    #[serde(flatten)]
    pub extra: Value,
}

/// Represents a complete conversation session
#[derive(Debug, Clone)]
pub struct ConversationSession {
    /// Unique identifier for this conversation session
    ///
    /// Either extracted from the first entry that contains a sessionId field,
    /// or derived from the filename (without extension) if no entries contain
    /// a session ID. Used to group related conversation entries together.
    pub session_id: String,

    /// All conversation entries in chronological order
    ///
    /// Contains the complete sequence of entries from the JSONL file, including
    /// user messages, assistant responses, and system events like file history
    /// snapshots. Preserves the original order from the file.
    pub entries: Vec<ConversationEntry>,

    /// Path to the JSONL file this session was loaded from
    ///
    /// Stores the filesystem path of the source file, used for tracking the
    /// origin of the conversation data and for potential file operations like
    /// rewriting or updating the session.
    pub file_path: String,
}

impl ConversationSession {
    /// Parse a JSONL file into a ConversationSession
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let file =
            File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;

        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut entry_session_id = None;

        for (line_num, line) in reader.lines().enumerate() {
            let line = line.with_context(|| {
                format!("Failed to read line {} in {}", line_num + 1, path.display())
            })?;

            if line.trim().is_empty() {
                continue;
            }

            let entry: ConversationEntry = serde_json::from_str(&line).with_context(|| {
                format!(
                    "Failed to parse JSON at line {} in {}",
                    line_num + 1,
                    path.display()
                )
            })?;

            // Remember the first interior sessionId, used only as a fallback below.
            if entry_session_id.is_none() {
                if let Some(ref sid) = entry.session_id {
                    entry_session_id = Some(sid.clone());
                }
            }

            entries.push(entry);
        }

        // Identity is the filename stem, which Claude Code guarantees is unique per
        // transcript: `<uuid>.jsonl` for a session and `agent-<hash>.jsonl` for each
        // of its subagents. The interior `sessionId` field is NOT unique — subagent
        // sidechain transcripts carry their *parent* session's id, so keying identity
        // on it collapses every subagent onto the parent and makes sync drop the
        // parent's main transcript. Fall back to the interior id only if the path has
        // no usable stem.
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .or(entry_session_id)
            .with_context(|| {
                format!(
                    "No session ID found in file or filename: {}",
                    path.display()
                )
            })?;

        Ok(ConversationSession {
            session_id,
            entries,
            file_path: path.to_string_lossy().to_string(),
        })
    }

    /// Write the conversation session to a JSONL file
    pub fn write_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        let mut file = File::create(path)
            .with_context(|| format!("Failed to create file: {}", path.display()))?;

        for entry in &self.entries {
            let json =
                serde_json::to_string(entry).context("Failed to serialize conversation entry")?;
            writeln!(file, "{json}")
                .with_context(|| format!("Failed to write to file: {}", path.display()))?;
        }

        Ok(())
    }

    /// Get the latest timestamp from the conversation
    pub fn latest_timestamp(&self) -> Option<String> {
        self.entries
            .iter()
            .filter_map(|e| e.timestamp.clone())
            .max()
    }

    /// Get the number of messages (user + assistant) in the conversation
    pub fn message_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.entry_type == "user" || e.entry_type == "assistant")
            .count()
    }

    /// Get the project name from the first entry's `cwd` path
    pub fn project_name(&self) -> Option<&str> {
        self.entries
            .iter()
            .find_map(|e| e.cwd.as_ref())
            .and_then(|cwd| std::path::Path::new(cwd).file_name())
            .and_then(|name| name.to_str())
    }

    /// Calculate a simple hash of the conversation content
    pub fn content_hash(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        for entry in &self.entries {
            if let Ok(json) = serde_json::to_string(entry) {
                json.hash(&mut hasher);
            }
        }
        format!("{:x}", hasher.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_conversation_entry() {
        let json =
            r#"{"type":"user","uuid":"123","sessionId":"abc","timestamp":"2025-01-01T00:00:00Z"}"#;
        let entry: ConversationEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.entry_type, "user");
        assert_eq!(entry.uuid.unwrap(), "123");
    }

    #[test]
    fn test_read_write_session() {
        use std::fs::File;
        use tempfile::TempDir;

        // Identity comes from the filename stem, so use a controlled filename.
        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("test-123.jsonl");
        let mut file = File::create(&session_path).unwrap();
        writeln!(file, r#"{{"type":"user","sessionId":"test-123","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","sessionId":"test-123","uuid":"2","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        drop(file);

        let session = ConversationSession::from_file(&session_path).unwrap();
        assert_eq!(session.session_id, "test-123");
        assert_eq!(session.entries.len(), 2);
        assert_eq!(session.message_count(), 2);

        // Round-trip: writing then re-reading the same path preserves content and id.
        let output_dir = TempDir::new().unwrap();
        let output_path = output_dir.path().join("test-123.jsonl");
        session.write_to_file(&output_path).unwrap();

        let reloaded = ConversationSession::from_file(&output_path).unwrap();
        assert_eq!(reloaded.session_id, session.session_id);
        assert_eq!(reloaded.entries.len(), session.entries.len());
    }

    #[test]
    fn test_session_id_from_filename() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_file = temp_dir
            .path()
            .join("248a0cdf-1466-48a7-b3d0-00f9e8e6e4ee.jsonl");

        // Create file with entries that don't have sessionId field
        let mut file = File::create(&session_file).unwrap();
        writeln!(file, r#"{{"type":"file-history-snapshot","messageId":"abc","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"file-history-snapshot","messageId":"def","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();

        // Parse should succeed using filename as session ID
        let session = ConversationSession::from_file(&session_file).unwrap();
        assert_eq!(session.session_id, "248a0cdf-1466-48a7-b3d0-00f9e8e6e4ee");
        assert_eq!(session.entries.len(), 2);
    }

    #[test]
    fn test_session_id_from_filename_preferred() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_file = temp_dir.path().join("filename-uuid.jsonl");

        // Create file with a (different) sessionId in entries
        let mut file = File::create(&session_file).unwrap();
        writeln!(file, r#"{{"type":"user","sessionId":"entry-uuid","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();

        // Identity must come from the filename, not the interior sessionId.
        let session = ConversationSession::from_file(&session_file).unwrap();
        assert_eq!(session.session_id, "filename-uuid");
    }

    #[test]
    fn test_subagent_does_not_inherit_parent_session_id() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        // Regression: Claude Code subagent transcripts are named `agent-<hash>.jsonl`
        // but every entry carries the *parent* session's id with isSidechain=true.
        // Keying on the interior id collapsed all subagents onto the parent and made
        // sync drop the parent's main transcript. Identity must be the filename stem.
        let temp_dir = TempDir::new().unwrap();
        let subagent_file = temp_dir.path().join("agent-a16263cbf10e1ad0b.jsonl");

        let mut file = File::create(&subagent_file).unwrap();
        writeln!(file, r#"{{"type":"user","sessionId":"56d02190-2a2d-4a55-9ec1-38e34fb25e84","isSidechain":true,"uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();

        let session = ConversationSession::from_file(&subagent_file).unwrap();
        assert_eq!(session.session_id, "agent-a16263cbf10e1ad0b");
        assert_ne!(session.session_id, "56d02190-2a2d-4a55-9ec1-38e34fb25e84");
    }

    #[test]
    fn test_mixed_entries_with_and_without_session_id() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_file = temp_dir.path().join("test-session.jsonl");

        // Create file with mix of entries
        let mut file = File::create(&session_file).unwrap();
        writeln!(file, r#"{{"type":"file-history-snapshot","messageId":"abc","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"user","sessionId":"test-123","uuid":"1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();

        // Identity comes from the filename stem regardless of interior ids.
        let session = ConversationSession::from_file(&session_file).unwrap();
        assert_eq!(session.session_id, "test-session");
        assert_eq!(session.entries.len(), 2);
    }

    #[test]
    fn test_project_name_from_cwd() {
        let json = r#"{"type":"user","uuid":"1","cwd":"/Users/abc/my-cool-project"}"#;
        let entry: ConversationEntry = serde_json::from_str(json).unwrap();
        let session = ConversationSession {
            session_id: "test".to_string(),
            entries: vec![entry],
            file_path: "test.jsonl".to_string(),
        };
        assert_eq!(session.project_name(), Some("my-cool-project"));
    }

    #[test]
    fn test_project_name_no_cwd() {
        let json = r#"{"type":"user","uuid":"1"}"#;
        let entry: ConversationEntry = serde_json::from_str(json).unwrap();
        let session = ConversationSession {
            session_id: "test".to_string(),
            entries: vec![entry],
            file_path: "test.jsonl".to_string(),
        };
        assert_eq!(session.project_name(), None);
    }
}
