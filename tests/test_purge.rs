//! Integration tests for transcript retention: what ages out, what never does,
//! and what goes with a transcript when it does.

use std::fs;
use std::path::{Path, PathBuf};

use claude_code_sync::purge::{self, MINIMUM_RETENTION_DAYS};
use tempfile::TempDir;

/// A transcript whose last message is `age_days` old.
fn transcript(projects_dir: &Path, project: &str, session: &str, age_days: i64) -> PathBuf {
    let last_message = chrono::Utc::now() - chrono::Duration::days(age_days);
    write_transcript(
        projects_dir,
        project,
        session,
        &format!(
            "{{\"type\":\"user\",\"uuid\":\"u1\",\"sessionId\":\"{session}\",\
             \"cwd\":\"/home/user/app\",\"timestamp\":\"{}\"}}\n",
            last_message.to_rfc3339()
        ),
    )
}

fn write_transcript(projects_dir: &Path, project: &str, session: &str, body: &str) -> PathBuf {
    let dir = projects_dir.join(project);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    fs::write(&path, body).unwrap();
    path
}

fn purge_with_window(claude_projects: &Path, repo_projects: &Path, days: u32) -> usize {
    let plan = purge::plan(claude_projects, repo_projects, days).unwrap();
    let planned_bytes = plan.total_bytes();
    let report = purge::apply(&plan).unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(
        report.bytes, planned_bytes,
        "what was freed is what the plan said it would free"
    );
    report.sessions
}

#[test]
fn an_aged_out_transcript_goes_from_both_sides_at_once() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let local = transcript(claude.path(), "-home-user-app", "old-session", 400);
    let stored = transcript(repo.path(), "-home-user-app", "old-session", 400);

    let purged = purge_with_window(claude.path(), repo.path(), MINIMUM_RETENTION_DAYS);

    assert_eq!(purged, 1, "one session, both copies");
    assert!(!local.exists());
    assert!(!stored.exists());
}

#[test]
fn a_transcript_inside_the_window_is_untouched() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let recent = transcript(claude.path(), "-home-user-app", "recent", 179);

    let plan = purge::plan(claude.path(), repo.path(), MINIMUM_RETENTION_DAYS).unwrap();
    purge::apply(&plan).unwrap();

    assert!(plan.is_empty());
    assert!(recent.is_file());
}

#[test]
fn age_comes_from_the_last_message_not_the_files_mtime() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    // Exactly what a pull produces: an old conversation written to disk today.
    let pulled = transcript(claude.path(), "-home-user-app", "pulled-today", 400);
    let mtime = fs::metadata(&pulled).unwrap().modified().unwrap();
    assert!(
        mtime.elapsed().unwrap().as_secs() < 60,
        "the file itself is brand new"
    );

    let purged = purge_with_window(claude.path(), repo.path(), MINIMUM_RETENTION_DAYS);

    assert_eq!(purged, 1);
    assert!(!pulled.exists());
}

#[test]
fn a_transcript_with_no_timestamp_is_never_purged() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let undated = write_transcript(
        claude.path(),
        "-home-user-app",
        "undated",
        "{\"type\":\"user\",\"uuid\":\"u1\",\"sessionId\":\"undated\"}\n",
    );

    let plan = purge::plan(claude.path(), repo.path(), MINIMUM_RETENTION_DAYS).unwrap();
    purge::apply(&plan).unwrap();

    assert_eq!(plan.undated, 1);
    assert!(plan.is_empty());
    assert!(undated.is_file(), "an unknown age is not an old age");
}

#[test]
fn everything_claude_code_keeps_beside_a_transcript_goes_with_it() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let old = transcript(claude.path(), "-home-user-app", "old-session", 400);
    let project = old.parent().unwrap().to_path_buf();
    fs::create_dir_all(project.join("old-session/subagents")).unwrap();
    fs::write(project.join("old-session/subagents/sub.jsonl"), "{}\n").unwrap();
    fs::create_dir_all(project.join("old-session/tool-results")).unwrap();
    fs::write(project.join("old-session/tool-results/out.txt"), "output").unwrap();
    fs::write(project.join("old-session.orphaned-123-a.jsonl"), "{}\n").unwrap();
    fs::write(project.join("old-session.jsonl.superseded-123"), "{}\n").unwrap();
    // A different session in the same project must survive untouched.
    let keep = transcript(claude.path(), "-home-user-app", "recent", 10);

    purge_with_window(claude.path(), repo.path(), MINIMUM_RETENTION_DAYS);

    assert!(!old.exists());
    assert!(!project.join("old-session").exists());
    assert!(!project.join("old-session.orphaned-123-a.jsonl").exists());
    assert!(!project.join("old-session.jsonl.superseded-123").exists());
    assert!(keep.is_file());
}

#[test]
fn a_subagent_transcript_never_ages_out_on_its_own() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let parent = transcript(claude.path(), "-home-user-app", "parent", 10);
    let session_dir = parent.parent().unwrap().join("parent");
    fs::create_dir_all(session_dir.join("subagents")).unwrap();
    let subagent = session_dir.join("subagents/agent.jsonl");
    let long_ago = chrono::Utc::now() - chrono::Duration::days(400);
    fs::write(
        &subagent,
        format!(
            "{{\"type\":\"user\",\"uuid\":\"u9\",\"sessionId\":\"agent\",\
             \"timestamp\":\"{}\"}}\n",
            long_ago.to_rfc3339()
        ),
    )
    .unwrap();

    let plan = purge::plan(claude.path(), repo.path(), MINIMUM_RETENTION_DAYS).unwrap();
    purge::apply(&plan).unwrap();

    assert!(
        plan.is_empty(),
        "the parent session is well inside the window"
    );
    assert!(parent.is_file());
    assert!(
        subagent.is_file(),
        "a subagent belongs to its session, not to its own age"
    );
}

#[test]
fn a_session_used_recently_elsewhere_is_not_old_here() {
    // The same session, stale on one side and recent on the other: whichever
    // side is fresh keeps both copies.
    for (local_age, repo_age) in [(400, 3), (3, 400)] {
        let claude = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let local = transcript(claude.path(), "-home-user-app", "shared", local_age);
        let remote = transcript(repo.path(), "-home-user-app", "shared", repo_age);

        let plan = purge::plan(claude.path(), repo.path(), MINIMUM_RETENTION_DAYS).unwrap();
        purge::apply(&plan).unwrap();

        assert!(plan.is_empty(), "local {local_age}d, repo {repo_age}d");
        assert!(local.is_file(), "local {local_age}d, repo {repo_age}d");
        assert!(remote.is_file(), "local {local_age}d, repo {repo_age}d");
    }
}

#[test]
fn an_unreadable_copy_keeps_the_whole_session() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let old = transcript(claude.path(), "-home-user-app", "half-written", 400);
    let mid_write = write_transcript(repo.path(), "-home-user-app", "half-written", "{\"type\"");

    let plan = purge::plan(claude.path(), repo.path(), MINIMUM_RETENTION_DAYS).unwrap();
    purge::apply(&plan).unwrap();

    assert_eq!(plan.unreadable, 1);
    assert!(plan.is_empty());
    assert!(old.is_file(), "a copy that cannot be read dates nothing");
    assert!(mid_write.is_file());
}

#[test]
fn a_sessions_companion_files_go_with_it() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let old = transcript(claude.path(), "-home-user-app", "old-session", 400);
    let project = old.parent().unwrap().to_path_buf();
    let stopoffset = project.join("old-session.jsonl.stopoffset");
    fs::write(&stopoffset, "42").unwrap();

    purge_with_window(claude.path(), repo.path(), MINIMUM_RETENTION_DAYS);

    assert!(!old.exists());
    assert!(!stopoffset.exists());
}

#[test]
fn a_shorter_window_purges_more() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    transcript(claude.path(), "-home-user-app", "a", 400);
    transcript(claude.path(), "-home-user-app", "b", 200);
    transcript(claude.path(), "-home-user-app", "c", 5);

    let plan = purge::plan(claude.path(), repo.path(), 30).unwrap();

    let purged: Vec<&str> = plan
        .targets
        .iter()
        .map(|target| target.session_id.as_str())
        .collect();
    assert_eq!(purged, vec!["a", "b"], "oldest first, newest kept");
    assert_eq!(plan.oldest_age_days(), 400);
}
