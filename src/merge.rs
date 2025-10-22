use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};

use crate::parser::{ConversationEntry, ConversationSession};

/// Represents a node in the conversation message tree.
///
/// Each node contains a conversation entry and can have multiple children,
/// allowing for branching conversations where a single message has multiple
/// different continuations (e.g., when edited on different machines).
#[derive(Debug, Clone)]
pub struct MessageNode {
    /// The conversation entry for this node
    pub entry: ConversationEntry,

    /// Child nodes (messages that have this message as their parent)
    pub children: Vec<MessageNode>,
}

impl MessageNode {
    /// Creates a new message node with no children
    fn new(entry: ConversationEntry) -> Self {
        MessageNode {
            entry,
            children: Vec::new(),
        }
    }

    /// Adds a child node to this message
    fn add_child(&mut self, child: MessageNode) {
        self.children.push(child);
    }

    /// Recursively collects all entries in this subtree in depth-first order
    fn collect_entries(&self) -> Vec<ConversationEntry> {
        let mut entries = vec![self.entry.clone()];

        // Sort children by timestamp to maintain chronological order
        let mut sorted_children = self.children.clone();
        sorted_children.sort_by(|a, b| {
            let a_ts = a.entry.timestamp.as_ref();
            let b_ts = b.entry.timestamp.as_ref();
            a_ts.cmp(&b_ts)
        });

        for child in &sorted_children {
            entries.extend(child.collect_entries());
        }

        entries
    }
}

/// Result of a smart merge operation
#[derive(Debug)]
pub struct MergeResult {
    /// The merged conversation entries
    pub merged_entries: Vec<ConversationEntry>,

    /// Statistics about the merge
    pub stats: MergeStats,
}

/// Statistics about a merge operation
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MergeStats {
    /// Number of messages from local
    pub local_messages: usize,

    /// Number of messages from remote
    pub remote_messages: usize,

    /// Number of messages in merged result
    pub merged_messages: usize,

    /// Number of duplicate messages detected
    pub duplicates_removed: usize,

    /// Number of edited messages detected and resolved
    pub edits_resolved: usize,

    /// Number of conversation branches detected
    pub branches_detected: usize,

    /// Number of entries merged by timestamp (non-UUID entries)
    pub timestamp_merged: usize,
}

/// Smart merger for combining conversation sessions
pub struct SmartMerger<'a> {
    local: &'a ConversationSession,
    remote: &'a ConversationSession,
    stats: MergeStats,
}

impl<'a> SmartMerger<'a> {
    /// Creates a new smart merger for the given sessions
    pub fn new(local: &'a ConversationSession, remote: &'a ConversationSession) -> Self {
        SmartMerger {
            local,
            remote,
            stats: MergeStats::default(),
        }
    }

    /// Performs the smart merge and returns the result
    pub fn merge(&mut self) -> Result<MergeResult> {
        // Count initial messages
        self.stats.local_messages = self.local.message_count();
        self.stats.remote_messages = self.remote.message_count();

        // Build UUID maps for both sessions
        let local_map = self.build_uuid_map(&self.local.entries);
        let remote_map = self.build_uuid_map(&self.remote.entries);

        // Detect and resolve edits (same UUID, different content)
        let resolved_edits = self.detect_and_resolve_edits(&local_map, &remote_map)?;

        // Separate entries into UUID-tracked and non-UUID entries
        let (local_uuid_entries, local_non_uuid): (Vec<_>, Vec<_>) = self.local.entries
            .iter()
            .partition(|e| e.uuid.is_some());

        let (remote_uuid_entries, remote_non_uuid): (Vec<_>, Vec<_>) = self.remote.entries
            .iter()
            .partition(|e| e.uuid.is_some());

        // Build message trees for UUID-tracked entries
        let local_roots = self.build_tree(&local_uuid_entries, &resolved_edits)?;
        let remote_roots = self.build_tree(&remote_uuid_entries, &resolved_edits)?;

        // Merge the trees
        let merged_roots = self.merge_trees(local_roots, remote_roots)?;

        // Flatten tree back to entries
        let mut merged_entries = Vec::new();
        for root in &merged_roots {
            merged_entries.extend(root.collect_entries());
        }

        // Merge non-UUID entries by timestamp
        let non_uuid_merged = self.merge_by_timestamp(
            &local_non_uuid.into_iter().cloned().collect(),
            &remote_non_uuid.into_iter().cloned().collect(),
        );

        self.stats.timestamp_merged = non_uuid_merged.len();

        // Combine UUID-based and timestamp-based entries, sorted by timestamp
        merged_entries.extend(non_uuid_merged);
        merged_entries.sort_by(|a, b| {
            let a_ts = a.timestamp.as_ref();
            let b_ts = b.timestamp.as_ref();
            a_ts.cmp(&b_ts)
        });

        self.stats.merged_messages = merged_entries.len();

        Ok(MergeResult {
            merged_entries,
            stats: self.stats.clone(),
        })
    }

    /// Builds a UUID to entry map
    fn build_uuid_map(&self, entries: &[ConversationEntry]) -> HashMap<String, ConversationEntry> {
        entries
            .iter()
            .filter_map(|e| {
                e.uuid.as_ref().map(|uuid| (uuid.clone(), e.clone()))
            })
            .collect()
    }

    /// Detects edits (same UUID, different content) and resolves them by timestamp
    fn detect_and_resolve_edits(
        &mut self,
        local_map: &HashMap<String, ConversationEntry>,
        remote_map: &HashMap<String, ConversationEntry>,
    ) -> Result<HashMap<String, ConversationEntry>> {
        let mut resolved = HashMap::new();

        // Find all UUIDs that exist in both maps
        let common_uuids: HashSet<_> = local_map
            .keys()
            .filter(|uuid| remote_map.contains_key(*uuid))
            .collect();

        for uuid in common_uuids {
            let local_entry = &local_map[uuid];
            let remote_entry = &remote_map[uuid];

            // Compare content to detect edits
            let local_json = serde_json::to_string(local_entry)?;
            let remote_json = serde_json::to_string(remote_entry)?;

            if local_json != remote_json {
                // Edit detected - resolve by timestamp
                self.stats.edits_resolved += 1;

                let chosen = self.resolve_by_timestamp(local_entry, remote_entry);
                resolved.insert(uuid.clone(), chosen.clone());
            } else {
                // Same content, just add one copy
                resolved.insert(uuid.clone(), local_entry.clone());
            }
        }

        Ok(resolved)
    }

    /// Resolves an edit conflict by choosing the entry with the newer timestamp
    fn resolve_by_timestamp<'b>(
        &self,
        local: &'b ConversationEntry,
        remote: &'b ConversationEntry,
    ) -> &'b ConversationEntry {
        match (&local.timestamp, &remote.timestamp) {
            (Some(local_ts), Some(remote_ts)) => {
                if remote_ts > local_ts {
                    remote
                } else {
                    local
                }
            }
            (Some(_), None) => local,
            (None, Some(_)) => remote,
            (None, None) => local, // Fallback to local if no timestamps
        }
    }

    /// Builds a message tree from entries
    fn build_tree(
        &mut self,
        entries: &[&ConversationEntry],
        resolved_edits: &HashMap<String, ConversationEntry>,
    ) -> Result<Vec<MessageNode>> {
        // Build UUID map, preferring resolved edits
        let mut uuid_to_entry: HashMap<String, ConversationEntry> = HashMap::new();

        for entry in entries {
            if let Some(uuid) = &entry.uuid {
                // Use resolved edit if available, otherwise use original entry
                let entry_to_use = resolved_edits
                    .get(uuid)
                    .unwrap_or(*entry);
                uuid_to_entry.insert(uuid.clone(), entry_to_use.clone());
            }
        }

        // Track which UUIDs have been used as children
        let mut used_as_child = HashSet::new();

        // Map parent UUID -> list of child nodes
        let mut parent_to_children: HashMap<Option<String>, Vec<MessageNode>> = HashMap::new();

        for entry in uuid_to_entry.values() {
            let node = MessageNode::new(entry.clone());

            parent_to_children
                .entry(entry.parent_uuid.clone())
                .or_default()
                .push(node);

            if let Some(parent_uuid) = &entry.parent_uuid {
                used_as_child.insert(parent_uuid.clone());
            }
        }

        // Build tree recursively
        fn build_subtree(
            parent_uuid: Option<String>,
            parent_to_children: &HashMap<Option<String>, Vec<MessageNode>>,
        ) -> Vec<MessageNode> {
            if let Some(children) = parent_to_children.get(&parent_uuid) {
                children
                    .iter()
                    .map(|child| {
                        let mut node = child.clone();
                        let child_uuid = child.entry.uuid.clone();
                        node.children = build_subtree(child_uuid, parent_to_children);
                        node
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }

        // Find root nodes (entries with no parent or parent not in this tree)
        let roots = build_subtree(None, &parent_to_children);

        // Also find orphaned subtrees (entries whose parent exists but isn't in our tree)
        let mut all_roots = roots;
        for (parent_uuid, children) in parent_to_children.iter() {
            if let Some(parent) = parent_uuid {
                if !uuid_to_entry.contains_key(parent) {
                    all_roots.extend(children.iter().map(|child| {
                        let mut node = child.clone();
                        let child_uuid = child.entry.uuid.clone();
                        node.children = build_subtree(child_uuid, &parent_to_children);
                        node
                    }));
                }
            }
        }

        Ok(all_roots)
    }

    /// Merges two message trees, keeping all branches
    fn merge_trees(
        &mut self,
        local_roots: Vec<MessageNode>,
        remote_roots: Vec<MessageNode>,
    ) -> Result<Vec<MessageNode>> {
        // Create a map of UUID -> MessageNode for efficient lookup
        let mut merged_nodes: HashMap<String, MessageNode> = HashMap::new();

        // Process all nodes from both trees
        fn collect_nodes(
            nodes: &[MessageNode],
            collected: &mut HashMap<String, MessageNode>,
        ) {
            for node in nodes {
                if let Some(uuid) = &node.entry.uuid {
                    collected.insert(uuid.clone(), node.clone());
                }
            }
        }

        collect_nodes(&local_roots, &mut merged_nodes);

        // Merge remote nodes, combining children where nodes have same UUID
        for remote_root in remote_roots {
            self.merge_node_into(&remote_root, &mut merged_nodes);
        }

        // Detect branches (nodes with multiple children)
        self.count_branches(&merged_nodes);

        // Extract root nodes
        let mut roots: Vec<MessageNode> = merged_nodes
            .values()
            .filter(|node| node.entry.parent_uuid.is_none())
            .cloned()
            .collect();

        // Sort roots by timestamp
        roots.sort_by(|a, b| {
            let a_ts = a.entry.timestamp.as_ref();
            let b_ts = b.entry.timestamp.as_ref();
            a_ts.cmp(&b_ts)
        });

        Ok(roots)
    }

    /// Merges a node into the existing tree
    fn merge_node_into(
        &mut self,
        node: &MessageNode,
        merged_nodes: &mut HashMap<String, MessageNode>,
    ) {
        if let Some(uuid) = &node.entry.uuid {
            // Collect children that need to be merged recursively
            let mut children_to_merge = Vec::new();

            if let Some(existing) = merged_nodes.get_mut(uuid) {
                // Node already exists, merge children
                for child in &node.children {
                    // Check if this child already exists
                    let child_exists = if let Some(child_uuid) = &child.entry.uuid {
                        existing.children.iter().any(|c| c.entry.uuid.as_ref() == Some(child_uuid))
                    } else {
                        false
                    };

                    if !child_exists {
                        // Add new child
                        existing.add_child(child.clone());
                    }

                    // Collect child for recursive merge
                    if child.entry.uuid.is_some() {
                        children_to_merge.push(child.clone());
                    }
                }
            } else {
                // New node, add it
                merged_nodes.insert(uuid.clone(), node.clone());

                // Collect all children for recursive merge
                for child in &node.children {
                    children_to_merge.push(child.clone());
                }
            }

            // Now recursively merge all collected children
            for child in &children_to_merge {
                self.merge_node_into(child, merged_nodes);
            }
        }
    }

    /// Counts the number of branches in the tree
    fn count_branches(&mut self, nodes: &HashMap<String, MessageNode>) {
        for node in nodes.values() {
            if node.children.len() > 1 {
                self.stats.branches_detected += 1;
            }
        }
    }

    /// Merges non-UUID entries by timestamp, removing duplicates
    fn merge_by_timestamp(
        &mut self,
        local: &Vec<ConversationEntry>,
        remote: &Vec<ConversationEntry>,
    ) -> Vec<ConversationEntry> {
        let mut all_entries = local.clone();
        all_entries.extend(remote.clone());

        // Sort by timestamp
        all_entries.sort_by(|a, b| {
            let a_ts = a.timestamp.as_ref();
            let b_ts = b.timestamp.as_ref();
            a_ts.cmp(&b_ts)
        });

        // Remove duplicates by comparing JSON representation
        let mut seen = HashSet::new();
        let mut unique_entries = Vec::new();

        for entry in all_entries {
            if let Ok(json) = serde_json::to_string(&entry) {
                if seen.insert(json.clone()) {
                    unique_entries.push(entry);
                } else {
                    self.stats.duplicates_removed += 1;
                }
            }
        }

        unique_entries
    }
}

/// Attempts to perform a smart merge on two conversation sessions
///
/// This is the main entry point for the smart merge feature. It will attempt
/// to intelligently combine messages from both conversations, handling:
/// - Non-overlapping messages (simple merge)
/// - Edited messages (resolved by timestamp)
/// - Conversation branches (all branches preserved)
/// - Entries without UUIDs (merged by timestamp)
///
/// # Arguments
///
/// * `local` - The local conversation session
/// * `remote` - The remote conversation session
///
/// # Returns
///
/// Returns `Ok(MergeResult)` if merge succeeds, or an error if the merge
/// cannot be completed (e.g., due to corrupted data or circular references).
pub fn merge_conversations(
    local: &ConversationSession,
    remote: &ConversationSession,
) -> Result<MergeResult> {
    // Validate sessions have same session ID
    if local.session_id != remote.session_id {
        return Err(anyhow!(
            "Cannot merge conversations with different session IDs: {} vs {}",
            local.session_id,
            remote.session_id
        ));
    }

    let mut merger = SmartMerger::new(local, remote);
    merger.merge()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_test_entry(uuid: &str, parent_uuid: Option<&str>, timestamp: &str) -> ConversationEntry {
        ConversationEntry {
            entry_type: "user".to_string(),
            uuid: Some(uuid.to_string()),
            parent_uuid: parent_uuid.map(|s| s.to_string()),
            session_id: Some("test-session".to_string()),
            timestamp: Some(timestamp.to_string()),
            message: Some(json!({"text": format!("Message {}", uuid)})),
            cwd: None,
            version: None,
            git_branch: None,
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn test_merge_non_overlapping_messages() {
        let local_entries = vec![
            create_test_entry("1", None, "2025-01-01T00:00:00Z"),
            create_test_entry("2", Some("1"), "2025-01-01T00:01:00Z"),
        ];

        let remote_entries = vec![
            create_test_entry("3", Some("2"), "2025-01-01T00:02:00Z"),
            create_test_entry("4", Some("3"), "2025-01-01T00:03:00Z"),
        ];

        let local = ConversationSession {
            session_id: "test-session".to_string(),
            entries: local_entries,
            file_path: "local.jsonl".to_string(),
        };

        let remote = ConversationSession {
            session_id: "test-session".to_string(),
            entries: remote_entries,
            file_path: "remote.jsonl".to_string(),
        };

        let result = merge_conversations(&local, &remote).unwrap();

        // Should have all 4 messages
        assert_eq!(result.merged_entries.len(), 4);
        assert_eq!(result.stats.merged_messages, 4);
    }

    #[test]
    fn test_merge_with_branches() {
        // Local: 1 -> 2 -> 3
        let local_entries = vec![
            create_test_entry("1", None, "2025-01-01T00:00:00Z"),
            create_test_entry("2", Some("1"), "2025-01-01T00:01:00Z"),
            create_test_entry("3", Some("2"), "2025-01-01T00:02:00Z"),
        ];

        // Remote: 1 -> 2 -> 4 (different continuation from message 2)
        let remote_entries = vec![
            create_test_entry("1", None, "2025-01-01T00:00:00Z"),
            create_test_entry("2", Some("1"), "2025-01-01T00:01:00Z"),
            create_test_entry("4", Some("2"), "2025-01-01T00:02:30Z"),
        ];

        let local = ConversationSession {
            session_id: "test-session".to_string(),
            entries: local_entries,
            file_path: "local.jsonl".to_string(),
        };

        let remote = ConversationSession {
            session_id: "test-session".to_string(),
            entries: remote_entries,
            file_path: "remote.jsonl".to_string(),
        };

        let result = merge_conversations(&local, &remote).unwrap();

        // Should detect branch
        assert!(result.stats.branches_detected > 0);

        // Should have 1, 2, 3, and 4
        assert_eq!(result.merged_entries.len(), 4);
    }

    #[test]
    fn test_edit_resolution_by_timestamp() {
        // Same message edited in both places
        let mut local_entry = create_test_entry("1", None, "2025-01-01T00:00:00Z");
        local_entry.message = Some(json!({"text": "Local version"}));

        let mut remote_entry = create_test_entry("1", None, "2025-01-01T00:01:00Z");
        remote_entry.message = Some(json!({"text": "Remote version (newer)"}));

        let local = ConversationSession {
            session_id: "test-session".to_string(),
            entries: vec![local_entry],
            file_path: "local.jsonl".to_string(),
        };

        let remote = ConversationSession {
            session_id: "test-session".to_string(),
            entries: vec![remote_entry],
            file_path: "remote.jsonl".to_string(),
        };

        let result = merge_conversations(&local, &remote).unwrap();

        // Should detect and resolve one edit
        assert_eq!(result.stats.edits_resolved, 1);

        // Should keep only the newer version (remote)
        assert_eq!(result.merged_entries.len(), 1);
        assert_eq!(
            result.merged_entries[0].message,
            Some(json!({"text": "Remote version (newer)"}))
        );
    }
}
