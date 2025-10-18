use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// Represents a single line/entry in the JSONL conversation file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationEntry {
    #[serde(rename = "type")]
    pub entry_type: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,

    #[serde(rename = "parentUuid", skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,

    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    #[serde(rename = "gitBranch", skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,

    // Catch-all for other fields we don't explicitly parse
    #[serde(flatten)]
    pub extra: Value,
}

/// Represents a complete conversation session
#[derive(Debug, Clone)]
pub struct ConversationSession {
    pub session_id: String,
    pub entries: Vec<ConversationEntry>,
    pub file_path: String,
}

impl ConversationSession {
    /// Parse a JSONL file into a ConversationSession
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)
            .with_context(|| format!("Failed to open file: {}", path.display()))?;

        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut session_id = None;

        for (line_num, line) in reader.lines().enumerate() {
            let line = line.with_context(|| {
                format!("Failed to read line {} in {}", line_num + 1, path.display())
            })?;

            if line.trim().is_empty() {
                continue;
            }

            let entry: ConversationEntry = serde_json::from_str(&line)
                .with_context(|| {
                    format!("Failed to parse JSON at line {} in {}", line_num + 1, path.display())
                })?;

            // Extract session ID from first entry that has one
            if session_id.is_none() {
                if let Some(ref sid) = entry.session_id {
                    session_id = Some(sid.clone());
                }
            }

            entries.push(entry);
        }

        let session_id = session_id
            .with_context(|| format!("No session ID found in {}", path.display()))?;

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
            let json = serde_json::to_string(entry)
                .context("Failed to serialize conversation entry")?;
            writeln!(file, "{}", json)
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
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_conversation_entry() {
        let json = r#"{"type":"user","uuid":"123","sessionId":"abc","timestamp":"2025-01-01T00:00:00Z"}"#;
        let entry: ConversationEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.entry_type, "user");
        assert_eq!(entry.uuid.unwrap(), "123");
    }

    #[test]
    fn test_read_write_session() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, r#"{{"type":"user","sessionId":"test-123","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(temp_file, r#"{{"type":"assistant","sessionId":"test-123","uuid":"2","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();

        let session = ConversationSession::from_file(temp_file.path()).unwrap();
        assert_eq!(session.session_id, "test-123");
        assert_eq!(session.entries.len(), 2);
        assert_eq!(session.message_count(), 2);

        // Test write
        let output_temp = NamedTempFile::new().unwrap();
        session.write_to_file(output_temp.path()).unwrap();

        let reloaded = ConversationSession::from_file(output_temp.path()).unwrap();
        assert_eq!(reloaded.session_id, session.session_id);
        assert_eq!(reloaded.entries.len(), session.entries.len());
    }
}
