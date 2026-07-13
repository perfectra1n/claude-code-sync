//! Integration tests for the artifact sync engine (push side + ignore files).
//! All engine functions take explicit paths, so no env-var isolation is needed.

use std::fs;
use std::path::Path;

use claude_code_sync::artifacts::engine::{ensure_ignore_files, push_artifacts};
use claude_code_sync::artifacts::registry::{ArtifactToggles, CategoryId};
use claude_code_sync::filter::FilterConfig;
use claude_code_sync::scm::Backend;
use tempfile::TempDir;

/// Build a `~/.claude`-shaped tree with every syncable artifact plus the
/// machine-local and secret files that must never move.
fn seed_claude_dir(claude: &Path) {
    fs::write(claude.join("settings.json"), b"{\"model\":\"opus\"}").unwrap();
    fs::write(claude.join("keybindings.json"), b"{\"ctrl+s\":\"save\"}").unwrap();
    fs::write(claude.join("settings.local.json"), b"{\"secret\":true}").unwrap();
    fs::write(claude.join(".credentials.json"), b"{\"token\":\"sk-123\"}").unwrap();
    fs::write(claude.join("CLAUDE.md"), b"# my memory\n").unwrap();
    fs::write(claude.join("history.jsonl"), history_line(1000, "one")).unwrap();
    fs::write(claude.join("stats-cache.json"), b"{}").unwrap();

    fs::create_dir_all(claude.join("skills/my-skill/references")).unwrap();
    fs::write(claude.join("skills/my-skill/SKILL.md"), b"# skill\n").unwrap();
    fs::write(
        claude.join("skills/my-skill/references/notes.md"),
        b"notes\n",
    )
    .unwrap();

    fs::create_dir_all(claude.join("agents")).unwrap();
    fs::write(claude.join("agents/reviewer.md"), b"# reviewer\n").unwrap();

    fs::create_dir_all(claude.join("commands")).unwrap();
    fs::write(claude.join("commands/deploy.md"), b"# deploy\n").unwrap();

    fs::create_dir_all(claude.join("plugins/cache/some-plugin")).unwrap();
    fs::write(
        claude.join("plugins/installed_plugins.json"),
        b"{\"plugins\":[]}",
    )
    .unwrap();
    fs::write(
        claude.join("plugins/known_marketplaces.json"),
        b"{\"marketplaces\":[]}",
    )
    .unwrap();
    fs::write(
        claude.join("plugins/cache/some-plugin/huge.bin"),
        b"cached plugin data",
    )
    .unwrap();

    fs::create_dir_all(claude.join("plans")).unwrap();
    fs::write(claude.join("plans/big-refactor.md"), b"# plan\n").unwrap();

    fs::create_dir_all(claude.join("todos")).unwrap();
    fs::write(claude.join("todos/session-1.json"), b"[]").unwrap();

    fs::create_dir_all(claude.join("shell-snapshots")).unwrap();
    fs::write(claude.join("shell-snapshots/snap.sh"), b"export FOO=1").unwrap();
}

fn history_line(ts: u64, display: &str) -> String {
    format!(
        "{{\"display\":\"{display}\",\"timestamp\":{ts},\"project\":\"/p\",\"sessionId\":\"s{ts}\"}}\n"
    )
}

fn all_on_filter() -> FilterConfig {
    FilterConfig {
        sync_artifacts: ArtifactToggles::all_enabled(),
        ..Default::default()
    }
}

#[test]
fn test_push_creates_expected_layout() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_claude_dir(claude.path());

    let report = push_artifacts(claude.path(), repo.path(), &all_on_filter()).unwrap();

    let a = repo.path().join("artifacts");
    assert!(a.join("settings/settings.json").is_file());
    assert!(a.join("settings/keybindings.json").is_file());
    assert!(a.join("memory/CLAUDE.md").is_file());
    assert!(a.join("skills/my-skill/SKILL.md").is_file());
    assert!(a.join("skills/my-skill/references/notes.md").is_file());
    assert!(a.join("agents/reviewer.md").is_file());
    assert!(a.join("commands/deploy.md").is_file());
    assert!(a.join("plugins/installed_plugins.json").is_file());
    assert!(a.join("plugins/known_marketplaces.json").is_file());
    assert!(a.join("plans/big-refactor.md").is_file());
    assert!(a.join("todos/session-1.json").is_file());
    assert!(a.join("prompt-history/history.jsonl").is_file());

    // Never-sync material is absent anywhere in the repo.
    assert!(!a.join("settings/settings.local.json").exists());
    let mut denied_found = false;
    for entry in walkdir::WalkDir::new(repo.path()) {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy();
        if name == ".credentials.json"
            || name == "settings.local.json"
            || name == "stats-cache.json"
            || name == "huge.bin"
            || name == "snap.sh"
        {
            denied_found = true;
        }
    }
    assert!(!denied_found, "no denied or cache file may reach the repo");

    // Everything was new on first push.
    let added: usize = report.counts.iter().map(|c| c.added).sum();
    assert!(added >= 12, "all seeded artifacts count as added: {added}");
    let modified: usize = report.counts.iter().map(|c| c.modified).sum();
    assert_eq!(modified, 0);
}

#[test]
fn test_second_push_is_all_unchanged() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_claude_dir(claude.path());
    let filter = all_on_filter();

    push_artifacts(claude.path(), repo.path(), &filter).unwrap();
    let second = push_artifacts(claude.path(), repo.path(), &filter).unwrap();

    let added: usize = second.counts.iter().map(|c| c.added).sum();
    let modified: usize = second.counts.iter().map(|c| c.modified).sum();
    let unchanged: usize = second.counts.iter().map(|c| c.unchanged).sum();
    assert_eq!(added, 0);
    assert_eq!(modified, 0);
    assert!(unchanged >= 12);
}

#[test]
fn test_modified_detection_on_changed_settings() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_claude_dir(claude.path());
    let filter = all_on_filter();

    push_artifacts(claude.path(), repo.path(), &filter).unwrap();
    fs::write(claude.path().join("settings.json"), b"{\"model\":\"sonnet\"}").unwrap();
    let report = push_artifacts(claude.path(), repo.path(), &filter).unwrap();

    let settings = report
        .counts
        .iter()
        .find(|c| c.category == CategoryId::Settings)
        .unwrap();
    assert_eq!(settings.modified, 1);
    assert_eq!(settings.added, 0);
    assert_eq!(
        fs::read(repo.path().join("artifacts/settings/settings.json")).unwrap(),
        b"{\"model\":\"sonnet\"}"
    );
}

#[test]
fn test_disabled_categories_untouched() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_claude_dir(claude.path());

    let filter = FilterConfig {
        sync_artifacts: ArtifactToggles {
            settings: true,
            ..Default::default()
        },
        ..Default::default()
    };

    push_artifacts(claude.path(), repo.path(), &filter).unwrap();

    let a = repo.path().join("artifacts");
    assert!(a.join("settings/settings.json").is_file());
    assert!(!a.join("skills").exists());
    assert!(!a.join("memory").exists());
    assert!(!a.join("prompt-history").exists());
}

#[test]
fn test_missing_sources_are_silent_noops() {
    let claude = TempDir::new().unwrap(); // empty ~/.claude
    let repo = TempDir::new().unwrap();

    let report = push_artifacts(claude.path(), repo.path(), &all_on_filter()).unwrap();
    let total: usize = report
        .counts
        .iter()
        .map(|c| c.added + c.modified + c.unchanged)
        .sum();
    assert_eq!(total, 0, "nothing to copy, nothing failed");
}

#[test]
fn test_denied_files_inside_categories_never_pushed() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_claude_dir(claude.path());

    // Plant secrets inside an otherwise-synced directory category.
    fs::write(
        claude.path().join("skills/my-skill/.credentials.json"),
        b"{\"token\":\"leaked\"}",
    )
    .unwrap();
    fs::write(claude.path().join("skills/my-skill/cert.pem"), b"PEM").unwrap();
    fs::write(claude.path().join("skills/my-skill/.env.local"), b"K=V").unwrap();
    fs::create_dir_all(claude.path().join("plans/cache")).unwrap();
    fs::write(claude.path().join("plans/cache/blob"), b"x").unwrap();

    // User config trying to force-include secrets must have no effect:
    // artifact sync never consults include/exclude patterns.
    let mut filter = all_on_filter();
    filter.include_patterns = vec!["*credentials*".to_string(), "*.pem".to_string()];

    push_artifacts(claude.path(), repo.path(), &filter).unwrap();

    for entry in walkdir::WalkDir::new(repo.path()) {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        assert_ne!(name, ".credentials.json");
        assert_ne!(name, "cert.pem");
        assert_ne!(name, ".env.local");
        assert_ne!(name, "blob");
    }
}

#[test]
fn test_push_unions_prompt_history() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_claude_dir(claude.path());
    let filter = all_on_filter();

    // The repo already holds lines from another machine.
    let repo_history = repo.path().join("artifacts/prompt-history/history.jsonl");
    fs::create_dir_all(repo_history.parent().unwrap()).unwrap();
    fs::write(
        &repo_history,
        format!("{}{}", history_line(500, "other-machine"), history_line(1000, "one")),
    )
    .unwrap();

    let report = push_artifacts(claude.path(), repo.path(), &filter).unwrap();

    let merged = fs::read_to_string(&repo_history).unwrap();
    assert!(merged.contains("other-machine"), "repo-only line survives push");
    assert!(merged.contains("one"));
    assert_eq!(merged.lines().count(), 2, "shared line dedups");

    let ph = report
        .counts
        .iter()
        .find(|c| c.category == CategoryId::PromptHistory)
        .unwrap();
    assert_eq!(ph.unchanged + ph.modified, 1, "history file counted once");
}

#[test]
fn test_oversized_files_are_skipped() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    fs::create_dir_all(claude.path().join("skills")).unwrap();
    fs::write(claude.path().join("skills/big.md"), vec![b'x'; 4096]).unwrap();
    fs::write(claude.path().join("skills/small.md"), b"ok").unwrap();

    let mut filter = all_on_filter();
    filter.max_file_size_bytes = 1024;

    let report = push_artifacts(claude.path(), repo.path(), &filter).unwrap();

    assert!(!repo.path().join("artifacts/skills/big.md").exists());
    assert!(repo.path().join("artifacts/skills/small.md").is_file());
    let skills = report
        .counts
        .iter()
        .find(|c| c.category == CategoryId::Skills)
        .unwrap();
    assert_eq!(skills.skipped, 1);
}

#[test]
fn test_ignore_file_managed_block_is_idempotent_and_preserving() {
    let repo = TempDir::new().unwrap();
    fs::write(repo.path().join(".gitignore"), "user-stuff/\n").unwrap();

    let changed_first = ensure_ignore_files(repo.path(), Backend::Git).unwrap();
    let after_first = fs::read_to_string(repo.path().join(".gitignore")).unwrap();
    let changed_second = ensure_ignore_files(repo.path(), Backend::Git).unwrap();
    let after_second = fs::read_to_string(repo.path().join(".gitignore")).unwrap();

    assert!(changed_first, "first run writes the block");
    assert!(!changed_second, "second run is a no-op");
    assert_eq!(after_first, after_second);
    assert!(after_first.starts_with("user-stuff/\n"), "user content preserved");
    assert!(after_first.contains(".credentials.json"));
    assert!(after_first.contains("settings.local.json"));
    assert!(after_first.contains("*.pem"));
    assert!(after_first.contains(".env*"));
    // Only the git ignore file for a git backend.
    assert!(!repo.path().join(".hgignore").exists());
}

#[test]
fn test_ignore_file_for_mercurial_backend() {
    let repo = TempDir::new().unwrap();

    ensure_ignore_files(repo.path(), Backend::Mercurial).unwrap();

    let hgignore = fs::read_to_string(repo.path().join(".hgignore")).unwrap();
    assert!(hgignore.contains("syntax: glob"));
    assert!(hgignore.contains(".credentials.json"));
}
