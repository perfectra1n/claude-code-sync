//! Snapshot-based undo functionality for sync operations.
//!
//! Creates point-in-time snapshots of conversation files before sync operations.
//! Snapshots enable undoing pull operations (by restoring files) and push operations
//! (by resetting Git commits). Includes validation and security checks for safe restoration.

mod cleanup;
mod differential;
mod operations;
mod preview;
mod restore;
mod snapshot;

#[cfg(test)]
mod test_support;

// Re-export public types and functions to maintain API compatibility
pub use cleanup::{cleanup_old_snapshots, cleanup_old_snapshots_with_dir, SnapshotCleanupConfig};
pub use operations::{undo_pull, undo_push};
pub use preview::{preview_undo_pull, preview_undo_push, UndoPreview, VerbosityLevel};
pub use snapshot::Snapshot;
