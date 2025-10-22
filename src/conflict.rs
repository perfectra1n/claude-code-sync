use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::parser::ConversationSession;

/// Represents a conflict between local and remote versions of the same conversation session.
///
/// A `Conflict` is detected when both local and remote filesystems contain a conversation
/// with the same session ID but different content (as determined by content hashes). This
/// typically occurs when the same conversation has been modified on different machines or
/// when changes haven't been synchronized properly.
///
/// The conflict contains metadata about both versions to help users make informed decisions
/// about how to resolve the discrepancy.
///
/// # Examples
///
/// ```no_run
/// use claude_sync::conflict::Conflict;
/// use claude_sync::parser::ConversationSession;
///
/// # fn example(local: &ConversationSession, remote: &ConversationSession) {
/// let conflict = Conflict::new(local, remote);
///
/// if conflict.is_real_conflict() {
///     println!("{}", conflict.description());
/// }
/// # }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    /// The unique identifier for the conversation session that has conflicted.
    ///
    /// This ID is shared between both the local and remote versions, as they represent
    /// different states of the same conversation.
    pub session_id: String,

    /// The file path to the local version of the conversation.
    ///
    /// This points to the conversation file in the local filesystem, typically in the
    /// user's local conversation storage directory.
    pub local_file: PathBuf,

    /// The file path to the remote version of the conversation.
    ///
    /// This points to the conversation file from the remote source (e.g., synced from
    /// another machine or cloud storage).
    pub remote_file: PathBuf,

    /// The timestamp of the most recent message in the local version.
    ///
    /// This is `None` if the local conversation has no messages with timestamps.
    /// The timestamp helps users understand which version is more recent.
    pub local_timestamp: Option<String>,

    /// The timestamp of the most recent message in the remote version.
    ///
    /// This is `None` if the remote conversation has no messages with timestamps.
    /// The timestamp helps users understand which version is more recent.
    pub remote_timestamp: Option<String>,

    /// The total number of messages in the local version of the conversation.
    ///
    /// This count includes all conversation entries (user messages, assistant responses, etc.)
    /// and helps users compare the relative completeness of each version.
    pub local_message_count: usize,

    /// The total number of messages in the remote version of the conversation.
    ///
    /// This count includes all conversation entries (user messages, assistant responses, etc.)
    /// and helps users compare the relative completeness of each version.
    pub remote_message_count: usize,

    /// A hash of the local conversation's content.
    ///
    /// This hash is used to detect whether the local and remote versions are truly different.
    /// If the hashes match, the conversations are identical despite any metadata differences.
    pub local_hash: String,

    /// A hash of the remote conversation's content.
    ///
    /// This hash is used to detect whether the local and remote versions are truly different.
    /// If the hashes match, the conversations are identical despite any metadata differences.
    pub remote_hash: String,

    /// The current resolution status of the conflict.
    ///
    /// Initially set to `ConflictResolution::Pending` when a conflict is detected.
    /// Updated to one of the other variants once the user or system decides how to
    /// resolve the conflict.
    pub resolution: ConflictResolution,
}

/// Represents the resolution strategy for a conversation conflict.
///
/// When a conflict is detected between local and remote versions of the same conversation,
/// the user or system must choose how to resolve it. This enum captures the different
/// resolution strategies available.
///
/// # Resolution Strategies
///
/// - **KeepBoth**: Preserves both versions by renaming the remote file to avoid overwriting
/// - **KeepLocal**: Discards the remote version and keeps only the local version
/// - **KeepRemote**: Discards the local version and keeps only the remote version
/// - **Pending**: No resolution has been chosen yet (default state for new conflicts)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConflictResolution {
    /// Keep both versions by renaming the remote file with a conflict suffix.
    ///
    /// This strategy preserves both the local and remote versions of the conversation.
    /// The local file retains its original name and location, while the remote file
    /// is renamed to include a conflict marker (typically a timestamp-based suffix)
    /// to prevent overwriting the local version.
    ///
    /// # Fields
    ///
    /// * `renamed_remote_file` - The new path where the remote file will be saved.
    ///   This path includes a conflict suffix to distinguish it from the local version.
    ///
    /// # Example
    ///
    /// If the local file is `conversation.jsonl` and a conflict is detected,
    /// the remote version might be saved as `conversation-conflict-20250122-143000.jsonl`.
    KeepBoth {
        /// The destination path for the renamed remote file, including the conflict suffix.
        renamed_remote_file: PathBuf
    },

    /// Keep only the local version and discard the remote version.
    ///
    /// This strategy assumes the local version is correct and the remote version
    /// should be ignored. The local file remains unchanged, and the remote version
    /// is not saved to disk.
    KeepLocal,

    /// Keep only the remote version and discard the local version.
    ///
    /// This strategy assumes the remote version is correct and should replace the
    /// local version. The local file will be overwritten with the remote content.
    KeepRemote,

    /// The conflict has not yet been resolved.
    ///
    /// This is the default state for newly detected conflicts. The user must choose
    /// one of the other resolution strategies before the conflict can be resolved.
    Pending,
}

impl Conflict {
    /// Creates a new `Conflict` by comparing local and remote conversation sessions.
    ///
    /// This function constructs a conflict record from two versions of the same conversation
    /// session (identified by matching session IDs). It extracts and stores relevant metadata
    /// from both versions, including timestamps, message counts, and content hashes.
    ///
    /// The conflict is initialized with a `Pending` resolution status, indicating that no
    /// resolution strategy has been chosen yet.
    ///
    /// # Arguments
    ///
    /// * `local` - A reference to the local version of the conversation session
    /// * `remote` - A reference to the remote version of the conversation session
    ///
    /// # Returns
    ///
    /// A new `Conflict` instance containing metadata from both conversation versions,
    /// with the resolution status set to `Pending`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use claude_sync::conflict::Conflict;
    /// use claude_sync::parser::ConversationSession;
    ///
    /// # fn example(local_session: &ConversationSession, remote_session: &ConversationSession) {
    /// let conflict = Conflict::new(local_session, remote_session);
    ///
    /// println!("Conflict in session: {}", conflict.session_id);
    /// println!("Local messages: {}", conflict.local_message_count);
    /// println!("Remote messages: {}", conflict.remote_message_count);
    /// # }
    /// ```
    pub fn new(local: &ConversationSession, remote: &ConversationSession) -> Self {
        Conflict {
            session_id: local.session_id.clone(),
            local_file: PathBuf::from(&local.file_path),
            remote_file: PathBuf::from(&remote.file_path),
            local_timestamp: local.latest_timestamp(),
            remote_timestamp: remote.latest_timestamp(),
            local_message_count: local.message_count(),
            remote_message_count: remote.message_count(),
            local_hash: local.content_hash(),
            remote_hash: remote.content_hash(),
            resolution: ConflictResolution::Pending,
        }
    }

    /// Resolve the conflict by keeping both versions
    pub fn resolve_keep_both(&mut self, conflict_suffix: &str) -> Result<PathBuf> {
        let remote_file_name = self
            .remote_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        let remote_file_ext = self
            .remote_file
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("jsonl");

        let parent = self.remote_file.parent().unwrap_or_else(|| Path::new("."));

        let new_name = format!(
            "{}-{}.{}",
            remote_file_name, conflict_suffix, remote_file_ext
        );
        let renamed_path = parent.join(new_name);

        self.resolution = ConflictResolution::KeepBoth {
            renamed_remote_file: renamed_path.clone(),
        };

        Ok(renamed_path)
    }

    /// Get a human-readable description of the conflict
    pub fn description(&self) -> String {
        format!(
            "Session {} has diverged:\n  Local: {} messages, last update: {}\n  Remote: {} messages, last update: {}",
            self.session_id,
            self.local_message_count,
            self.local_timestamp.as_deref().unwrap_or("unknown"),
            self.remote_message_count,
            self.remote_timestamp.as_deref().unwrap_or("unknown")
        )
    }

    /// Determine if this is a real conflict (different content)
    pub fn is_real_conflict(&self) -> bool {
        self.local_hash != self.remote_hash
    }
}

/// Conflict detector for conversation sessions
pub struct ConflictDetector {
    conflicts: Vec<Conflict>,
}

impl ConflictDetector {
    /// Creates a new `ConflictDetector` with an empty conflict list.
    ///
    /// The conflict detector is used to identify and manage conflicts between local and remote
    /// conversation sessions. It maintains a list of detected conflicts and provides methods
    /// to resolve them according to different strategies.
    ///
    /// The detector starts with no conflicts; conflicts are added by calling the [`detect`]
    /// method with local and remote conversation sessions.
    ///
    /// [`detect`]: ConflictDetector::detect
    ///
    /// # Returns
    ///
    /// A new `ConflictDetector` instance with an empty internal conflict list, ready to
    /// detect and manage conflicts between conversation sessions.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use claude_sync::conflict::ConflictDetector;
    /// use claude_sync::parser::ConversationSession;
    ///
    /// # fn example(local_sessions: Vec<ConversationSession>, remote_sessions: Vec<ConversationSession>) {
    /// let mut detector = ConflictDetector::new();
    ///
    /// // Detect conflicts between local and remote sessions
    /// detector.detect(&local_sessions, &remote_sessions);
    ///
    /// if detector.has_conflicts() {
    ///     println!("Found {} conflicts", detector.conflict_count());
    /// }
    /// # }
    /// ```
    ///
    /// # See Also
    ///
    /// * [`detect`] - Method to scan for conflicts between local and remote sessions
    /// * [`has_conflicts`] - Check if any conflicts have been detected
    /// * [`conflict_count`] - Get the number of detected conflicts
    ///
    /// [`detect`]: ConflictDetector::detect
    /// [`has_conflicts`]: ConflictDetector::has_conflicts
    /// [`conflict_count`]: ConflictDetector::conflict_count
    pub fn new() -> Self {
        ConflictDetector {
            conflicts: Vec::new(),
        }
    }

    /// Compare local and remote sessions and detect conflicts
    pub fn detect(
        &mut self,
        local_sessions: &[ConversationSession],
        remote_sessions: &[ConversationSession],
    ) {
        // Build a map of session_id -> local session
        let local_map: std::collections::HashMap<_, _> = local_sessions
            .iter()
            .map(|s| (s.session_id.clone(), s))
            .collect();

        // Check each remote session against local
        for remote in remote_sessions {
            if let Some(local) = local_map.get(&remote.session_id) {
                // Session exists in both - check for conflicts
                if local.content_hash() != remote.content_hash() {
                    let conflict = Conflict::new(local, remote);
                    if conflict.is_real_conflict() {
                        self.conflicts.push(conflict);
                    }
                }
            }
        }
    }

    /// Resolve all conflicts using the "keep both" strategy
    pub fn resolve_all_keep_both(&mut self) -> Result<Vec<(PathBuf, PathBuf)>> {
        let mut renames = Vec::new();

        for conflict in &mut self.conflicts {
            let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
            let conflict_suffix = format!("conflict-{}", timestamp);

            let renamed_path = conflict.resolve_keep_both(&conflict_suffix)?;
            renames.push((conflict.remote_file.clone(), renamed_path));
        }

        Ok(renames)
    }

    /// Get all detected conflicts
    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
    }

    /// Get mutable reference to all detected conflicts
    pub fn conflicts_mut(&mut self) -> &mut [Conflict] {
        &mut self.conflicts
    }

    /// Check if any conflicts were detected
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }

    /// Get count of conflicts
    pub fn conflict_count(&self) -> usize {
        self.conflicts.len()
    }
}

impl Default for ConflictDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ConversationEntry;

    fn create_test_session(session_id: &str, message_count: usize) -> ConversationSession {
        let mut entries = Vec::new();

        for i in 0..message_count {
            entries.push(ConversationEntry {
                entry_type: if i % 2 == 0 { "user" } else { "assistant" }.to_string(),
                uuid: Some(format!("uuid-{}", i)),
                parent_uuid: if i > 0 {
                    Some(format!("uuid-{}", i - 1))
                } else {
                    None
                },
                session_id: Some(session_id.to_string()),
                timestamp: Some(format!("2025-01-01T{:02}:00:00Z", i)),
                message: None,
                cwd: None,
                version: None,
                git_branch: None,
                extra: serde_json::Value::Null,
            });
        }

        ConversationSession {
            session_id: session_id.to_string(),
            entries,
            file_path: format!("/test/{}.jsonl", session_id),
        }
    }

    #[test]
    fn test_conflict_detection() {
        let local_session = create_test_session("session-1", 5);
        let remote_session = create_test_session("session-1", 6);

        let mut detector = ConflictDetector::new();
        detector.detect(&[local_session], &[remote_session]);

        assert!(detector.has_conflicts());
        assert_eq!(detector.conflict_count(), 1);

        let conflict = &detector.conflicts()[0];
        assert_eq!(conflict.session_id, "session-1");
        assert_eq!(conflict.local_message_count, 5);
        assert_eq!(conflict.remote_message_count, 6);
    }

    #[test]
    fn test_no_conflict_same_content() {
        let local_session = create_test_session("session-1", 5);
        let remote_session = create_test_session("session-1", 5);

        let mut detector = ConflictDetector::new();
        detector.detect(&[local_session], &[remote_session]);

        assert!(!detector.has_conflicts());
    }
}
