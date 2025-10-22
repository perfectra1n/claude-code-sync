use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Maximum number of operation records to keep in history
const MAX_HISTORY_SIZE: usize = 5;

/// Type of sync operation performed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationType {
    /// Pull operation: syncing from remote to local
    Pull,
    /// Push operation: syncing from local to remote
    Push,
}

impl OperationType {
    /// Returns a human-readable string representation
    pub fn as_str(&self) -> &str {
        match self {
            OperationType::Pull => "pull",
            OperationType::Push => "push",
        }
    }
}

/// Type of operation performed on a specific conversation during sync
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncOperation {
    /// Conversation was newly added
    Added,
    /// Existing conversation was modified
    Modified,
    /// Conflict detected that needs resolution
    Conflict,
    /// Conversation exists but was not changed
    Unchanged,
}

impl SyncOperation {
    /// Returns a human-readable string representation
    pub fn as_str(&self) -> &str {
        match self {
            SyncOperation::Added => "added",
            SyncOperation::Modified => "modified",
            SyncOperation::Conflict => "conflict",
            SyncOperation::Unchanged => "unchanged",
        }
    }
}

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
    /// TODO(snapshot): Implement snapshot creation during sync operations
    /// The snapshot should contain:
    /// - Complete state of all conversation files before the operation
    /// - Metadata about the sync operation for context
    /// - Timestamp and operation type for verification
    ///
    ///   This will enable a future `claude-code-sync undo` command to restore
    ///   the previous state if a sync operation needs to be reversed.
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

/// Manages operation history with persistence to disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationHistory {
    /// List of operation records, most recent first
    pub operations: Vec<OperationRecord>,
}

impl OperationHistory {
    /// Create a new empty operation history
    fn new() -> Self {
        Self {
            operations: Vec::new(),
        }
    }

    /// Get the path to the history file
    fn history_file_path() -> Result<PathBuf> {
        crate::config::ConfigManager::operation_history_path()
    }

    /// Load operation history from a custom path
    /// Creates a new empty history if the file doesn't exist
    ///
    /// # Arguments
    /// * `path` - Optional custom path to load from. If None, uses default location.
    pub fn from_path(path: Option<PathBuf>) -> Result<Self> {
        let file_path = match path {
            Some(p) => p,
            None => Self::history_file_path()?,
        };

        if !file_path.exists() {
            return Ok(Self::new());
        }

        let content = fs::read_to_string(&file_path).with_context(|| {
            format!(
                "Failed to read operation history file from: {}",
                file_path.display()
            )
        })?;

        let history: OperationHistory = serde_json::from_str(&content).with_context(|| {
            format!(
                "Failed to parse operation history JSON from: {}",
                file_path.display()
            )
        })?;

        Ok(history)
    }

    /// Load operation history from disk using default location
    /// Creates a new empty history if the file doesn't exist
    pub fn load() -> Result<Self> {
        Self::from_path(None)
    }

    /// Save operation history to a custom path
    ///
    /// # Arguments
    /// * `path` - Optional custom path to save to. If None, uses default location.
    pub fn save_to(&self, path: Option<PathBuf>) -> Result<()> {
        let file_path = match path {
            Some(p) => p,
            None => Self::history_file_path()?,
        };

        // Ensure parent directory exists
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create history directory: {}", parent.display())
            })?;
        }

        let content =
            serde_json::to_string_pretty(self).context("Failed to serialize operation history")?;

        fs::write(&file_path, content).with_context(|| {
            format!(
                "Failed to write operation history file to: {}",
                file_path.display()
            )
        })?;

        Ok(())
    }

    /// Save operation history to disk using default location
    pub fn save(&self) -> Result<()> {
        self.save_to(None)
    }

    /// Add a new operation record to history
    /// Automatically rotates older entries if history exceeds MAX_HISTORY_SIZE
    pub fn add_operation(&mut self, record: OperationRecord) -> Result<()> {
        // Insert at the beginning (most recent first)
        self.operations.insert(0, record);

        // Rotate if we exceed the maximum size
        if self.operations.len() > MAX_HISTORY_SIZE {
            self.operations.truncate(MAX_HISTORY_SIZE);
        }

        // Persist to disk
        self.save()?;

        Ok(())
    }

    /// Get the most recent operation record
    ///
    /// This will be used to implement features like showing the last sync status
    /// or to enable "undo last operation" functionality.
    pub fn get_last_operation(&self) -> Option<&OperationRecord> {
        self.operations.first()
    }

    /// Get the most recent operation of a specific type
    ///
    /// This will be used to show when the last pull or push occurred,
    /// which helps users understand the sync state of their local files.
    pub fn get_last_operation_by_type(&self, op_type: OperationType) -> Option<&OperationRecord> {
        self.operations
            .iter()
            .find(|op| op.operation_type == op_type)
    }

    /// Get all operation records
    ///
    /// This will be used by the `claude-code-sync history` command to display
    /// the full history of sync operations to the user.
    pub fn list_operations(&self) -> &[OperationRecord] {
        &self.operations
    }

    /// Clear all operation history
    ///
    /// This will be used by a future `claude-code-sync history clear` command
    /// to allow users to reset their operation history.
    pub fn clear(&mut self) -> Result<()> {
        self.operations.clear();
        self.save()?;
        Ok(())
    }

    /// Get the number of operations in history
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Check if history is empty
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Remove the most recent operation of a specific type
    ///
    /// This is used by undo operations to remove the undone operation from history.
    /// Returns true if an operation was removed, false if no matching operation was found.
    ///
    /// # Arguments
    /// * `op_type` - The type of operation to remove
    /// * `path` - Optional custom path to save to (for testing)
    ///
    /// # Returns
    /// * `Ok(true)` if an operation was removed
    /// * `Ok(false)` if no matching operation was found
    pub fn remove_last_operation_by_type(
        &mut self,
        op_type: OperationType,
        path: Option<PathBuf>,
    ) -> Result<bool> {
        if let Some(index) = self
            .operations
            .iter()
            .position(|op| op.operation_type == op_type)
        {
            self.operations.remove(index);
            self.save_to(path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl Default for OperationHistory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper to create a temporary history file path
    fn setup_test_env() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let history_path = temp_dir.path().join("operation-history.json");
        (temp_dir, history_path)
    }

    #[test]
    fn test_operation_type_as_str() {
        assert_eq!(OperationType::Pull.as_str(), "pull");
        assert_eq!(OperationType::Push.as_str(), "push");
    }

    #[test]
    fn test_operation_type_serde() {
        let pull = OperationType::Pull;
        let serialized = serde_json::to_string(&pull).unwrap();
        assert_eq!(serialized, r#""pull""#);

        let push = OperationType::Push;
        let serialized = serde_json::to_string(&push).unwrap();
        assert_eq!(serialized, r#""push""#);

        let deserialized: OperationType = serde_json::from_str(r#""pull""#).unwrap();
        assert_eq!(deserialized, OperationType::Pull);
    }

    #[test]
    fn test_sync_operation_as_str() {
        assert_eq!(SyncOperation::Added.as_str(), "added");
        assert_eq!(SyncOperation::Modified.as_str(), "modified");
        assert_eq!(SyncOperation::Conflict.as_str(), "conflict");
        assert_eq!(SyncOperation::Unchanged.as_str(), "unchanged");
    }

    #[test]
    fn test_sync_operation_serde() {
        let added = SyncOperation::Added;
        let serialized = serde_json::to_string(&added).unwrap();
        assert_eq!(serialized, r#""added""#);

        let modified = SyncOperation::Modified;
        let serialized = serde_json::to_string(&modified).unwrap();
        assert_eq!(serialized, r#""modified""#);

        let deserialized: SyncOperation = serde_json::from_str(r#""conflict""#).unwrap();
        assert_eq!(deserialized, SyncOperation::Conflict);
    }

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
    fn test_operation_history_new() {
        let history = OperationHistory::new();
        assert_eq!(history.len(), 0);
        assert!(history.is_empty());
        assert!(history.get_last_operation().is_none());
    }

    #[test]
    fn test_operation_history_add_operation() {
        let (_temp_dir, path) = setup_test_env();

        let mut history = OperationHistory::new();

        let record = OperationRecord::new(OperationType::Push, Some("main".to_string()), vec![]);

        // Add operation and save
        history.add_operation(record).unwrap();

        // Save to test path
        history.save_to(Some(path.clone())).unwrap();

        // Load and verify
        let loaded = OperationHistory::from_path(Some(path)).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(!loaded.is_empty());

        let op = loaded.get_last_operation().unwrap();
        assert_eq!(op.operation_type, OperationType::Push);
        assert_eq!(op.branch, Some("main".to_string()));
    }

    #[test]
    fn test_operation_history_save_load_roundtrip() {
        let (_temp_dir, path) = setup_test_env();

        // Create a history with multiple operations
        let mut history = OperationHistory::new();

        let conversations1 = vec![
            ConversationSummary::new(
                "session-1".to_string(),
                "path/1".to_string(),
                Some("2025-01-15T10:00:00Z".to_string()),
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

        let record1 = OperationRecord::new(
            OperationType::Push,
            Some("main".to_string()),
            conversations1,
        );

        let conversations2 = vec![ConversationSummary::new(
            "session-3".to_string(),
            "path/3".to_string(),
            None,
            15,
            SyncOperation::Conflict,
        )
        .unwrap()];

        let record2 = OperationRecord::new(
            OperationType::Pull,
            Some("develop".to_string()),
            conversations2,
        );

        history.operations.push(record1);
        history.operations.insert(0, record2);

        // Save to temporary path
        history.save_to(Some(path.clone())).unwrap();

        // Verify file exists
        assert!(path.exists());

        // Load from path
        let loaded = OperationHistory::from_path(Some(path)).unwrap();

        // Verify the data matches
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.operations.len(), history.operations.len());

        let last = loaded.get_last_operation().unwrap();
        assert_eq!(last.operation_type, OperationType::Pull);
        assert_eq!(last.branch, Some("develop".to_string()));
        assert_eq!(last.affected_conversations.len(), 1);
        assert_eq!(
            last.affected_conversations[0].operation,
            SyncOperation::Conflict
        );

        let first = &loaded.operations[1];
        assert_eq!(first.operation_type, OperationType::Push);
        assert_eq!(first.affected_conversations.len(), 2);
    }

    #[test]
    fn test_operation_history_rotation() {
        let mut history = OperationHistory::new();

        // Add more than MAX_HISTORY_SIZE operations
        for i in 0..7 {
            let record =
                OperationRecord::new(OperationType::Push, Some(format!("branch-{i}")), vec![]);
            history.operations.insert(0, record);
        }

        // Manually truncate to simulate rotation
        if history.operations.len() > MAX_HISTORY_SIZE {
            history.operations.truncate(MAX_HISTORY_SIZE);
        }

        assert_eq!(history.len(), MAX_HISTORY_SIZE);

        // Most recent should be branch-6
        let last = history.get_last_operation().unwrap();
        assert_eq!(last.branch, Some("branch-6".to_string()));
    }

    #[test]
    fn test_operation_history_get_last_operation() {
        let mut history = OperationHistory::new();

        let record1 = OperationRecord::new(OperationType::Pull, Some("main".to_string()), vec![]);

        let record2 =
            OperationRecord::new(OperationType::Push, Some("develop".to_string()), vec![]);

        history.operations.push(record1);
        history.operations.insert(0, record2);

        let last = history.get_last_operation().unwrap();
        assert_eq!(last.operation_type, OperationType::Push);
        assert_eq!(last.branch, Some("develop".to_string()));
    }

    #[test]
    fn test_operation_history_get_last_operation_by_type() {
        let mut history = OperationHistory::new();

        let record1 = OperationRecord::new(OperationType::Pull, Some("main".to_string()), vec![]);

        let record2 =
            OperationRecord::new(OperationType::Push, Some("develop".to_string()), vec![]);

        let record3 =
            OperationRecord::new(OperationType::Pull, Some("feature".to_string()), vec![]);

        history.operations.push(record1);
        history.operations.insert(0, record2);
        history.operations.insert(0, record3);

        let last_pull = history
            .get_last_operation_by_type(OperationType::Pull)
            .unwrap();
        assert_eq!(last_pull.branch, Some("feature".to_string()));

        let last_push = history
            .get_last_operation_by_type(OperationType::Push)
            .unwrap();
        assert_eq!(last_push.branch, Some("develop".to_string()));
    }

    #[test]
    fn test_operation_history_list_operations() {
        let mut history = OperationHistory::new();

        let record1 = OperationRecord::new(OperationType::Pull, None, vec![]);
        let record2 = OperationRecord::new(OperationType::Push, None, vec![]);

        history.operations.push(record1);
        history.operations.insert(0, record2);

        let operations = history.list_operations();
        assert_eq!(operations.len(), 2);
        assert_eq!(operations[0].operation_type, OperationType::Push);
        assert_eq!(operations[1].operation_type, OperationType::Pull);
    }

    #[test]
    fn test_operation_history_clear() {
        let (_temp_dir, path) = setup_test_env();

        let mut history = OperationHistory::new();

        let record = OperationRecord::new(OperationType::Push, None, vec![]);
        history.operations.push(record);

        assert_eq!(history.len(), 1);

        // Save to test path
        history.save_to(Some(path.clone())).unwrap();

        // Clear and verify
        history.clear().unwrap();
        assert_eq!(history.len(), 0);
        assert!(history.is_empty());

        // Save cleared state
        history.save_to(Some(path.clone())).unwrap();

        // Load and verify it's still empty
        let loaded = OperationHistory::from_path(Some(path)).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_operation_history_serialization() {
        let conversations = vec![ConversationSummary::new(
            "session-1".to_string(),
            "path/1".to_string(),
            Some("2025-01-15T10:00:00Z".to_string()),
            5,
            SyncOperation::Added,
        )
        .unwrap()];

        let record =
            OperationRecord::new(OperationType::Push, Some("main".to_string()), conversations);

        let mut history = OperationHistory::new();
        history.operations.push(record);

        let json = serde_json::to_string(&history).unwrap();
        let deserialized: OperationHistory = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.len(), 1);
        let op = deserialized.get_last_operation().unwrap();
        assert_eq!(op.operation_type, OperationType::Push);
        assert_eq!(op.branch, Some("main".to_string()));
        assert_eq!(op.affected_conversations.len(), 1);
        assert_eq!(op.affected_conversations[0].operation, SyncOperation::Added);
    }

    #[test]
    fn test_operation_history_default() {
        let history = OperationHistory::default();
        assert!(history.is_empty());
    }

    #[test]
    fn test_empty_history_last_operation() {
        let history = OperationHistory::new();
        assert!(history.get_last_operation().is_none());
        assert!(history
            .get_last_operation_by_type(OperationType::Pull)
            .is_none());
        assert!(history
            .get_last_operation_by_type(OperationType::Push)
            .is_none());
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

    #[test]
    fn test_max_history_size_constant() {
        assert_eq!(MAX_HISTORY_SIZE, 5);
    }

    #[test]
    fn test_operation_type_equality() {
        assert_eq!(OperationType::Pull, OperationType::Pull);
        assert_eq!(OperationType::Push, OperationType::Push);
        assert_ne!(OperationType::Pull, OperationType::Push);
    }

    #[test]
    fn test_sync_operation_equality() {
        assert_eq!(SyncOperation::Added, SyncOperation::Added);
        assert_eq!(SyncOperation::Modified, SyncOperation::Modified);
        assert_ne!(SyncOperation::Added, SyncOperation::Conflict);
    }

    #[test]
    fn test_error_messages_include_file_paths() {
        let (_temp_dir, path) = setup_test_env();

        // Write invalid JSON to test parse error message
        fs::write(&path, "{ invalid json }").unwrap();

        let result = OperationHistory::from_path(Some(path.clone()));
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        // Error should include the file path
        assert!(error_msg.contains(&path.display().to_string()));

        // Test write error with read-only parent directory (Unix-specific)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let readonly_dir = _temp_dir.path().join("readonly");
            fs::create_dir(&readonly_dir).unwrap();
            let readonly_path = readonly_dir.join("history.json");

            // Make directory read-only
            let mut perms = fs::metadata(&readonly_dir).unwrap().permissions();
            perms.set_mode(0o444);
            fs::set_permissions(&readonly_dir, perms).unwrap();

            let history = OperationHistory::new();
            let result = history.save_to(Some(readonly_path.clone()));
            if result.is_err() {
                let error_msg = result.unwrap_err().to_string();
                // Error should reference the path
                assert!(
                    error_msg.contains("history")
                        || error_msg.contains(&readonly_path.display().to_string())
                );
            }
        }
    }

    #[test]
    fn test_from_path_creates_new_when_missing() {
        let (_temp_dir, path) = setup_test_env();

        // Path doesn't exist yet
        assert!(!path.exists());

        // Should create new empty history
        let history = OperationHistory::from_path(Some(path)).unwrap();
        assert!(history.is_empty());
        assert_eq!(history.len(), 0);
    }

    #[test]
    fn test_operation_history_maintains_order() {
        let mut history = OperationHistory::new();

        for i in 0..3 {
            let record = OperationRecord::new(
                if i % 2 == 0 {
                    OperationType::Pull
                } else {
                    OperationType::Push
                },
                Some(format!("branch-{i}")),
                vec![],
            );
            history.operations.insert(0, record);
        }

        let operations = history.list_operations();
        assert_eq!(operations[0].branch, Some("branch-2".to_string()));
        assert_eq!(operations[1].branch, Some("branch-1".to_string()));
        assert_eq!(operations[2].branch, Some("branch-0".to_string()));
    }

    #[test]
    fn test_remove_last_operation_by_type() {
        let (_temp_dir, path) = setup_test_env();
        let mut history = OperationHistory::new();

        // Add operations in order: Pull, Push, Pull
        let record1 =
            OperationRecord::new(OperationType::Pull, Some("branch-1".to_string()), vec![]);
        let record2 =
            OperationRecord::new(OperationType::Push, Some("branch-2".to_string()), vec![]);
        let record3 =
            OperationRecord::new(OperationType::Pull, Some("branch-3".to_string()), vec![]);

        history.operations.push(record1);
        history.operations.insert(0, record2);
        history.operations.insert(0, record3);

        assert_eq!(history.len(), 3);

        // Remove last Pull (should remove branch-3 which is at index 0)
        let removed = history
            .remove_last_operation_by_type(OperationType::Pull, Some(path.clone()))
            .unwrap();
        assert!(removed);
        assert_eq!(history.len(), 2);

        // Verify branch-3 was removed
        let operations = history.list_operations();
        assert_eq!(operations[0].branch, Some("branch-2".to_string()));
        assert_eq!(operations[1].branch, Some("branch-1".to_string()));

        // Remove last Push
        let removed = history
            .remove_last_operation_by_type(OperationType::Push, Some(path.clone()))
            .unwrap();
        assert!(removed);
        assert_eq!(history.len(), 1);

        // Only branch-1 should remain
        let operations = history.list_operations();
        assert_eq!(operations[0].branch, Some("branch-1".to_string()));

        // Try to remove when none exists
        let removed = history
            .remove_last_operation_by_type(OperationType::Push, Some(path.clone()))
            .unwrap();
        assert!(!removed);
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn test_remove_last_operation_by_type_persists() {
        let (_temp_dir, path) = setup_test_env();
        let mut history = OperationHistory::new();

        let record1 = OperationRecord::new(OperationType::Pull, Some("main".to_string()), vec![]);
        let record2 =
            OperationRecord::new(OperationType::Push, Some("develop".to_string()), vec![]);

        history.operations.push(record1);
        history.operations.insert(0, record2);

        // Save initial state
        history.save_to(Some(path.clone())).unwrap();

        // Remove and verify persistence
        let removed = history
            .remove_last_operation_by_type(OperationType::Push, Some(path.clone()))
            .unwrap();
        assert!(removed);

        // Reload from disk
        let loaded = OperationHistory::from_path(Some(path)).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.operations[0].operation_type, OperationType::Pull);
    }
}
