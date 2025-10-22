use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
