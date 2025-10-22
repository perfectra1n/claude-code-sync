use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::types::SyncOperation;

/// Summary of a conversation affected during a sync operation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConversationSummary {
    /// Unique identifier for the conversation session
    pub session_id: String,

    /// Relative path from claude projects directory
    pub project_path: String,

    /// Timestamp of the conversation (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,

    /// Number of messages in the conversation
    pub message_count: usize,

    /// Type of operation performed on this conversation
    pub operation: SyncOperation,
}

impl ConversationSummary {
    /// Create a new conversation summary with validation
    ///
    /// # Errors
    /// Returns an error if session_id or project_path are empty
    pub fn new(
        session_id: String,
        project_path: String,
        timestamp: Option<String>,
        message_count: usize,
        operation: SyncOperation,
    ) -> Result<Self> {
        if session_id.is_empty() {
            anyhow::bail!("session_id cannot be empty");
        }
        if project_path.is_empty() {
            anyhow::bail!("project_path cannot be empty");
        }

        Ok(Self {
            session_id,
            project_path,
            timestamp,
            message_count,
            operation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversation_summary_creation() {
        let summary = ConversationSummary::new(
            "session-123".to_string(),
            "project/path".to_string(),
            Some("2025-01-15T10:30:00Z".to_string()),
            42,
            SyncOperation::Added,
        )
        .unwrap();

        assert_eq!(summary.session_id, "session-123");
        assert_eq!(summary.project_path, "project/path");
        assert_eq!(summary.timestamp, Some("2025-01-15T10:30:00Z".to_string()));
        assert_eq!(summary.message_count, 42);
        assert_eq!(summary.operation, SyncOperation::Added);
    }

    #[test]
    fn test_conversation_summary_validation() {
        // Empty session_id should fail
        let result = ConversationSummary::new(
            "".to_string(),
            "project/path".to_string(),
            None,
            10,
            SyncOperation::Added,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("session_id"));

        // Empty project_path should fail
        let result = ConversationSummary::new(
            "session-123".to_string(),
            "".to_string(),
            None,
            10,
            SyncOperation::Added,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("project_path"));
    }

    #[test]
    fn test_conversation_summary_serde() {
        let summary = ConversationSummary::new(
            "session-456".to_string(),
            "test/path".to_string(),
            None,
            10,
            SyncOperation::Modified,
        )
        .unwrap();

        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: ConversationSummary = serde_json::from_str(&json).unwrap();

        assert_eq!(summary, deserialized);
    }

    #[test]
    fn test_conversation_summary_with_all_fields() {
        let summary = ConversationSummary::new(
            "test-session".to_string(),
            "test/project".to_string(),
            Some("2025-01-15T12:00:00Z".to_string()),
            100,
            SyncOperation::Conflict,
        )
        .unwrap();

        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("test-session"));
        assert!(json.contains("test/project"));
        assert!(json.contains("2025-01-15T12:00:00Z"));
        assert!(json.contains("100"));
        assert!(json.contains("conflict"));
    }
}
