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
    fs::write(
        claude.path().join("settings.json"),
        b"{\"model\":\"sonnet\"}",
    )
    .unwrap();
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
        format!(
            "{}{}",
            history_line(500, "other-machine"),
            history_line(1000, "one")
        ),
    )
    .unwrap();

    let report = push_artifacts(claude.path(), repo.path(), &filter).unwrap();

    let merged = fs::read_to_string(&repo_history).unwrap();
    assert!(
        merged.contains("other-machine"),
        "repo-only line survives push"
    );
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
    assert!(
        after_first.starts_with("user-stuff/\n"),
        "user content preserved"
    );
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

// ============================================================================
// Pull side
// ============================================================================

use claude_code_sync::artifacts::engine::{apply_pull, plan_pull};

#[test]
fn test_pull_restores_artifacts_to_fresh_machine() {
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_claude_dir(machine_a.path());
    let filter = all_on_filter();

    push_artifacts(machine_a.path(), repo.path(), &filter).unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert!(!plan.is_empty());
    assert!(
        plan.overwrites.is_empty(),
        "fresh machine has nothing to overwrite"
    );
    let report = apply_pull(&plan, false).unwrap();

    assert_eq!(
        fs::read(machine_b.path().join("settings.json")).unwrap(),
        fs::read(machine_a.path().join("settings.json")).unwrap()
    );
    assert_eq!(
        fs::read(machine_b.path().join("skills/my-skill/SKILL.md")).unwrap(),
        b"# skill\n"
    );
    assert!(machine_b.path().join("CLAUDE.md").is_file());
    assert!(machine_b
        .path()
        .join("plugins/installed_plugins.json")
        .is_file());
    assert!(machine_b.path().join("history.jsonl").is_file());
    assert!(report.total_added() >= 12);
    assert_eq!(report.total_modified(), 0);
}

#[test]
fn test_pull_remote_wins_when_bytes_differ() {
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_claude_dir(machine_a.path());
    let filter = all_on_filter();
    push_artifacts(machine_a.path(), repo.path(), &filter).unwrap();

    // Machine B has its own, different settings.
    fs::write(
        machine_b.path().join("settings.json"),
        b"{\"model\":\"local\"}",
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    let overwrite_targets: Vec<_> = plan
        .overwrites
        .iter()
        .map(|w| w.local_path.clone())
        .collect();
    assert!(overwrite_targets.contains(&machine_b.path().join("settings.json")));

    apply_pull(&plan, false).unwrap();
    assert_eq!(
        fs::read(machine_b.path().join("settings.json")).unwrap(),
        b"{\"model\":\"opus\"}",
        "remote bytes win"
    );
}

#[test]
fn test_pull_does_not_rewrite_identical_files() {
    let machine_a = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_claude_dir(machine_a.path());
    let filter = all_on_filter();
    push_artifacts(machine_a.path(), repo.path(), &filter).unwrap();

    // Pulling straight back into the same machine: everything identical.
    let plan = plan_pull(machine_a.path(), repo.path(), &filter).unwrap();
    assert!(
        plan.is_empty(),
        "no writes planned when bytes match: {plan:?}"
    );
    assert!(plan.unchanged >= 12);
}

#[test]
fn test_pull_unions_prompt_history_with_local() {
    let machine_b = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let filter = all_on_filter();

    let repo_history = repo.path().join("artifacts/prompt-history/history.jsonl");
    fs::create_dir_all(repo_history.parent().unwrap()).unwrap();
    fs::write(
        &repo_history,
        format!(
            "{}{}",
            history_line(500, "remote-old"),
            history_line(2000, "remote-new")
        ),
    )
    .unwrap();
    fs::write(
        machine_b.path().join("history.jsonl"),
        history_line(1000, "local-only"),
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    let report = apply_pull(&plan, false).unwrap();

    let merged = fs::read_to_string(machine_b.path().join("history.jsonl")).unwrap();
    let displays: Vec<_> = merged
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["display"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        displays,
        vec!["remote-old", "local-only", "remote-new"],
        "union, chronological"
    );
    let ph = report
        .counts
        .iter()
        .find(|c| c.category == CategoryId::PromptHistory)
        .unwrap();
    assert_eq!(ph.merged_entries, 2, "two remote lines were new locally");
}

#[test]
fn test_pull_refuses_denied_files_planted_in_repo() {
    let machine_b = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let filter = all_on_filter();

    // A poisoned sync repo tries to deliver credentials and key material.
    fs::create_dir_all(repo.path().join("artifacts/settings")).unwrap();
    fs::write(repo.path().join("artifacts/settings/settings.json"), b"{}").unwrap();
    fs::write(
        repo.path().join("artifacts/settings/.credentials.json"),
        b"{\"t\":1}",
    )
    .unwrap();
    fs::create_dir_all(repo.path().join("artifacts/skills/s")).unwrap();
    fs::write(repo.path().join("artifacts/skills/s/evil.pem"), b"PEM").unwrap();
    fs::write(repo.path().join("artifacts/skills/s/ok.md"), b"fine").unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    apply_pull(&plan, false).unwrap();

    assert!(machine_b.path().join("settings.json").is_file());
    assert!(machine_b.path().join("skills/s/ok.md").is_file());
    assert!(!machine_b.path().join(".credentials.json").exists());
    assert!(!machine_b.path().join("skills/s/evil.pem").exists());
    assert!(
        plan.skipped >= 2,
        "denied repo files are counted as skipped"
    );
}

#[test]
fn test_pull_plan_snapshot_paths_enable_exact_undo() {
    // The pull plan's snapshot inputs must make undo an exact inverse:
    // overwritten files restore to their old bytes, created files are deleted.
    use claude_code_sync::history::OperationType;
    use claude_code_sync::undo::Snapshot;

    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_claude_dir(machine_a.path());
    let filter = all_on_filter();
    push_artifacts(machine_a.path(), repo.path(), &filter).unwrap();

    // Machine B: one file that will be overwritten, everything else created.
    fs::write(
        machine_b.path().join("settings.json"),
        b"{\"model\":\"mine\"}",
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();

    let mut snapshot =
        Snapshot::create(OperationType::Pull, plan.paths_to_snapshot().iter(), None).unwrap();
    snapshot.deleted_files = plan.created_paths();

    apply_pull(&plan, false).unwrap();
    assert_eq!(
        fs::read(machine_b.path().join("settings.json")).unwrap(),
        b"{\"model\":\"opus\"}"
    );
    assert!(machine_b.path().join("CLAUDE.md").is_file());

    // Undo: restore the snapshot.
    snapshot.restore_with_base(Some(machine_b.path())).unwrap();
    assert_eq!(
        fs::read(machine_b.path().join("settings.json")).unwrap(),
        b"{\"model\":\"mine\"}",
        "overwritten file restored"
    );
    assert!(
        !machine_b.path().join("CLAUDE.md").exists(),
        "created file removed by undo"
    );
    assert!(
        !machine_b.path().join("skills/my-skill/SKILL.md").exists(),
        "created skill removed by undo"
    );
}

// ============================================================================
// Project attachments (non-JSONL files in the session tree)
// ============================================================================

fn seed_project_with_attachment(claude: &Path) {
    let proj = claude.join("projects/-home-user-myproj");
    fs::create_dir_all(proj.join("memory")).unwrap();
    fs::write(
        proj.join("abc-123.jsonl"),
        r#"{"type":"user","sessionId":"abc-123","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    fs::write(proj.join("diagram.png"), b"PNGDATA").unwrap();
    fs::write(proj.join("memory/MEMORY.md"), b"# memory index\n").unwrap();
}

#[test]
fn test_attachments_push_copies_non_jsonl_into_session_tree() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_project_with_attachment(claude.path());

    // Default config: no artifact toggles, attachments NOT excluded.
    let filter = FilterConfig::default();
    let report = push_artifacts(claude.path(), repo.path(), &filter).unwrap();

    let proj = repo.path().join("projects/-home-user-myproj");
    assert!(
        proj.join("diagram.png").is_file(),
        "attachment lands in session tree"
    );
    assert!(
        proj.join("memory/MEMORY.md").is_file(),
        "project memory syncs as attachment"
    );
    assert!(
        !proj.join("abc-123.jsonl").exists(),
        "session transcripts belong to the session pipeline, not the engine"
    );
    assert!(
        !repo.path().join("artifacts").exists(),
        "no artifacts dir needed"
    );

    let att = report
        .counts
        .iter()
        .find(|c| c.category == CategoryId::ProjectAttachments)
        .unwrap();
    assert_eq!(att.added, 2);
}

#[test]
fn test_attachments_respect_exclude_attachments_flag() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_project_with_attachment(claude.path());

    let filter = FilterConfig {
        exclude_attachments: true,
        ..Default::default()
    };
    let report = push_artifacts(claude.path(), repo.path(), &filter).unwrap();

    assert!(!repo.path().join("projects").exists());
    assert!(report
        .counts
        .iter()
        .all(|c| c.category != CategoryId::ProjectAttachments));
}

#[test]
fn test_attachments_pull_ignores_remote_transcripts() {
    let machine_b = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();

    // Sync repo has a session transcript AND an attachment side by side.
    let proj = repo.path().join("projects/-home-user-myproj");
    fs::create_dir_all(&proj).unwrap();
    fs::write(
        proj.join("abc-123.jsonl"),
        r#"{"type":"user","sessionId":"abc-123","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    fs::write(proj.join("diagram.png"), b"PNGDATA").unwrap();

    let filter = FilterConfig::default();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    apply_pull(&plan, false).unwrap();

    assert!(
        machine_b
            .path()
            .join("projects/-home-user-myproj/diagram.png")
            .is_file(),
        "attachment restored"
    );
    assert!(
        !machine_b
            .path()
            .join("projects/-home-user-myproj/abc-123.jsonl")
            .exists(),
        "transcripts are restored by the session pipeline, never the engine"
    );
}

#[test]
fn test_attachments_map_project_names_in_name_only_mode() {
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_project_with_attachment(claude.path());

    let filter = FilterConfig {
        use_project_name_only: true,
        ..Default::default()
    };
    push_artifacts(claude.path(), repo.path(), &filter).unwrap();
    assert!(
        repo.path().join("projects/myproj/diagram.png").is_file(),
        "encoded dir collapses to the bare project name on push"
    );

    // Pulling into a machine whose encoded path differs but project name matches.
    let machine_b = TempDir::new().unwrap();
    fs::create_dir_all(machine_b.path().join("projects/-Users-other-dev-myproj")).unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    apply_pull(&plan, false).unwrap();
    assert!(
        machine_b
            .path()
            .join("projects/-Users-other-dev-myproj/diagram.png")
            .is_file(),
        "attachment maps back through the local project dir with the same name"
    );

    // No matching local project: the file is skipped, not misplaced.
    let machine_c = TempDir::new().unwrap();
    let plan = plan_pull(machine_c.path(), repo.path(), &filter).unwrap();
    apply_pull(&plan, false).unwrap();
    assert!(
        !machine_c.path().join("projects").exists(),
        "ambiguous/no-match attachments are skipped in name-only mode"
    );
    assert!(plan.skipped >= 1);
}

#[test]
fn test_pull_refuses_unlisted_files_in_allowlist_categories() {
    // A Files-sourced category (settings, plugins, ...) is an exact
    // allowlist: a repo carrying an unexpected filename inside that
    // category directory must be skipped, never written into ~/.claude.
    let machine_b = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let filter = all_on_filter();

    fs::create_dir_all(repo.path().join("artifacts/settings")).unwrap();
    fs::write(repo.path().join("artifacts/settings/settings.json"), b"{}").unwrap();
    fs::write(
        repo.path().join("artifacts/settings/unexpected.json"),
        b"{\"planted\":true}",
    )
    .unwrap();
    fs::create_dir_all(repo.path().join("artifacts/plugins")).unwrap();
    fs::write(
        repo.path().join("artifacts/plugins/rogue-manifest.json"),
        b"{}",
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    apply_pull(&plan, false).unwrap();

    assert!(machine_b.path().join("settings.json").is_file());
    for entry in walkdir::WalkDir::new(machine_b.path()) {
        let name = entry.unwrap().file_name().to_string_lossy().to_string();
        assert_ne!(
            name, "unexpected.json",
            "unlisted file must not be restored"
        );
        assert_ne!(name, "rogue-manifest.json");
    }
    assert!(
        plan.skipped >= 2,
        "unlisted allowlist files count as skipped"
    );
}
