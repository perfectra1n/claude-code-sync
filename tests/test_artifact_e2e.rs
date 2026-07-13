//! Full-pipeline end-to-end tests for artifact sync: the real push_history /
//! pull_history / sync_bidirectional / undo_pull entry points, real git
//! commits, and two simulated machines (distinct HOME +
//! CLAUDE_CODE_SYNC_CONFIG_DIR sharing one sync repository).
//!
//! Serialized: HOME and the config-dir override are process-global.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use claude_code_sync::artifacts::registry::ArtifactToggles;
use claude_code_sync::filter::FilterConfig;
use claude_code_sync::history::OperationHistory;
use claude_code_sync::sync::{pull_history, push_history, sync_bidirectional, SyncState};
use claude_code_sync::VerbosityLevel;
use serial_test::serial;
use tempfile::TempDir;

/// One simulated machine: its own HOME and tool-config dir, pointed at a
/// shared sync repository. `activate()` switches the process env to it.
struct Machine {
    _root: TempDir,
    home: PathBuf,
    config: PathBuf,
}

impl Machine {
    fn new(sync_repo: &Path) -> Machine {
        let root = TempDir::new().unwrap();
        let home = root.path().join("home");
        let config = root.path().join("cfg");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(config.join("claude-code-sync")).unwrap();

        let state = SyncState {
            sync_repo_path: sync_repo.to_path_buf(),
            has_remote: false,
            is_cloned_repo: false,
        };
        fs::write(
            config.join("claude-code-sync/state.json"),
            serde_json::to_string_pretty(&state).unwrap(),
        )
        .unwrap();

        let filter = FilterConfig {
            sync_artifacts: ArtifactToggles::all_enabled(),
            ..Default::default()
        };
        fs::write(
            config.join("claude-code-sync/config.toml"),
            toml::to_string_pretty(&filter).unwrap(),
        )
        .unwrap();

        Machine {
            _root: root,
            home,
            config,
        }
    }

    fn activate(&self) {
        std::env::set_var("HOME", &self.home);
        std::env::set_var("CLAUDE_CODE_SYNC_CONFIG_DIR", &self.config);
    }

    fn claude(&self) -> PathBuf {
        self.home.join(".claude")
    }
}

struct EnvRestore {
    home: Option<String>,
    cfg: Option<String>,
}

impl EnvRestore {
    fn capture() -> Self {
        Self {
            home: std::env::var("HOME").ok(),
            cfg: std::env::var("CLAUDE_CODE_SYNC_CONFIG_DIR").ok(),
        }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match &self.cfg {
            Some(v) => std::env::set_var("CLAUDE_CODE_SYNC_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CODE_SYNC_CONFIG_DIR"),
        }
    }
}

fn seed_full_claude_home(claude: &Path) {
    fs::write(claude.join("settings.json"), b"{\"model\":\"opus\"}").unwrap();
    fs::write(claude.join("CLAUDE.md"), b"# global memory\n").unwrap();
    fs::write(claude.join(".credentials.json"), b"{\"token\":\"sk-e2e\"}").unwrap();
    fs::create_dir_all(claude.join("skills/deploy")).unwrap();
    fs::write(claude.join("skills/deploy/SKILL.md"), b"# deploy\n").unwrap();
    fs::write(claude.join("history.jsonl"), history_line(1000, "from A")).unwrap();

    let proj = claude.join("projects/-home-user-webapp");
    fs::create_dir_all(proj.join("memory")).unwrap();
    fs::write(
        proj.join("aaaa-1111.jsonl"),
        "{\"type\":\"user\",\"sessionId\":\"aaaa-1111\",\"uuid\":\"u1\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/home/user/webapp\"}\n",
    )
    .unwrap();
    fs::write(proj.join("diagram.png"), b"PNGDATA").unwrap();
    fs::write(proj.join("memory/MEMORY.md"), b"# project memory\n").unwrap();
}

fn history_line(ts: u64, display: &str) -> String {
    format!(
        "{{\"display\":\"{display}\",\"timestamp\":{ts},\"project\":\"/w\",\"sessionId\":\"s{ts}\"}}\n"
    )
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn init_git_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    claude_code_sync::scm::init(path).unwrap();
}

#[test]
#[serial]
fn test_full_pipeline_push_pull_undo_across_two_machines() {
    let _restore = EnvRestore::capture();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());

    // ---- Machine A pushes its whole environment ----
    let machine_a = Machine::new(repo.path());
    machine_a.activate();
    seed_full_claude_home(&machine_a.claude());

    let report = push_history(
        Some("e2e initial"),
        false,
        None,
        false,
        false,
        VerbosityLevel::Quiet,
    )
    .unwrap();
    assert_eq!(report.added, 1, "one session pushed");
    assert!(report.artifacts.total_added() >= 6, "artifacts pushed");

    // One commit contains sessions, artifacts, and the ignore guard.
    let log = git(repo.path(), &["log", "--oneline"]);
    assert_eq!(log.lines().count(), 1, "single commit: {log}");
    let tracked = git(repo.path(), &["ls-files"]);
    assert!(tracked.contains("artifacts/settings/settings.json"));
    assert!(tracked.contains("artifacts/skills/deploy/SKILL.md"));
    assert!(tracked.contains("projects/-home-user-webapp/diagram.png"));
    assert!(tracked.contains(".gitignore"));
    assert!(
        !tracked.contains(".credentials.json"),
        "secrets never reach the repo: {tracked}"
    );

    // The push record carries per-category artifact counts.
    let history = OperationHistory::load().unwrap();
    let last = history.get_last_operation_by_type(claude_code_sync::history::OperationType::Push);
    assert!(!last.unwrap().artifact_counts.is_empty());

    // ---- Machine B pulls the environment onto a fresh home ----
    let machine_b = Machine::new(repo.path());
    machine_b.activate();

    pull_history(false, None, false, VerbosityLevel::Quiet).unwrap();
    let b = machine_b.claude();
    assert_eq!(
        fs::read(b.join("settings.json")).unwrap(),
        b"{\"model\":\"opus\"}"
    );
    assert_eq!(fs::read(b.join("CLAUDE.md")).unwrap(), b"# global memory\n");
    assert!(b.join("skills/deploy/SKILL.md").is_file());
    assert!(b.join("projects/-home-user-webapp/diagram.png").is_file());
    assert!(b
        .join("projects/-home-user-webapp/memory/MEMORY.md")
        .is_file());
    assert!(b.join("history.jsonl").is_file());
    assert!(!b.join(".credentials.json").exists());

    // ---- Undo the pull: every artifact the pull created disappears ----
    let summary = claude_code_sync::undo::undo_pull(None, Some(&machine_b.home)).unwrap();
    assert!(summary.contains("undone"), "undo summary: {summary}");
    assert!(
        !b.join("settings.json").exists(),
        "created settings removed"
    );
    assert!(!b.join("skills/deploy/SKILL.md").exists());
    assert!(!b.join("CLAUDE.md").exists());

    // ---- Pull again: environment restored once more ----
    pull_history(false, None, false, VerbosityLevel::Quiet).unwrap();
    assert!(b.join("settings.json").is_file());
}

#[test]
#[serial]
fn test_full_pipeline_second_push_creates_no_commit() {
    let _restore = EnvRestore::capture();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());

    let machine = Machine::new(repo.path());
    machine.activate();
    seed_full_claude_home(&machine.claude());

    push_history(
        Some("first"),
        false,
        None,
        false,
        false,
        VerbosityLevel::Quiet,
    )
    .unwrap();
    let report = push_history(
        Some("second"),
        false,
        None,
        false,
        false,
        VerbosityLevel::Quiet,
    )
    .unwrap();

    // Issue #68 end-to-end: nothing changed, nothing is added/modified,
    // and git records no second commit.
    assert_eq!(report.added, 0);
    assert_eq!(report.modified, 0);
    assert_eq!(report.artifacts.total_added(), 0);
    assert_eq!(report.artifacts.total_modified(), 0);
    let log = git(repo.path(), &["log", "--oneline"]);
    assert_eq!(log.lines().count(), 1, "no empty second commit: {log}");
}

#[test]
#[serial]
fn test_full_pipeline_sync_converges_prompt_history() {
    let _restore = EnvRestore::capture();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());

    // Machine A seeds and pushes.
    let machine_a = Machine::new(repo.path());
    machine_a.activate();
    seed_full_claude_home(&machine_a.claude());
    push_history(Some("A"), false, None, false, false, VerbosityLevel::Quiet).unwrap();

    // Machine B has its own prompt history and runs a bidirectional sync.
    let machine_b = Machine::new(repo.path());
    machine_b.activate();
    fs::write(
        machine_b.claude().join("history.jsonl"),
        history_line(2000, "from B"),
    )
    .unwrap();
    sync_bidirectional(Some("B sync"), None, false, false, VerbosityLevel::Quiet).unwrap();

    // Machine A pulls; both machines now hold the identical superset.
    machine_a.activate();
    pull_history(false, None, false, VerbosityLevel::Quiet).unwrap();

    let a_history = fs::read_to_string(machine_a.claude().join("history.jsonl")).unwrap();
    let b_history = fs::read_to_string(machine_b.claude().join("history.jsonl")).unwrap();
    assert_eq!(a_history, b_history, "machines converge");
    assert!(a_history.contains("from A"));
    assert!(a_history.contains("from B"));
    let ts: Vec<u64> = a_history
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["timestamp"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(ts, vec![1000, 2000], "chronological order");
}
