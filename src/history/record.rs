use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::summary::ConversationSummary;
use super::types::{OperationType, SyncOperation};

/// Record of a single sync operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRecord {
    /// Type of operation (pull or push)
    pub operation_type: OperationType,

    /// When the operation was performed
    pub timestamp: DateTime<Utc>,

    /// Git branch the operation was performed on (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// List of conversations affected by this operation
    pub affected_conversations: Vec<ConversationSummary>,

    /// Path to snapshot for undo capability
    ///
    /// Snapshots are created during sync operations to enable undo functionality.
    /// The snapshot implementation is in `src/undo/snapshot.rs` and provides:
    /// - Complete state of all conversation files before the operation
    /// - Differential snapshots to minimize disk usage
    /// - Metadata about the sync operation for context
    /// - Timestamp and operation type for verification
    ///
    /// This enables the `claude-code-sync undo` command to restore
    /// the previous state if a sync operation needs to be reversed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_path: Option<PathBuf>,
}

impl OperationRecord {
    /// Create a new operation record
    pub fn new(
        operation_type: OperationType,
        branch: Option<String>,
        affected_conversations: Vec<ConversationSummary>,
    ) -> Self {
        Self {
            operation_type,
            timestamp: Utc::now(),
            branch,
            affected_conversations,
            snapshot_path: None,
        }
    }

    /// Get a summary string for this operation
    ///
    /// This method will be used in future CLI commands to display
    /// operation history to users (e.g., `claude-code-sync history`).
    #[allow(dead_code)]
    pub fn summary(&self) -> String {
        format!(
            "{} operation on {} at {} ({} conversations affected)",
            self.operation_type.as_str(),
            self.branch.as_deref().unwrap_or("unknown branch"),
            self.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
            self.affected_conversations.len()
        )
    }

    /// Count conversations by operation type
    ///
    /// This method will be used in future CLI commands to provide
    /// statistics about sync operations (e.g., showing how many files
    /// were added, modified, or had conflicts).
    pub fn operation_stats(&self) -> std::collections::HashMap<SyncOperation, usize> {
        let mut stats = std::collections::HashMap::new();
        for conv in &self.affected_conversations {
            *stats.entry(conv.operation).or_insert(0) += 1;
        }
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operation_record_creation() {
        let conversations = vec![
            ConversationSummary::new(
                "session-1".to_string(),
                "path/1".to_string(),
                None,
                5,
                SyncOperation::Added,
            )
            .unwrap(),
            ConversationSummary::new(
                "session-2".to_string(),
                "path/2".to_string(),
                None,
                10,
                SyncOperation::Modified,
            )
            .unwrap(),
        ];

        let record = OperationRecord::new(
            OperationType::Push,
            Some("main".to_string()),
            conversations.clone(),
        );

        assert_eq!(record.operation_type, OperationType::Push);
        assert_eq!(record.branch, Some("main".to_string()));
        assert_eq!(record.affected_conversations.len(), 2);
        assert!(record.snapshot_path.is_none());
    }

    #[test]
    fn test_operation_record_summary() {
        let conversations = vec![ConversationSummary::new(
            "session-1".to_string(),
            "path/1".to_string(),
            None,
            5,
            SyncOperation::Added,
        )
        .unwrap()];

        let record = OperationRecord::new(
            OperationType::Pull,
            Some("feature-branch".to_string()),
            conversations,
        );

        let summary = record.summary();
        assert!(summary.contains("pull"));
        assert!(summary.contains("feature-branch"));
        assert!(summary.contains("1 conversations affected"));
    }

    #[test]
    fn test_operation_record_stats() {
        let conversations = vec![
            ConversationSummary::new(
                "s1".to_string(),
                "p1".to_string(),
                None,
                5,
                SyncOperation::Added,
            )
            .unwrap(),
            ConversationSummary::new(
                "s2".to_string(),
                "p2".to_string(),
                None,
                10,
                SyncOperation::Added,
            )
            .unwrap(),
            ConversationSummary::new(
                "s3".to_string(),
                "p3".to_string(),
                None,
                15,
                SyncOperation::Modified,
            )
            .unwrap(),
            ConversationSummary::new(
                "s4".to_string(),
                "p4".to_string(),
                None,
                20,
                SyncOperation::Conflict,
            )
            .unwrap(),
        ];

        let record = OperationRecord::new(OperationType::Push, None, conversations);
        let stats = record.operation_stats();

        assert_eq!(stats.get(&SyncOperation::Added), Some(&2));
        assert_eq!(stats.get(&SyncOperation::Modified), Some(&1));
        assert_eq!(stats.get(&SyncOperation::Conflict), Some(&1));
    }

    #[test]
    fn test_operation_record_with_snapshot() {
        let mut record =
            OperationRecord::new(OperationType::Pull, Some("main".to_string()), vec![]);

        record.snapshot_path = Some(PathBuf::from("/tmp/snapshot.tar.gz"));

        let json = serde_json::to_string(&record).unwrap();
        let deserialized: OperationRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(
            deserialized.snapshot_path,
            Some(PathBuf::from("/tmp/snapshot.tar.gz"))
        );
    }
}
