// Module declarations
pub mod discovery;
mod init;
mod pull;
pub mod push;
mod remote;
mod state;
mod status;

// Re-export public types and functions
pub use init::{init_from_onboarding, init_sync_repo};
pub use pull::pull_history;
pub use push::push_history;
pub use remote::{remove_remote, set_remote, show_remote};
pub use state::{MultiRepoState, RepoConfig, SyncState};
pub use status::show_status;

use anyhow::Result;
use colored::Colorize;

use crate::artifacts::engine::{descriptor, ArtifactChangeKind, ArtifactReport, CategoryCounts};

/// Maximum number of conversations to display per project in summary
const MAX_CONVERSATIONS_TO_DISPLAY: usize = 10;

/// List the artifact files a push or pull changed, one block per category with
/// its counters, the way conversations are listed per project.
pub(crate) fn print_artifact_changes(report: &ArtifactReport) {
    let has_no_changes = report.changes.is_empty();
    if has_no_changes {
        return;
    }

    println!("\n{}", "Changed Artifacts:".bold());

    for counts in &report.counts {
        let mut changes: Vec<_> = report
            .changes
            .iter()
            .filter(|change| change.category == counts.category)
            .collect();
        let change_count = changes.len();
        if change_count == 0 {
            continue;
        }
        changes.sort_by(|left, right| left.path.cmp(&right.path));

        let category_name = descriptor(counts.category).name;
        println!(
            "\n  {} {}/  {}",
            "Category:".bold(),
            category_name.cyan(),
            describe_change_counts(counts).dimmed()
        );

        for change in changes.iter().take(MAX_CONVERSATIONS_TO_DISPLAY) {
            let kind_label = match change.kind {
                ArtifactChangeKind::Added => "ADD".green(),
                ArtifactChangeKind::Modified => "MOD".cyan(),
                ArtifactChangeKind::Deleted => "DEL".red(),
                ArtifactChangeKind::KeptLocal => "KEEP".yellow(),
                ArtifactChangeKind::KeptLocalDelete => "KEEP".yellow(),
                ArtifactChangeKind::HeldBackRemoteLost => "HOLD".magenta(),
                ArtifactChangeKind::Unfinished => "SKIP".dimmed(),
            };
            println!("    {} {}", kind_label, change.path.display());
        }

        if change_count > MAX_CONVERSATIONS_TO_DISPLAY {
            let hidden_count = change_count - MAX_CONVERSATIONS_TO_DISPLAY;
            println!(
                "    {}",
                format!("... and {hidden_count} more files").dimmed()
            );
        }
    }
}

fn describe_change_counts(counts: &CategoryCounts) -> String {
    let labelled_counts = [
        (counts.added, "added"),
        (counts.modified, "modified"),
        (counts.deleted, "deleted"),
        (counts.kept_local, "kept local"),
        (counts.pending_push, "to push"),
        (counts.skipped, "skipped"),
    ];

    labelled_counts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Write the `.gitattributes` sync rules into the sync repository and commit
/// them.
///
/// Two machines that both appended between syncs leave git with a conflict in
/// a transcript or in the prompt history, which stops the pull. The rules tell
/// git to keep both sides' lines instead, and to write text files with LF.
/// A repository with other pending work is left alone: staging everything
/// would sweep that work into this commit, and a push commits the rules along
/// with it anyway.
pub(crate) fn commit_sync_attributes(
    repo: &dyn crate::scm::Scm,
    repo_path: &std::path::Path,
) -> Result<()> {
    if repo.has_changes()? {
        return Ok(());
    }
    if !crate::scm::attributes::ensure_sync_attributes(repo_path)? {
        return Ok(());
    }

    repo.stage_all()?;
    repo.stage_renormalized()?;
    repo.commit("Store text files with LF and merge append-only files")?;

    Ok(())
}

/// Bidirectional sync: pull remote changes, then push local changes
pub fn sync_bidirectional(
    commit_message: Option<&str>,
    branch: Option<&str>,
    exclude_attachments: bool,
    interactive: bool,
    verbosity: crate::VerbosityLevel,
) -> Result<()> {
    use crate::VerbosityLevel;
    use std::collections::HashSet;

    if verbosity != VerbosityLevel::Quiet {
        println!("{}", "=== Bidirectional Sync ===".bold().cyan());
        println!();
        println!("{}", "Step 1: Pulling remote changes...".bold());
    }

    // First, pull remote changes. The artifact report from the apply
    // is the source of truth for the next push: every KeptLocal /
    // KeptLocalDelete file the pull just protected is added to the
    // push's per-file skip set so the next push does not reverse
    // those decisions (a clean-keep file's local==base bytes would
    // otherwise overwrite the repo's newer version).
    let artifact_report = pull_history(true, branch, interactive, verbosity, true)?;
    // The banner counts only files that actually exist locally and were
    // held (KeptLocal): every KeptLocalDelete record site has the local
    // file already gone, so the push collects nothing for them — nothing
    // is held. The skip set still includes KeptLocalDelete defensively
    // (a file recreated before the push runs must not resurrect the
    // repo copy the user just accepted the deletion of).
    let kept_local = artifact_report.total_kept_local();
    let skip_artifact_paths: HashSet<(crate::artifacts::registry::CategoryId, std::path::PathBuf)> =
        artifact_report.paths_the_push_must_skip();

    if kept_local > 0 && verbosity != VerbosityLevel::Quiet {
        println!();
        println!(
            "{}",
            format!("Step 2: {kept_local} artifact file(s) held back — pushed around them")
                .yellow()
                .bold()
        );
        println!(
            "  {} take the repo copy with {} (a merge alone does not clear the window), or publish deliberately with {}",
            "→".cyan(),
            "claude-code-sync pull -i".bold(),
            "claude-code-sync push".bold()
        );
        println!();
        println!("{}", "Pushing local changes (held files skipped)...".bold());
    }

    // Purge between the two, when it is turned on, so the removals travel
    // with the push below instead of waiting for the next one.
    let filter = crate::filter::FilterConfig::load()?;
    if filter.purge_after_sync {
        let state = SyncState::load()?;
        crate::handlers::purge::purge_after_sync(&filter, &state.sync_repo_path)?;
    }

    if kept_local == 0 && verbosity != VerbosityLevel::Quiet {
        println!();
        println!("{}", "Step 2: Pushing local changes...".bold());
    }

    // Push local changes. The skip set is per-file, not per-category:
    // unrelated categories travel normally even when one file is held.
    push_history(
        commit_message,
        true,
        branch,
        exclude_attachments,
        interactive,
        verbosity,
        &skip_artifact_paths,
        &[], // resurrect: no CLI flag exposed in the sync path
    )?;

    if verbosity == VerbosityLevel::Quiet && kept_local > 0 {
        // Quiet mode still owes cron/CI the one fact that matters.
        println!("Sync complete ({kept_local} artifact file(s) held back)");
    } else if verbosity == VerbosityLevel::Quiet {
        println!("Sync complete");
    } else if kept_local > 0 {
        // The honest footer: the held files did not travel; everything
        // else did.
        println!();
        println!("{}", "=== Sync Complete ===".green().bold());
        println!(
            "  {} sessions synced; {kept_local} artifact file(s) held back; rest pushed",
            "✓".green()
        );
    } else {
        println!();
        println!("{}", "=== Sync Complete ===".green().bold());
        println!(
            "  {} Your local and remote histories are now in sync",
            "✓".green()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::FilterConfig;
    use crate::scm;
    use serial_test::serial;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    fn git_output(dir: &Path, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    #[test]
    fn adding_the_rules_converts_a_hook_stored_with_crlf_to_lf() {
        let repo_dir = TempDir::new().unwrap();
        let repo_path = repo_dir.path();
        let repo = scm::init(repo_path).unwrap();
        git_output(repo_path, &["config", "user.name", "Old"]);
        git_output(repo_path, &["config", "user.email", "old@local"]);
        git_output(repo_path, &["config", "core.autocrlf", "false"]);
        std::fs::write(repo_path.join("hook.sh"), "#!/bin/sh\r\necho hi\r\n").unwrap();
        std::fs::write(repo_path.join(".gitattributes"), "*.jsonl merge=union\n").unwrap();
        repo.stage_all().unwrap();
        repo.commit("written by an older version").unwrap();

        commit_sync_attributes(repo.as_ref(), repo_path).unwrap();

        let stored_hook = git_output(repo_path, &["show", "HEAD:hook.sh"]);
        let commit_count = git_output(repo_path, &["rev-list", "--count", "HEAD"]);
        assert_eq!(stored_hook, b"#!/bin/sh\necho hi\n");
        assert_eq!(commit_count, b"2\n");
    }

    #[test]
    #[serial]
    fn test_url_validation() {
        let temp_dir = TempDir::new().unwrap();
        // Isolate the config dir: this test writes and then deletes state.json at the
        // resolved default location, so without an override it would clobber the real
        // ~/Library/Application Support/claude-code-sync/state.json on macOS.
        // #[serial] keeps the process-global override from racing other tests.
        let prev = std::env::var("CLAUDE_CODE_SYNC_CONFIG_DIR").ok();
        std::env::set_var("CLAUDE_CODE_SYNC_CONFIG_DIR", temp_dir.path());
        let repo_path = temp_dir.path().join("test-repo");

        // Initialize a test repo
        scm::init(&repo_path).unwrap();

        // Save a test state
        let state = SyncState {
            sync_repo_path: repo_path.clone(),
            has_remote: false,
            is_cloned_repo: false,
        };

        // Create state directory using ConfigManager
        let _state_path = crate::config::ConfigManager::ensure_config_dir().unwrap();
        let state_file = crate::config::ConfigManager::state_file_path().unwrap();
        std::fs::write(&state_file, serde_json::to_string(&state).unwrap()).unwrap();

        // Valid HTTPS URL
        let result = set_remote("origin", "https://github.com/user/repo.git");
        assert!(result.is_ok());

        // Valid HTTP URL
        let result = set_remote("origin", "http://gitlab.com/user/repo.git");
        assert!(result.is_ok());

        // Valid SSH URL
        let result = set_remote("origin", "git@github.com:user/repo.git");
        assert!(result.is_ok());

        // Invalid URL (missing protocol)
        let result = set_remote("origin", "github.com/user/repo.git");
        assert!(result.is_err());
        if let Err(e) = result {
            let error_msg = e.to_string();
            assert!(error_msg.contains("Invalid URL format"));
        }

        // Cleanup
        std::fs::remove_file(&state_file).ok();
        match prev {
            Some(v) => std::env::set_var("CLAUDE_CODE_SYNC_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CODE_SYNC_CONFIG_DIR"),
        }
    }

    #[test]
    fn test_filter_with_attachments() {
        let filter = FilterConfig {
            exclude_attachments: true,
            ..Default::default()
        };

        // JSONL files should be included
        assert!(filter.should_include(Path::new("session.jsonl")));
        assert!(filter.should_include(Path::new("/path/to/session.jsonl")));

        // Non-JSONL files should be excluded
        assert!(!filter.should_include(Path::new("image.png")));
        assert!(!filter.should_include(Path::new("document.pdf")));
        assert!(!filter.should_include(Path::new("archive.zip")));
        assert!(!filter.should_include(Path::new("/path/to/file.jpg")));
    }

    #[test]
    fn test_filter_without_attachments_exclusion() {
        let filter = FilterConfig::default();
        // By default, exclude_attachments is false

        // All files should be included (subject to other filters)
        assert!(filter.should_include(Path::new("session.jsonl")));
        assert!(filter.should_include(Path::new("image.png")));
        assert!(filter.should_include(Path::new("document.pdf")));
    }
}
