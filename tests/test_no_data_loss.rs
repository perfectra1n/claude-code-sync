//! No data loss without consent: the guarantees every pull and sync must keep,
//! whatever happened on either machine since they last agreed.
//!
//! Issue #103: `sync` (pull, then push) used to overwrite every local artifact
//! that differed from the sync repository, so an edit made since the last push
//! was silently reverted and never published. These tests pin the contract
//! that replaced that behaviour:
//!
//! 1. A non-interactive pull never discards bytes that exist only on this
//!    machine. The only artifact a pull may replace without asking is one this
//!    machine has not touched since the last sync.
//! 2. A sync never publishes this machine's version over a change another
//!    machine made since the last sync — unless the two differ only in their
//!    date-times, which settle on the later ones.
//! 3. So after any sync, every version anyone wrote is still on this machine's
//!    disk or in the repository.
//!
//! Two `~/.claude` trees share one repository directory. The engine functions
//! take explicit paths and apply non-interactively here, which is exactly the
//! cron-driven `sync` the data loss struck. Scenarios ported from #106 (by
//! FluffyDiscord) and #105 (by Jerome Revillard) are marked where they came
//! from.

use std::fs;
use std::path::Path;

use claude_code_sync::artifacts::bases;
use claude_code_sync::artifacts::engine::{apply_pull, plan_pull, push_artifacts, ArtifactReport};
use claude_code_sync::artifacts::registry::ArtifactToggles;
use claude_code_sync::filter::FilterConfig;
use tempfile::TempDir;

fn all_on_filter() -> FilterConfig {
    FilterConfig {
        sync_artifacts: ArtifactToggles::all_enabled(),
        ..Default::default()
    }
}

const SKILL: &str = "skills/s/SKILL.md";
const REPO_SKILL: &str = "artifacts/skills/s/SKILL.md";

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// A non-interactive pull, the way a cron-driven `sync` runs it.
fn pull(claude: &Path, repo: &Path) -> ArtifactReport {
    let plan = plan_pull(claude, repo, &all_on_filter()).unwrap();
    apply_pull(&plan, false).unwrap()
}

/// What `sync` does: pull, then push everything the pull did not hold back.
fn sync(claude: &Path, repo: &Path) -> ArtifactReport {
    let report = pull(claude, repo);
    let held = report.paths_the_push_must_skip();
    push_artifacts(claude, repo, &all_on_filter(), &held).unwrap();
    report
}

fn kept_local(report: &ArtifactReport) -> usize {
    report.counts.iter().map(|c| c.kept_local).sum()
}

/// Machine A pushes the seed, machine B syncs: both agree on everything and
/// both records describe that agreement.
fn two_machines_in_sync(seed: &[(&str, &str)]) -> (TempDir, TempDir, TempDir) {
    let repo = TempDir::new().unwrap();
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    for (relative, text) in seed {
        write(&a.path().join(relative), text);
    }
    sync(a.path(), repo.path());
    sync(b.path(), repo.path());
    for (relative, text) in seed {
        assert_eq!(read(&b.path().join(relative)).as_deref(), Some(*text));
    }
    (repo, a, b)
}

/// What one machine did to the file since the last sync.
#[derive(Debug, Clone, Copy)]
enum Change {
    Nothing,
    Edit,
    Delete,
}

impl Change {
    fn apply(self, path: &Path, text: &str) {
        match self {
            Change::Nothing => {}
            Change::Edit => write(path, text),
            Change::Delete => fs::remove_file(path).unwrap(),
        }
    }
}

/// Every combination of what this machine (B) and the other machine (A) did
/// to one skill since they last agreed, through one cron-style sync on B. In
/// every case, no version anyone wrote may be lost.
#[test]
fn no_combination_of_local_and_remote_changes_loses_a_version() {
    let changes = [Change::Nothing, Change::Edit, Change::Delete];
    for local in changes {
        for remote in changes {
            let (repo, a, b) =
                two_machines_in_sync(&[(SKILL, "base\n"), ("skills/k/SKILL.md", "k\n")]);
            let case = format!("B {local:?}, A {remote:?}");

            remote.apply(&a.path().join(SKILL), "from A\n");
            sync(a.path(), repo.path());
            local.apply(&b.path().join(SKILL), "from B\n");

            sync(b.path(), repo.path());
            let on_b = read(&b.path().join(SKILL));
            let in_repo = read(&repo.path().join(REPO_SKILL));

            if matches!(local, Change::Edit) {
                assert_eq!(
                    on_b.as_deref(),
                    Some("from B\n"),
                    "{case}: B's edit stays on B"
                );
            }
            if matches!(remote, Change::Edit) {
                assert!(
                    on_b.as_deref() == Some("from A\n") || in_repo.as_deref() == Some("from A\n"),
                    "{case}: A's edit is on B or still in the repository \
                     (B: {on_b:?}, repo: {in_repo:?})"
                );
            }
            match (local, remote) {
                // Both edited: B keeps its edit, the repository keeps A's,
                // and neither was published over the other.
                (Change::Edit, Change::Edit) => {
                    assert_eq!(in_repo.as_deref(), Some("from A\n"), "{case}");
                }
                // Only B changed it: the sync publishes B's side.
                (Change::Edit, Change::Nothing) => {
                    assert_eq!(in_repo.as_deref(), Some("from B\n"), "{case}");
                }
                (Change::Delete, Change::Nothing) => {
                    assert_eq!(on_b, None, "{case}: B's deletion stands");
                    assert_eq!(in_repo, None, "{case}: and reaches the repository");
                }
                // Only A changed it: B takes A's side.
                (Change::Nothing, Change::Edit) => {
                    assert_eq!(on_b.as_deref(), Some("from A\n"), "{case}");
                }
                _ => {}
            }
        }
    }
}

/// The #103 scenario as `sync` on a timer meets it: edit here, sync, and the
/// edit reaches the other machine instead of being reverted.
#[test]
fn an_edit_made_only_here_survives_sync_and_reaches_the_other_machine() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n")]);
    write(&b.path().join(SKILL), "edited on B\n");

    let report = sync(b.path(), repo.path());
    assert_eq!(kept_local(&report), 0, "nothing is held back");
    assert_eq!(
        read(&b.path().join(SKILL)).as_deref(),
        Some("edited on B\n")
    );
    assert_eq!(
        read(&repo.path().join(REPO_SKILL)).as_deref(),
        Some("edited on B\n"),
        "the push half of the sync publishes the edit"
    );

    sync(a.path(), repo.path());
    assert_eq!(
        read(&a.path().join(SKILL)).as_deref(),
        Some("edited on B\n")
    );
}

/// Settings and memory files, not only skills: every raw-overwrite category
/// the issue named keeps a local edit through sync.
#[test]
fn edits_to_settings_claude_md_and_memory_survive_sync() {
    let files = [
        ("settings.json", "{\"model\":\"opus\"}"),
        ("CLAUDE.md", "# global\n"),
        ("projects/-home-a-work-app/memory/fact.md", "fact v1\n"),
    ];
    let (repo, _a, b) = two_machines_in_sync(&files);
    let edits = [
        ("settings.json", "{\"model\":\"opus\",\"hooks\":{}}"),
        ("CLAUDE.md", "# global\nMARKER\n"),
        (
            "projects/-home-a-work-app/memory/fact.md",
            "fact v1\nMARKER\n",
        ),
    ];
    for (relative, text) in edits {
        write(&b.path().join(relative), text);
    }

    sync(b.path(), repo.path());

    for (relative, text) in edits {
        assert_eq!(
            read(&b.path().join(relative)).as_deref(),
            Some(text),
            "{relative} kept its local edit"
        );
    }
}

/// A file both machines changed is never overwritten by a non-interactive
/// pull, however many times the timer fires — and the push half never sends
/// it over the other machine's version either. (#106 scenario.)
#[test]
fn a_file_both_machines_changed_is_never_resolved_without_the_user() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n")]);
    write(&a.path().join(SKILL), "edited on A\n");
    sync(a.path(), repo.path());
    write(&b.path().join(SKILL), "edited on B\n");

    for round in 0..5 {
        let report = sync(b.path(), repo.path());
        assert_eq!(kept_local(&report), 1, "round {round}: held, and reported");
        assert_eq!(
            read(&b.path().join(SKILL)).as_deref(),
            Some("edited on B\n"),
            "round {round}: B's edit is untouched"
        );
        assert_eq!(
            read(&repo.path().join(REPO_SKILL)).as_deref(),
            Some("edited on A\n"),
            "round {round}: A's edit is not overwritten in the repository"
        );
    }
}

/// Both machines changed only date-times: no prompt, no hold-back, and both
/// converge on the later dates. (#106 scenario.)
#[test]
fn a_file_that_differs_only_in_dates_converges_on_the_later_ones() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "updated: 2026-01-01T00:00:00Z\n")]);
    write(&a.path().join(SKILL), "updated: 2026-01-02T00:00:00Z\n");
    sync(a.path(), repo.path());
    write(&b.path().join(SKILL), "updated: 2026-01-03T00:00:00Z\n");

    let report = sync(b.path(), repo.path());
    assert_eq!(kept_local(&report), 0, "a date-only difference is not held");
    assert_eq!(
        read(&b.path().join(SKILL)).as_deref(),
        Some("updated: 2026-01-03T00:00:00Z\n"),
        "the later date wins"
    );
    sync(a.path(), repo.path());
    assert_eq!(
        read(&a.path().join(SKILL)).as_deref(),
        Some("updated: 2026-01-03T00:00:00Z\n"),
        "and reaches the other machine"
    );
}

/// The earlier date never wins: when this machine holds the earlier dates, it
/// takes the other machine's later ones.
#[test]
fn a_date_only_difference_takes_the_later_date_from_either_side() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "updated: 2026-01-01T00:00:00Z\n")]);
    write(&a.path().join(SKILL), "updated: 2026-01-05T00:00:00Z\n");
    sync(a.path(), repo.path());
    write(&b.path().join(SKILL), "updated: 2026-01-02T00:00:00Z\n");

    sync(b.path(), repo.path());
    assert_eq!(
        read(&b.path().join(SKILL)).as_deref(),
        Some("updated: 2026-01-05T00:00:00Z\n")
    );
    assert_eq!(
        read(&repo.path().join(REPO_SKILL)).as_deref(),
        Some("updated: 2026-01-05T00:00:00Z\n")
    );
}

/// Content that differs beside the dates is a real conflict, not a date-only
/// settle: held like any other both-sides change.
#[test]
fn a_content_change_next_to_a_date_change_is_held_not_settled() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "a\nupdated: 2026-01-01T00:00:00Z\n")]);
    write(&a.path().join(SKILL), "a\nupdated: 2026-01-02T00:00:00Z\n");
    sync(a.path(), repo.path());
    write(&b.path().join(SKILL), "b\nupdated: 2026-01-03T00:00:00Z\n");

    let report = sync(b.path(), repo.path());
    assert_eq!(kept_local(&report), 1);
    assert_eq!(
        read(&b.path().join(SKILL)).as_deref(),
        Some("b\nupdated: 2026-01-03T00:00:00Z\n")
    );
    assert_eq!(
        read(&repo.path().join(REPO_SKILL)).as_deref(),
        Some("a\nupdated: 2026-01-02T00:00:00Z\n")
    );
}

/// A file deleted here is not resurrected by the pull half of a sync; the
/// deletion reaches the repository instead. (#106 scenario.)
#[test]
fn a_deletion_made_only_here_is_not_undone_by_sync() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n"), ("skills/k/SKILL.md", "k\n")]);
    fs::remove_file(b.path().join(SKILL)).unwrap();

    sync(b.path(), repo.path());
    assert!(!b.path().join(SKILL).exists(), "the deletion stands");
    assert!(
        !repo.path().join(REPO_SKILL).exists(),
        "and reaches the repository"
    );
    sync(a.path(), repo.path());
    assert!(!a.path().join(SKILL).exists(), "and the other machine");
}

/// A local deletion never discards a change the other machine made: the
/// remote edit comes back rather than being deleted from the repository.
#[test]
fn a_deletion_here_does_not_delete_the_other_machines_edit() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n"), ("skills/k/SKILL.md", "k\n")]);
    write(&a.path().join(SKILL), "edited on A\n");
    sync(a.path(), repo.path());
    fs::remove_file(b.path().join(SKILL)).unwrap();

    sync(b.path(), repo.path());
    assert_eq!(
        read(&repo.path().join(REPO_SKILL)).as_deref(),
        Some("edited on A\n"),
        "A's edit is still in the repository"
    );
}

/// The other machine deleted a file this one edited since: the edit is kept,
/// not deleted. (#105 / #106 scenario.)
#[test]
fn a_remote_deletion_does_not_delete_a_local_edit() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n"), ("skills/k/SKILL.md", "k\n")]);
    fs::remove_file(a.path().join(SKILL)).unwrap();
    sync(a.path(), repo.path());
    write(&b.path().join(SKILL), "edited on B\n");

    sync(b.path(), repo.path());
    assert_eq!(
        read(&b.path().join(SKILL)).as_deref(),
        Some("edited on B\n")
    );
}

/// A whole category folder missing here is a machine that does not have it
/// yet, not a mass deletion: the pull restores it. (#106 scenario.)
#[test]
fn a_missing_category_folder_is_restored_not_mass_deleted() {
    let (repo, _a, b) = two_machines_in_sync(&[(SKILL, "v1\n")]);
    fs::remove_dir_all(b.path().join("skills")).unwrap();

    sync(b.path(), repo.path());
    assert_eq!(read(&b.path().join(SKILL)).as_deref(), Some("v1\n"));
    assert_eq!(read(&repo.path().join(REPO_SKILL)).as_deref(), Some("v1\n"));
}

/// A held base (a declined overwrite, a file created while a pull waited) is
/// not a common ancestor. Reading it as one would let the push publish the
/// held local file over the very repository version the user set aside.
#[test]
fn a_held_file_is_never_published_over_the_repository() {
    let (repo, _a, b) = two_machines_in_sync(&[(SKILL, "v1\n")]);
    let repo_bytes = fs::read(repo.path().join(REPO_SKILL)).unwrap();
    let mut recorded = bases::load(b.path(), repo.path());
    recorded.insert(REPO_SKILL.to_string(), bases::held_base(&repo_bytes));
    bases::save(b.path(), repo.path(), recorded).unwrap();
    write(&b.path().join(SKILL), "held on B\n");

    let report = sync(b.path(), repo.path());
    assert_eq!(kept_local(&report), 1, "a held file stays held");
    assert_eq!(read(&b.path().join(SKILL)).as_deref(), Some("held on B\n"));
    assert_eq!(
        read(&repo.path().join(REPO_SKILL)).as_deref(),
        Some("v1\n"),
        "the repository version the user set aside is not overwritten"
    );
}

/// The protection does not depend on the pull running right after the edit:
/// a pull that fast-forwards one file must leave an unrelated local edit
/// alone in the same run.
#[test]
fn a_remote_change_to_one_file_does_not_touch_a_local_edit_to_another() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n"), ("CLAUDE.md", "# global\n")]);
    write(&a.path().join("CLAUDE.md"), "# global from A\n");
    sync(a.path(), repo.path());
    write(&b.path().join(SKILL), "edited on B\n");

    sync(b.path(), repo.path());
    assert_eq!(
        read(&b.path().join(SKILL)).as_deref(),
        Some("edited on B\n")
    );
    assert_eq!(
        read(&b.path().join("CLAUDE.md")).as_deref(),
        Some("# global from A\n")
    );
    assert_eq!(
        read(&repo.path().join(REPO_SKILL)).as_deref(),
        Some("edited on B\n")
    );
}

/// A memory index edited on both machines is not union-merged over the
/// local edit by a non-interactive pull. (#105 scenario.)
#[test]
fn a_memory_index_edited_on_both_machines_keeps_the_local_entry() {
    let index = "projects/-home-a-work-app/memory/MEMORY.md";
    let (repo, a, b) = two_machines_in_sync(&[(index, "- [shared](shared.md) v1\n")]);
    write(
        &a.path().join(index),
        "- [shared](shared.md) v1\n- [a](a.md) from A\n",
    );
    sync(a.path(), repo.path());
    write(&b.path().join(index), "- [shared](shared.md) edited on B\n");

    sync(b.path(), repo.path());
    let on_b = read(&b.path().join(index)).unwrap();
    assert!(on_b.contains("edited on B"), "B's entry survives: {on_b}");
}

/// A pull with nothing to take writes nothing to a file: undo history and
/// the user's file both stay exactly as they were.
#[test]
fn a_local_only_pull_rewrites_no_local_file() {
    let (repo, _a, b) = two_machines_in_sync(&[(SKILL, "v1\n")]);
    write(&b.path().join(SKILL), "edited on B\n");
    let before = fs::metadata(b.path().join(SKILL))
        .unwrap()
        .modified()
        .unwrap();

    let plan = plan_pull(b.path(), repo.path(), &all_on_filter()).unwrap();
    assert_eq!(plan.local_only.len(), 1);
    assert!(plan.overwrites.is_empty() && plan.kept_local.is_empty());
    let report = apply_pull(&plan, false).unwrap();

    assert!(
        report.changes.is_empty(),
        "nothing written: {:?}",
        report.changes
    );
    let after = fs::metadata(b.path().join(SKILL))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after);
}

/// Ported from #106 (FluffyDiscord): a setting that embeds this machine's
/// paths is compared after rendering for this machine, so a change only the
/// other machine made reads as remote-only and arrives — it is not mistaken
/// for a local edit (held forever) or a both-sides change.
#[test]
fn a_remote_change_to_a_path_bearing_setting_arrives_rendered_for_this_machine() {
    let repo = TempDir::new().unwrap();
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    // Built with serde_json so a Windows path is escaped as Claude Code
    // writes it (the form PathTokens matches).
    let setting = |claude: &Path, model: &str| {
        let hook = format!("{}/hooks/run.sh", claude.display());
        serde_json::json!({ "model": model, "hook": hook }).to_string()
    };
    write(&a.path().join("settings.json"), &setting(a.path(), "opus"));
    sync(a.path(), repo.path());
    sync(b.path(), repo.path());
    write(
        &a.path().join("settings.json"),
        &setting(a.path(), "sonnet"),
    );
    sync(a.path(), repo.path());

    let plan = plan_pull(b.path(), repo.path(), &all_on_filter()).unwrap();
    assert!(
        plan.kept_local.is_empty() && plan.local_only.is_empty(),
        "B never touched it, so only the remote changed it"
    );
    apply_pull(&plan, false).unwrap();

    assert_eq!(
        read(&b.path().join("settings.json")).as_deref(),
        Some(setting(b.path(), "sonnet").as_str())
    );
}

/// Upgrading from 0.4.x: the machine has synced this repository before but
/// holds no bases yet. A file that differs may be an edit made here, and with
/// no base nothing tells it apart from a stale copy — so the first sync after
/// the upgrade holds it instead of overwriting it.
#[test]
fn the_first_sync_after_an_upgrade_does_not_overwrite_a_local_edit() {
    let files = [
        (SKILL, "v1\n"),
        ("settings.json", "{\"model\":\"opus\"}"),
        ("CLAUDE.md", "# global\n"),
        ("projects/-home-a-work-app/memory/fact.md", "fact v1\n"),
    ];
    let (repo, a, b) = two_machines_in_sync(&files);
    for (relative, _) in files {
        write(&a.path().join(relative), "from A\n");
    }
    sync(a.path(), repo.path());
    for (relative, _) in files {
        write(&b.path().join(relative), "edited on B before upgrading\n");
    }
    // What 0.4.x leaves behind: the tracked record, no bases.
    fs::remove_file(bases::record_path(b.path())).unwrap();

    sync(b.path(), repo.path());
    for (relative, _) in files {
        assert_eq!(
            read(&b.path().join(relative)).as_deref(),
            Some("edited on B before upgrading\n"),
            "{relative}: the pre-upgrade edit is not overwritten"
        );
    }
}

/// A machine new to this repository (no records at all) takes the
/// repository's versions: the hold applies only to a machine that synced
/// before, so onboarding a second machine still works.
#[test]
fn a_machine_new_to_the_repository_takes_the_repository_versions() {
    let (repo, _a, _b) = two_machines_in_sync(&[("settings.json", "{\"model\":\"opus\"}")]);
    let fresh = TempDir::new().unwrap();
    write(&fresh.path().join("settings.json"), "{}");

    sync(fresh.path(), repo.path());
    assert_eq!(
        read(&fresh.path().join("settings.json")).as_deref(),
        Some("{\"model\":\"opus\"}")
    );
}

/// Both machines created the same path independently — this machine's copy
/// was never synced, so it has neither a base nor a tracked entry. It is not
/// overwritten by the other machine's copy; it is held like any both-sides
/// change.
#[test]
fn a_file_created_here_and_on_the_other_machine_is_not_overwritten() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n")]);
    let new_skill = "skills/new/SKILL.md";
    write(&a.path().join(new_skill), "created on A\n");
    sync(a.path(), repo.path());
    write(&b.path().join(new_skill), "created on B\n");

    let report = sync(b.path(), repo.path());
    assert_eq!(kept_local(&report), 1);
    assert_eq!(
        read(&b.path().join(new_skill)).as_deref(),
        Some("created on B\n")
    );
    assert_eq!(
        read(&repo.path().join("artifacts/skills/new/SKILL.md")).as_deref(),
        Some("created on A\n")
    );
}

/// A sync whose pull is interrupted between planning and applying (a prompt,
/// a slow merge tool): `during` runs in that window, the way the user's own
/// edits would.
fn sync_with_a_window(
    claude: &Path,
    repo: &Path,
    during: impl FnOnce(&mut claude_code_sync::artifacts::engine::PullPlan),
) -> ArtifactReport {
    let mut plan = plan_pull(claude, repo, &all_on_filter()).unwrap();
    during(&mut plan);
    let report = apply_pull(&plan, false).unwrap();
    push_artifacts(
        claude,
        repo,
        &all_on_filter(),
        &report.paths_the_push_must_skip(),
    )
    .unwrap();
    report
}

/// The other machine created a file; this machine creates the same path
/// while the pull waits. The pull must not overwrite it — and the push right
/// after must not publish it over the other machine's version either.
#[test]
fn a_file_created_here_while_the_pull_waits_is_not_published_over_the_other_machines() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n")]);
    let new_skill = "skills/new/SKILL.md";
    write(&a.path().join(new_skill), "created on A\n");
    sync(a.path(), repo.path());

    sync_with_a_window(b.path(), repo.path(), |_| {
        write(&b.path().join(new_skill), "created on B mid-pull\n")
    });
    let repo_copy = repo.path().join("artifacts/skills/new/SKILL.md");
    assert_eq!(
        read(&b.path().join(new_skill)).as_deref(),
        Some("created on B mid-pull\n")
    );
    assert_eq!(read(&repo_copy).as_deref(), Some("created on A\n"));

    // And it stays that way: later syncs hold it until the user decides.
    for _ in 0..3 {
        sync(b.path(), repo.path());
        assert_eq!(read(&repo_copy).as_deref(), Some("created on A\n"));
        assert_eq!(
            read(&b.path().join(new_skill)).as_deref(),
            Some("created on B mid-pull\n")
        );
    }
}

/// Only the other machine edited the file, and this machine deletes it while
/// the pull waits. The push must not mirror that deletion over the other
/// machine's edit.
#[test]
fn a_deletion_made_while_the_pull_waits_does_not_delete_the_other_machines_edit() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n"), ("skills/k/SKILL.md", "k\n")]);
    write(&a.path().join(SKILL), "edited on A\n");
    sync(a.path(), repo.path());

    sync_with_a_window(b.path(), repo.path(), |plan| {
        assert_eq!(plan.overwrites.len(), 1);
        fs::remove_file(b.path().join(SKILL)).unwrap();
    });
    assert_eq!(
        read(&repo.path().join(REPO_SKILL)).as_deref(),
        Some("edited on A\n"),
        "A's edit is still in the repository"
    );
}

/// An overwrite the pull could not back up is not carried out — and the push
/// must not then publish the stale local copy over the other machine's edit.
#[test]
fn an_overwrite_without_a_backup_does_not_publish_the_stale_local_copy() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n")]);
    write(&a.path().join(SKILL), "edited on A\n");
    sync(a.path(), repo.path());

    let local = b.path().join(SKILL);
    sync_with_a_window(b.path(), repo.path(), |plan| {
        plan.unsnapshotted.push(local.clone())
    });
    assert_eq!(
        read(&local).as_deref(),
        Some("v1\n"),
        "not modified without a backup"
    );
    assert_eq!(
        read(&repo.path().join(REPO_SKILL)).as_deref(),
        Some("edited on A\n"),
        "and the stale v1 is not published over A's edit"
    );
    sync(b.path(), repo.path());
    assert_eq!(
        read(&local).as_deref(),
        Some("edited on A\n"),
        "the next pull takes it"
    );
}

/// A number that merely looks like a timestamp is content: two different
/// values are a conflict to hold, not a date difference to settle silently
/// on the larger one.
#[test]
fn a_number_shaped_like_a_timestamp_is_not_settled_silently() {
    let (repo, a, b) = two_machines_in_sync(&[("settings.json", "{\"accountId\": 1700000000000}")]);
    write(
        &a.path().join("settings.json"),
        "{\"accountId\": 1790837225831}",
    );
    sync(a.path(), repo.path());
    write(
        &b.path().join("settings.json"),
        "{\"accountId\": 1790137212675}",
    );

    let report = sync(b.path(), repo.path());
    assert_eq!(kept_local(&report), 1, "held, not settled");
    assert_eq!(
        read(&b.path().join("settings.json")).as_deref(),
        Some("{\"accountId\": 1790137212675}")
    );
    assert_eq!(
        read(&repo.path().join("artifacts/settings/settings.json")).as_deref(),
        Some("{\"accountId\": 1790837225831}")
    );
}

/// A memory index that already matches the repository is in sync, even with
/// no recorded base (an upgrade from 0.4.x): it must not be held on every
/// sync, and it must publish later edits normally.
#[test]
fn an_identical_memory_index_without_a_base_is_not_held_forever() {
    let index = "projects/-home-a-work-app/memory/MEMORY.md";
    let (repo, _a, b) = two_machines_in_sync(&[(index, "- [a](a.md) one\n")]);
    fs::remove_file(bases::record_path(b.path())).unwrap();

    let report = sync(b.path(), repo.path());
    assert_eq!(kept_local(&report), 0, "identical copies are in sync");
    let report = sync(b.path(), repo.path());
    assert_eq!(kept_local(&report), 0, "and stay that way");

    write(&b.path().join(index), "- [a](a.md) one\n- [b](b.md) two\n");
    let report = sync(b.path(), repo.path());
    assert_eq!(kept_local(&report), 0);
    let in_repo = read(&repo.path().join(index)).unwrap();
    assert!(
        in_repo.contains("[b](b.md) two"),
        "the later edit is published: {in_repo}"
    );
}

/// A pull whose only change is a memory-index union merge records a new base
/// for it, so the snapshot must carry the base record and the undo must know
/// the key: otherwise `undo pull` restores the file but not its base, and the
/// restored file reads as edited here from then on.
#[test]
fn a_memory_index_union_merge_is_declared_for_undo() {
    let index = "projects/-home-a-work-app/memory/MEMORY.md";
    let (repo, a, b) = two_machines_in_sync(&[(index, "- [a](a.md) one\n")]);
    write(
        &a.path().join(index),
        "- [a](a.md) one\n- [b](b.md) from A\n",
    );
    sync(a.path(), repo.path());

    let plan = plan_pull(b.path(), repo.path(), &all_on_filter()).unwrap();
    assert_eq!(plan.unions.len(), 1);
    assert!(plan.rewrites_bases, "the apply writes the base record");
    assert!(
        plan.touched_base_keys().contains(&index.to_string()),
        "undo restores the index's base key"
    );
    let report = apply_pull(&plan, false).unwrap();
    for key in &report.bases_keys_written {
        assert!(
            plan.touched_base_keys().contains(key),
            "every base key the apply wrote was declared: {key}"
        );
    }
}

/// What an interactive merge (or keep-both) leaves: the merged file locally,
/// with the repository version it was resolved against recorded as the base.
/// The next cron sync publishes the resolution instead of holding it forever.
#[test]
fn a_resolved_conflict_is_published_by_the_next_sync() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n")]);
    write(&a.path().join(SKILL), "edited on A\n");
    sync(a.path(), repo.path());
    write(&b.path().join(SKILL), "edited on B\n");
    assert_eq!(
        kept_local(&sync(b.path(), repo.path())),
        1,
        "held until resolved"
    );

    resolve_against_the_repository(b.path(), repo.path(), "merged A and B\n");
    let report = sync(b.path(), repo.path());
    assert_eq!(kept_local(&report), 0, "the resolution is not held");
    assert_eq!(
        read(&repo.path().join(REPO_SKILL)).as_deref(),
        Some("merged A and B\n")
    );
    sync(a.path(), repo.path());
    assert_eq!(
        read(&a.path().join(SKILL)).as_deref(),
        Some("merged A and B\n")
    );
}

/// A resolution made against a repository version the other machine has since
/// replaced is held again: it does not overwrite the newer change.
#[test]
fn a_resolution_overtaken_by_a_newer_remote_change_is_held() {
    let (repo, a, b) = two_machines_in_sync(&[(SKILL, "v1\n")]);
    write(&a.path().join(SKILL), "edited on A\n");
    sync(a.path(), repo.path());
    write(&b.path().join(SKILL), "edited on B\n");
    sync(b.path(), repo.path());

    resolve_against_the_repository(b.path(), repo.path(), "merged A and B\n");
    write(&a.path().join(SKILL), "edited on A again\n");
    sync(a.path(), repo.path());

    let report = sync(b.path(), repo.path());
    assert_eq!(kept_local(&report), 1);
    assert_eq!(
        read(&repo.path().join(REPO_SKILL)).as_deref(),
        Some("edited on A again\n")
    );
    assert_eq!(
        read(&b.path().join(SKILL)).as_deref(),
        Some("merged A and B\n")
    );
}

/// The state `pull -i` leaves after the user resolves the skill: `resolution`
/// written locally, the repository version it was made against as the base.
fn resolve_against_the_repository(claude: &Path, repo: &Path, resolution: &str) {
    let repo_bytes = fs::read(repo.join(REPO_SKILL)).unwrap();
    let mut recorded = bases::load(claude, repo);
    recorded.insert(REPO_SKILL.to_string(), bases::hash_bytes(&repo_bytes));
    bases::save(claude, repo, recorded).unwrap();
    write(&claude.join(SKILL), resolution);
}

/// A "keep both" conflict copy (the other machine's version, saved beside
/// this machine's) stays on this machine: syncing it would plant an extra
/// command, skill or agent on every machine.
#[test]
fn a_keep_both_conflict_copy_is_not_pushed() {
    let (repo, _a, b) = two_machines_in_sync(&[("commands/deploy.md", "deploy\n")]);
    let copy = "commands/deploy.sync-conflict-20261009-120000-host.md";
    write(&b.path().join(copy), "the other machine's deploy\n");

    sync(b.path(), repo.path());
    assert!(!repo.path().join("artifacts").join(copy).exists());
    assert!(
        b.path().join(copy).exists(),
        "the copy itself is left alone"
    );
}

/// A user's own `deleted/` folder inside project memory (archived notes, say)
/// is content, not deletion markers: a memory-index merge must never drop an
/// entry because a file of the same name sits there.
#[test]
fn a_users_deleted_folder_does_not_drop_memory_index_entries() {
    let memory = "projects/-home-a-work-app/memory";
    let index = format!("{memory}/MEMORY.md");
    let (repo, a, b) = two_machines_in_sync(&[
        (&index, "- [a](a.md) keep me\n"),
        (&format!("{memory}/a.md"), "a\n"),
        (&format!("{memory}/deleted/a.md"), "an archived note\n"),
    ]);
    write(
        &a.path().join(&index),
        "- [a](a.md) keep me\n- [b](b.md) from A\n",
    );
    sync(a.path(), repo.path());

    sync(b.path(), repo.path());
    let on_b = read(&b.path().join(&index)).unwrap();
    assert!(
        on_b.contains("[a](a.md) keep me"),
        "entry a survives: {on_b}"
    );
    assert!(
        on_b.contains("[b](b.md) from A"),
        "and A's entry arrives: {on_b}"
    );
}
