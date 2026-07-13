//! Shared fixtures for the `undo` unit tests.
//!
//! These live here rather than in each sibling module because the setup they
//! encode is genuinely shared: the ten `operations` tests each need a snapshot
//! on disk plus an `OperationHistory` referencing it, and before this module
//! existed they all spelled that out longhand.

// Each sibling test module uses a different subset of these. `clippy
// --all-targets` compiles `cfg(test)` code, so a helper unused by one of them
// would otherwise be a `dead_code` warning, and CI runs with `-D warnings`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Duration;
use tempfile::{tempdir, TempDir};

use super::Snapshot;
use crate::history::{
    ConversationSummary, OperationHistory, OperationRecord, OperationType, SyncOperation,
};
use crate::scm::{self, Scm};

/// Write `content` to `dir/name` and return the path.
pub(super) fn create_test_file(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

/// A temp dir holding an initialized SCM repo with one committed file.
pub(super) fn setup_test_repo() -> (TempDir, Box<dyn Scm>) {
    let temp_dir = tempdir().unwrap();
    let repo = scm::init(temp_dir.path()).unwrap();

    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, "initial content").unwrap();
    repo.stage_all().unwrap();
    repo.commit("Initial commit").unwrap();

    (temp_dir, repo)
}

/// A snapshot carrying metadata but no file contents.
///
/// Cleanup selects purely on `operation_type` and `timestamp`, so the tests
/// that exercise it never need real file bodies. Callers that care about the
/// commit hash or branch set those fields afterwards.
pub(super) fn metadata_only_snapshot(
    id: &str,
    operation_type: OperationType,
    age: Duration,
) -> Snapshot {
    Snapshot {
        snapshot_id: id.to_string(),
        timestamp: chrono::Utc::now() - age,
        operation_type,
        git_commit_hash: None,
        files: HashMap::new(),
        branch: None,
        base_snapshot_id: None,
        deleted_files: Vec::new(),
    }
}

/// Builds an `OperationHistory` on disk.
///
/// The undo tests care about which operations are present, in what order, and
/// which snapshot each can be undone from. The conversation summaries attached
/// to each record are filler — nothing asserts on them.
pub(super) struct HistoryBuilder {
    path: PathBuf,
    history: OperationHistory,
}

impl HistoryBuilder {
    pub(super) fn new(history_path: &Path) -> Self {
        Self {
            path: history_path.to_path_buf(),
            history: OperationHistory::from_path(Some(history_path.to_path_buf())).unwrap(),
        }
    }

    /// Append one operation. `snapshot` is the snapshot file it can be undone
    /// from; `None` models an operation recorded without one.
    pub(super) fn push(
        mut self,
        operation_type: OperationType,
        branch: &str,
        snapshot: Option<&Path>,
    ) -> Self {
        let sync_op = match operation_type {
            OperationType::Pull => SyncOperation::Modified,
            _ => SyncOperation::Added,
        };
        let summary = ConversationSummary::new(
            "test-session".to_string(),
            "test/path".to_string(),
            None,
            5,
            sync_op,
        )
        .unwrap();

        let mut record =
            OperationRecord::new(operation_type, Some(branch.to_string()), vec![summary]);
        record.snapshot_path = snapshot.map(Path::to_path_buf);

        self.history.add_operation(record).unwrap();
        self
    }

    /// Persist the accumulated operations.
    pub(super) fn save(self) {
        self.history.save_to(Some(self.path)).unwrap();
    }
}

/// Number of operations currently recorded at `history_path`.
pub(super) fn operation_count(history_path: &Path) -> usize {
    OperationHistory::from_path(Some(history_path.to_path_buf()))
        .unwrap()
        .len()
}

/// The operations recorded at `history_path`, **most recent first** —
/// `add_operation` inserts at index 0.
pub(super) fn operation_types(history_path: &Path) -> Vec<OperationType> {
    OperationHistory::from_path(Some(history_path.to_path_buf()))
        .unwrap()
        .list_operations()
        .iter()
        .map(|op| op.operation_type)
        .collect()
}
