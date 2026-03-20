use claude_code_sync::sync::merge_settings_json;
use serde_json::{json, Map, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TS_KEY: &str = "lastModifiedTimestamp";

/// Build a SystemTime from a Unix millisecond timestamp — convenience for tests.
fn sys_time_ms(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms as u64)
}

fn map(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

/// Current time as Unix milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Superset rules
// ---------------------------------------------------------------------------

#[test]
fn test_local_only_key_is_preserved() {
    // A key that exists locally but not remotely must survive in the merged output.
    let local = map(json!({ "theme": "dark" }));
    let remote = map(json!({}));
    let (merged, changed) = merge_settings_json(&local, &remote, SystemTime::now());

    assert_eq!(merged["theme"], "dark", "local-only key must be in merged");
    assert!(changed, "adding a local key should mark result as changed");
}

#[test]
fn test_remote_only_key_is_preserved() {
    // A key that exists only in remote must survive in the merged output.
    let local = map(json!({}));
    let remote = map(json!({ "fontSize": 14 }));
    let (merged, _) = merge_settings_json(&local, &remote, SystemTime::now());

    assert_eq!(merged["fontSize"], 14, "remote-only key must be in merged");
}

#[test]
fn test_superset_contains_all_keys() {
    // Both local-only AND remote-only keys must appear in the merged output.
    let local = map(json!({ "theme": "dark" }));
    let remote = map(json!({ "fontSize": 14 }));
    let (merged, changed) = merge_settings_json(&local, &remote, SystemTime::now());

    assert!(merged.contains_key("theme"), "local key missing from merged");
    assert!(merged.contains_key("fontSize"), "remote key missing from merged");
    assert!(changed);
}

// ---------------------------------------------------------------------------
// Same-value keys — no conflict
// ---------------------------------------------------------------------------

#[test]
fn test_same_value_is_unchanged() {
    let local = map(json!({ "theme": "dark" }));
    let remote = map(json!({ "theme": "dark" }));
    let (merged, _) = merge_settings_json(&local, &remote, SystemTime::now());

    assert_eq!(merged["theme"], "dark");
}

// ---------------------------------------------------------------------------
// Conflict resolution by timestamp
// ---------------------------------------------------------------------------

#[test]
fn test_conflict_local_newer_wins() {
    // Local file is newer than the remote's lastModifiedTimestamp → local value wins.
    let remote_ts_ms: i64 = 1_000_000; // old
    let local_mtime = sys_time_ms(2_000_000); // newer

    let local = map(json!({ "theme": "dark" }));
    let remote = map(json!({
        "theme": "light",
        TS_KEY: remote_ts_ms,
    }));
    let (merged, changed) = merge_settings_json(&local, &remote, local_mtime);

    assert_eq!(merged["theme"], "dark", "local should win when mtime is newer");
    assert!(changed);
}

#[test]
fn test_conflict_remote_newer_wins() {
    // Remote's lastModifiedTimestamp is newer than local file mtime → remote value wins.
    let remote_ts_ms: i64 = 2_000_000; // newer
    let local_mtime = sys_time_ms(1_000_000); // older

    let local = map(json!({ "theme": "dark" }));
    let remote = map(json!({
        "theme": "light",
        TS_KEY: remote_ts_ms,
    }));
    let (merged, changed) = merge_settings_json(&local, &remote, local_mtime);

    assert_eq!(merged["theme"], "light", "remote should win when its timestamp is newer");
    assert!(!changed, "if remote wins every conflict the result equals remote (modulo timestamp key)");
}

#[test]
fn test_conflict_no_remote_timestamp_local_wins() {
    // Remote has no lastModifiedTimestamp → local value wins by default.
    let local = map(json!({ "theme": "dark" }));
    let remote = map(json!({ "theme": "light" })); // no TS_KEY
    let (merged, changed) = merge_settings_json(&local, &remote, SystemTime::now());

    assert_eq!(merged["theme"], "dark", "local should win when remote has no timestamp");
    assert!(changed);
}

// ---------------------------------------------------------------------------
// lastModifiedTimestamp handling
// ---------------------------------------------------------------------------

#[test]
fn test_timestamp_key_present_in_merged_output() {
    // The merged output must always carry a lastModifiedTimestamp as an integer.
    let local = map(json!({}));
    let remote = map(json!({}));
    let (merged, _) = merge_settings_json(&local, &remote, SystemTime::now());

    assert!(
        merged.contains_key(TS_KEY),
        "merged output must contain lastModifiedTimestamp"
    );
    // It should be a non-negative integer (Unix milliseconds).
    let ts = merged[TS_KEY].as_i64().expect("timestamp should be an integer");
    assert!(ts > 0, "timestamp should be a positive Unix millisecond value");
}

#[test]
fn test_timestamp_key_is_not_subject_to_conflict_resolution() {
    // lastModifiedTimestamp in the merged output should be the *current* time,
    // not copied from local or remote wholesale.
    let before_ms = now_ms() - 5_000; // 5 seconds ago

    let local = map(json!({ TS_KEY: 1_000i64 }));
    let remote = map(json!({ TS_KEY: 2_000i64 }));
    let (merged, _) = merge_settings_json(&local, &remote, SystemTime::now());

    let merged_ts = merged[TS_KEY].as_i64().unwrap();
    assert!(
        merged_ts > before_ms,
        "merged timestamp should be recent (set at merge time), not copied from local/remote"
    );
}

// ---------------------------------------------------------------------------
// changed_vs_remote flag
// ---------------------------------------------------------------------------

#[test]
fn test_changed_flag_true_when_local_key_added() {
    let local = map(json!({ "extraKey": true }));
    let remote = map(json!({}));
    let (_, changed) = merge_settings_json(&local, &remote, SystemTime::now());

    assert!(changed, "adding a local-only key should set changed=true");
}

#[test]
fn test_changed_flag_false_when_remote_wins_all_conflicts() {
    // When the remote is strictly newer and all conflicting values adopt the remote's version,
    // changed_vs_remote should be false (the sync repo does not need updating).
    let remote_ts_ms: i64 = i64::MAX; // far future
    let local_mtime = sys_time_ms(1_000_000); // old

    let local = map(json!({ "theme": "dark" }));
    let remote = map(json!({
        "theme": "light",
        TS_KEY: remote_ts_ms,
    }));
    let (merged, changed) = merge_settings_json(&local, &remote, local_mtime);

    assert_eq!(merged["theme"], "light");
    assert!(
        !changed,
        "when remote wins every conflict, changed should be false"
    );
}
