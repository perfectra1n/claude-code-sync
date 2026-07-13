//! Regression tests for issue #68: pushing twice with no local changes must
//! report every session as Unchanged, including subagent sidechain transcripts
//! that carry their parent session's interior `sessionId`.

use std::fs;
use std::io::Write;
use std::path::Path;

use claude_code_sync::filter::FilterConfig;
use claude_code_sync::sync::discovery::discover_sessions;
use claude_code_sync::sync::push::plan_push;
use tempfile::TempDir;

const PARENT_SESSION_ID: &str = "56d02190-2a2d-4a55-9ec1-38e34fb25e84";

fn write_transcript(path: &Path, session_id: &str, marker: &str) {
    let mut file = fs::File::create(path).unwrap();
    writeln!(
        file,
        r#"{{"type":"user","sessionId":"{session_id}","uuid":"{marker}-1","timestamp":"2025-01-01T00:00:00Z","cwd":"/home/user/myproj","message":{{"text":"{marker} hello"}}}}"#
    )
    .unwrap();
    writeln!(
        file,
        r#"{{"type":"assistant","sessionId":"{session_id}","uuid":"{marker}-2","timestamp":"2025-01-01T00:01:00Z","message":{{"text":"{marker} reply"}}}}"#
    )
    .unwrap();
}

/// Build a claude-projects tree where a parent transcript and two subagent
/// transcripts all share one interior sessionId (as Claude Code produces).
fn seed_claude_projects(claude_dir: &Path) {
    let project = claude_dir.join("-home-user-myproj");
    let subagents = project.join(PARENT_SESSION_ID).join("subagents");
    fs::create_dir_all(&subagents).unwrap();

    write_transcript(
        &project.join(format!("{PARENT_SESSION_ID}.jsonl")),
        PARENT_SESSION_ID,
        "parent",
    );
    write_transcript(
        &subagents.join("agent-a16263cbf10e1ad0b.jsonl"),
        PARENT_SESSION_ID,
        "sub-a",
    );
    write_transcript(
        &subagents.join("agent-b7f2915d40c88e221.jsonl"),
        PARENT_SESSION_ID,
        "sub-b",
    );
}

#[test]
fn test_second_push_plan_is_all_unchanged_despite_shared_session_id() {
    let claude = TempDir::new().unwrap();
    let repo_projects = TempDir::new().unwrap();
    seed_claude_projects(claude.path());
    let filter = FilterConfig::default();

    let sessions = discover_sessions(claude.path(), &filter).unwrap();
    assert_eq!(sessions.len(), 3, "parent + two subagents discovered");

    // First push: everything is new.
    let plan1 = plan_push(&sessions, claude.path(), repo_projects.path(), &filter).unwrap();
    assert_eq!(plan1.added, 3);
    assert_eq!(plan1.modified, 0);
    assert_eq!(plan1.unchanged, 0);

    // Apply the plan the way push_history does.
    for entry in &plan1.entries {
        let dest = repo_projects.path().join(&entry.relative_path);
        sessions[entry.session_index].write_to_file(&dest).unwrap();
    }

    // Second push with no local changes: issue #68 reported these as Modified
    // because all three files collapsed onto one sessionId key.
    let sessions2 = discover_sessions(claude.path(), &filter).unwrap();
    let plan2 = plan_push(&sessions2, claude.path(), repo_projects.path(), &filter).unwrap();
    assert_eq!(plan2.added, 0, "second push must add nothing");
    assert_eq!(
        plan2.modified, 0,
        "second push must modify nothing (issue #68)"
    );
    assert_eq!(plan2.unchanged, 3, "all three files are unchanged");
}

#[test]
fn test_push_plan_detects_real_modification() {
    let claude = TempDir::new().unwrap();
    let repo_projects = TempDir::new().unwrap();
    seed_claude_projects(claude.path());
    let filter = FilterConfig::default();

    let sessions = discover_sessions(claude.path(), &filter).unwrap();
    let plan1 = plan_push(&sessions, claude.path(), repo_projects.path(), &filter).unwrap();
    for entry in &plan1.entries {
        let dest = repo_projects.path().join(&entry.relative_path);
        sessions[entry.session_index].write_to_file(&dest).unwrap();
    }

    // Append a new message to ONE subagent transcript only.
    let subagent_path = claude
        .path()
        .join("-home-user-myproj")
        .join(PARENT_SESSION_ID)
        .join("subagents")
        .join("agent-a16263cbf10e1ad0b.jsonl");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&subagent_path)
        .unwrap();
    writeln!(
        file,
        r#"{{"type":"assistant","sessionId":"{PARENT_SESSION_ID}","uuid":"sub-a-3","timestamp":"2025-01-01T00:02:00Z","message":{{"text":"more"}}}}"#
    )
    .unwrap();

    let sessions2 = discover_sessions(claude.path(), &filter).unwrap();
    let plan2 = plan_push(&sessions2, claude.path(), repo_projects.path(), &filter).unwrap();
    assert_eq!(plan2.added, 0);
    assert_eq!(
        plan2.modified, 1,
        "only the appended transcript is modified"
    );
    assert_eq!(plan2.unchanged, 2);
}
