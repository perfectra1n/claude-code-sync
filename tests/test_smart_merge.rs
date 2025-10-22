use claude_code_sync::merge::merge_conversations;
use claude_code_sync::parser::{ConversationEntry, ConversationSession};
use serde_json::json;

/// Helper to create a test entry
fn create_entry(uuid: &str, parent: Option<&str>, timestamp: &str, content: &str) -> ConversationEntry {
    ConversationEntry {
        entry_type: "user".to_string(),
        uuid: Some(uuid.to_string()),
        parent_uuid: parent.map(|s| s.to_string()),
        session_id: Some("test-session".to_string()),
        timestamp: Some(timestamp.to_string()),
        message: Some(json!({"text": content})),
        cwd: None,
        version: None,
        git_branch: None,
        extra: serde_json::Value::Null,
    }
}

#[test]
fn test_simple_extension() {
    // Local: A -> B
    let local = ConversationSession {
        session_id: "test".to_string(),
        file_path: "local.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:00:00Z", "Message A"),
            create_entry("B", Some("A"), "2025-01-01T00:01:00Z", "Message B"),
        ],
    };

    // Remote: A -> B -> C -> D (extended conversation)
    let remote = ConversationSession {
        session_id: "test".to_string(),
        file_path: "remote.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:00:00Z", "Message A"),
            create_entry("B", Some("A"), "2025-01-01T00:01:00Z", "Message B"),
            create_entry("C", Some("B"), "2025-01-01T00:02:00Z", "Message C"),
            create_entry("D", Some("C"), "2025-01-01T00:03:00Z", "Message D"),
        ],
    };

    let result = merge_conversations(&local, &remote).unwrap();

    println!("Merged {} messages", result.merged_entries.len());
    println!("Stats: {:?}", result.stats);

    for (i, entry) in result.merged_entries.iter().enumerate() {
        println!("Entry {}: UUID={:?}, Parent={:?}",
            i, entry.uuid, entry.parent_uuid);
    }

    // Should have A, B, C, D = 4 messages
    assert_eq!(result.merged_entries.len(), 4, "Should have 4 messages after merge");
}

#[test]
fn test_conversation_branch() {
    // Local: A -> B -> C
    let local = ConversationSession {
        session_id: "test".to_string(),
        file_path: "local.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:00:00Z", "Message A"),
            create_entry("B", Some("A"), "2025-01-01T00:01:00Z", "Message B"),
            create_entry("C", Some("B"), "2025-01-01T00:02:00Z", "Message C - local branch"),
        ],
    };

    // Remote: A -> B -> D (different branch from B)
    let remote = ConversationSession {
        session_id: "test".to_string(),
        file_path: "remote.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:00:00Z", "Message A"),
            create_entry("B", Some("A"), "2025-01-01T00:01:00Z", "Message B"),
            create_entry("D", Some("B"), "2025-01-01T00:02:30Z", "Message D - remote branch"),
        ],
    };

    let result = merge_conversations(&local, &remote).unwrap();

    println!("Merged {} messages", result.merged_entries.len());
    println!("Stats: {:?}", result.stats);
    println!("Branches detected: {}", result.stats.branches_detected);

    // Should have A, B, C, D = 4 messages
    assert_eq!(result.merged_entries.len(), 4, "Should have 4 messages with both branches");

    // Should detect that B has two children
    assert!(result.stats.branches_detected > 0, "Should detect conversation branch");
}

#[test]
fn test_edited_message_resolution() {
    // Local: A with old timestamp
    let local = ConversationSession {
        session_id: "test".to_string(),
        file_path: "local.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:00:00Z", "Original message"),
        ],
    };

    // Remote: A with newer timestamp (edited)
    let remote = ConversationSession {
        session_id: "test".to_string(),
        file_path: "remote.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:05:00Z", "Edited message"),
        ],
    };

    let result = merge_conversations(&local, &remote).unwrap();

    // Should have 1 message (the edited one)
    assert_eq!(result.merged_entries.len(), 1);

    // Should have detected and resolved 1 edit
    assert_eq!(result.stats.edits_resolved, 1, "Should detect one edit");

    // Should keep the newer version
    let content = result.merged_entries[0].message.as_ref().unwrap()["text"].as_str().unwrap();
    assert_eq!(content, "Edited message", "Should keep newer version");
}

#[test]
fn test_non_overlapping_additions() {
    // Local adds C, Remote adds D
    // Local: A -> B -> C
    let local = ConversationSession {
        session_id: "test".to_string(),
        file_path: "local.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:00:00Z", "Message A"),
            create_entry("B", Some("A"), "2025-01-01T00:01:00Z", "Message B"),
            create_entry("C", Some("B"), "2025-01-01T00:02:00Z", "Local addition"),
        ],
    };

    // Remote: A -> B -> D
    let remote = ConversationSession {
        session_id: "test".to_string(),
        file_path: "remote.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:00:00Z", "Message A"),
            create_entry("B", Some("A"), "2025-01-01T00:01:00Z", "Message B"),
            create_entry("D", Some("B"), "2025-01-01T00:02:30Z", "Remote addition"),
        ],
    };

    let result = merge_conversations(&local, &remote).unwrap();

    // Both additions should be preserved as branches
    assert_eq!(result.merged_entries.len(), 4, "Should have both additions");
    assert!(result.stats.branches_detected > 0, "Should detect branch");
}

#[test]
fn test_real_conversation_data() {
    // Load real conversation data from test fixtures
    let test_file = "data/-tmp-test1/4dd02356-9e88-4c94-a858-20da56e2c0d3.jsonl";

    // If file doesn't exist, skip test
    if !std::path::Path::new(test_file).exists() {
        println!("Skipping test - test data not found");
        return;
    }

    let session = ConversationSession::from_file(test_file).unwrap();

    println!("Loaded real conversation with {} messages", session.message_count());

    // Simulate a scenario where one machine has the first half of messages
    // and another has extended it
    let midpoint = session.entries.len() / 2;

    let local = ConversationSession {
        session_id: session.session_id.clone(),
        file_path: "local.jsonl".to_string(),
        entries: session.entries[..midpoint].to_vec(),
    };

    let remote = ConversationSession {
        session_id: session.session_id.clone(),
        file_path: "remote.jsonl".to_string(),
        entries: session.entries.clone(),
    };

    let result = merge_conversations(&local, &remote).unwrap();

    println!("Merge result: {} messages", result.merged_entries.len());
    println!("Stats: {:?}", result.stats);

    // Should have all messages from the full session
    assert_eq!(
        result.merged_entries.len(),
        session.entries.len(),
        "Should preserve all messages from extended conversation"
    );
}

#[test]
fn test_complex_branching_scenario() {
    // Create a complex scenario with multiple branches
    //        A
    //       / \
    //      B1  B2
    //     /     \
    //    C1      C2
    //             \
    //              D2

    let local = ConversationSession {
        session_id: "test".to_string(),
        file_path: "local.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:00:00Z", "Root"),
            create_entry("B1", Some("A"), "2025-01-01T00:01:00Z", "Branch 1 from A"),
            create_entry("C1", Some("B1"), "2025-01-01T00:02:00Z", "Continuation of B1"),
        ],
    };

    let remote = ConversationSession {
        session_id: "test".to_string(),
        file_path: "remote.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:00:00Z", "Root"),
            create_entry("B2", Some("A"), "2025-01-01T00:01:30Z", "Branch 2 from A"),
            create_entry("C2", Some("B2"), "2025-01-01T00:02:30Z", "Continuation of B2"),
            create_entry("D2", Some("C2"), "2025-01-01T00:03:00Z", "Further continuation"),
        ],
    };

    let result = merge_conversations(&local, &remote).unwrap();

    println!("Complex branching result: {} messages", result.merged_entries.len());
    println!("Branches detected: {}", result.stats.branches_detected);

    // Should have all 6 unique messages
    assert_eq!(result.merged_entries.len(), 6, "Should have all unique messages");

    // Should detect branch at A (has B1 and B2 as children)
    assert!(result.stats.branches_detected > 0, "Should detect branching at A");

    // Verify all messages are present
    let uuids: Vec<String> = result.merged_entries.iter()
        .filter_map(|e| e.uuid.clone())
        .collect();

    for expected_uuid in &["A", "B1", "B2", "C1", "C2", "D2"] {
        assert!(
            uuids.contains(&expected_uuid.to_string()),
            "Should contain message {}",
            expected_uuid
        );
    }
}

#[test]
fn test_no_conflicts_when_identical() {
    // When both sides have identical conversations, merge should work seamlessly
    let entries = vec![
        create_entry("A", None, "2025-01-01T00:00:00Z", "Message A"),
        create_entry("B", Some("A"), "2025-01-01T00:01:00Z", "Message B"),
        create_entry("C", Some("B"), "2025-01-01T00:02:00Z", "Message C"),
    ];

    let local = ConversationSession {
        session_id: "test".to_string(),
        file_path: "local.jsonl".to_string(),
        entries: entries.clone(),
    };

    let remote = ConversationSession {
        session_id: "test".to_string(),
        file_path: "remote.jsonl".to_string(),
        entries: entries.clone(),
    };

    let result = merge_conversations(&local, &remote).unwrap();

    // Should have exact same messages (no duplicates)
    assert_eq!(result.merged_entries.len(), 3, "Should deduplicate identical messages");
    assert_eq!(result.stats.duplicates_removed, 0, "No duplicates to remove (deduplicated during merge)");
    assert_eq!(result.stats.branches_detected, 0, "No branches in linear conversation");
}

#[test]
fn test_mixed_uuid_and_non_uuid_entries() {
    // Test merging with both UUID-tracked messages and non-UUID system events
    let local = ConversationSession {
        session_id: "test".to_string(),
        file_path: "local.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:00:00Z", "User message"),
            ConversationEntry {
                entry_type: "file-history-snapshot".to_string(),
                uuid: None, // System events may not have UUIDs
                parent_uuid: None,
                session_id: Some("test".to_string()),
                timestamp: Some("2025-01-01T00:00:30Z".to_string()),
                message: Some(json!({"snapshot": "data"})),
                cwd: None,
                version: None,
                git_branch: None,
                extra: serde_json::Value::Null,
            },
            create_entry("B", Some("A"), "2025-01-01T00:01:00Z", "Assistant response"),
        ],
    };

    let remote = ConversationSession {
        session_id: "test".to_string(),
        file_path: "remote.jsonl".to_string(),
        entries: vec![
            create_entry("A", None, "2025-01-01T00:00:00Z", "User message"),
            create_entry("B", Some("A"), "2025-01-01T00:01:00Z", "Assistant response"),
            create_entry("C", Some("B"), "2025-01-01T00:02:00Z", "Continuation"),
        ],
    };

    let result = merge_conversations(&local, &remote).unwrap();

    println!("Mixed entries result: {} messages", result.merged_entries.len());
    println!("Timestamp-merged: {}", result.stats.timestamp_merged);

    // Should have A, snapshot, B, C = 4 entries
    assert_eq!(result.merged_entries.len(), 4, "Should merge UUID and non-UUID entries");
    assert!(result.stats.timestamp_merged > 0, "Should use timestamp merging for non-UUID entries");
}
