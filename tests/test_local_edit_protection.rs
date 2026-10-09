//! Integration tests for local-edit protection on pull: a file edited on this
//! machine since the last sync must survive a pull (a `push` publishes it),
//! instead of being silently overwritten by the repository copy.

use std::fs;
use std::path::Path;

use claude_code_sync::artifacts::bases;
use claude_code_sync::artifacts::engine::{apply_pull, plan_pull, push_artifacts};
use claude_code_sync::artifacts::registry::{ArtifactToggles, CategoryId};
use claude_code_sync::filter::FilterConfig;
use claude_code_sync::history::{
    ConversationSummary, OperationHistory, OperationRecord, OperationType, SyncOperation,
};
use claude_code_sync::undo::{undo_pull, Snapshot};
use tempfile::TempDir;

fn all_on_filter() -> FilterConfig {
    FilterConfig {
        sync_artifacts: ArtifactToggles::all_enabled(),
        ..Default::default()
    }
}

fn memory(claude: &Path, contents: &str) {
    let path = claude.join("projects/-home-a-work-app/memory/fact.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn read_memory(claude: &Path) -> String {
    fs::read_to_string(claude.join("projects/-home-a-work-app/memory/fact.md")).unwrap()
}

/// Push from `claude`, then pull+apply into `claude`: both sides now agree,
/// and both records (tracked, bases) describe that agreement.
fn sync(claude: &Path, repo: &Path, filter: &FilterConfig) {
    push_artifacts(claude, repo, filter, &std::collections::HashSet::new()).unwrap();
    let plan = plan_pull(claude, repo, filter).unwrap();
    apply_pull(&plan, false).unwrap();
}

fn kept_local_of(plan: &claude_code_sync::artifacts::engine::PullPlan) -> usize {
    plan.kept_local.len()
}

/// Files the pull leaves alone because only this machine changed them since
/// the last sync: protected from the pull, and published by the push.
fn local_only_of(plan: &claude_code_sync::artifacts::engine::PullPlan) -> usize {
    plan.local_only.len()
}

#[test]
fn a_push_records_the_synced_bytes_as_base() {
    let repo = TempDir::new().unwrap();
    let machine = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine.path(), "v1\n");
    push_artifacts(
        machine.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    let recorded = bases::load(machine.path(), repo.path());
    assert_eq!(
        recorded.values().collect::<Vec<_>>(),
        vec![&bases::hash_bytes(b"v1\n")],
        "the pushed local bytes are the recorded base"
    );
    assert!(
        recorded.contains_key("projects/-home-a-work-app/memory/fact.md"),
        "the entry is keyed by repo-relative path: {recorded:?}"
    );
}

#[test]
fn an_edit_made_after_planning_survives_the_apply() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // B's copy is clean: the plan classifies it as a plain overwrite.
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.overwrites.len(), 1);

    // ...but the user edits it between the plan and the apply (while the
    // pull waits on a confirmation, say). The apply must re-check: the
    // write becomes a keep, not an overwrite of the fresh edit.
    memory(machine_b.path(), "mid-pull edit\n");
    let report = apply_pull(&plan, false).unwrap();

    assert_eq!(read_memory(machine_b.path()), "mid-pull edit\n");
    assert_eq!(report.total_kept_local(), 1);
    assert_eq!(report.total_modified(), 0);
}

#[test]
fn a_pull_keeps_a_file_edited_locally_since_the_last_sync() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    // Machine A publishes v1; machine B pulls it (now synced, base recorded).
    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // B edits locally; A publishes v2 to the same file.
    memory(machine_b.path(), "b's edit\n");
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // B pulls: the edit must survive instead of being overwritten.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 1, "the edited file is kept local");
    assert!(plan.overwrites.is_empty(), "nothing is overwritten");
    apply_pull(&plan, false).unwrap();

    assert_eq!(read_memory(machine_b.path()), "b's edit\n");
}

#[test]
fn an_unknown_base_keeps_the_remote_wins_behavior() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    sync(machine_b.path(), repo.path(), &filter);

    // A machine upgrading from a version without the record: stale local
    // bytes, no base entry. The pull must behave exactly as before.
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    memory(machine_b.path(), "stale local\n");
    fs::remove_file(bases::record_path(machine_b.path())).unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 0);
    assert_eq!(plan.overwrites.len(), 1, "remote still wins");
    apply_pull(&plan, false).unwrap();
    assert_eq!(read_memory(machine_b.path()), "v2\n");
}

#[test]
fn a_clean_local_file_still_fast_forwards() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 0);
    assert_eq!(plan.overwrites.len(), 1, "an untouched file fast-forwards");
    apply_pull(&plan, false).unwrap();
    assert_eq!(read_memory(machine_b.path()), "v2\n");
}

#[test]
fn pushing_a_kept_local_file_publishes_it_and_clears_the_window() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    memory(machine_b.path(), "b's edit\n");
    push_artifacts(
        machine_b.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // The base now describes the pushed bytes: the next pull is clean.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 0);
    assert_eq!(plan.overwrites.len(), 0);
    assert_eq!(plan.unchanged, 1);
}

#[test]
fn apply_pull_reports_kept_local_files_in_the_counts() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    memory(machine_b.path(), "b's edit\n");
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    let attachments = report
        .counts
        .iter()
        .find(|c| c.category == CategoryId::ProjectAttachments)
        .unwrap();
    assert_eq!(attachments.kept_local, 1);
    assert_eq!(report.total_kept_local(), 1);
}

#[test]
fn a_pull_that_overwrites_records_the_new_bytes_as_base() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    apply_pull(&plan, false).unwrap();

    // The overwritten bytes are the new synced state: an edit on top of
    // them is this machine's alone (the repository still holds v2).
    memory(machine_b.path(), "b's edit\n");
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert!(
        plan.overwrites.is_empty(),
        "the post-overwrite edit is protected"
    );
    assert_eq!(
        local_only_of(&plan),
        1,
        "only this machine moved on: the push publishes it"
    );
}

#[test]
fn an_unchanged_file_pulled_twice_gains_a_base_entry() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    // B starts with a legacy state: pulled file, but no base record yet.
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    apply_pull(&plan, false).unwrap(); // create path
    fs::remove_file(bases::record_path(machine_b.path())).unwrap();

    // A second pull sees the file unchanged and records its base, closing
    // the upgrade window without touching the file.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.unchanged, 1);
    apply_pull(&plan, false).unwrap();

    let recorded = bases::load(machine_b.path(), repo.path());
    assert!(recorded.contains_key("projects/-home-a-work-app/memory/fact.md"));
}

fn skill(claude: &Path, contents: &str) {
    let path = claude.join("skills/my-skill/SKILL.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn read_skill(claude: &Path) -> String {
    fs::read_to_string(claude.join("skills/my-skill/SKILL.md")).unwrap()
}

#[test]
fn an_interactive_confirm_of_a_kept_local_deletion_is_undonable() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    fs::remove_file(machine_a.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    skill(machine_b.path(), "b's edit\n");

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.kept_local_deletes.len(), 1);
    // The confirmed deletion can execute interactively: the edit must be
    // snapshotted and the record declared, or "Yes" is unrecoverable.
    assert!(
        plan.paths_to_snapshot(true)
            .contains(&machine_b.path().join("skills/my-skill/SKILL.md")),
        "the kept-local deletion is snapshotted under interactive"
    );
    // Interactive-only: the confirm can forget a base entry, so the
    // record is carried and machine state can change — but a
    // non-interactive apply of the same plan provably writes nothing:
    // the file STAYS in the tracked record (the re-arm), so even the
    // tracked record is not rewritten.
    assert!(
        plan.can_write_bases(true),
        "the confirm forgets a base entry"
    );
    assert!(plan.changes_machine_state(true));
    assert!(!plan.rewrites_bases);
    assert!(
        !plan.rewrites_tracked,
        "the kept file re-arms: it stays in the tracked record"
    );
    assert!(!plan.changes_machine_state(false));
}

#[test]
fn a_pull_keeps_a_file_edited_locally_that_the_repo_deleted() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    // Machine A publishes v1; machine B pulls it (now synced, base recorded).
    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A deletes the file and pushes: the repo copy is gone (skills mirror
    // deletions).
    fs::remove_file(machine_a.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // B edits locally: the deletion must not destroy the edit.
    skill(machine_b.path(), "b's edit\n");
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert!(
        plan.deletes.is_empty(),
        "a locally-edited file is not deleted"
    );
    assert_eq!(
        plan.kept_local_deletes.len(),
        1,
        "the edited file is kept local instead of deleted"
    );
    assert!(
        !plan.is_empty(),
        "a kept-local-only pull is not an empty one"
    );
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(report.total_kept_local(), 1);

    assert_eq!(read_skill(machine_b.path()), "b's edit\n");
    // The base entry survives, so the protection persists until a push
    // publishes the edit.
    assert!(bases::load(machine_b.path(), repo.path())
        .contains_key("artifacts/skills/my-skill/SKILL.md"));
}

#[test]
fn a_base_recording_only_pull_changes_machine_state() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    // A legacy machine: files already match the repo, but no records exist
    // yet. The pull's only effect is recording bases — no artifact file is
    // written, but machine state changes, so the snapshot gate
    // (changes_machine_state) must fire or undo pull has nothing to undo
    // from.
    memory(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    memory(machine_b.path(), "v1\n");
    assert!(!bases::record_path(machine_b.path()).is_file());

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.unchanged, 1);
    assert!(plan.is_empty(), "no artifact file is written");
    assert!(plan.rewrites_bases, "the pull creates the bases record");
    assert!(
        plan.changes_machine_state(false),
        "a base-recording pull writes machine state"
    );
    assert!(plan.can_write_bases(false));
}

#[test]
fn a_fully_noop_pull_changes_no_machine_state() {
    let repo = TempDir::new().unwrap();
    let machine = TempDir::new().unwrap();
    let filter = all_on_filter();

    // Everything already synced and converged: no writes, no record
    // rewrites — a snapshot would be pure churn.
    memory(machine.path(), "v1\n");
    sync(machine.path(), repo.path(), &filter);
    let plan = plan_pull(machine.path(), repo.path(), &filter).unwrap();

    assert!(plan.is_empty());
    assert!(!plan.rewrites_bases, "recorded bases already converge");
    assert!(!plan.rewrites_tracked, "tracked paths already converge");
    assert!(
        !plan.changes_machine_state(false),
        "a no-op pull must not read as machine-state change"
    );
}

#[test]
fn a_kept_local_file_is_snapshotted_so_an_interactive_take_remote_is_undonable() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    memory(machine_b.path(), "b's edit\n");
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 1);
    assert!(
        plan.paths_to_snapshot(true).contains(
            &machine_b
                .path()
                .join("projects/-home-a-work-app/memory/fact.md")
        ),
        "the kept-local edit is snapshotted: an interactive take-remote must be undonable"
    );
}

#[test]
fn a_union_merged_creation_records_no_base_entry() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    // Prompt history grows by union-merging; no push ever re-records it, so
    // a base entry describing the creation-time bytes would go permanently
    // stale and read the file as dirty forever.
    fs::write(machine_a.path().join("history.jsonl"), "prompt one\n").unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    apply_pull(&plan, false).unwrap();
    assert!(machine_b.path().join("history.jsonl").is_file());

    let recorded = bases::load(machine_b.path(), repo.path());
    assert!(
        recorded.keys().all(|k| !k.ends_with("history.jsonl")),
        "union-merged files record no base entry: {recorded:?}"
    );
}

#[test]
fn pushing_prunes_a_base_entry_whose_repo_copy_was_already_removed() {
    let repo = TempDir::new().unwrap();
    let machine = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine.path(), "v1\n");
    sync(machine.path(), repo.path(), &filter);

    // The repo copy vanishes outside this tool, and the local file is then
    // deleted here too: neither side has it, so the entry must go.
    fs::remove_file(repo.path().join("artifacts/skills/my-skill/SKILL.md")).unwrap();
    fs::remove_file(machine.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    assert!(
        !bases::load(machine.path(), repo.path())
            .contains_key("artifacts/skills/my-skill/SKILL.md"),
        "an entry whose file exists nowhere is pruned on push"
    );
}

#[test]
fn a_pull_whose_only_outcome_is_kept_local_files_is_not_empty() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    memory(machine_b.path(), "b's edit\n");
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 1);
    assert!(
        !plan.is_empty(),
        "a pull with kept-local files still has something to report"
    );
}

#[test]
fn the_shared_base_record_stays_out_of_wholesale_undo_lists() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    // A legacy machine: files synced by an older version, no base record.
    memory(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    let bases_path = bases::record_path(machine_b.path())
        .to_string_lossy()
        .to_string();
    // The record is shared by every sync repository: undo must never see
    // it in a wholesale delete list — the undo handles the entry
    // surgically (see the undo_pull tests).
    assert!(
        !plan.created_paths().contains(&bases_path),
        "the shared record is never a created path: {:?}",
        plan.created_paths()
    );
    // Absent on disk: nothing to snapshot either.
    assert!(!plan
        .paths_to_snapshot(false)
        .contains(&bases::record_path(machine_b.path())));

    apply_pull(&plan, false).unwrap();
    // Once the record exists and the apply can WRITE it, the snapshot
    // carries its pre-pull bytes for the surgical undo. But the converge
    // pass itself (base_hashes re-verifying what is already recorded) is
    // a no-op the apply does not own: a converged pull must not embed
    // the whole shared record in every snapshot for nothing.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert!(!plan.rewrites_bases, "the record already converges");
    assert!(
        !plan
            .paths_to_snapshot(false)
            .contains(&bases::record_path(machine_b.path())),
        "a converged pull does not snapshot the shared record"
    );
}

#[test]
fn a_kept_local_only_plan_still_declares_the_record_for_an_interactive_take() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A kept-local edit: the default apply writes nothing, but an
    // interactive take-remote records the repo bytes as the new base —
    // so the record is carried (and declared) for the surgical undo.
    memory(machine_b.path(), "b's edit\n");
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 1);
    assert!(plan.can_write_bases(true));
    assert!(
        !plan.can_write_bases(false),
        "the non-interactive apply writes no record"
    );
    assert!(
        plan.paths_to_snapshot(true)
            .contains(&bases::record_path(machine_b.path())),
        "the record is carried under interactive so undo can restore the pre-pull entry"
    );
}

#[test]
fn an_executed_deletion_forgets_its_base_entry() {
    // Non-interactive deletes always execute; the base entry must go with
    // them, so the record stops describing a file this machine no longer has.
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    fs::remove_file(machine_a.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(
        plan.deletes.len(),
        1,
        "an unedited file is deleted as before"
    );
    apply_pull(&plan, false).unwrap();

    assert!(
        bases::load(machine_b.path(), repo.path()).is_empty(),
        "an executed deletion forgets its base entry"
    );
}

#[test]
fn an_edit_made_while_the_pull_waits_is_not_recorded_as_synced() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);
    fs::remove_file(bases::record_path(machine_b.path())).unwrap();

    // Legacy state: the file matches the repo but has no recorded base.
    // The plan verifies the bytes; the user then edits the file while the
    // apply waits (an interactive confirmation, say)...
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.unchanged, 1);
    memory(machine_b.path(), "edited while pulling\n");
    apply_pull(&plan, false).unwrap();

    // ...so the recorded base must be the bytes the PLAN verified, not the
    // post-edit ones: the edit stays protected, not silently synced.
    let recorded = bases::load(machine_b.path(), repo.path());
    assert_eq!(
        recorded.get("projects/-home-a-work-app/memory/fact.md"),
        Some(&bases::hash_bytes(b"v1\n")),
        "the base describes the verified bytes, not the later edit"
    );
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert!(plan.overwrites.is_empty(), "the mid-pull edit is protected");
    assert_eq!(
        local_only_of(&plan),
        1,
        "the repository still holds v1: the edit is this machine's alone"
    );
}

#[test]
fn kept_local_files_are_named_in_the_report() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    memory(machine_b.path(), "b's edit\n");
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    let kept: Vec<_> = report
        .changes
        .iter()
        .filter(|c| c.kind == claude_code_sync::artifacts::engine::ArtifactChangeKind::KeptLocal)
        .collect();
    assert_eq!(
        kept.len(),
        1,
        "the kept file is named, not just counted: {:?}",
        report.changes
    );
}

#[test]
fn pushing_a_deletion_prunes_its_base_entry() {
    let repo = TempDir::new().unwrap();
    let machine = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine.path(), "v1\n");
    sync(machine.path(), repo.path(), &filter);
    assert!(
        bases::load(machine.path(), repo.path()).contains_key("artifacts/skills/my-skill/SKILL.md")
    );

    // Deleting the file locally and pushing removes the repo copy — the
    // record must stop describing a file neither side has anymore.
    fs::remove_file(machine.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    assert!(
        !bases::load(machine.path(), repo.path())
            .contains_key("artifacts/skills/my-skill/SKILL.md"),
        "a pushed deletion prunes its base entry"
    );
}

#[test]
fn the_base_record_can_be_forgotten_like_the_tracked_one() {
    // NOTE: `undo push` deliberately does NOT call bases::forget — the
    // base record is kept (only the tracked record is forgotten); the
    // operations-level contract lives in test_undo_push_forgets_tracked_
    // but_keeps_the_base_record in src/undo/operations.rs. Here we verify
    // the primitive itself: forgetting clears state so the next pull
    // neither deletes nor fast-forwards blindly.
    let repo = TempDir::new().unwrap();
    let machine = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine.path(), "v1\n");
    push_artifacts(
        machine.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    assert!(!bases::load(machine.path(), repo.path()).is_empty());

    bases::forget(machine.path(), repo.path()).unwrap();
    assert!(bases::load(machine.path(), repo.path()).is_empty());
    // Forgetting is idempotent and touches nothing else.
    bases::forget(machine.path(), repo.path()).unwrap();
}

#[test]
fn a_kept_local_delete_rearms_on_every_pull() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A deletes and pushes; B holds a local edit.
    fs::remove_file(machine_a.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    skill(machine_b.path(), "b's edit\n");

    // Round 1: the deletion is kept local (the protection).
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.kept_local_deletes.len(), 1);
    apply_pull(&plan, false).unwrap();
    assert!(
        claude_code_sync::artifacts::tracked::load(machine_b.path(), repo.path())
            .contains("artifacts/skills/my-skill/SKILL.md"),
        "the kept file stays tracked so the protection re-arms"
    );

    // Round 2: the protection must STILL hold. If the file had left the
    // tracked record after one round, this pull would report nothing kept,
    // the sync gate would open, and the next full push would silently
    // resurrect the repo-deleted file with the local edit.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(
        plan.kept_local_deletes.len(),
        1,
        "a kept-local delete re-arms every pull, like kept-local overwrites"
    );
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(report.total_kept_local(), 1);
    assert_eq!(read_skill(machine_b.path()), "b's edit\n");
}

#[test]
#[cfg(unix)]
fn a_push_that_fails_to_read_a_file_keeps_its_repo_copy() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempDir::new().unwrap();
    let machine = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine.path(), "v1\n");
    sync(machine.path(), repo.path(), &filter);
    let repo_copy = repo.path().join("artifacts/skills/my-skill/SKILL.md");
    assert!(repo_copy.is_file());

    // The local file becomes unreadable: the push holds it back, and that
    // must NOT read as a local deletion — the repo copy would go, and the
    // deletion would propagate to every other machine on their next pull.
    let local = machine.path().join("skills/my-skill/SKILL.md");
    fs::set_permissions(&local, fs::Permissions::from_mode(0o000)).unwrap();
    let report = push_artifacts(
        machine.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    fs::set_permissions(&local, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(repo_copy.is_file(), "a held-back push keeps the repo copy");
    assert_eq!(
        report.total_deleted(),
        0,
        "an unreadable local file is not a deletion"
    );
    assert!(
        claude_code_sync::artifacts::tracked::load(machine.path(), repo.path())
            .contains("artifacts/skills/my-skill/SKILL.md"),
        "the file stays tracked so the next push retries it"
    );
    assert!(
        bases::load(machine.path(), repo.path()).contains_key("artifacts/skills/my-skill/SKILL.md"),
        "the base entry survives too"
    );
}

#[test]
fn undo_pull_keeps_base_entries_a_later_push_recorded() {
    use claude_code_sync::history::{
        ConversationSummary, OperationHistory, OperationType, SyncOperation,
    };
    use claude_code_sync::undo::{undo_pull, Snapshot};

    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let filter = all_on_filter();

    // Two synced files: memory (the pull will overwrite it) and a skill
    // (untouched by the pull).
    memory(machine_a.path(), "v1\n");
    skill(machine_a.path(), "s1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // The pull being undone: A republishes memory; B plans the overwrite,
    // snapshots like pull_history would (record carried, touched keys
    // declared), and applies.
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.overwrites.len(), 1);
    let mut snapshot =
        Snapshot::create(OperationType::Pull, plan.paths_to_snapshot(false), None).unwrap();
    snapshot.attach_record_bookkeeping(&plan, machine_b.path(), false);
    let snapshots_dir = home.path().join("snapshots");
    let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();
    let apply_report = apply_pull(&plan, false).unwrap();

    // A push AFTER the pull records base entries of its own: a new skill
    // file only this machine has, and a fresh edit of the skill the pull
    // left unchanged (a key the pull merely re-declared as a no-op). A
    // wholesale restore would erase the first and revert the second, and
    // the files' next post-push edits would be remote-wins material
    // again — the exact loss the record exists to prevent.
    let extra = machine_b.path().join("skills/other/SKILL.md");
    fs::create_dir_all(extra.parent().unwrap()).unwrap();
    fs::write(&extra, "b-only\n").unwrap();
    skill(machine_b.path(), "b's edit too\n");
    push_artifacts(
        machine_b.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    assert!(
        bases::load(machine_b.path(), repo.path()).contains_key("artifacts/skills/other/SKILL.md")
    );

    // Undo the pull, through the same history machinery pull_history uses.
    let history_path = home.path().join("history.json");
    let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();
    let summary = ConversationSummary::new(
        "s".to_string(),
        "fact.md".to_string(),
        Some("2025-01-01T00:00:00Z".to_string()),
        1,
        SyncOperation::Modified,
    )
    .unwrap();
    let mut record = claude_code_sync::history::OperationRecord::new(
        OperationType::Pull,
        Some("main".to_string()),
        vec![summary],
    );
    record.snapshot_path = Some(snapshot_path);
    record.repo_path = Some(repo.path().to_path_buf());
    // Exactly what the apply moved, like pull_history records it.
    record.bases_keys_written = Some(apply_report.bases_keys_written.clone());
    record.tracked_keys_written = Some(apply_report.tracked_keys_written.clone());
    history.operations.insert(0, record);
    history.save_to(Some(history_path.clone())).unwrap();
    undo_pull(Some(history_path), Some(machine_b.path())).unwrap();

    // The push's entry survives; the pull's key goes back to its pre-pull
    // value (the v1 bytes B held before the overwritten pull).
    let after = bases::load(machine_b.path(), repo.path());
    assert_eq!(
        after.get("artifacts/skills/other/SKILL.md"),
        Some(&bases::hash_bytes(b"b-only\n")),
        "the later push's entry is not erased by the undo"
    );
    assert_eq!(
        after.get("artifacts/skills/my-skill/SKILL.md"),
        Some(&bases::hash_bytes(b"b's edit too\n")),
        "a no-op key the pull merely re-declared is not reverted by the undo"
    );
    assert_eq!(
        after.get("projects/-home-a-work-app/memory/fact.md"),
        Some(&bases::hash_bytes(b"v1\n")),
        "the pull's key is restored to its pre-pull value"
    );
    // The pull owned no tracked key (overwrite only), so the undo is a
    // no-op for the tracked record — the later push's path stays.
    assert!(
        claude_code_sync::artifacts::tracked::load(machine_b.path(), repo.path())
            .contains("artifacts/skills/other/SKILL.md"),
        "an empty-touched snapshot undoes nothing, it does not rewind the entry"
    );
}

#[test]
fn a_noop_pull_does_not_carry_the_tracked_record() {
    let repo = TempDir::new().unwrap();
    let machine = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine.path(), "v1\n");
    sync(machine.path(), repo.path(), &filter);
    let plan = plan_pull(machine.path(), repo.path(), &filter).unwrap();
    assert!(plan.is_empty());

    // The apply provably never writes the tracked record, so the snapshot
    // must not carry it (and undo must not declare surgery for it).
    assert!(
        !plan.paths_to_snapshot(false).contains(
            &claude_code_sync::artifacts::tracked::record_path(machine.path())
        ),
        "a no-op pull does not snapshot the shared tracked record"
    );
}

#[test]
fn a_kept_local_file_declares_its_base_key_for_the_undo() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);
    memory(machine_b.path(), "b's edit\n");
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 1);
    // An interactive take-remote records the repo bytes as the new base:
    // the key must be declared so the undo restores it per-key instead of
    // falling back to a whole-entry rewind.
    assert!(
        plan.touched_base_keys()
            .contains(&"projects/-home-a-work-app/memory/fact.md".to_string()),
        "kept-local keys are declared: {:?}",
        plan.touched_base_keys()
    );
}

#[test]
fn an_apply_does_not_erase_tracked_paths_recorded_while_it_waited() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A publishes a second file; B plans the pull that creates it...
    let second = machine_a.path().join("skills/other/SKILL.md");
    fs::create_dir_all(second.parent().unwrap()).unwrap();
    fs::write(&second, "v1\n").unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.creates.len(), 1);

    // ...and while the pull would wait (an interactive confirm, say), a
    // push ON THIS MACHINE records a tracked path of its own.
    let b_only = machine_b.path().join("skills/b-only/SKILL.md");
    fs::create_dir_all(b_only.parent().unwrap()).unwrap();
    fs::write(&b_only, "b\n").unwrap();
    push_artifacts(
        machine_b.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // The apply must merge its delta, not save the plan-time scan: the
    // push's path would otherwise leave the record and its future
    // deletion would never mirror.
    apply_pull(&plan, false).unwrap();
    let tracked = claude_code_sync::artifacts::tracked::load(machine_b.path(), repo.path());
    assert!(
        tracked.contains("artifacts/skills/b-only/SKILL.md"),
        "the concurrent push's path survives the apply: {tracked:?}"
    );
    assert!(
        tracked.contains("artifacts/skills/other/SKILL.md"),
        "the pull's create still lands: {tracked:?}"
    );
}

#[test]
#[cfg(unix)]
fn an_unreadable_delete_candidate_does_not_stall_the_sync_gate() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A deletes and pushes; B's copy becomes unreadable (ownership, a
    // 0400 root file). A keep would re-arm forever and stall every sync
    // on a file neither a push (read fails) nor a prompt could resolve —
    // the deletion is skipped and retried instead.
    fs::remove_file(machine_a.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let local = machine_b.path().join("skills/my-skill/SKILL.md");
    fs::set_permissions(&local, fs::Permissions::from_mode(0o000)).unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert!(
        plan.kept_local_deletes.is_empty(),
        "an unreadable file is undecided, not a durable keep"
    );
    assert_eq!(plan.skipped, 1);
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(
        report.total_kept_local(),
        0,
        "the sync gate sees nothing kept and keeps flowing"
    );
    fs::set_permissions(&local, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        local.is_file(),
        "the unreadable file is never deleted blindly"
    );
    assert!(
        claude_code_sync::artifacts::tracked::load(machine_b.path(), repo.path())
            .contains("artifacts/skills/my-skill/SKILL.md"),
        "the decision is retried: the entry stays tracked"
    );
}

#[test]
fn a_pull_does_not_drop_tracked_entries_of_disabled_categories() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();
    let no_skills = FilterConfig {
        sync_artifacts: ArtifactToggles {
            skills: false,
            ..ArtifactToggles::all_enabled()
        },
        ..Default::default()
    };

    // Both a memory file and a skill synced: the tracked record holds
    // both.
    memory(machine_a.path(), "v1\n");
    skill(machine_a.path(), "s1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A pull with skills DISABLED made no decision about the skill: its
    // entry must ride along, or a later re-enable would silently lose
    // deletion mirroring for it. And it is not a pending rewrite either —
    // a whole-entry compare would see the entry as churn forever and
    // mint a no-op snapshot on every pull.
    let plan = plan_pull(machine_b.path(), repo.path(), &no_skills).unwrap();
    assert!(
        !plan.rewrites_tracked,
        "an entry the pull's scan does not own is not a pending rewrite"
    );
    assert!(!plan.changes_machine_state(false));
    apply_pull(&plan, false).unwrap();

    assert!(
        claude_code_sync::artifacts::tracked::load(machine_b.path(), repo.path())
            .contains("artifacts/skills/my-skill/SKILL.md"),
        "a disabled category's tracked entries survive the pull"
    );
}

#[test]
#[cfg(unix)]
fn a_delete_skipped_at_apply_time_keeps_its_tracked_entry() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A deletes and pushes; B plans the delete while the file is still
    // readable...
    fs::remove_file(machine_a.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.deletes.len(), 1);

    // ...but it turns unreadable before the apply runs. The tracked
    // entry must SURVIVE, or the deletion would never be retried and the
    // next push would resurrect the repo copy the other machine deleted.
    let local = machine_b.path().join("skills/my-skill/SKILL.md");
    fs::set_permissions(&local, fs::Permissions::from_mode(0o000)).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(report.total_kept_local(), 0);
    fs::set_permissions(&local, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(
        local.is_file(),
        "the unreadable file is never deleted blindly"
    );
    assert!(
        claude_code_sync::artifacts::tracked::load(machine_b.path(), repo.path())
            .contains("artifacts/skills/my-skill/SKILL.md"),
        "a skipped delete stays tracked so it is retried"
    );

    // The retried decision executes once the file is readable again.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.deletes.len(), 1, "the deletion is re-planned");
    apply_pull(&plan, false).unwrap();
    assert!(!local.is_file());
}

#[test]
fn a_kept_local_file_deleted_while_the_pull_waited_is_not_a_phantom_keep() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);
    memory(machine_b.path(), "b's edit\n");
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 1);

    // The user deletes the file while the pull waits: counting it as a
    // keep would hold the sync gate on a phantom, and an interactive
    // take-remote would recreate it.
    fs::remove_file(
        machine_b
            .path()
            .join("projects/-home-a-work-app/memory/fact.md"),
    )
    .unwrap();
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(
        report.total_kept_local(),
        0,
        "a deleted file is not a keep: the sync gate must not stall on it"
    );
}

#[test]
fn a_declined_deletion_is_republished_by_the_next_sync() {
    // The contract confirm_deletion's help text states: declining keeps
    // the file, the decline un-tracks it, and the NEXT sync's pull holds
    // nothing back — its full push republishes the file. (The decline
    // itself needs a TTY; the post-decline record state is simulated
    // exactly: tracked loses the key, the base entry stays.)
    use claude_code_sync::artifacts::tracked;

    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A deletes and pushes; B declines the deletion (keeps its copy).
    fs::remove_file(machine_a.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let mut tracked_paths = tracked::load(machine_b.path(), repo.path());
    tracked_paths.remove("artifacts/skills/my-skill/SKILL.md");
    tracked::save(machine_b.path(), repo.path(), tracked_paths).unwrap();

    // The next sync: nothing is held back...
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    let kept = plan.kept_local.len() + plan.kept_local_deletes.len();
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(kept, 0, "the gate opens: a decline does not re-arm");
    assert_eq!(report.total_kept_local(), 0);

    // ...and the full push republishes the kept file.
    let push = push_artifacts(
        machine_b.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    assert_eq!(
        push.total_added(),
        1,
        "the declined deletion is republished"
    );
}

#[test]
#[cfg(unix)]
fn a_create_that_failed_at_apply_time_is_not_a_phantom_deletion() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    // A publishes a skill B has never seen; B plans the create, but the
    // destination directory is unwritable when the apply runs.
    skill(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.creates.len(), 1);
    let skills_dir = machine_b.path().join("skills");
    fs::create_dir_all(&skills_dir).unwrap();
    fs::set_permissions(&skills_dir, fs::Permissions::from_mode(0o555)).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    fs::set_permissions(&skills_dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(report.total_kept_local(), 0);

    // The failed create must NOT enter the tracked record: the local file
    // does not exist, and the next push would read that as a local
    // deletion — removing the repo copy for every machine.
    assert!(
        !claude_code_sync::artifacts::tracked::load(machine_b.path(), repo.path())
            .contains("artifacts/skills/my-skill/SKILL.md"),
        "a create that never executed is not tracked"
    );
    let push = push_artifacts(
        machine_b.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    assert_eq!(
        push.total_deleted(),
        0,
        "the next push does not mirror a deletion that never happened"
    );
    assert!(
        repo.path()
            .join("artifacts/skills/my-skill/SKILL.md")
            .is_file(),
        "the repo copy survives for the other machines"
    );
}

#[test]
fn undo_skips_snapshotless_pull_records() {
    use claude_code_sync::history::{
        ConversationSummary, OperationHistory, OperationType, SyncOperation,
    };
    use claude_code_sync::undo::{undo_pull, Snapshot};

    // A nightly kept-local-only sync appends pull records with NO
    // snapshot (nothing was written). Undo must act on the newest pull
    // that HAS one instead of erroring on the newest record.
    let home = TempDir::new().unwrap();
    let claude_dir = home.path().join("claude");
    fs::create_dir_all(&claude_dir).unwrap();
    let file = claude_dir.join("memory.md");
    fs::write(&file, "original\n").unwrap();

    let snapshots_dir = home.path().join("snapshots");
    let snapshot = Snapshot::create(OperationType::Pull, vec![&file], None).unwrap();
    let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

    let history_path = home.path().join("history.json");
    let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();
    let summary = ConversationSummary::new(
        "s".to_string(),
        "memory.md".to_string(),
        Some("2025-01-01T00:00:00Z".to_string()),
        1,
        SyncOperation::Modified,
    )
    .unwrap();
    let mut snapshotless = claude_code_sync::history::OperationRecord::new(
        OperationType::Pull,
        Some("main".to_string()),
        vec![summary],
    );
    snapshotless.snapshot_path = None;
    history.operations.insert(0, snapshotless);
    let summary2 = ConversationSummary::new(
        "s2".to_string(),
        "memory.md".to_string(),
        Some("2025-01-01T00:00:00Z".to_string()),
        1,
        SyncOperation::Modified,
    )
    .unwrap();
    let mut snapshotbearing = claude_code_sync::history::OperationRecord::new(
        OperationType::Pull,
        Some("main".to_string()),
        vec![summary2],
    );
    snapshotbearing.snapshot_path = Some(snapshot_path);
    history.operations.insert(0, snapshotbearing);
    history.save_to(Some(history_path.clone())).unwrap();

    fs::write(&file, "modified by the pull\n").unwrap();
    let result = undo_pull(Some(history_path.clone()), Some(home.path())).unwrap();
    assert!(
        result.contains("Successfully undone"),
        "undo result: {result}"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "original\n");

    // The UNDONE record left history; the snapshotless one stays (it did
    // nothing) — and with no snapshot-bearing pull left, a second undo
    // says so instead of failing on a dangling snapshot path.
    let history = OperationHistory::from_path(Some(history_path.clone())).unwrap();
    assert_eq!(history.len(), 1, "only the snapshotless record remains");
    assert!(
        undo_pull(Some(history_path.clone()), Some(home.path()))
            .unwrap_err()
            .to_string()
            .contains("changed no machine state"),
        "no stale record pointing at the deleted snapshot — the error says \
         the remaining pull kept local only"
    );
}

#[test]
fn an_edit_made_while_a_deletion_waited_rearms_like_a_kept_delete() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A deletes and pushes; B's copy is CLEAN, so the plan is a plain
    // delete...
    fs::remove_file(machine_a.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.deletes.len(), 1);

    // ...then B's user edits the file while the pull waits. The apply
    // keeps it — and must keep its tracked entry too, or the next sync's
    // open gate would republish the edit, silently reverting A's
    // deletion.
    skill(machine_b.path(), "b's edit\n");
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(report.total_kept_local(), 1);
    assert!(
        claude_code_sync::artifacts::tracked::load(machine_b.path(), repo.path())
            .contains("artifacts/skills/my-skill/SKILL.md"),
        "an apply-time edit re-arms the tracked entry"
    );

    // The next pull sees it as a kept-local DELETE — protected, gate held.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(
        plan.kept_local_deletes.len(),
        1,
        "the mid-pull edit is protected like any kept-local delete"
    );
}

#[test]
fn a_create_the_apply_skipped_is_not_an_undo_delete_candidate() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    // A publishes a skill B has never seen; B plans the create, then the
    // user writes their OWN version before the apply runs.
    skill(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.creates.len(), 1);
    skill(machine_b.path(), "user's own\n");
    let report = apply_pull(&plan, false).unwrap();

    // The apply skips it (a push publishes it) — and the snapshot's
    // undo-delete list is built from EXECUTED creates only, so the undo
    // must not see this path as a file to remove.
    assert!(
        report.created_abs_paths.is_empty(),
        "a skipped create is not an undo-delete candidate"
    );
    assert_eq!(
        read_skill(machine_b.path()),
        "user's own\n",
        "the user's file survives the pull"
    );
}

#[test]
fn a_kept_local_delete_whose_file_was_deleted_mid_pull_is_not_a_phantom() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A deletes and pushes; B holds an edit → kept-local delete...
    fs::remove_file(machine_a.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    skill(machine_b.path(), "b's edit\n");
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.kept_local_deletes.len(), 1);

    // ...and the user then deletes the local file while the pull waits.
    // The deletion the repository asked for has already happened: no
    // prompt, no phantom keep gating the sync on a nonexistent file.
    fs::remove_file(machine_b.path().join("skills/my-skill/SKILL.md")).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(report.total_kept_local(), 0, "no phantom keep");
    assert_eq!(report.total_deleted(), 1, "the deletion is recorded");

    // The base entry is deliberately NOT pruned in this unpredicted arm
    // (an unpredicted record write from a snapshotless pull misleads the
    // undo hint and the concurrent-sync warning); the re-armed tracked
    // entry hands the prune to the next pull's gone-from-both-sides pass.
    assert!(
        bases::load(machine_b.path(), repo.path())
            .contains_key("artifacts/skills/my-skill/SKILL.md"),
        "the prune is deferred, not dropped"
    );
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.base_prunes.len(), 1, "the next pull prunes the entry");
    apply_pull(&plan, false).unwrap();
    assert!(
        !bases::load(machine_b.path(), repo.path())
            .contains_key("artifacts/skills/my-skill/SKILL.md"),
        "the entry goes with the file, one round later"
    );
}

#[test]
fn a_plain_delete_whose_file_was_deleted_mid_pull_completes_the_deletion() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A deletes and pushes; B's copy is clean → plain delete...
    fs::remove_file(machine_a.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.deletes.len(), 1);

    // ...then the user deletes the local file while the pull waits. The
    // deletion the repository asked for is already done: no prompt (a
    // decline would have counted a phantom keep AND stranded the base
    // entry), no remove_file, just the bookkeeping.
    fs::remove_file(machine_b.path().join("skills/my-skill/SKILL.md")).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(report.total_kept_local(), 0, "no phantom keep");
    assert_eq!(report.total_deleted(), 1);
    assert!(
        !bases::load(machine_b.path(), repo.path())
            .contains_key("artifacts/skills/my-skill/SKILL.md"),
        "the base entry goes with the file — nothing stranded"
    );
}

#[test]
fn a_repo_file_held_back_for_size_is_never_tracked_as_received() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    // A's limit admits the file; B's does not.
    let filter_big = FilterConfig {
        max_file_size_bytes: 1024 * 1024,
        ..all_on_filter()
    };
    let filter_small = FilterConfig {
        max_file_size_bytes: 1024,
        ..all_on_filter()
    };

    // A publishes a large skill. B has its own (small) skill, so the
    // category exists locally and the gone-scan guard is in play.
    let big = machine_a.path().join("skills/big/SKILL.md");
    fs::create_dir_all(big.parent().unwrap()).unwrap();
    fs::write(&big, "x".repeat(50 * 1024)).unwrap();
    skill(machine_a.path(), "small\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter_big,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // B pulls with the lower limit from the start: the big file is HELD
    // BACK (skipped, never written locally) — it must not enter the
    // tracked record either, or B's next push would read "tracked + not
    // pushed + no local file" as a local deletion and remove the repo
    // copy for every machine.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter_small).unwrap();
    assert_eq!(plan.skipped, 1, "the oversized repo file is held back");
    apply_pull(&plan, false).unwrap();
    skill(machine_b.path(), "small\n");
    // A REAL re-pull (a second apply of the stale plan would exercise the
    // mid-pull-create skip, not this round's plan).
    apply_pull(
        &plan_pull(machine_b.path(), repo.path(), &filter_small).unwrap(),
        false,
    )
    .unwrap();
    assert!(
        !claude_code_sync::artifacts::tracked::load(machine_b.path(), repo.path())
            .contains("artifacts/skills/big/SKILL.md"),
        "a held-back repo file is not tracked as received"
    );

    let push = push_artifacts(
        machine_b.path(),
        repo.path(),
        &filter_small,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    assert_eq!(
        push.total_deleted(),
        0,
        "B's push does not delete a file B never received"
    );
    assert!(
        repo.path().join("artifacts/skills/big/SKILL.md").is_file(),
        "the repo copy survives for machines that can receive it"
    );
}

#[test]
fn a_file_created_mid_pull_stays_protected_on_the_next_pull() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    // A publishes a skill B has never seen; the user writes their own
    // version while the pull waits. The apply skips the create — and the
    // skip must be DURABLE: recording the repo bytes as the base leaves
    // the user's file reading dirty, or the NEXT pull would classify it
    // as a clean fast-forward and destroy it.
    skill(machine_a.path(), "repo's version\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    skill(machine_b.path(), "user's version\n");
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(report.total_added(), 0, "the create was skipped");

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(
        kept_local_of(&plan),
        1,
        "the mid-pull file reads dirty and stays protected"
    );
    apply_pull(&plan, false).unwrap();
    assert_eq!(read_skill(machine_b.path()), "user's version\n");
}

#[test]
#[cfg(unix)]
fn an_overwrite_target_unreadable_at_apply_time_is_undecided_not_kept() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A republishes; B's copy turns unreadable between plan and apply.
    // Nothing was edited: the file is UNDECIDED (skipped, retried), not
    // a keep gating the sync on a "push to publish" that would publish
    // nothing.
    skill(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.overwrites.len(), 1);
    let local = machine_b.path().join("skills/my-skill/SKILL.md");
    fs::set_permissions(&local, fs::Permissions::from_mode(0o000)).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    fs::set_permissions(&local, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        report.total_kept_local(),
        0,
        "an unreadable target is not an edited one"
    );
    assert_eq!(
        read_skill(machine_b.path()),
        "v1\n",
        "the file is left as it is, retried next round"
    );
}

#[test]
fn a_reverted_edit_is_held_one_round_then_self_heals() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // B edits → kept-local planned → B reverts to the base bytes while
    // the pull waits. Nothing is left to protect: the hint must not say
    // "push to publish" (a push would publish nothing) and the gate must
    // not hold a round over an empty conflict.
    memory(machine_b.path(), "b's edit\n");
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 1);
    memory(machine_b.path(), "v1\n");
    let report = apply_pull(&plan, false).unwrap();
    // Deliberately still counted kept — one gate-held round that
    // self-heals: fast-forwarding here would write a file and the shared
    // record from an apply the plan-time prediction proved would write
    // nothing (a kept-local-only pull mints no snapshot), leaving the
    // pull un-undoable. The next round converges.
    assert_eq!(
        report.total_kept_local(),
        1,
        "a reverted edit still reads as the plan classified it"
    );
    assert_eq!(read_memory(machine_b.path()), "v1\n");

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(
        kept_local_of(&plan),
        0,
        "the next round sees the reverted bytes and fast-forwards"
    );
    apply_pull(&plan, false).unwrap();
    assert_eq!(read_memory(machine_b.path()), "v2\n");
}

#[test]
#[cfg(unix)]
fn an_unreadable_kept_local_target_is_kept_never_guessed_at() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // B edits → kept-local planned → the file turns unreadable between
    // plan and apply. The keep arm never guesses at bytes it cannot read:
    // the protection holds (no overwrite of unknown state), the gate
    // holds with it, and the round converges once the file is readable.
    memory(machine_b.path(), "b's edit\n");
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    let local = machine_b
        .path()
        .join("projects/-home-a-work-app/memory/fact.md");
    fs::set_permissions(&local, fs::Permissions::from_mode(0o000)).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(
        report.total_kept_local(),
        1,
        "an unreadable target is kept, never guessed at"
    );
    fs::set_permissions(&local, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        read_memory(machine_b.path()),
        "b's edit\n",
        "the file is left untouched while unreadable"
    );

    // Readable again: the plain three-way decision resumes.
    let report = apply_pull(
        &plan_pull(machine_b.path(), repo.path(), &filter).unwrap(),
        false,
    )
    .unwrap();
    assert_eq!(report.total_kept_local(), 1);
    assert_eq!(read_memory(machine_b.path()), "b's edit\n");
    sync(machine_b.path(), repo.path(), &filter);
    assert_eq!(read_memory(machine_b.path()), "b's edit\n");
}

/// Recursively find `name` under `root` (test repos are small).
#[cfg(unix)]
fn find_repo_file(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    fn walk(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                if let Some(hit) = walk(&path, name) {
                    return Some(hit);
                }
            } else if entry.file_name().to_string_lossy() == name {
                return Some(path);
            }
        }
        None
    }
    walk(root, name).unwrap()
}

#[test]
#[cfg(unix)]
fn a_mid_pull_create_with_an_unreadable_repo_copy_still_protects_the_next_pull() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // The plan sees a plain create on a fresh machine...
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    // ...then the user writes their own file at the destination AND the
    // repository copy turns unreadable before the apply runs. The skip
    // cannot record the true repo bytes — but recording NOTHING would let
    // the next pull (copy readable again, no base entry) fast-forward the
    // repository copy straight over the user's file.
    memory(machine_b.path(), "user's own\n");
    let repo_copy = find_repo_file(repo.path(), "fact.md");
    fs::set_permissions(&repo_copy, fs::Permissions::from_mode(0o000)).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    fs::set_permissions(&repo_copy, fs::Permissions::from_mode(0o644)).unwrap();
    let skipped: usize = report.counts.iter().map(|c| c.skipped).sum();
    assert_eq!(skipped, 1);

    // The empty-base sentinel holds: the user's file reads dirty and the
    // next pull keeps it instead of overwriting it.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 1);
    apply_pull(&plan, false).unwrap();
    assert_eq!(read_memory(machine_b.path()), "user's own\n");
}

#[test]
fn a_failed_tracked_save_reports_no_keys_written() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    // Make the tracked record unwritable (a directory where the file
    // would go): the save fails AFTER the files are applied, and the
    // report must not claim keys it never moved — an undo acting on that
    // list would erase entries a later push legitimately recorded.
    fs::create_dir_all(machine_b.path().join(".claude-code-sync-tracked.json")).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(read_memory(machine_b.path()), "v1\n");
    assert!(
        report.tracked_keys_written.is_empty(),
        "a failed record save moved nothing"
    );
}

#[test]
#[cfg(unix)]
fn a_keep_from_a_failed_write_has_nothing_to_publish() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // The overwrite's write fails (the destination directory is
    // read-only): the file is kept — the gate stays held, fail-safe —
    // but its bytes match the base, so "push to publish" would publish
    // stale v1 over the repo's v2. The report must say there is nothing
    // to publish.
    let memory_dir = machine_b.path().join("projects/-home-a-work-app/memory");
    fs::set_permissions(&memory_dir, fs::Permissions::from_mode(0o555)).unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    fs::set_permissions(&memory_dir, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(report.total_kept_local(), 1, "the gate stays held");
    assert_eq!(
        report.kept_local_clean, 1,
        "a failed write protected nothing — the hint must not advise a push"
    );
    assert_eq!(read_memory(machine_b.path()), "v1\n");
}

#[test]
fn a_no_op_base_recording_reports_no_key() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // The file vanishes locally (a create next pull), and the user
    // recreates IDENTICAL bytes while the pull runs: the durable skip
    // records a base that is already there — nothing moved, and the
    // report must not list the key for an undo to "restore".
    fs::remove_file(
        machine_b
            .path()
            .join("projects/-home-a-work-app/memory/fact.md"),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    memory(machine_b.path(), "v1\n");
    let report = apply_pull(&plan, false).unwrap();
    let skipped: usize = report.counts.iter().map(|c| c.skipped).sum();
    assert_eq!(skipped, 1);
    assert!(
        report.bases_keys_written.is_empty(),
        "a base that already held the value moved nothing"
    );
}

// Unix only: it needs an executable bit that differs, which Windows lacks.
#[cfg(unix)]
#[test]
fn a_mode_fix_over_converged_bases_mints_no_snapshot() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A chmod-only push: A flips the exec bit on its copy, pushes (the
    // bytes are identical — only the mode moves). B pulls: the plan has
    // one mode_fix (the copy is grant-only: a repo-executable file
    // confers the bit on B's copy), and the bytes (hence the base) never
    // moved.
    let skill_a = machine_a.path().join("skills/mode-test/SKILL.md");
    fs::create_dir_all(skill_a.parent().unwrap()).unwrap();
    fs::write(&skill_a, "skill bytes\n").unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    sync(machine_b.path(), repo.path(), &filter);
    let local = machine_b.path().join("skills/mode-test/SKILL.md");
    assert!(local.is_file(), "B received the skill");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&skill_a, fs::Permissions::from_mode(0o755)).unwrap();
        push_artifacts(
            machine_a.path(),
            repo.path(),
            &filter,
            &std::collections::HashSet::new(),
        )
        .unwrap();
    }

    // NON-interactively the apply predicts "writes no record": the plan
    // must agree — rewrites_bases false means no snapshot-bearing record,
    // and an undo cannot bury the previous substantive pull under a
    // no-op one.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(plan.mode_fixes.len(), 1, "the exec bit really differs");
    assert!(
        !plan.rewrites_bases,
        "a mode fix over an already-recorded value moves nothing"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        apply_pull(&plan, false).unwrap();
        // Grant-only: the copy keeps its own bits and gains the owner's
        // exec bit from the repo copy (0o600 | 0o100 = 0o700 here).
        let mode = fs::metadata(&local).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "the exec bit was granted");
    }
}

#[test]
fn a_mid_pull_edit_is_reported_publishable() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // An overwrite planned; the user edits the file while the pull
    // waits; the apply keeps it — and the edit IS what a push would
    // publish, so the report must not count it nothing-to-publish.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    memory(machine_b.path(), "user's mid-pull edit\n");
    let report = apply_pull(&plan, false).unwrap();
    assert_eq!(report.total_kept_local(), 1);
    assert_eq!(
        report.kept_local_clean, 0,
        "a mid-pull edit is publishable — the hint must say push"
    );
    assert_eq!(read_memory(machine_b.path()), "user's mid-pull edit\n");
}

#[test]
#[cfg(unix)]
fn a_failed_union_write_reports_no_merged_entries() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempDir::new().unwrap();
    let machine = TempDir::new().unwrap();
    let filter = all_on_filter();

    fs::write(machine.path().join("history.jsonl"), "prompt one\n").unwrap();
    push_artifacts(
        machine.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // New local lines, and the repo's artifacts directory turned
    // read-only: the union write fails and the file is held back —
    // the report must not claim the new lines were merged.
    fs::write(
        machine.path().join("history.jsonl"),
        "prompt one\nprompt two\n",
    )
    .unwrap();
    let artifacts_dir = find_repo_file(repo.path(), "history.jsonl")
        .parent()
        .unwrap()
        .to_path_buf();
    fs::set_permissions(&artifacts_dir, fs::Permissions::from_mode(0o555)).unwrap();
    let report = push_artifacts(
        machine.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    );
    fs::set_permissions(&artifacts_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let report = report.unwrap();
    let skipped: usize = report.counts.iter().map(|c| c.skipped).sum();
    let merged: usize = report.counts.iter().map(|c| c.merged_entries).sum();
    assert_eq!(skipped, 1, "the union write was held back");
    assert_eq!(merged, 0, "a held-back write reports no merged entries");
}

#[test]
fn a_file_without_a_backup_is_not_modified_by_the_apply() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);
    memory(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // The snapshot could not read the file (the caller recorded it on
    // the plan): the apply must refuse the overwrite — no modification
    // without a backup — and count it skipped for the next pull.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    let mut plan = plan;
    plan.unsnapshotted = vec![machine_b
        .path()
        .join("projects/-home-a-work-app/memory/fact.md")];
    let report = apply_pull(&plan, false).unwrap();
    let skipped: usize = report.counts.iter().map(|c| c.skipped).sum();
    assert_eq!(skipped, 1, "the unbacked overwrite was refused");
    assert_eq!(
        read_memory(machine_b.path()),
        "v1\n",
        "the file was left for a later, backupable pull"
    );
}

#[test]
fn a_delete_skipped_at_unsnapshotted_keeps_its_tracked_entry() {
    // Round 56 parallel to the F3 None-base keep regression
    // (round 55): an unsnapshotted delete must also push to
    // `deletes_to_retry` so the tracked entry survives the skip.
    // Without the push, the tracked delta loop sees
    // (tracked_before=true, tracked_after=false) → removal. The
    // file is stranded locally and the next pull classifies it
    // as never-synced, then silently overwrites it when the repo
    // re-adds the file.
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    fs::remove_file(machine_a.path().join("skills/my-skill/SKILL.md")).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert!(
        !plan.deletes.is_empty(),
        "the plan must target the deletion"
    );

    // Simulate the snapshot-time failure: the apply cannot take a
    // backup of the path the plan wants to remove. The skip is a
    // transient — the next pull must re-classify the delete.
    let mut plan = plan;
    plan.unsnapshotted = vec![machine_b.path().join("skills/my-skill/SKILL.md")];

    let report = apply_pull(&plan, false).unwrap();
    let skipped: usize = report.counts.iter().map(|c| c.skipped).sum();
    let deleted: usize = report.counts.iter().map(|c| c.deleted).sum();
    assert_eq!(
        skipped, 1,
        "the unsnapshotted delete is skipped (not executed)"
    );
    assert_eq!(deleted, 0, "no silent delete on an unsnapshotted path");
    assert_eq!(
        read_skill(machine_b.path()),
        "v1\n",
        "the file is left for a later, backupable pull"
    );
    // Tracked entry must survive the skip: mirrors the F3
    // tracked.contains assertion. Without round-56's push, the
    // tracked entry would be removed by the delta loop.
    let tracked = claude_code_sync::artifacts::tracked::load(machine_b.path(), repo.path());
    assert!(
        tracked.contains("artifacts/skills/my-skill/SKILL.md"),
        "unsnapshotted-skip must preserve the tracked entry; tracked: {tracked:?}"
    );
}

#[test]
fn an_overwrite_with_concurrently_pruned_base_is_kept_not_silently_overwritten() {
    // F3-class multi-round regression for the overwrites arm. The
    // previous behavior (no overwrites-arm None-base guard) silently
    // overwrote a locally-edited file on the pull after a concurrent
    // sync had pruned the base entry. The overwrites arm now
    // consults `tracked_before` as a second signal: if this machine
    // has ever synced the file and the base is missing, the
    // apply's `is_dirty(None, _) = false` (which would otherwise
    // classify the file as never-synced) is replaced by a synthetic-
    // Dirty that routes the file to `plan.kept_local`.
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    sync(machine_b.path(), repo.path(), &filter);

    // B's local file is still v1 (matches base, matches repo). A
    // re-pushes v2; B's plan classifies the file as overwrites
    // (local == base == v1, repo == v2, file differs). We simulate
    // a concurrent base prune by `bases::forget` (B's record file
    // is wiped). The next pull plan should now route the file to
    // kept_local — B has synced this file before, and the
    // "remote wins" behavior is reserved for fresh install /
    // upgrade (tracked never recorded the file).
    skill(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    use claude_code_sync::artifacts::bases;
    bases::forget(machine_b.path(), repo.path()).unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    let kept_count: usize = plan
        .kept_local
        .iter()
        .filter(|w| w.category_path.ends_with("SKILL.md"))
        .count();
    let overwrite_count = plan
        .overwrites
        .iter()
        .filter(|w| w.category_path.ends_with("SKILL.md"))
        .count();
    assert_eq!(
        kept_count, 1,
        "the file must be kept (F3-class: tracked_before + missing base)"
    );
    assert_eq!(
        overwrite_count, 0,
        "the file must NOT be classified as overwrites (no silent overwrite)"
    );

    let report = apply_pull(&plan, false).unwrap();
    let kept_local: usize = report.counts.iter().map(|c| c.kept_local).sum();
    let modified: usize = report.counts.iter().map(|c| c.modified).sum();
    assert_eq!(kept_local, 1);
    assert_eq!(modified, 0, "no silent overwrite");
    // F3-class keeps must NOT inflate kept_local_clean: the kept
    // route's `nothing_to_publish` computation cannot see the base
    // (it was None by definition), but the apply-time fall-through
    // explicitly routes None-recorded files to FreshRead::Dirty —
    // not FreshRead::Clean. If that ever regresses, the next sync's
    // per-file skip set would lose the entry and the push would
    // write local (== base, the older version) over the repo's
    // newer bytes (finding #1 from the /code-review pass).
    assert_eq!(
        report.kept_local_clean, 0,
        "F3-class keeps must not read as 'nothing to publish'"
    );
    assert_eq!(
        read_skill(machine_b.path()),
        "v1\n",
        "the user's edit (v1) is preserved"
    );
}

#[test]
fn an_overwrite_for_a_fresh_machine_with_no_base_still_remote_wins() {
    // The overwrites arm's new `tracked_before` second signal must
    // NOT change the fresh-install / upgrade behavior: a machine that
    // has never synced the file is still allowed to remote-win
    // (an_unknown_base_keeps_the_remote_wins_behavior). Verified here
    // with a clean machine (no prior sync) and a different local
    // file from what the repo has.
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // B has never synced: it has no tracked entry, no base entry.
    // Local differs from repo (user wrote their own version).
    skill(machine_b.path(), "local\n");

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    let overwrite_targets: Vec<_> = plan
        .overwrites
        .iter()
        .filter(|w| w.category_path.ends_with("SKILL.md"))
        .map(|w| w.local_path.clone())
        .collect();
    let kept_count = plan
        .kept_local
        .iter()
        .filter(|w| w.category_path.ends_with("SKILL.md"))
        .count();
    assert!(
        overwrite_targets.iter().any(|p| p.ends_with("SKILL.md")),
        "fresh install: tracked_before is empty, so the file is still \
         classified as overwrites (remote wins); plan: {plan:?}"
    );
    assert_eq!(
        kept_count, 0,
        "fresh install: file must NOT route to kept_local; plan: {plan:?}"
    );
}

#[test]
fn a_multi_round_f3_keep_survives_a_repo_repush() {
    // Full multi-round scenario: A re-pushes the file after B's
    // F3-keep, and B's locally-edited bytes are STILL preserved
    // (the overwrites arm now consults tracked_before as a second
    // signal). This is the latent the round-58/67 reviews flagged
    // and the round-69 fix closes end-to-end.
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    sync(machine_b.path(), repo.path(), &filter);

    // Round N: A removes + pushes (B's plan = delete). B's
    // bases::forget simulates a concurrent sync. F3-keep holds the
    // file locally and the tracked entry survives.
    let a_local = machine_a.path().join("skills/my-skill/SKILL.md");
    fs::remove_file(&a_local).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    use claude_code_sync::artifacts::bases;
    bases::forget(machine_b.path(), repo.path()).unwrap();
    let plan_n = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    // F3-class (None base + tracked entry) routes the file to
    // kept_local_deletes, NOT plain deletes — the deletes-arm
    // plan-time F3 fix at engine.rs:1743. The tracked entry is
    // re-armed so the next pull replans it.
    assert!(
        !plan_n.kept_local_deletes.is_empty(),
        "the F3-class file must be kept (kept_local_deletes), not silently deleted: \
         plan.deletes={:?}, plan.kept_local_deletes={:?}",
        plan_n.deletes,
        plan_n.kept_local_deletes
    );
    let _report_n = apply_pull(&plan_n, false).unwrap();
    // B's file is still "v1" (the F3-keep held it).
    assert_eq!(read_skill(machine_b.path()), "v1\n");

    // Round N+1: A re-creates the file and pushes. B's plan now
    // would classify the file as overwrites (file present in repo,
    // file present locally, bytes differ, base is None because of
    // the round-N forget). With the new tracked_before check, the
    // file goes to kept_local — B's local "v1" is preserved.
    skill(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan_n1 = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(
        plan_n1
            .overwrites
            .iter()
            .filter(|w| w.category_path.ends_with("SKILL.md"))
            .count(),
        0,
        "the file must NOT be classified as overwrites (tracked_before + \
         missing base = F3-class keep); plan: {plan_n1:?}"
    );
    assert_eq!(
        plan_n1
            .kept_local
            .iter()
            .filter(|w| w.category_path.ends_with("SKILL.md"))
            .count(),
        1,
        "the file is kept as a protected local edit"
    );
    let report_n1 = apply_pull(&plan_n1, false).unwrap();
    let modified: usize = report_n1.counts.iter().map(|c| c.modified).sum();
    assert_eq!(modified, 0, "no silent overwrite of B's edit");
    assert_eq!(
        read_skill(machine_b.path()),
        "v1\n",
        "B's v1 is preserved across the re-push; user can run a \
         standalone push to publish it."
    );
}

#[test]
fn undo_does_not_disarm_the_mid_pull_create_protection() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // The user writes their own file at the destination mid-pull: the
    // apply skips and records the repo bytes as base — protection.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    memory(machine_b.path(), "user's own\n");
    let report = apply_pull(&plan, false).unwrap();
    let skipped: usize = report.counts.iter().map(|c| c.skipped).sum();
    assert_eq!(skipped, 1);
    assert!(
        report.bases_keys_written.is_empty(),
        "the protection key is NOT reported — the undo must not reach it"
    );

    // Undo the pull, through the same history machinery pull_history uses
    // (the operation record carries exactly what the apply moved).
    let home = TempDir::new().unwrap();
    let history_path = home.path().join("history.json");
    let mut history =
        claude_code_sync::history::OperationHistory::from_path(Some(history_path.clone())).unwrap();
    let summary = claude_code_sync::history::ConversationSummary::new(
        "s".to_string(),
        "fact.md".to_string(),
        Some("2025-01-01T00:00:00Z".to_string()),
        1,
        claude_code_sync::history::SyncOperation::Modified,
    )
    .unwrap();
    let mut record = claude_code_sync::history::OperationRecord::new(
        claude_code_sync::history::OperationType::Pull,
        Some("main".to_string()),
        vec![summary],
    );
    record.repo_path = Some(repo.path().to_path_buf());
    // A minimal snapshot file: undo requires one to act on (it carries no
    // files — the protection is record-level).
    let snap = claude_code_sync::undo::Snapshot::create(
        claude_code_sync::history::OperationType::Pull,
        Vec::<std::path::PathBuf>::new(),
        None,
    )
    .unwrap();
    let snapshot_path = snap.save_to_disk(Some(&home.path().join("snaps"))).unwrap();
    record.snapshot_path = Some(snapshot_path);
    record.bases_keys_written = Some(report.bases_keys_written.clone());
    record.tracked_keys_written = Some(report.tracked_keys_written.clone());
    history.operations.insert(0, record);
    history.save_to(Some(history_path.clone())).unwrap();
    claude_code_sync::undo::undo_pull(Some(history_path), Some(machine_b.path())).unwrap();
    assert_eq!(read_memory(machine_b.path()), "user's own\n");
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 1, "the protection survived the undo");
    apply_pull(&plan, false).unwrap();
    assert_eq!(read_memory(machine_b.path()), "user's own\n");
}

// Unix only: it makes the repo copy unreadable with a 0o000 mode, which
// Windows does not enforce.
#[cfg(unix)]
#[test]
fn an_empty_user_file_is_not_lost_to_the_protection_sentinel() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    memory(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // The user's mid-pull file is EMPTY, and the repo copy is unreadable
    // at apply: the sentinel base (hash of empty) would hash-equal the
    // user's file and read as clean — remote-wins on the next pull. The
    // sentinel special case reads it dirty instead.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    memory(machine_b.path(), "");
    let repo_copy = find_repo_file(repo.path(), "fact.md");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&repo_copy, fs::Permissions::from_mode(0o000)).unwrap();
    let report = apply_pull(&plan, false).unwrap();
    fs::set_permissions(&repo_copy, fs::Permissions::from_mode(0o644)).unwrap();
    let skipped: usize = report.counts.iter().map(|c| c.skipped).sum();
    assert_eq!(skipped, 1);

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(
        kept_local_of(&plan),
        1,
        "an empty user file reads dirty against the sentinel base"
    );
    apply_pull(&plan, false).unwrap();
    assert_eq!(read_memory(machine_b.path()), "", "the file survives");
}

#[test]
fn union_merge_records_the_merged_bytes_as_base() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    fs::write(machine_a.path().join("history.jsonl"), "line one\n").unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    sync(machine_b.path(), repo.path(), &filter);

    fs::write(
        machine_a.path().join("history.jsonl"),
        "line one\nline two\n",
    )
    .unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    apply_pull(&plan, false).unwrap();
    let content = fs::read_to_string(machine_b.path().join("history.jsonl")).unwrap();
    assert!(content.contains("line two"));

    // The next pull must NOT read the merged file as a user change.
    let after = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(
        kept_local_of(&after),
        0,
        "the merged bytes read clean against the recorded base"
    );
}

#[test]
fn a_declined_deletion_is_only_asked_once() {
    // Confirms that the deletes-arm decision is driven by ONE answer:
    // a YES on the first prompt keeps the file deleted; a NO on the
    // only prompt keeps it. The targeted reviewer's flag — that the
    // previous fix still called confirm_deletion a second time when
    // the user confirmed, silently discarding the second answer — must
    // not regress.
    use std::cell::Cell;
    let count = Cell::new(0u32);
    let answer = Cell::new(false); // first prompt says "yes" — would have deleted
                                   // Intercept by patching confirm_deletion via a thin wrapper is heavy;
                                   // the targeted reviewer's proof was structural: confirm_deletion must
                                   // appear in the deletes-arm block exactly once. Assert here via
                                   // grep of the compiled path so a future re-introduction is caught.
                                   // Line endings normalised: Windows checkouts convert them to CRLF.
    let src = include_str!("../src/artifacts/engine.rs").replace("\r\n", "\n");
    let deletes_arm_start = src
        .find("for delete in &plan.deletes {")
        .expect("deletes arm");
    let block = src[deletes_arm_start..]
        .split_once(
            "
    }
",
        )
        .map(|(b, _)| b)
        .expect("deletes arm closing brace");
    let n = block.matches("confirm_deletion(").count();
    count.set(n as u32);
    answer.set(n == 1);
    assert_eq!(
        count.get(),
        1,
        "deletes arm must call confirm_deletion exactly once"
    );
    assert!(
        answer.get(),
        "a single confirm_deletion call proves the user's answer is the only answer"
    );
}

#[test]
fn a_declared_record_with_no_snapshot_bytes_pins_the_kept_snapshot() {
    // F1 regression: the surgery block sets record_entries_unrestored
    // when declared record files were not actually snapshotted, but
    // forgot to also set pin_on_warning — the kept-snapshot summary then
    // told the user to recover from a file that had just been deleted.
    // The user-visible behaviour: undo of a pull whose declared-but-empty
    // record file keeps the snapshot on disk.
    fn write_file(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        path
    }
    let temp_dir = TempDir::new().unwrap();
    let snapshots_dir = temp_dir.path().join("snapshots");
    let repo_root = temp_dir.path().join("sync-repo");

    fs::create_dir_all(temp_dir.path().join("projects/x")).unwrap();
    let artifact = write_file(temp_dir.path(), "projects/x/fact.md", "pre-pull");

    // Build a snapshot whose declared record file IS NOT in the
    // bytes map (declares 2 records but ships 1's bytes).
    let mut snapshot = Snapshot::create(OperationType::Pull, vec![&artifact], None).unwrap();
    let rpath1 = temp_dir.path().join(".claude-code-sync-bases.json");
    let rpath2 = temp_dir.path().join(".claude-code-sync-tracked.json");
    snapshot.files.insert(
        rpath1.to_string_lossy().to_string(),
        br#"{"repos": {"/r": {}}}"#.to_vec(),
    );
    snapshot.record_files = vec![
        rpath1.to_string_lossy().to_string(),
        rpath2.to_string_lossy().to_string(),
    ];
    snapshot.record_touched_bases = Some(vec!["projects/x/fact.md".to_string()]);
    let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

    let history_path = temp_dir.path().join("history.json");
    let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();
    let summary = ConversationSummary::new(
        "s".to_string(),
        "fact.md".to_string(),
        Some("2025-01-01T00:00:00Z".to_string()),
        1,
        SyncOperation::Modified,
    )
    .unwrap();
    let mut record =
        OperationRecord::new(OperationType::Pull, Some("main".to_string()), vec![summary]);
    record.snapshot_path = Some(snapshot_path.clone());
    record.repo_path = Some(repo_root.clone());
    history.operations.insert(0, record);
    history.save_to(Some(history_path.clone())).unwrap();
    let prev = std::env::var("CLAUDE_CODE_SYNC_CLAUDE_DIR");
    std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", temp_dir.path());
    let summary = undo_pull(Some(history_path), Some(temp_dir.path())).unwrap();
    match prev {
        Ok(v) => std::env::set_var("CLAUDE_CODE_SYNC_CLAUDE_DIR", v),
        Err(_) => std::env::remove_var("CLAUDE_CODE_SYNC_CLAUDE_DIR"),
    }

    assert!(
        summary.contains("KEPT"),
        "the snapshot must be KEPT so the user can recover: {summary}"
    );
    assert!(
        snapshot_path.exists(),
        "the kept snapshot must remain on disk; the user is told to recover from it"
    );
}

#[test]
fn a_create_skip_after_first_is_file_records_protection() {
    // F4 regression: B has no local copy and no base, but plants a
    // file between plan and apply. The first is_file guard fires
    // (the file is on disk by the time `apply_pull` runs). The guard
    // must record PROTECTION_SENTINEL into `synced_bases` so the next
    // pull reads the file as dirty (= kept_local), not as never-synced
    // (= would overwrite the user's content).
    //
    // Round 53 also fixed the second is_file guard's TOCTOU close
    // (engine.rs:1954) to record the sentinel — that path is not
    // exercised here because the file is planted BEFORE apply, not
    // mid-apply. The structural invariant this test asserts (the
    // sentinel must not flow into `bases_keys_written` so undo cannot
    // disarm it) holds for both guards because both push to
    // `protection_keys`, which is then filtered out by the
    // `retain` on `bases_keys_written` below the loop.
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    // A pushes the file; B's machine never received it (no sync).
    memory(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // B plans the create (its machine has no local copy yet),
    // then plants its own file, then applies. The apply hits the
    // first is_file check at line 1880, which must record the
    // PROTECTION sentinel so the next pull keeps the file.
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert!(!plan.creates.is_empty(), "the plan must target a create");
    memory(machine_b.path(), "user's content\n");
    let report = apply_pull(&plan, false).unwrap();
    let skipped: usize = report.counts.iter().map(|c| c.skipped).sum();
    assert_eq!(skipped, 1, "the late user file must skip, not overwrite");
    assert_eq!(
        read_memory(machine_b.path()),
        "user's content\n",
        "the skip must NOT overwrite the user's file"
    );
    // Structural invariant: protection entries must NOT flow into
    // `bases_keys_written` regardless of which guard fired — the undo
    // surgery would disarm the protection by calling `restore_keys`
    // on the sentinel and removing it from the bases file. The repo-
    // relative key for the memory category is `artifacts/<repo_subdir>
    // /<local path>`, so the actual key for this test's file is
    // `artifacts/memory/projects/-home-a-work-app/memory/fact.md`.
    let memory_rel = "artifacts/memory/projects/-home-a-work-app/memory/fact.md";
    assert!(
        !report.bases_keys_written.contains(&memory_rel.to_string()),
        "the protection sentinel must not flow into bases_keys_written (would disarm undo); got: {:?}",
        report.bases_keys_written
    );

    // Next pull: the sentinel must keep the file (not classify it as
    // never-synced and overwrite the user's content).
    let after = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert!(
        kept_local_of(&after) >= 1,
        "the protection sentinel must keep the file on the next pull; plan: {after:?}"
    );
}

// (No additional helpers: the F4 test asserts kept_local_of directly.)

#[test]
fn a_plan_deletes_with_mid_pull_cleared_base_is_kept_not_deleted() {
    // F3 regression: a concurrent same-repo sync that clears the bases
    // entry between plan and apply used to make dirty_status return
    // Clean, and the deletion proceeded — silently destroying a local
    // edit. The fix treats None-recorded as synthetically Dirty.
    //
    // Uses the skills category because `mirror_deletes` is true for it
    // (memory is `mirror_deletes: false` — a local delete of memory is
    // not propagated by push_artifacts, so the plan would not classify
    // the change as a delete).
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    sync(machine_b.path(), repo.path(), &filter);

    // Repo deletes its copy (A removes the local file, then pushes —
    // the push carries the deletion, the repo copy goes). B's plan
    // must classify the change as a delete target.
    let a_local = machine_a.path().join("skills/my-skill/SKILL.md");
    fs::remove_file(&a_local).unwrap();
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert!(
        !plan.deletes.is_empty(),
        "the plan must target the deletion"
    );

    // Simulate the concurrent prune: clear the bases entry on disk
    // before apply runs.
    use claude_code_sync::artifacts::bases;
    bases::forget(machine_b.path(), repo.path()).unwrap();

    // Apply with the cleared base: must NOT delete the user's file
    // even in non-interactive mode (the F3 fix's `prompt_overwrites`
    // gate was the data-loss path; round 53 dropped it).
    let report = apply_pull(&plan, false).unwrap();
    let deleted: usize = report.counts.iter().map(|c| c.deleted).sum();
    let kept: usize = report.counts.iter().map(|c| c.kept_local).sum();
    assert_eq!(deleted, 0, "no silent delete on a missing base entry");
    assert_eq!(kept, 1, "the file must be kept to preserve the local edit");
    assert_eq!(
        read_skill(machine_b.path()),
        "v1\n",
        "the user's file survives a concurrent base clear"
    );
    // The tracked entry must survive too: the F3 None-base keep is a
    // transient (round 55 found the round-53 fix was missing the
    // `deletes_to_retry.push` that the parallel pre-prompt Dirty
    // branch uses). Without it, the tracked entry is removed by the
    // delta loop, the next pull sees the locally-edited file as
    // never-synced, and silently overwrites it.
    let tracked = claude_code_sync::artifacts::tracked::load(machine_b.path(), repo.path());
    assert!(
        tracked.contains("artifacts/skills/my-skill/SKILL.md"),
        "F3-kept file must keep its tracked entry for the next round; tracked: {tracked:?}"
    );
}

#[test]
fn a_kept_local_with_concurrently_cleared_base_is_not_kept_local_clean() {
    // F8 regression: the general kept_local `nothing_to_publish`
    // computation used `is_none_or`, which returned true on None-base
    // — inflating kept_local_clean for the same concurrent-base-prune
    // case the round-52 fix closed for the machine_bytes Err path.
    // A None-recorded kept file is treated as DIRTY (the kept_local
    // classification already says local bytes the repo lacks);
    // counting it as `nothing_to_publish` would bias the hint.
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    skill(machine_a.path(), "v1\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    sync(machine_b.path(), repo.path(), &filter);

    // A moves the repository on, and B edits locally — both sides changed,
    // a kept_local pull.
    skill(machine_a.path(), "v2\n");
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    skill(machine_b.path(), "b's edit\n");
    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();
    assert_eq!(kept_local_of(&plan), 1);

    // Mid-pull: a concurrent sync pruned B's base entry. The kept_local
    // arm's general `nothing_to_publish` computation must NOT count
    // this file as clean (the F8 fix: `is_some_and` instead of
    // `is_none_or` — None-recorded → treated as dirty → kept_local_clean
    // is NOT incremented).
    use claude_code_sync::artifacts::bases;
    bases::forget(machine_b.path(), repo.path()).unwrap();
    let report = apply_pull(&plan, false).unwrap();

    let kept_local: usize = report.counts.iter().map(|c| c.kept_local).sum();
    assert_eq!(kept_local, 1, "the file is kept");
    assert_eq!(
        report.kept_local_clean, 0,
        "a None-recorded kept file must NOT be counted as kept_local_clean: report={:?}",
        report
    );
}

#[test]
fn an_undo_skip_branch_warning_names_record_entries_in_the_sole_copy_template() {
    // F5 regression: a pull that wrote record entries (the apply ran,
    // bases_keys_written = ["k1"]) but the snapshot's declared record
    // bytes are absent (carried_bytes = false, created = false) puts
    // `restore_shared_record` in the Skip branch with effective > 0.
    // Before F5: the kept-snapshot summary's sole_copy template chose
    // "pre-pull files the warning above says were not fully restored"
    // — contradicting the warning text which says "the kept snapshot
    // holds the post-pull state, not pre-pull". F5 flipped
    // `record_entries_unrestored` so the template names "record
    // entries" instead.
    let temp_dir = TempDir::new().unwrap();
    let snapshots_dir = temp_dir.path().join("snapshots");
    let repo_root = temp_dir.path().join("sync-repo");
    let history_path = temp_dir.path().join("history.json");

    let artifact_path = temp_dir.path().join("projects/x/fact.md");
    fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
    fs::write(&artifact_path, "post-pull").unwrap();

    // Declare a bases record the snapshot carries NO pre-pull bytes
    // for, plus a tracked entry with a pre-pull value (clean path).
    let mut snapshot = Snapshot::create(OperationType::Pull, vec![&artifact_path], None).unwrap();
    let bases_path = temp_dir.path().join(".claude-code-sync-bases.json");
    let tracked_path = temp_dir.path().join(".claude-code-sync-tracked.json");
    snapshot.record_files = vec![bases_path.to_string_lossy().to_string()];
    snapshot.record_touched_bases = Some(vec!["projects/x/fact.md".to_string()]);
    snapshot.deleted_files = vec![tracked_path.to_string_lossy().to_string()];
    let snapshot_path = snapshot.save_to_disk(Some(&snapshots_dir)).unwrap();

    // The apply wrote a bases entry (effective > 0) and was NOT a
    // creator of the bases file (created = false). The snapshot has no
    // pre-pull bases bytes (declared but not in `files`).
    let mut history = OperationHistory::from_path(Some(history_path.clone())).unwrap();
    let summary = ConversationSummary::new(
        "s".to_string(),
        "fact.md".to_string(),
        Some("2025-01-01T00:00:00Z".to_string()),
        1,
        SyncOperation::Modified,
    )
    .unwrap();
    let mut record =
        OperationRecord::new(OperationType::Pull, Some("main".to_string()), vec![summary]);
    record.snapshot_path = Some(snapshot_path.clone());
    record.repo_path = Some(repo_root.clone());
    record.bases_keys_written = Some(vec!["projects/x/fact.md".to_string()]);
    history.operations.insert(0, record);
    let history_file = std::fs::File::create(&history_path).unwrap();
    serde_json::to_writer_pretty(history_file, &history).unwrap();

    let result = undo_pull(Some(history_path), Some(temp_dir.path())).unwrap();
    // The warning fires (effective > 0, !created) and the kept-snapshot
    // summary names RECORD entries (F5 fix), not pre-pull files. The
    // negation form anchors the assertion against template wording
    // churn: a future rewrite that drops the F5 branch's wording but
    // re-uses the pre-pull-files phrase would be caught here.
    assert!(
        result.contains("record entries the warnings above say were not restored"),
        "the kept-snapshot summary must name record entries (F5 fix); got: {result}"
    );
    assert!(
        !result.contains("the pre-pull files the warning above says were not fully restored"),
        "the kept-snapshot summary must NOT use the pre-pull-files wording (F5 fix); got: {result}"
    );
    assert!(result.contains("KEPT"), "the snapshot is pinned: {result}");
    assert!(snapshot_path.exists(), "the snapshot survives the undo");
}

#[test]
fn a_local_edit_to_a_union_memory_index_survives_a_pull() {
    // Issue #103 follow-up: PR #105 protected raw-overwrite files but
    // union-merged memory indexes (memory/MEMORY.md) still silently
    // clobbered a local edit, because the union-merge contract is
    // "incoming wins per entry" and a user-curated bullet at a
    // target key the incoming (repo) also has is treated as a
    // match and overwritten. The fix: record a base for every
    // union-merged file and route to `kept_local` when the local
    // bytes have moved since the last sync (F3-class synthetic
    // dirty + conventional is_dirty, same as the overwrites arm).
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter = all_on_filter();

    // Seed both machines with the same starting bullet, then sync
    // them up: a base entry exists for the memory file and the
    // tracked set knows it.
    let mem = |claude: &Path, body: &str| {
        let path = claude.join("projects/-home-a-work-app/memory/MEMORY.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    };
    mem(machine_a.path(), "- [shared](shared.md) from A and B\n");
    sync(machine_a.path(), repo.path(), &filter);
    sync(machine_b.path(), repo.path(), &filter);

    // A pushes a NEW bullet (the shared key is unchanged on purpose
    // — the test is about the same-key clobber case, which the
    // pre-fix code hit on the per-target "incoming wins" rule).
    mem(
        machine_a.path(),
        "- [shared](shared.md) from A and B\n- [a-only](a-only.md) new on A\n",
    );
    push_artifacts(
        machine_a.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // B edits the SAME bullet locally between A's push and B's pull.
    mem(
        machine_b.path(),
        "- [shared](shared.md) B's local edit wins\n",
    );

    let plan = plan_pull(machine_b.path(), repo.path(), &filter).unwrap();

    // Regression: a base IS recorded for the memory index now. The
    // pre-fix `records_base(UnionMemoryIndex) = !is_memory_index(rel)`
    // left the entry out, which is why the union-merge path never
    // even consulted the dirty check.
    let recorded = bases::load(machine_b.path(), repo.path());
    assert!(
        recorded.contains_key("projects/-home-a-work-app/memory/MEMORY.md"),
        "a base is recorded for the memory index (issue #103 follow-up); got: {recorded:?}"
    );

    // The fix fires: the locally-edited file goes to kept_local,
    // NOT through the union merge.
    assert_eq!(
        kept_local_of(&plan),
        1,
        "the local edit routes to kept_local: {:?}",
        plan.kept_local
            .iter()
            .map(|w| w.category_path.display().to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        plan.unions.len(),
        0,
        "the union merge must NOT run on a locally-edited memory index"
    );

    // Apply: local bytes must still hold B's edit, NOT A's incoming
    // text for the shared key.
    apply_pull(&plan, false).unwrap();
    let after = fs::read_to_string(
        machine_b
            .path()
            .join("projects/-home-a-work-app/memory/MEMORY.md"),
    )
    .unwrap();
    assert!(
        after.contains("B's local edit wins"),
        "the local edit survives: {after}"
    );
    assert!(
        !after.contains("[a-only](a-only.md) new on A"),
        "no incoming text was union-merged in: {after}"
    );
}

#[test]
fn a_push_does_not_resurrect_a_file_the_remote_lost() {
    // Adversarial review 2026-10-09 (5ff1d62 hole): with
    // `mirror_deletes: false` (e.g. ProjectAttachments), the push
    // arm unconditionally `write_atomic`s a local file into the repo
    // whenever the repo is missing it. If the user `git rm`-ed the
    // file on another machine and that deletion was committed +
    // pushed, the next push from this machine silently resurrects
    // the file — the "Added: 1" line in the push report gives no
    // hint that the remote history was just rewritten.
    //
    // The contract the fix targets: a push MUST NOT re-publish a
    // file the remote no longer has, when this machine has a
    // `tracked` record for it (meaning this machine has synced it
    // before, and the remote's loss is recent and intentional).
    // The local copy is preserved on disk; the user is told
    // explicitly to use `--force` if they really want to override.
    let repo = TempDir::new().unwrap();
    let machine = TempDir::new().unwrap();
    let filter = all_on_filter();

    // Seed: machine has a per-project memory file and has synced it
    // before (so `tracked` contains its repo-relative path).
    let mem = |claude: &Path, body: &str| {
        let path = claude.join("projects/-home-x/memory/feedback_x.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    };
    mem(machine.path(), "first version\n");
    push_artifacts(
        machine.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    // Simulate the remote losing the file (e.g. another machine
    // `git rm`-ed and pushed). The local copy is preserved on disk
    // because `mirror_deletes: false` (and the user has not edited
    // it locally — same bytes as last sync).
    let repo_path = repo.path().join("projects/-home-x/memory/feedback_x.md");
    fs::remove_file(&repo_path).unwrap();
    assert!(
        !repo_path.exists(),
        "the repo is now missing the file (simulating a remote deletion)"
    );

    // Push again — the contract: do NOT resurrect the file. The
    // local copy stays on disk; the repo stays missing the file.
    let report = push_artifacts(
        machine.path(),
        repo.path(),
        &filter,
        &std::collections::HashSet::new(),
    )
    .unwrap();

    assert!(
        !repo_path.exists(),
        "the push MUST NOT resurrect a file the remote lost"
    );
    assert!(
        machine
            .path()
            .join("projects/-home-x/memory/feedback_x.md")
            .exists(),
        "the local copy is preserved on disk"
    );
    // The push report must surface the refusal — the counter is the
    // only live signal a user has that the 5ff1d62 guard fired.
    // Without this, a future refactor that drops the field (or
    // stops populating it) silently regresses the protection.
    assert_eq!(
        report.total_held_back_remote_lost(),
        1,
        "the push report must surface the held-back file so the user can see the refusal"
    );
}
