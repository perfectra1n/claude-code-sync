//! Integration tests for the portability additions: the project map, path
//! tokenizing, memory-index union and deletion mirroring. Two temp `~/.claude`
//! trees stand in for two machines sharing one sync repository.

use std::fs;
use std::path::{Path, PathBuf};

use claude_code_sync::artifacts::engine::{
    apply_pull, plan_pull, push_artifacts, ArtifactChange, ArtifactChangeKind,
};
use claude_code_sync::artifacts::registry::{ArtifactToggles, CategoryId};
use claude_code_sync::artifacts::tracked;
use claude_code_sync::filter::FilterConfig;
use tempfile::TempDir;

fn all_on_filter() -> FilterConfig {
    FilterConfig {
        sync_artifacts: ArtifactToggles::all_enabled(),
        ..Default::default()
    }
}

fn mapped_filter(id: &str, project_path: &str) -> FilterConfig {
    let mut filter = all_on_filter();
    filter
        .project_map
        .insert(id.to_string(), PathBuf::from(project_path));
    filter
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn project_memory(claude: &Path, encoded_project: &str, file: &str) -> PathBuf {
    claude
        .join("projects")
        .join(encoded_project)
        .join("memory")
        .join(file)
}

fn sync_both_ways(claude: &Path, repo: &Path, filter: &FilterConfig) {
    push_artifacts(claude, repo, filter).unwrap();
    let plan = plan_pull(claude, repo, filter).unwrap();
    apply_pull(&plan, false).unwrap();
}

#[test]
fn a_mapped_project_lands_under_its_own_path_on_the_other_machine() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();

    write(
        &project_memory(machine_a.path(), "-home-a-work-app", "fact.md"),
        "a fact\n",
    );

    push_artifacts(
        machine_a.path(),
        repo.path(),
        &mapped_filter("app", "/home/a/work/app"),
    )
    .unwrap();

    assert!(
        repo.path().join("projects/app/memory/fact.md").is_file(),
        "the repo stores the project under its canonical id"
    );

    let filter_b = mapped_filter("app", "/srv/b/checkouts/app-renamed");
    let plan = plan_pull(machine_b.path(), repo.path(), &filter_b).unwrap();
    apply_pull(&plan, false).unwrap();

    let landed = project_memory(machine_b.path(), "-srv-b-checkouts-app-renamed", "fact.md");
    assert_eq!(fs::read_to_string(landed).unwrap(), "a fact\n");
}

#[test]
fn an_unmapped_project_keeps_its_encoded_directory() {
    let repo = TempDir::new().unwrap();
    let claude = TempDir::new().unwrap();
    write(
        &project_memory(claude.path(), "-home-a-work-other", "fact.md"),
        "a fact\n",
    );

    push_artifacts(claude.path(), repo.path(), &all_on_filter()).unwrap();

    assert!(repo
        .path()
        .join("projects/-home-a-work-other/memory/fact.md")
        .is_file());
}

#[test]
fn a_memory_index_gains_the_other_machines_entries_instead_of_losing_them() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let filter_a = mapped_filter("app", "/home/a/app");
    let filter_b = mapped_filter("app", "/home/b/app");

    write(
        &project_memory(machine_a.path(), "-home-a-app", "MEMORY.md"),
        "# Index\n\n- [a](a.md) from A\n",
    );
    write(
        &project_memory(machine_b.path(), "-home-b-app", "MEMORY.md"),
        "# Index\n\n- [z](z.md) from B\n",
    );

    push_artifacts(machine_a.path(), repo.path(), &filter_a).unwrap();
    sync_both_ways(machine_b.path(), repo.path(), &filter_b);
    sync_both_ways(machine_a.path(), repo.path(), &filter_a);

    for (claude, encoded) in [
        (machine_a.path(), "-home-a-app"),
        (machine_b.path(), "-home-b-app"),
    ] {
        let index = fs::read_to_string(project_memory(claude, encoded, "MEMORY.md")).unwrap();
        assert!(index.contains("[a](a.md)"), "{encoded}: {index}");
        assert!(index.contains("[z](z.md)"), "{encoded}: {index}");
        assert!(index.contains("# Index"), "{encoded}: {index}");
    }
}

#[test]
fn settings_paths_are_stored_neutrally_and_rendered_per_machine() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let hook = format!(
        "{{\"hooks\":{{\"Stop\":[{{\"command\":\"{}/hooks/stop.sh\"}}]}}}}",
        machine_a.path().display()
    );
    fs::write(machine_a.path().join("settings.json"), &hook).unwrap();

    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();

    let stored = fs::read_to_string(repo.path().join("artifacts/settings/settings.json")).unwrap();
    assert!(stored.contains("__CLAUDE_DIR__/hooks/stop.sh"), "{stored}");
    assert!(!stored.contains(&machine_a.path().display().to_string()));

    let plan = plan_pull(machine_b.path(), repo.path(), &all_on_filter()).unwrap();
    apply_pull(&plan, false).unwrap();

    let rendered = fs::read_to_string(machine_b.path().join("settings.json")).unwrap();
    assert!(
        rendered.contains(&format!("{}/hooks/stop.sh", machine_b.path().display())),
        "{rendered}"
    );
}

#[test]
fn a_settings_file_that_only_differs_by_machine_path_is_not_rewritten() {
    let repo = TempDir::new().unwrap();
    let claude = TempDir::new().unwrap();
    let hook = format!(
        "{{\"hooks\":{{\"Stop\":[{{\"command\":\"{}/hooks/stop.sh\"}}]}}}}",
        claude.path().display()
    );
    fs::write(claude.path().join("settings.json"), &hook).unwrap();
    push_artifacts(claude.path(), repo.path(), &all_on_filter()).unwrap();

    let plan = plan_pull(claude.path(), repo.path(), &all_on_filter()).unwrap();

    assert!(
        plan.is_empty(),
        "tokenized settings must compare equal to their own machine rendering"
    );
}

#[test]
fn a_skill_deleted_locally_is_removed_from_the_repo_on_the_next_push() {
    let repo = TempDir::new().unwrap();
    let claude = TempDir::new().unwrap();
    write(&claude.path().join("skills/keep/SKILL.md"), "# keep\n");
    write(&claude.path().join("skills/drop/SKILL.md"), "# drop\n");

    push_artifacts(claude.path(), repo.path(), &all_on_filter()).unwrap();
    fs::remove_dir_all(claude.path().join("skills/drop")).unwrap();
    let report = push_artifacts(claude.path(), repo.path(), &all_on_filter()).unwrap();

    assert!(repo.path().join("artifacts/skills/keep/SKILL.md").is_file());
    assert!(!repo.path().join("artifacts/skills/drop/SKILL.md").exists());
    assert_eq!(
        report.counts.iter().map(|c| c.deleted).sum::<usize>(),
        1,
        "the removal is reported"
    );
}

#[test]
fn a_skill_deleted_on_another_machine_is_removed_here_on_pull() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    write(&machine_a.path().join("skills/gone/SKILL.md"), "# gone\n");

    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();
    sync_both_ways(machine_b.path(), repo.path(), &all_on_filter());
    assert!(machine_b.path().join("skills/gone/SKILL.md").is_file());

    fs::remove_dir_all(machine_a.path().join("skills/gone")).unwrap();
    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();

    let plan = plan_pull(machine_b.path(), repo.path(), &all_on_filter()).unwrap();
    let snapshotted = plan.paths_to_snapshot();
    let report = apply_pull(&plan, false).unwrap();

    assert!(!machine_b.path().join("skills/gone/SKILL.md").exists());
    assert_eq!(report.counts.iter().map(|c| c.deleted).sum::<usize>(), 1);
    assert!(
        snapshotted.contains(&machine_b.path().join("skills/gone/SKILL.md")),
        "a deletion is snapshotted so undo can restore it"
    );
}

fn build_skill_change(kind: ArtifactChangeKind, path: &str) -> ArtifactChange {
    ArtifactChange {
        category: CategoryId::Skills,
        kind,
        path: PathBuf::from(path),
    }
}

#[test]
fn a_push_lists_each_skill_it_added_modified_and_deleted() {
    let repo = TempDir::new().unwrap();
    let claude = TempDir::new().unwrap();
    write(&claude.path().join("skills/keep/SKILL.md"), "# keep\n");
    write(&claude.path().join("skills/edit/SKILL.md"), "# edit\n");
    write(&claude.path().join("skills/drop/SKILL.md"), "# drop\n");
    push_artifacts(claude.path(), repo.path(), &all_on_filter()).unwrap();

    write(&claude.path().join("skills/edit/SKILL.md"), "# edited\n");
    write(&claude.path().join("skills/new/SKILL.md"), "# new\n");
    fs::remove_dir_all(claude.path().join("skills/drop")).unwrap();
    let report = push_artifacts(claude.path(), repo.path(), &all_on_filter()).unwrap();

    let mut changes = report.changes.clone();
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    assert_eq!(
        changes,
        vec![
            build_skill_change(ArtifactChangeKind::Deleted, "drop/SKILL.md"),
            build_skill_change(ArtifactChangeKind::Modified, "edit/SKILL.md"),
            build_skill_change(ArtifactChangeKind::Added, "new/SKILL.md"),
        ]
    );
}

#[test]
fn a_pull_lists_each_skill_it_created_and_deleted_here() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    write(&machine_a.path().join("skills/gone/SKILL.md"), "# gone\n");
    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();
    sync_both_ways(machine_b.path(), repo.path(), &all_on_filter());

    fs::remove_dir_all(machine_a.path().join("skills/gone")).unwrap();
    write(&machine_a.path().join("skills/fresh/SKILL.md"), "# fresh\n");
    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &all_on_filter()).unwrap();
    let report = apply_pull(&plan, false).unwrap();

    assert_eq!(
        report.changes,
        vec![
            build_skill_change(ArtifactChangeKind::Added, "fresh/SKILL.md"),
            build_skill_change(ArtifactChangeKind::Deleted, "gone/SKILL.md"),
        ]
    );
}

#[test]
fn a_machine_that_never_synced_deletes_nothing() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    write(
        &machine_a.path().join("skills/shared/SKILL.md"),
        "# shared\n",
    );
    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();

    // B has never received anything, so it has no record and must not read its
    // empty skills directory as "everything was deleted".
    fs::create_dir_all(machine_b.path().join("skills")).unwrap();
    push_artifacts(machine_b.path(), repo.path(), &all_on_filter()).unwrap();

    assert!(repo
        .path()
        .join("artifacts/skills/shared/SKILL.md")
        .is_file());
}

#[test]
fn a_machine_missing_the_category_entirely_leaves_the_repo_alone() {
    let repo = TempDir::new().unwrap();
    let claude = TempDir::new().unwrap();
    write(&claude.path().join("skills/shared/SKILL.md"), "# shared\n");
    push_artifacts(claude.path(), repo.path(), &all_on_filter()).unwrap();

    fs::remove_dir_all(claude.path().join("skills")).unwrap();
    push_artifacts(claude.path(), repo.path(), &all_on_filter()).unwrap();

    assert!(
        repo.path()
            .join("artifacts/skills/shared/SKILL.md")
            .is_file(),
        "an absent skills directory says nothing about what the others hold"
    );
    assert!(
        !tracked::load(claude.path(), repo.path()).is_empty(),
        "and its tracked paths survive, so a later deletion still propagates"
    );
}

/// Run a git command in `repo`, failing loudly.
fn git(repo: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(status.status.success(), "git {args:?} failed: {status:?}");
}

#[test]
fn deleting_the_last_file_of_a_category_survives_a_commit_and_clone() {
    let origin = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let clone = TempDir::new().unwrap();
    git(origin.path(), &["init", "-q", "-b", "main"]);
    write(&machine_a.path().join("skills/only/SKILL.md"), "# only\n");

    // A publishes the skill; B picks it up from a clone of the repo.
    push_artifacts(machine_a.path(), origin.path(), &all_on_filter()).unwrap();
    git(origin.path(), &["add", "-A"]);
    git(origin.path(), &["commit", "-qm", "add skill"]);
    git(
        origin.path(),
        &["clone", "-q", ".", &clone.path().to_string_lossy()],
    );
    sync_both_ways(machine_b.path(), clone.path(), &all_on_filter());
    assert!(machine_b.path().join("skills/only/SKILL.md").is_file());

    // A deletes its last skill and publishes that. Git does not track empty
    // directories, so without a marker the category would vanish from the
    // clone and read as "this repo has no skills" instead of "no skills left".
    fs::remove_dir_all(machine_a.path().join("skills/only")).unwrap();
    push_artifacts(machine_a.path(), origin.path(), &all_on_filter()).unwrap();
    git(origin.path(), &["add", "-A"]);
    git(origin.path(), &["commit", "-qm", "drop skill"]);
    git(clone.path(), &["pull", "-q", "origin", "main"]);

    assert!(
        clone.path().join("artifacts/skills").is_dir(),
        "the emptied category still exists in a fresh checkout"
    );
    let plan = plan_pull(machine_b.path(), clone.path(), &all_on_filter()).unwrap();
    let report = apply_pull(&plan, false).unwrap();

    assert_eq!(report.total_deleted(), 1);
    assert!(!machine_b.path().join("skills/only/SKILL.md").exists());
    // And nothing resurrects it: B's next push leaves the repo empty.
    push_artifacts(machine_b.path(), clone.path(), &all_on_filter()).unwrap();
    assert!(!clone.path().join("artifacts/skills/only/SKILL.md").exists());
}

#[test]
fn the_category_marker_never_reaches_a_machine() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    write(&machine_a.path().join("skills/kept/SKILL.md"), "# kept\n");

    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();
    sync_both_ways(machine_b.path(), repo.path(), &all_on_filter());

    assert!(repo.path().join("artifacts/skills/.synced").is_file());
    assert!(!machine_b.path().join("skills/.synced").exists());
}

#[test]
fn a_category_the_repo_does_not_have_deletes_nothing_locally() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    write(
        &machine_a.path().join("skills/shared/SKILL.md"),
        "# shared\n",
    );

    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();
    sync_both_ways(machine_b.path(), repo.path(), &all_on_filter());
    assert!(machine_b.path().join("skills/shared/SKILL.md").is_file());

    // A rewound branch, an older branch or `undo push` can all leave the repo
    // without the category — which says nothing about the other machines.
    fs::remove_dir_all(repo.path().join("artifacts/skills")).unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &all_on_filter()).unwrap();
    apply_pull(&plan, false).unwrap();

    assert!(plan.deletes.is_empty());
    assert!(machine_b.path().join("skills/shared/SKILL.md").is_file());
    assert!(
        !tracked::load(machine_b.path(), repo.path()).is_empty(),
        "the record survives, so a real deletion still propagates later"
    );
}

#[test]
fn a_home_path_in_settings_is_stored_as_a_token() {
    let repo = TempDir::new().unwrap();
    let claude = TempDir::new().unwrap();
    // Both machines in this process share one home directory, so this can only
    // show that the home path is neutralized on the way in; rendering it back
    // per machine is covered by the unit tests in artifacts::tokens.
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let settings = format!(
        "{{\"hooks\":{{\"Stop\":[{{\"command\":\"{}/bin/stop.sh\"}}]}}}}",
        home.display()
    );
    fs::write(claude.path().join("settings.json"), &settings).unwrap();

    push_artifacts(claude.path(), repo.path(), &all_on_filter()).unwrap();

    let stored = fs::read_to_string(repo.path().join("artifacts/settings/settings.json")).unwrap();
    assert!(stored.contains("__HOME__/bin/stop.sh"), "{stored}");
    assert!(!stored.contains(&home.display().to_string()));
}

#[test]
fn rules_sync_like_any_other_curated_directory() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    write(&machine_a.path().join("rules/rule_go.md"), "# go rules\n");

    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &all_on_filter()).unwrap();
    apply_pull(&plan, false).unwrap();

    assert_eq!(
        fs::read_to_string(machine_b.path().join("rules/rule_go.md")).unwrap(),
        "# go rules\n"
    );
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path).unwrap().permissions().mode() & 0o111 != 0
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    write(path, contents);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// settings.json names hook scripts by path, so the scripts have to travel
/// with it -- and a hook that arrives without its executable bit is a hook
/// Claude Code cannot run.
#[cfg(unix)]
#[test]
fn a_hook_arrives_on_the_other_machine_ready_to_run() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let script = "#!/bin/bash\nexit 0\n";
    write_executable(&machine_a.path().join("hooks/rule-check.sh"), script);

    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();
    let stored = repo.path().join("artifacts/hooks/rule-check.sh");
    assert!(
        is_executable(&stored),
        "the repo copy carries the bit, so git records mode 100755"
    );

    let plan = plan_pull(machine_b.path(), repo.path(), &all_on_filter()).unwrap();
    apply_pull(&plan, false).unwrap();

    let landed = machine_b.path().join("hooks/rule-check.sh");
    assert_eq!(fs::read_to_string(&landed).unwrap(), script);
    assert!(is_executable(&landed));
}

/// Only the executable bit travels. A pull must never hand out read rights the
/// local file did not give away: prompt history, plans and settings are private
/// files on a shared machine.
#[cfg(unix)]
#[test]
fn a_pull_does_not_widen_a_private_local_file() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    write(&machine_a.path().join("plans/plan.md"), "from A\n");
    write(&machine_b.path().join("plans/plan.md"), "from B\n");
    let private = machine_b.path().join("plans/plan.md");
    fs::set_permissions(&private, fs::Permissions::from_mode(0o600)).unwrap();

    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &all_on_filter()).unwrap();
    apply_pull(&plan, false).unwrap();

    assert_eq!(fs::read_to_string(&private).unwrap(), "from A\n");
    assert_eq!(
        fs::metadata(&private).unwrap().permissions().mode() & 0o777,
        0o600,
        "the repo's checkout mode must not replace the local file's"
    );
    assert!(!is_executable(&private));
}

/// Every repository written before the bit was synced holds its files
/// non-executable, so a pull from one must leave this machine's scripts alone
/// rather than disarm them — and `sync` pulls before it pushes.
#[cfg(unix)]
#[test]
fn a_pull_does_not_disarm_a_local_script() {
    let repo = TempDir::new().unwrap();
    let claude = TempDir::new().unwrap();
    let script = "#!/bin/bash\nexit 0\n";
    write_executable(&claude.path().join("hooks/rule-check.sh"), script);
    // What an older version left in the repo: same bytes, no executable bit.
    write(&repo.path().join("artifacts/hooks/rule-check.sh"), script);

    let plan = plan_pull(claude.path(), repo.path(), &all_on_filter()).unwrap();
    apply_pull(&plan, false).unwrap();

    assert!(is_executable(&claude.path().join("hooks/rule-check.sh")));
}

/// A `chmod +x` changes no bytes, so content comparison alone would leave the
/// other machine running a script it cannot execute.
#[cfg(unix)]
#[test]
fn making_a_hook_executable_reaches_the_other_machine_on_its_own() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let hook = machine_a.path().join("hooks/late.sh");
    write(&hook, "#!/bin/bash\n");

    sync_both_ways(machine_a.path(), repo.path(), &all_on_filter());
    let plan = plan_pull(machine_b.path(), repo.path(), &all_on_filter()).unwrap();
    apply_pull(&plan, false).unwrap();
    let landed = machine_b.path().join("hooks/late.sh");
    assert!(!is_executable(&landed), "not a script yet");

    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();
    assert!(
        is_executable(&repo.path().join("artifacts/hooks/late.sh")),
        "the bit alone is enough to update the repo copy"
    );

    let plan = plan_pull(machine_b.path(), repo.path(), &all_on_filter()).unwrap();
    assert!(!plan.is_empty(), "a mode-only difference is still a change");
    apply_pull(&plan, false).unwrap();

    assert!(is_executable(&landed));
}

#[cfg(unix)]
#[test]
fn a_hook_deleted_here_is_gone_on_the_other_machine_too() {
    let repo = TempDir::new().unwrap();
    let machine_a = TempDir::new().unwrap();
    let machine_b = TempDir::new().unwrap();
    let hook = machine_a.path().join("hooks/stale.sh");
    write_executable(&hook, "#!/bin/bash\n");

    sync_both_ways(machine_a.path(), repo.path(), &all_on_filter());
    let plan = plan_pull(machine_b.path(), repo.path(), &all_on_filter()).unwrap();
    apply_pull(&plan, false).unwrap();
    assert!(machine_b.path().join("hooks/stale.sh").is_file());

    fs::remove_file(&hook).unwrap();
    push_artifacts(machine_a.path(), repo.path(), &all_on_filter()).unwrap();
    let plan = plan_pull(machine_b.path(), repo.path(), &all_on_filter()).unwrap();
    apply_pull(&plan, false).unwrap();

    assert!(!repo.path().join("artifacts/hooks/stale.sh").exists());
    assert!(!machine_b.path().join("hooks/stale.sh").exists());
}
