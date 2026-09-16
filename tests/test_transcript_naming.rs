//! Integration tests for the directory a transcript takes in the sync repo:
//! the project map, and the `use_project_name_only` fallback it must not
//! disturb.

use std::fs;
use std::path::{Path, PathBuf};

use claude_code_sync::filter::FilterConfig;
use claude_code_sync::parser::ConversationSession;
use claude_code_sync::sync::push::plan_push;
use tempfile::TempDir;

/// One session file under `<projects>/<encoded>/`, whose entries carry `cwd`
/// the way Claude Code writes them.
fn seed_session(projects_dir: &Path, encoded: &str, cwd: &str) -> ConversationSession {
    let project_dir = projects_dir.join(encoded);
    fs::create_dir_all(&project_dir).unwrap();
    let file = project_dir.join("11111111-2222-3333-4444-555555555555.jsonl");
    let line = format!(
        "{{\"type\":\"user\",\"uuid\":\"u1\",\"sessionId\":\"s1\",\"cwd\":\"{cwd}\",\
         \"timestamp\":\"2026-01-01T00:00:00Z\"}}\n"
    );
    fs::write(&file, line).unwrap();
    ConversationSession::from_file(&file).unwrap()
}

fn only_destination(
    sessions: &[ConversationSession],
    projects_dir: &Path,
    repo_dir: &Path,
    filter: &FilterConfig,
) -> PathBuf {
    let plan = plan_push(sessions, projects_dir, repo_dir, filter).unwrap();
    assert_eq!(
        plan.entries.len(),
        1,
        "one session in, one planned push out"
    );
    plan.entries[0].relative_path.clone()
}

#[test]
fn an_unmapped_project_keeps_its_encoded_directory() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let projects = claude.path().join("projects");
    let session = seed_session(
        &projects,
        "-home-user-work-shop-web",
        "/home/user/work/shop-web",
    );

    let destination = only_destination(
        std::slice::from_ref(&session),
        &projects,
        repo.path(),
        &FilterConfig::default(),
    );

    assert_eq!(
        destination,
        Path::new("-home-user-work-shop-web/11111111-2222-3333-4444-555555555555.jsonl")
    );
}

#[test]
fn name_only_keeps_the_whole_folder_name_hyphens_included() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let projects = claude.path().join("projects");
    let session = seed_session(
        &projects,
        "-home-user-work-shop-web",
        "/home/user/work/shop-web",
    );
    let filter = FilterConfig {
        use_project_name_only: true,
        ..Default::default()
    };

    let destination = only_destination(
        std::slice::from_ref(&session),
        &projects,
        repo.path(),
        &filter,
    );

    assert_eq!(
        destination,
        Path::new("shop-web/11111111-2222-3333-4444-555555555555.jsonl"),
        "the project's folder name, not the last dash-separated segment of the encoded path"
    );
}

#[test]
fn name_only_pulls_back_into_the_project_it_was_pushed_from() {
    let claude = TempDir::new().unwrap();
    let projects = claude.path().join("projects");
    let encoded = "-home-user-work-shop-web";
    seed_session(&projects, encoded, "/home/user/work/shop-web");
    let filter = FilterConfig {
        use_project_name_only: true,
        ..Default::default()
    };

    let destination =
        claude_code_sync::project_map::local_project_dir(&filter, &projects, "shop-web");

    assert_eq!(
        destination,
        Some(projects.join(encoded)),
        "what push writes must be what pull resolves"
    );
}

#[test]
fn a_mapped_project_pushes_under_its_canonical_id() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let projects = claude.path().join("projects");
    let session = seed_session(
        &projects,
        "-home-user-work-shop-web",
        "/home/user/work/shop-web",
    );
    let mut filter = FilterConfig::default();
    filter.project_map.insert(
        "shop".to_string(),
        PathBuf::from("/home/user/work/shop-web"),
    );

    let destination = only_destination(
        std::slice::from_ref(&session),
        &projects,
        repo.path(),
        &filter,
    );

    assert_eq!(
        destination,
        Path::new("shop/11111111-2222-3333-4444-555555555555.jsonl")
    );
}

#[test]
fn the_map_beats_name_only_so_two_checkouts_stay_apart() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let projects = claude.path().join("projects");
    let work = seed_session(&projects, "-home-user-work-myapp", "/home/user/work/myapp");
    let personal = seed_session(
        &projects,
        "-home-user-personal-myapp",
        "/home/user/personal/myapp",
    );
    let mut filter = FilterConfig {
        use_project_name_only: true,
        ..Default::default()
    };
    filter.project_map.insert(
        "work-myapp".to_string(),
        PathBuf::from("/home/user/work/myapp"),
    );

    let plan = plan_push(&[work, personal], &projects, repo.path(), &filter).unwrap();

    let destinations: Vec<String> = plan
        .entries
        .iter()
        .map(|entry| {
            entry
                .relative_path
                .parent()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert!(
        destinations.contains(&"work-myapp".to_string()),
        "{destinations:?}"
    );
    assert!(
        destinations.contains(&"myapp".to_string()),
        "{destinations:?}"
    );
}
