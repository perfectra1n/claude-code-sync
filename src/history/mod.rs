//! Operation history tracking and persistence.
//!
//! Records all sync operations (push and pull) with metadata about affected
//! conversations. Maintains a rolling history of recent operations with automatic
//! rotation. Each operation record includes a snapshot path for undo functionality.

mod record;
mod storage;
mod summary;
mod types;

// Re-export public types and functions
pub use record::OperationRecord;
pub use storage::OperationHistory;
pub use summary::ConversationSummary;
pub use types::{OperationType, SyncOperation};
