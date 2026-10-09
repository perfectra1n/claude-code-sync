//! Full-pipeline end-to-end tests for artifact sync: the real push_history /
//! pull_history / sync_bidirectional / undo_pull entry points, real git
//! commits, and two simulated machines (distinct CLAUDE_CODE_SYNC_CLAUDE_DIR +
//! CLAUDE_CODE_SYNC_CONFIG_DIR sharing one sync repository).
//!
//! Serialized: HOME and the config-dir override are process-global.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, Once};

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
        // HOME alone is not enough on Windows (dirs::home_dir() ignores it
        // there), so point the product at this machine's .claude explicitly.
        std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", self.home.join(".claude"));
        std::env::set_var("CLAUDE_CODE_SYNC_CONFIG_DIR", &self.config);
    }

    fn claude(&self) -> PathBuf {
        self.home.join(".claude")
    }

    fn write_filter(&self, filter: &FilterConfig) {
        fs::write(
            self.config.join("claude-code-sync/config.toml"),
            toml::to_string_pretty(filter).unwrap(),
        )
        .unwrap();
    }
}

struct EnvRestore {
    home: Option<String>,
    claude: Option<String>,
    cfg: Option<String>,
}

impl EnvRestore {
    fn capture() -> Self {
        Self {
            home: std::env::var("HOME").ok(),
            claude: std::env::var("CLAUDE_CODE_SYNC_CLAUDE_DIR").ok(),
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
        match &self.claude {
            Some(v) => std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", v),
            None => std::env::remove_var("CLAUDE_CODE_SYNC_CLAUDE_DIR"),
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
        &std::collections::HashSet::new(),
        &[] as &[String],
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

    pull_history(false, None, false, VerbosityLevel::Quiet, false).unwrap();
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
    pull_history(false, None, false, VerbosityLevel::Quiet, false).unwrap();
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
        &std::collections::HashSet::new(),
        &[] as &[String],
    )
    .unwrap();
    let report = push_history(
        Some("second"),
        false,
        None,
        false,
        false,
        VerbosityLevel::Quiet,
        &std::collections::HashSet::new(),
        &[] as &[String],
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
    push_history(
        Some("A"),
        false,
        None,
        false,
        false,
        VerbosityLevel::Quiet,
        &std::collections::HashSet::new(),
        &[] as &[String],
    )
    .unwrap();

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
    pull_history(false, None, false, VerbosityLevel::Quiet, false).unwrap();

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

/// Ported from #106 (FluffyDiscord): the full `sync` entry point keeps an
/// edit and a deletion made since the last sync, and publishes both.
#[test]
#[serial]
fn test_sync_keeps_local_edits_and_deletions_made_since_the_last_sync() {
    let _restore = EnvRestore::capture();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let machine = Machine::new(repo.path());
    machine.activate();
    let claude = machine.claude();
    seed_full_claude_home(&claude);
    fs::create_dir_all(claude.join("skills/review")).unwrap();
    fs::write(claude.join("skills/review/SKILL.md"), b"# review\n").unwrap();
    push_history(
        Some("first"),
        false,
        None,
        false,
        false,
        VerbosityLevel::Quiet,
        &std::collections::HashSet::new(),
        &[] as &[String],
    )
    .unwrap();

    fs::write(
        claude.join("skills/deploy/SKILL.md"),
        b"# deploy, edited here\n",
    )
    .unwrap();
    fs::remove_file(claude.join("skills/review/SKILL.md")).unwrap();
    sync_bidirectional(Some("sync"), None, false, false, VerbosityLevel::Quiet).unwrap();

    assert_eq!(
        fs::read(claude.join("skills/deploy/SKILL.md")).unwrap(),
        b"# deploy, edited here\n",
        "the local edit survives the pull half of the sync"
    );
    assert_eq!(
        fs::read(repo.path().join("artifacts/skills/deploy/SKILL.md")).unwrap(),
        b"# deploy, edited here\n",
        "and the push half sends it"
    );
    assert!(
        !claude.join("skills/review/SKILL.md").exists(),
        "the local deletion stands"
    );
    let tracked = git(repo.path(), &["ls-files"]);
    assert!(
        !tracked.contains("skills/review/SKILL.md"),
        "and reaches the repo: {tracked}"
    );
}

/// Ported from #106 (FluffyDiscord): a pull takes what only the remote
/// changed, and a file that differs only in dates keeps the later ones.
#[test]
#[serial]
fn test_pull_takes_what_only_the_remote_changed_and_the_later_dates() {
    let _restore = EnvRestore::capture();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let skill = |claude: &Path, name: &str| claude.join("skills").join(name).join("SKILL.md");
    let write_skill = |claude: &Path, name: &str, text: &str| {
        fs::create_dir_all(claude.join("skills").join(name)).unwrap();
        fs::write(skill(claude, name), text).unwrap();
    };
    let push = |message: &str| {
        push_history(
            Some(message),
            false,
            None,
            false,
            false,
            VerbosityLevel::Quiet,
            &std::collections::HashSet::new(),
            &[] as &[String],
        )
        .unwrap();
    };

    let machine_a = Machine::new(repo.path());
    machine_a.activate();
    let a = machine_a.claude();
    write_skill(&a, "remote-only", "v1\n");
    write_skill(&a, "dates", "updated: 2026-01-01T00:00:00Z\n");
    push("A1");

    let machine_b = Machine::new(repo.path());
    machine_b.activate();
    let b = machine_b.claude();
    pull_history(false, None, false, VerbosityLevel::Quiet, false).unwrap();
    write_skill(&b, "dates", "updated: 2026-01-03T00:00:00Z\n");

    machine_a.activate();
    write_skill(&a, "remote-only", "v2 from A\n");
    write_skill(&a, "dates", "updated: 2026-01-02T00:00:00Z\n");
    push("A2");

    machine_b.activate();
    pull_history(false, None, false, VerbosityLevel::Quiet, false).unwrap();

    assert_eq!(
        fs::read_to_string(skill(&b, "remote-only")).unwrap(),
        "v2 from A\n",
        "a change only the remote made arrives"
    );
    assert_eq!(
        fs::read_to_string(skill(&b, "dates")).unwrap(),
        "updated: 2026-01-03T00:00:00Z\n",
        "a file that differs only in dates keeps the later one"
    );
}

/// Both machines edited the same skill: the full `sync` neither reverts this
/// machine's edit nor commits it over the other machine's, and the sessions
/// still travel.
#[test]
#[serial]
fn test_sync_holds_a_file_both_machines_edited_without_losing_either() {
    let _restore = EnvRestore::capture();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let push = |message: &str| {
        push_history(
            Some(message),
            false,
            None,
            false,
            false,
            VerbosityLevel::Quiet,
            &std::collections::HashSet::new(),
            &[] as &[String],
        )
        .unwrap();
    };

    let machine_a = Machine::new(repo.path());
    machine_a.activate();
    seed_full_claude_home(&machine_a.claude());
    push("A seeds");

    let machine_b = Machine::new(repo.path());
    machine_b.activate();
    pull_history(false, None, false, VerbosityLevel::Quiet, false).unwrap();

    machine_a.activate();
    fs::write(
        machine_a.claude().join("skills/deploy/SKILL.md"),
        b"# deploy\nA's edit\n",
    )
    .unwrap();
    push("A edits");

    machine_b.activate();
    fs::write(
        machine_b.claude().join("skills/deploy/SKILL.md"),
        b"# deploy\nB's edit\n",
    )
    .unwrap();
    fs::write(
        machine_b
            .claude()
            .join("projects/-home-user-webapp/bbbb-2222.jsonl"),
        "{\"type\":\"user\",\"sessionId\":\"bbbb-2222\",\"uuid\":\"u9\",\"timestamp\":\"2025-01-02T00:00:00Z\",\"cwd\":\"/home/user/webapp\"}\n",
    )
    .unwrap();

    for round in 0..3 {
        sync_bidirectional(Some("B sync"), None, false, false, VerbosityLevel::Quiet).unwrap();
        assert_eq!(
            fs::read_to_string(machine_b.claude().join("skills/deploy/SKILL.md")).unwrap(),
            "# deploy\nB's edit\n",
            "round {round}: B's edit is untouched"
        );
        assert_eq!(
            fs::read_to_string(repo.path().join("artifacts/skills/deploy/SKILL.md")).unwrap(),
            "# deploy\nA's edit\n",
            "round {round}: A's edit is not overwritten"
        );
    }
    assert!(
        git(repo.path(), &["ls-files"]).contains("bbbb-2222.jsonl"),
        "the held artifact does not stop the sessions"
    );
}

/// Collects what the tool warns about, so a test can count the lines a pull
/// prints rather than trust that it prints the right number.
struct WarningCollector;

static WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static WARNING_LOGGER: Once = Once::new();

impl log::Log for WarningCollector {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            WARNINGS.lock().unwrap().push(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

fn collected_warnings_since_reset() -> Vec<String> {
    WARNINGS.lock().unwrap().clone()
}

fn reset_collected_warnings() {
    WARNING_LOGGER.call_once(|| {
        log::set_boxed_logger(Box::new(WarningCollector)).unwrap();
        log::set_max_level(log::LevelFilter::Warn);
    });
    WARNINGS.lock().unwrap().clear();
}

fn lines_about_unplaceable_files(warnings: &[String]) -> Vec<&String> {
    warnings
        .iter()
        .filter(|line| line.contains("no local project"))
        .collect()
}

#[test]
#[serial]
fn test_pull_warns_once_for_a_project_this_machine_never_mapped() {
    let _restore = EnvRestore::capture();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());

    // Machine A syncs its project under the canonical id "webapp".
    let machine_a = Machine::new(repo.path());
    machine_a.activate();
    seed_full_claude_home(&machine_a.claude());
    let mut mapped = FilterConfig {
        sync_artifacts: ArtifactToggles::all_enabled(),
        ..Default::default()
    };
    mapped.project_map.insert(
        "webapp".to_string(),
        PathBuf::from("/home/user/webapp"), // what -home-user-webapp encodes
    );
    machine_a.write_filter(&mapped);
    push_history(
        Some("A"),
        false,
        None,
        false,
        false,
        VerbosityLevel::Quiet,
        &std::collections::HashSet::new(),
        &[] as &[String],
    )
    .unwrap();
    assert!(repo.path().join("projects/webapp").is_dir());

    // Machine B has never mapped "webapp": one transcript, one attachment and
    // one memory index all land in the same warning.
    let machine_b = Machine::new(repo.path());
    machine_b.activate();
    reset_collected_warnings();
    pull_history(false, None, false, VerbosityLevel::Quiet, false).unwrap();

    let warnings = collected_warnings_since_reset();
    let unplaceable = lines_about_unplaceable_files(&warnings);
    assert_eq!(
        unplaceable.len(),
        1,
        "three unplaceable files, one warning: {warnings:?}"
    );
    assert!(
        unplaceable[0].contains("webapp (3)"),
        "the warning names the project and counts its files: {}",
        unplaceable[0]
    );
    assert!(
        !machine_b.claude().join("projects").exists(),
        "nothing is written into a project this machine has no directory for"
    );

    // With the config key on, every file gets its line back.
    let mut warn_each = mapped.clone();
    warn_each.project_map.clear();
    warn_each.warn_each_skipped_file = true;
    machine_b.write_filter(&warn_each);
    reset_collected_warnings();
    pull_history(false, None, false, VerbosityLevel::Quiet, false).unwrap();

    let warnings = collected_warnings_since_reset();
    assert_eq!(
        lines_about_unplaceable_files(&warnings).len(),
        3,
        "warn_each_skipped_file restores the line per file: {warnings:?}"
    );
}

#[test]
#[serial]
fn test_sync_publishes_an_edit_made_only_here_and_pushes_sessions() {
    let _restore = EnvRestore::capture();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());

    // Machine A seeds everything and pushes.
    let machine_a = Machine::new(repo.path());
    machine_a.activate();
    seed_full_claude_home(&machine_a.claude());
    push_history(
        Some("A"),
        false,
        None,
        false,
        false,
        VerbosityLevel::Quiet,
        &std::collections::HashSet::new(),
        &[] as &[String],
    )
    .unwrap();

    // Machine B pulls, then edits an artifact and gains a new session.
    let machine_b = Machine::new(repo.path());
    machine_b.activate();
    pull_history(false, None, false, VerbosityLevel::Quiet, false).unwrap();
    fs::write(
        machine_b.claude().join("skills/deploy/SKILL.md"),
        b"# deploy\nB's local edit\n",
    )
    .unwrap();
    fs::write(
        machine_b
            .claude()
            .join("projects/-home-user-webapp/bbbb-2222.jsonl"),
        "{\"type\":\"user\",\"sessionId\":\"bbbb-2222\",\"uuid\":\"u9\",\"timestamp\":\"2025-01-02T00:00:00Z\",\"cwd\":\"/home/user/webapp\"}\n",
    )
    .unwrap();

    // Issue #103: only B changed the skill since its last sync, so the
    // pull must neither revert it nor hold it back — the push publishes it.
    sync_bidirectional(Some("B sync"), None, false, false, VerbosityLevel::Quiet).unwrap();

    let files = git(repo.path(), &["ls-files"]);
    assert!(
        files.contains("bbbb-2222.jsonl"),
        "the new session reached the repository: {files}"
    );
    let repo_skill = fs::read_to_string(repo.path().join("artifacts/skills/deploy/SKILL.md"))
        .unwrap_or_default();
    assert_eq!(
        repo_skill, "# deploy\nB's local edit\n",
        "the edit made only here was published"
    );
    assert_eq!(
        fs::read_to_string(machine_b.claude().join("skills/deploy/SKILL.md")).unwrap(),
        "# deploy\nB's local edit\n",
        "the local edit survived untouched"
    );
}

// Unix only: it makes a write fail with a read-only directory, which
// Windows does not enforce the same way.
#[cfg(unix)]
#[test]
#[serial]
fn test_sync_keeps_a_clean_held_file_from_overwriting_repo() {
    use std::os::unix::fs::PermissionsExt;
    let _restore = EnvRestore::capture();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());

    // Machine A seeds the repo: one skill, one CLAUDE.md, no extras.
    let machine_a = Machine::new(repo.path());
    machine_a.activate();
    seed_full_claude_home(&machine_a.claude());
    push_history(
        Some("A seeds"),
        false,
        None,
        false,
        false,
        VerbosityLevel::Quiet,
        &std::collections::HashSet::new(),
        &[] as &[String],
    )
    .unwrap();

    // Machine B pulls. Now B has skill v1, base v1.
    let machine_b = Machine::new(repo.path());
    machine_b.activate();
    pull_history(false, None, false, VerbosityLevel::Quiet, false).unwrap();

    // A edits the skill and pushes. Repo now has skill v2.
    machine_a.activate();
    fs::write(
        machine_a.claude().join("skills/deploy/SKILL.md"),
        b"# deploy\nv2 from A\n",
    )
    .unwrap();
    push_history(
        Some("A edits skill"),
        false,
        None,
        false,
        false,
        VerbosityLevel::Quiet,
        &std::collections::HashSet::new(),
        &[] as &[String],
    )
    .unwrap();
    machine_b.activate();

    // On B: make the skill overwrite FAIL (read-only dir) so the
    // pull's keep path fires with nothing_to_publish=true
    // (kept_local_clean). The local file still matches the base.
    let skill_dir = machine_b.claude().join("skills/deploy");
    fs::set_permissions(&skill_dir, fs::Permissions::from_mode(0o555)).unwrap();

    // B also creates a DIFFERENT skill in the SAME category. The OLD
    // session-only gate would have held this back along with the held
    // skill. The NEW per-file skip must let it through (its decision
    // is unrelated to the held skill).
    fs::create_dir_all(machine_b.claude().join("skills/other")).unwrap();
    fs::write(
        machine_b.claude().join("skills/other/SKILL.md"),
        b"# other\nB's new skill\n",
    )
    .unwrap();

    // sync_bidirectional: the per-file skip should skip the held
    // skill but publish the unrelated new skill.
    sync_bidirectional(Some("B sync"), None, false, false, VerbosityLevel::Quiet).unwrap();

    // Restore the permission so the test cleanup can read the file
    // if needed.
    fs::set_permissions(&skill_dir, fs::Permissions::from_mode(0o755)).unwrap();

    // The held skill: local==base, repo has the newer v2. The push
    // must NOT have written local (== base, the OLDER bytes) over
    // the repo's v2.
    let repo_skill =
        fs::read_to_string(repo.path().join("artifacts/skills/deploy/SKILL.md")).unwrap();
    assert_eq!(
        repo_skill, "# deploy\nv2 from A\n",
        "the held skill was NOT overwritten by the push: {repo_skill}"
    );

    // The unrelated new skill: B's local file IS in the repo
    // (the per-file skip only affects the held file, not the
    // whole category).
    let repo_other =
        fs::read_to_string(repo.path().join("artifacts/skills/other/SKILL.md")).unwrap();
    assert_eq!(
        repo_other, "# other\nB's new skill\n",
        "B's unrelated new skill reached the repo: {repo_other}"
    );
}
