use anyhow::{Context, Result};
use colored::Colorize;
use inquire::Confirm;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::conflict::ConflictDetector;
use crate::filter::FilterConfig;
use crate::history::{
    ConversationSummary, OperationHistory, OperationRecord, OperationType, SyncOperation,
};
use crate::interactive_conflict;
use crate::later_timestamps::keep_later_timestamps;
use crate::parser::ConversationSession;
use crate::report::{save_conflict_report, ConflictReport};
use crate::scm;
use crate::undo::Snapshot;

use super::discovery::{claude_home_dir, claude_projects_dir, discover_sessions, warn_large_files};
use super::state::SyncState;
use super::MAX_CONVERSATIONS_TO_DISPLAY;

/// The file in `~/.claude/projects` a transcript in the sync repository
/// belongs to, or `None` when this machine has no project for it.
///
/// This is the identity a pull pairs on. Two transcripts that share an
/// interior session id — a session resumed in another project, and every
/// subagent of a session — are different conversations living in different
/// files, and only the file says which copy is which.
fn local_destination(
    remote_session: &ConversationSession,
    remote_projects_dir: &Path,
    claude_dir: &Path,
    filter: &FilterConfig,
) -> Option<PathBuf> {
    let remote_relative = remote_session
        .file_path
        .strip_prefix(remote_projects_dir)
        .ok()?;
    let (repo_project_dir, inside_project) =
        crate::project_map::split_project_path(remote_relative)?;
    let local_project_dir =
        crate::project_map::local_project_dir(filter, claude_dir, repo_project_dir)?;

    Some(local_project_dir.join(inside_project))
}

fn settle_conflict(
    file: &scm::ConflictedFile,
    can_ask: bool,
    filter: &FilterConfig,
) -> Result<scm::ConflictChoice> {
    let differing_only_in_dates = match (&file.local, &file.remote) {
        (Some(local), Some(remote)) => keep_later_timestamps(local, remote),
        _ => None,
    };
    if let Some(merged) = differing_only_in_dates {
        println!("  {} {}: kept the later dates", "✓".green(), file.path);
        return Ok(scm::ConflictChoice::WriteMerged(merged));
    }

    if !can_ask {
        return Ok(scm::ConflictChoice::AbortMerge);
    }
    crate::merge_tool::resolve_conflict(&filter.merge_tool, filter.prefer_merge_tool, file)
}

/// Whether the artifact counts say anything worth a line: a kept-local-only
/// pull has zeros everywhere else, and all-zero rows read as "artifacts did
/// nothing". ONE rule for the early counts line and the summary row.
fn artifact_counts_say_something(report: &crate::artifacts::engine::ArtifactReport) -> bool {
    report.total_added() > 0 || report.total_modified() > 0 || report.total_deleted() > 0
}

/// Run a pull. Returns the number of artifact files kept local — what
/// `sync` gates its push step on, straight from the apply that just ran
/// instead of a second plan (which would re-read everything and could
/// fail after the pull has already mutated the machine).
pub fn pull_history(
    fetch_remote: bool,
    branch: Option<&str>,
    interactive: bool,
    verbosity: crate::VerbosityLevel,
    cancel_is_error: bool,
) -> Result<crate::artifacts::engine::ArtifactReport> {
    use crate::VerbosityLevel;

    if verbosity != VerbosityLevel::Quiet {
        println!("{}", "Pulling Claude Code history...".cyan().bold());
    }

    let state = SyncState::load()?;
    let repo = scm::open(&state.sync_repo_path)?;
    let filter = FilterConfig::load()?;
    let claude_dir = claude_projects_dir()?;

    // Get the current branch name for operation record
    let branch_name = branch
        .map(|s| s.to_string())
        .or_else(|| repo.current_branch().ok())
        .unwrap_or_else(|| "main".to_string());

    super::commit_sync_attributes(repo.as_ref(), &state.sync_repo_path)?;

    // Fetch from remote if configured
    if fetch_remote && state.has_remote {
        println!("  {} from remote...", "Fetching".cyan());

        // Merging a stale sync repository into ~/.claude looks like a
        // successful pull and silently loses whatever the remote holds, so a
        // remote that cannot be reached or reconciled stops the pull instead.
        let resolve_conflict = |file: &scm::ConflictedFile| {
            let can_ask = interactive_conflict::is_interactive();
            settle_conflict(file, can_ask, &filter)
        };
        repo.pull("origin", &branch_name, &resolve_conflict)
            .context(
                "Nothing was merged into ~/.claude. Pull again in a terminal to choose \
                 a version of each file both machines changed, or fix the remote — or \
                 run `claude-code-sync pull --fetch-remote false` to merge only what is \
                 already in the local sync repository.",
            )?;
        println!("  {} Pulled from origin/{}", "✓".green(), branch_name);

        // A first pull into a repository with no commits of its own sets the
        // uncommitted rules aside; restore and commit them now.
        super::commit_sync_attributes(repo.as_ref(), &state.sync_repo_path)?;
    }

    // Discover local sessions
    println!("  {} local sessions...", "Discovering".cyan());
    let local_sessions = discover_sessions(&claude_dir, &filter)?;
    println!(
        "  {} {} local sessions",
        "Found".green(),
        local_sessions.len()
    );

    // Discover remote sessions
    let remote_projects_dir = state.sync_repo_path.join(&filter.sync_subdirectory);
    println!("  {} remote sessions...", "Discovering".cyan());
    let remote_sessions = discover_sessions(&remote_projects_dir, &filter)?;
    println!(
        "  {} {} remote sessions",
        "Found".green(),
        remote_sessions.len()
    );

    // ============================================================================
    // CONFLICT DETECTION (moved before snapshot for efficiency)
    // ============================================================================
    // Detect conflicts FIRST so we only backup files that will be modified
    if verbosity != VerbosityLevel::Quiet {
        println!("  {} conflicts...", "Detecting".cyan());
    }
    let local_by_path: HashMap<&Path, &ConversationSession> = local_sessions
        .iter()
        .map(|session| (session.file_path.as_path(), session))
        .collect();
    let paired: Vec<(&ConversationSession, &ConversationSession)> = remote_sessions
        .iter()
        .filter_map(|remote| {
            let destination =
                local_destination(remote, &remote_projects_dir, &claude_dir, &filter)?;
            let local = local_by_path.get(destination.as_path())?;
            Some((*local, remote))
        })
        .collect();

    let mut detector = ConflictDetector::new();
    detector.detect(&paired);

    // ============================================================================
    // ARTIFACT PULL PLAN (read-only, so the snapshot below can cover it)
    // ============================================================================
    let mut artifact_plan =
        crate::artifacts::engine::plan_pull(&claude_home_dir()?, &state.sync_repo_path, &filter)?;

    // ============================================================================
    // SNAPSHOT CREATION: Only backup files that will actually change
    // ============================================================================
    // Optimization: Only backup local files that have conflicts and will be merged,
    // plus artifact files this pull will overwrite. Files that are new (remote-only)
    // or unchanged don't need backup — created artifact paths are recorded as
    // deleted_files so undo removes them again.
    // This reduces snapshot size from potentially gigabytes to typically <1MB.
    // Only snapshot when the apply will actually change machine state:
    // conflicted conversations, artifact writes, or shared-record writes
    // (a pull whose only effect is creating a record still needs its
    // undo; a fully no-op pull must not churn a snapshot). Kept-local
    // files count as writes because an interactive apply may take the
    // repository copy over the local edit.
    // The same effective flag apply_pull gates prompts on: `-i` without a
    // TTY (cron, CI) cannot write kept-local files, and must not mint a
    // throwaway snapshot on every run for an unresolved edit.
    let prompts_possible = interactive && crate::interactive_conflict::is_interactive();
    // The snapshot object stays at hand after the apply: the apply learns
    // which creates actually EXECUTED, and the snapshot's undo-delete
    // list must be narrowed to them before the record is written.
    let mut created_snapshot: Option<(crate::undo::Snapshot, PathBuf)> = None;
    // Set once the snapshot is taken (and left None when nothing will
    // touch the machine); the narrowing below re-saves the snapshot
    // file in place — a failed narrowing pins the snapshot and warns
    // rather than dropping the undo hint.
    let snapshot_path: Option<PathBuf>;
    let mut session_unsnapshotted: Vec<std::path::PathBuf> = Vec::new();
    // Set when a shared record was unreadable at snapshot time and is
    // therefore not carried: if the apply still moves its keys (the file
    // became readable again), the undo cannot restore them and the
    // restored files will read as locally edited — the user needs the
    // recovery path, not just the snapshot-time note.
    let mut record_not_carried = false;
    if detector.has_conflicts() || artifact_plan.changes_machine_state(prompts_possible) {
        let mut files_to_snapshot: Vec<PathBuf> = detector
            .conflicts()
            .iter()
            .map(|c| c.local_file.clone())
            .collect();
        files_to_snapshot.extend(artifact_plan.paths_to_snapshot(prompts_possible));
        // The fail-open record contract: an unreadable shared record (a
        // root-owned or 0o000 file, say) reads as EMPTY everywhere else —
        // letting it abort the whole pull at snapshot time would make
        // every sync on that machine fatal over a file the records layer
        // itself ignores. Drop it from the snapshot instead: the undo
        // then treats the record as not carried and says so.
        //
        // Only the EXACT record paths this plan can write — the SAME
        // collapsed predicates the snapshot gate and the concurrent-sync
        // warnings use (a record is either carried or created, and each
        // pair collapses to one predicate): matching by name alone would
        // also drop an ordinary artifact that happens to share a
        // record's file name (the decoy case).
        let claude_dir = claude_home_dir()?;
        let declared_record_paths: Vec<PathBuf> = [
            artifact_plan
                .can_write_bases(prompts_possible)
                .then(|| crate::artifacts::bases::record_path(&claude_dir)),
            artifact_plan
                .rewrites_tracked
                .then(|| crate::artifacts::tracked::record_path(&claude_dir)),
        ]
        .into_iter()
        .flatten()
        .collect();
        files_to_snapshot.retain(|path| {
            if !declared_record_paths.contains(path) {
                return true;
            }
            // Open, not read: the bytes are discarded here (the snapshot
            // reads the file itself moments later) — only the
            // absent/unreadable/readable triage matters.
            match std::fs::File::open(path) {
                Ok(_) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => {
                    // log::warn reaches the log file at any verbosity; the
                    // console line honors Quiet like its neighbors.
                    record_not_carried = true;
                    log::warn!(
                        "unreadable shared record {} — not snapshotted; undo pull cannot \
                         restore its entries",
                        path.display()
                    );
                    if verbosity != VerbosityLevel::Quiet {
                        println!(
                            "  {} unreadable shared record {} — not snapshotted; {}",
                            "⚠".yellow(),
                            path.display(),
                            "undo pull cannot restore its entries".yellow()
                        );
                    }
                    false
                }
            }
        });

        println!(
            "  {} snapshot of {} files to be modified...",
            "Creating".cyan(),
            files_to_snapshot.len()
        );

        // Check for large conversation files and warn users
        warn_large_files(&files_to_snapshot);

        // Create snapshot of ONLY files this pull will modify
        let mut snapshot = Snapshot::create(
            OperationType::Pull,
            files_to_snapshot.iter(),
            None, // No git manager needed for pull snapshots
        )
        .context("Failed to create snapshot before pull")?;

        // Artifact files the pull will create: undo deletes them again.
        snapshot.deleted_files = artifact_plan.created_paths();

        // The shared records (bases, tracked) are never deleted or
        // restored wholesale — other repositories may hold newer state in
        // the same files. Declare which ones this snapshot carries (or may
        // create); `undo pull` restores just this repository's entry.
        snapshot.attach_record_bookkeeping(&artifact_plan, &claude_dir, prompts_possible);

        // Save snapshot to disk
        let path = snapshot
            .save_to_disk(None)
            .context("Failed to save snapshot to disk")?;

        if verbosity != VerbosityLevel::Quiet {
            println!(
                "  {} Snapshot created: {} ({} files)",
                "✓".green(),
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string()),
                files_to_snapshot.len()
            );
        }

        // Whatever the snapshot could not read is now on the plan: the
        // apply will refuse to modify those files — nothing is modified
        // without a backup, so a permission error at snapshot time that
        // recovers before the apply cannot become an unbackupable
        // modification.
        artifact_plan.unsnapshotted = snapshot
            .unreadable_files
            .iter()
            .map(std::path::PathBuf::from)
            .collect();
        // Session-merge needs the same refusal the artifact apply gets:
        // a conversation file whose backup failed is not rewritten by the
        // merge either.
        session_unsnapshotted = snapshot
            .unreadable_files
            .iter()
            .map(std::path::PathBuf::from)
            .collect();
        created_snapshot = Some((snapshot, path.clone()));
        snapshot_path = Some(path);
    } else {
        println!(
            "  {} Nothing to snapshot - skipping (no files change on disk)",
            "✓".green()
        );
        snapshot_path = None;
    };

    // ============================================================================
    // SHOW SUMMARY AND INTERACTIVE CONFIRMATION
    // ============================================================================
    if verbosity != VerbosityLevel::Quiet {
        println!();
        println!("{}", "Pull Summary:".bold().cyan());
        println!("  {} Local sessions: {}", "•".cyan(), local_sessions.len());
        println!(
            "  {} Remote sessions: {}",
            "•".cyan(),
            remote_sessions.len()
        );
        println!();
    }

    // Show detailed file list in verbose mode
    if verbosity == VerbosityLevel::Verbose {
        println!("{}", "Remote sessions to be pulled:".bold());
        for (idx, session) in remote_sessions.iter().enumerate().take(20) {
            let relative_path = session
                .file_path
                .strip_prefix(&remote_projects_dir)
                .unwrap_or(&session.file_path);

            println!(
                "  {}. {} ({} messages)",
                idx + 1,
                relative_path.display(),
                session.message_count()
            );
        }
        if remote_sessions.len() > 20 {
            println!("  ... and {} more", remote_sessions.len() - 20);
        }
        println!();
    }

    // Interactive confirmation
    if interactive && interactive_conflict::is_interactive() {
        let confirm = Confirm::new("Pull?")
            .with_default(true)
            .prompt()
            .context("Failed to get confirmation")?;

        if !confirm {
            println!("\n{}", "Pull cancelled.".yellow());
            // Under `sync`, a cancelled pull must abort the whole command:
            // returning "nothing kept" would send the gate straight into
            // a full push that publishes local edits over the
            // repository's newer versions. A bare pull stays a friendly
            // Ok — cancelling is a normal outcome there.
            // The snapshot was written before the confirmation: it is
            // owned by no operation record now (the pull never applied,
            // no record is written) — delete it instead of leaving an
            // orphan for age/count cleanup to find.
            if let Some(orphan) = snapshot_path.as_ref() {
                let _ = std::fs::remove_file(orphan);
            }
            if cancel_is_error {
                return Err(anyhow::anyhow!("Pull cancelled"));
            }
            return Ok(crate::artifacts::engine::ArtifactReport::default());
        }
    }

    // ============================================================================
    // CONFLICT RESOLUTION (detection already done above)
    // ============================================================================
    // Track affected conversations for operation record
    let mut affected_conversations: Vec<ConversationSummary> = Vec::new();

    if detector.has_conflicts() {
        println!(
            "  {} {} conflicts detected",
            "!".yellow(),
            detector.conflict_count()
        );

        // ============================================================================
        // ATTEMPT SMART MERGE FIRST
        // ============================================================================
        println!("  {} smart merge...", "Attempting".cyan());

        let remote_by_path: HashMap<&Path, &ConversationSession> = remote_sessions
            .iter()
            .map(|session| (session.file_path.as_path(), session))
            .collect();

        let mut smart_merge_success_count = 0;
        let mut smart_merge_failed_conflicts = Vec::new();

        for conflict in detector.conflicts_mut() {
            // No backup, no merge (see `session_unsnapshotted`): the
            // conflict is left unresolved — reported as failed — instead
            // of overwriting a file nothing could restore.
            if session_unsnapshotted.contains(&conflict.local_file) {
                // Not added to the failed-conflicts list either: the
                // interactive resolver would rewrite the same unbacked
                // file. The conflict stays detected-but-unresolved, said
                // in the log.
                log::warn!(
                    "Leaving conflict {} unresolved (no pre-pull backup could be taken)",
                    conflict.session_id
                );
                continue;
            }
            // The two transcripts this conflict is between, by their own paths:
            // an interior session id is shared by every subagent of a session,
            // and by a session resumed in another project.
            if let (Some(local_session), Some(remote_session)) = (
                local_by_path.get(conflict.local_file.as_path()),
                remote_by_path.get(conflict.remote_file.as_path()),
            ) {
                // Try smart merge
                match conflict.smart_merge_into_local_file(local_session, remote_session) {
                    Ok(()) => {
                        smart_merge_success_count += 1;
                        if let crate::conflict::ConflictResolution::SmartMerge { ref stats } =
                            conflict.resolution
                        {
                            println!(
                                "  {} Smart merged {} ({} local + {} remote = {} total, {} branches)",
                                "✓".green(),
                                conflict.session_id,
                                stats.local_messages,
                                stats.remote_messages,
                                stats.merged_messages,
                                stats.branches_detected
                            );
                        }
                    }
                    Err(e) => {
                        log::warn!("Smart merge failed for {}: {}", conflict.session_id, e);
                        log::info!("Falling back to manual resolution...");
                        smart_merge_failed_conflicts.push(conflict.clone());
                    }
                }
            }
        }

        println!(
            "  {} Successfully smart merged {}/{} conflicts",
            "✓".green(),
            smart_merge_success_count,
            detector.conflict_count()
        );

        // If some smart merges failed, handle them with interactive/keep-both resolution
        let renames = if !smart_merge_failed_conflicts.is_empty() {
            println!(
                "  {} {} conflicts require manual resolution",
                "!".yellow(),
                smart_merge_failed_conflicts.len()
            );

            // Check if we can run interactively
            let use_interactive = crate::interactive_conflict::is_interactive();

            if use_interactive {
                // Interactive conflict resolution for failed merges
                println!(
                    "\n{} Running in interactive mode for remaining conflicts",
                    "→".cyan()
                );

                let resolution_result = crate::interactive_conflict::resolve_conflicts_interactive(
                    &mut smart_merge_failed_conflicts,
                )?;

                // Apply the resolutions
                let renames = crate::interactive_conflict::apply_resolutions(
                    &resolution_result,
                    &remote_sessions,
                    &claude_dir,
                    &remote_projects_dir,
                )?;

                // Save conflict report
                let report = ConflictReport::from_conflicts(detector.conflicts());
                save_conflict_report(&report)?;

                renames
            } else {
                // Non-interactive mode: use "keep both" strategy for failed merges
                println!(
                    "\n{} Using automatic conflict resolution (keep both versions)",
                    "→".cyan()
                );

                let mut renames = Vec::new();

                println!("\n{}", "Conflict Resolution:".yellow().bold());
                for conflict in &smart_merge_failed_conflicts {
                    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                    let conflict_suffix = format!("conflict-{timestamp}");

                    if let Ok(renamed_path) = conflict.clone().resolve_keep_both(&conflict_suffix) {
                        let relative_renamed = renamed_path
                            .strip_prefix(&claude_dir)
                            .unwrap_or(&renamed_path);
                        println!(
                            "  {} remote version saved as: {}",
                            "→".yellow(),
                            relative_renamed.display().to_string().cyan()
                        );

                        // Find and write the remote session
                        if let Some(session) = remote_sessions
                            .iter()
                            .find(|s| s.session_id == conflict.session_id)
                        {
                            session.copy_to(&renamed_path)?;
                        }

                        renames.push((conflict.remote_file.clone(), renamed_path));
                    }
                }

                // Save conflict report
                let report = ConflictReport::from_conflicts(detector.conflicts());
                save_conflict_report(&report)?;

                renames
            }
        } else {
            // All conflicts resolved via smart merge
            Vec::new()
        };

        // Track all conflicts in affected conversations
        for (_original_path, renamed_path) in &renames {
            let relative_path = renamed_path
                .strip_prefix(&claude_dir)
                .unwrap_or(renamed_path)
                .to_string_lossy()
                .to_string();

            // Find the session ID from the renamed path
            if let Some(session) = remote_sessions.iter().find(|s| {
                let session_file = s.file_path.file_name();
                let renamed_file = renamed_path.file_name();
                // Try to match based on session ID in filename
                session_file
                    .and_then(|f| f.to_str())
                    .and_then(|name| name.split('-').next())
                    == renamed_file
                        .and_then(|f| f.to_str())
                        .and_then(|name| name.split('-').next())
            }) {
                match ConversationSummary::new(
                    session.session_id.clone(),
                    relative_path.clone(),
                    session.latest_timestamp().map(str::to_string),
                    session.message_count(),
                    SyncOperation::Conflict,
                ) {
                    Ok(summary) => affected_conversations.push(summary),
                    Err(e) => log::warn!(
                        "Failed to create summary for conflict {}: {}",
                        relative_path,
                        e
                    ),
                }
            }
        }

        println!(
            "\n{} View details with: claude-code-sync report",
            "Hint:".cyan()
        );
    } else {
        println!("  {} No conflicts detected", "✓".green());
    }

    // ============================================================================
    // MERGE NON-CONFLICTING SESSIONS
    // ============================================================================
    println!("  {} non-conflicting sessions...", "Merging".cyan());
    let mut merged_count = 0;
    let mut added_count = 0;
    let mut modified_count = 0;
    let mut unchanged_count = 0;
    let mut skipped_no_local_match = 0;
    let mut skipped_by_project = crate::project_map::SkippedByProject::new();

    for remote_session in &remote_sessions {
        let remote_relative = remote_session
            .file_path
            .strip_prefix(&remote_projects_dir)
            .unwrap_or(&remote_session.file_path);

        let destination =
            local_destination(remote_session, &remote_projects_dir, &claude_dir, &filter);
        let Some(dest_path) = destination else {
            match crate::project_map::split_project_path(remote_relative) {
                Some((repo_project_dir, _)) => {
                    skipped_no_local_match += 1;
                    skipped_by_project
                        .entry(repo_project_dir.to_string())
                        .or_default()
                        .push(remote_session.file_path.clone());
                }
                None => log::warn!(
                    "Skipping {} (not a transcript inside a project directory)",
                    remote_session.file_path.display()
                ),
            }
            continue;
        };

        // A conflicting transcript is merged above, not overwritten here.
        if detector
            .conflicts()
            .iter()
            .any(|conflict| conflict.local_file == dest_path)
        {
            continue;
        }

        let relative_path_for_tracking = dest_path
            .strip_prefix(&claude_dir)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| remote_relative.to_path_buf());

        // Determine operation type based on local state
        let operation = if let Some(local) = local_by_path.get(dest_path.as_path()) {
            if local.content_hash() == remote_session.content_hash() {
                unchanged_count += 1;
                SyncOperation::Unchanged
            } else {
                modified_count += 1;
                SyncOperation::Modified
            }
        } else {
            added_count += 1;
            SyncOperation::Added
        };

        // Copy file if it's not unchanged
        if operation != SyncOperation::Unchanged {
            remote_session.copy_to(&dest_path)?;
            merged_count += 1;
        }

        // Track all sessions (including unchanged) in affected conversations
        let relative_path_str = relative_path_for_tracking.to_string_lossy().to_string();
        match ConversationSummary::new(
            remote_session.session_id.clone(),
            relative_path_str.clone(),
            remote_session.latest_timestamp().map(str::to_string),
            remote_session.message_count(),
            operation,
        ) {
            Ok(summary) => affected_conversations.push(summary),
            Err(e) => log::warn!("Failed to create summary for {}: {}", relative_path_str, e),
        }
    }

    println!("  {} Merged {} sessions", "✓".green(), merged_count);

    crate::project_map::merge_skipped(&mut skipped_by_project, &artifact_plan.unmapped_projects);
    for line in crate::project_map::skipped_project_warnings(
        &skipped_by_project,
        filter.warn_each_skipped_file,
    ) {
        log::warn!("{line}");
    }

    // ============================================================================
    // APPLY ARTIFACT PULL PLAN (remote wins; snapshot already covers changes)
    // ============================================================================
    let artifact_report = crate::artifacts::engine::apply_pull(&artifact_plan, interactive)?;
    // Undo deletes only what the apply actually CREATED: a user-written
    // file that appeared mid-pull is deliberately skipped (a push
    // publishes it) and must survive the undo.
    if let Some((snapshot, path)) = created_snapshot.as_mut() {
        let executed: Vec<String> = artifact_report
            .created_abs_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        // Only a SKIPPED create needs the rewrite (it spares the skipped
        // path from the undo's delete list); an unchanged list skips the
        // full re-serialize of what can be megabytes entirely.
        if executed != snapshot.deleted_files {
            snapshot.deleted_files = executed;
            if let Err(e) = snapshot.save_to_disk(path.parent()) {
                // The re-save failed, so the on-disk snapshot keeps its
                // PLANNED delete list — undo could then delete a file the
                // pull never wrote (a mid-pull create). Deleting the file
                // would discard the only backups of files the pull DID
                // overwrite — a worse, unconditional loss. Keep it, pin
                // it in memory if the re-save allows, and say exactly
                // what to check before undoing.
                let reason = "the snapshot could not be updated; undo pull would restore \
                              the overwritten files but its delete list was NOT narrowed — \
                              check it for files this pull never wrote before undoing";
                log::warn!("Snapshot undo-delete list not narrowed: {e}; {reason}");
                if verbosity != VerbosityLevel::Quiet {
                    println!("  {} {}", "⚠".yellow(), reason.yellow());
                }
                // Pin it so the regular cleanup spares the sole backups
                // — the re-save may have failed transiently, so one
                // pinned re-write is worth attempting; if even that
                // fails, the warning above is all the user gets.
                snapshot.pinned = true;
                if let Err(pin_err) = snapshot.save_to_disk(path.parent()) {
                    log::warn!(
                        "Snapshot could not be pinned either: {pin_err} — it may be \
                         deleted by the regular cleanup"
                    );
                }
            }
        }
    }
    // The counts line only when it says something: a kept-local-only
    // pull has zeros everywhere else, and "0 created, 0 overwritten,
    // 0 deleted" reads as "artifacts did nothing". The kept-local hint
    // itself must survive every outcome — it IS the pull's result.
    if !artifact_plan.is_empty() && artifact_counts_say_something(&artifact_report) {
        println!(
            "  {} Artifacts: {} created, {} overwritten locally, {} deleted locally",
            "✓".green(),
            artifact_report.total_added(),
            artifact_report.total_modified(),
            artifact_report.total_deleted()
        );
    }
    if verbosity != VerbosityLevel::Quiet && artifact_report.total_kept_local() > 0 {
        let kept = artifact_report.total_kept_local();
        let clean = artifact_report.kept_local_clean;
        let plural = if kept == 1 { "" } else { "s" };
        if clean == kept {
            // Every keep protected nothing — a write that failed, an edit
            // reverted while the pull ran. `sync`'s push skips these files
            // per-file, so the older local bytes cannot overwrite a newer
            // repository copy — but a BARE `push` has no skip set and
            // would republish the older local bytes over the repo's newer
            // version. Say both facts.
            println!(
                "  {} {} file{plural} held local with no edit to publish — \
                 sync holds them back; a bare push would republish their older bytes",
                "✋".yellow(),
                kept,
            );
        } else if clean > 0 {
            // Mixed: the dirty subset has real local edits to
            // publish; the clean subset matches the base. No command
            // selectively publishes one subset: a bare `push`
            // publishes the edits AND republishes the clean files'
            // older bytes over the repo's newer versions. Advise
            // pulling again first so the transient ones settle, then
            // pushing.
            println!(
                "  {} {} file{plural} kept local: {} edited, {} transient — \
                 pull again to settle the transient ones, then push to publish the edits",
                "✋".yellow(),
                kept,
                kept - clean,
                clean,
            );
        } else {
            println!(
                "  {} {} file{plural} kept local: edited here since the last sync — push to publish",
                "✋".yellow(),
                kept,
            );
        }
    }
    // KeptLocalDelete: the repo's deletion was honored (the local
    // edit is gone) — the standalone `pull` is the place to mention
    // it because the next `sync` would have nothing to publish for
    // these files. Skipping this block when the previous kept-local
    // block already ran would suppress the deletion notice.
    let kept_local_deletes = artifact_report.total_kept_local_deletes();
    if verbosity != VerbosityLevel::Quiet && kept_local_deletes > 0 {
        let plural = if kept_local_deletes == 1 { "" } else { "s" };
        println!(
            "  {} {} file{plural} removed locally: the repo's deletion was honored — the next pull will prune the record",
            "🗑".yellow(),
            kept_local_deletes,
        );
    }
    // Only when THIS pull has a snapshot: a snapshotless record (the
    // demotion above) is skipped by undo, which would silently target
    // the previous pull and revert changes the user did not ask about.
    if (artifact_report.total_modified() > 0 || artifact_report.total_deleted() > 0)
        && snapshot_path.is_some()
    {
        println!("    {}", "Undo with: claude-code-sync undo pull".dimmed());
    }
    // The snapshot gate predicts at plan time; the apply re-decides from a
    // fresh load. When a concurrent same-repo sync made the apply move
    // record keys the snapshot never carried, this pull's undo cannot
    // restore that record's pre-pull state — say so instead of leaving a
    // silently weaker undo.
    // `prompts_possible`, not `interactive`: the snapshot gate and the
    // declarations used it, and `pull -i` without a TTY mints nothing —
    // evaluating the kept-local term here would suppress the warning
    // exactly when it applies.
    // ONE shape for both records: the apply moved keys the snapshot's
    // declarations never carried (a concurrent sync's commits), so this
    // pull's undo cannot restore that record's pre-pull state. The bases
    // gate keeps the interactive term (the snapshot gate used
    // prompts_possible, and `pull -i` without a TTY mints nothing);
    // tracked writes are fully predictable, so rewrites_tracked alone.
    let warn_concurrent = |keys_moved: &[String], carried: bool, record: &str| {
        if verbosity != VerbosityLevel::Quiet && !keys_moved.is_empty() && !carried {
            println!(
                "  {} a concurrent sync changed the artifact {record} record during this pull; {}",
                "⚠".yellow(),
                "undo pull cannot restore its pre-pull state".yellow()
            );
        }
    };
    if record_not_carried
        && (!artifact_report.bases_keys_written.is_empty()
            || !artifact_report.tracked_keys_written.is_empty())
    {
        // The record the apply DID move is the one the snapshot cannot
        // restore: after an undo, the restored files read as locally
        // edited (their bases still say post-pull). One push re-records
        // the true bases and clears it — say so instead of leaving the
        // warned-but-destructive path to be discovered.
        println!(
            "  {} a shared record was unreadable this pull and the apply still updated it; \
             after an undo, run {} to re-record the bases (restored files would otherwise \
             read as locally edited)",
            "⚠".yellow(),
            "claude-code-sync push".cyan()
        );
    }
    warn_concurrent(
        &artifact_report.bases_keys_written,
        artifact_plan.can_write_bases(prompts_possible),
        "base",
    );
    warn_concurrent(
        &artifact_report.tracked_keys_written,
        artifact_plan.rewrites_tracked,
        "tracked",
    );
    // ============================================================================
    // CREATE AND SAVE OPERATION RECORD
    // ============================================================================
    let mut operation_record = OperationRecord::new(
        OperationType::Pull,
        Some(branch_name.clone()),
        affected_conversations.clone(),
    );

    // Attach the snapshot path to the operation record (only if we created one)
    operation_record.snapshot_path = snapshot_path;
    operation_record.artifact_counts = artifact_report.counts.clone();
    // Exactly the keys the apply moved in each shared record: the undo
    // restores those and only those — a superset would revert entries a
    // later push legitimately recorded. None on older records reads as
    // the snapshot's full declared set (the conservative superset).
    operation_record.bases_keys_written = Some(artifact_report.bases_keys_written.clone());
    operation_record.tracked_keys_written = Some(artifact_report.tracked_keys_written.clone());
    // Undo scopes its artifact-record surgery to this repository alone.
    operation_record.repo_path = Some(state.sync_repo_path.clone());

    // Load operation history and add this operation
    let mut history = match OperationHistory::load() {
        Ok(h) => h,
        Err(e) => {
            log::warn!("Failed to load operation history: {}", e);
            log::info!("Creating new history...");
            OperationHistory::default()
        }
    };

    if let Err(e) = history.add_operation(operation_record) {
        log::warn!("Failed to save operation to history: {}", e);
        log::info!("Pull completed successfully, but history was not updated.");
    }

    // ============================================================================
    // DISPLAY SUMMARY TO USER
    // ============================================================================
    println!("\n{}", "=== Pull Summary ===".bold().cyan());

    // Show operation statistics
    let conflict_count = detector.conflict_count();
    let stats_msg = format!(
        "  {} Added    {} Modified    {} Conflicts    {} Unchanged",
        format!("{added_count}").green(),
        format!("{modified_count}").cyan(),
        format!("{conflict_count}").yellow(),
        format!("{unchanged_count}").dimmed(),
    );
    println!("{stats_msg}");
    if skipped_no_local_match > 0 {
        println!(
            "  {} Skipped sessions (no local match): {}",
            "!".yellow(),
            skipped_no_local_match
        );
    }
    // Same zero-noise rule as the earlier counts line (plus unchanged,
    // which the summary row carries): a kept-local-only pull prints its
    // KEEP hint, not an all-zeros summary row.
    if artifact_counts_say_something(&artifact_report) || artifact_plan.unchanged > 0 {
        println!(
            "  {} Artifacts: {} added, {} modified, {} deleted, {} unchanged",
            "•".cyan(),
            artifact_report.total_added(),
            artifact_report.total_modified(),
            artifact_report.total_deleted(),
            artifact_plan.unchanged
        );
    }
    println!();

    // Group conversations by project (top-level directory)
    let mut by_project: HashMap<String, Vec<&ConversationSummary>> = HashMap::new();
    for conv in &affected_conversations {
        // Skip unchanged conversations in detailed output
        if conv.operation == SyncOperation::Unchanged {
            continue;
        }

        let project = conv
            .project_path
            .split('/')
            .next()
            .unwrap_or("unknown")
            .to_string();
        by_project.entry(project).or_default().push(conv);
    }

    // Display conversations grouped by project
    if !by_project.is_empty() {
        println!("{}", "Affected Conversations:".bold());

        let mut projects: Vec<_> = by_project.keys().collect();
        projects.sort();

        for project in projects {
            let conversations = &by_project[project];
            println!("\n  {} {}/", "Project:".bold(), project.cyan());

            for conv in conversations.iter().take(MAX_CONVERSATIONS_TO_DISPLAY) {
                let operation_str = match conv.operation {
                    SyncOperation::Added => "ADD".green(),
                    SyncOperation::Modified => "MOD".cyan(),
                    SyncOperation::Conflict => "CONFLICT".yellow(),
                    SyncOperation::Unchanged => "---".dimmed(),
                };

                let timestamp_str = conv
                    .timestamp
                    .as_ref()
                    .and_then(|t| {
                        // Extract just the date portion for compact display
                        t.split('T').next()
                    })
                    .unwrap_or("unknown");

                println!(
                    "    {} {} ({}msg, {})",
                    operation_str,
                    conv.project_path,
                    conv.message_count,
                    timestamp_str.dimmed()
                );
            }

            if conversations.len() > MAX_CONVERSATIONS_TO_DISPLAY {
                println!(
                    "    {} ... and {} more conversations",
                    "...".dimmed(),
                    conversations.len() - MAX_CONVERSATIONS_TO_DISPLAY
                );
            }
        }
    }

    super::print_artifact_changes(&artifact_report);

    println!("\n{}", "Pull complete!".green().bold());

    // Clean up old snapshots automatically
    if let Err(e) = crate::undo::cleanup_old_snapshots(None, false) {
        log::warn!("Failed to cleanup old snapshots: {}", e);
    }

    Ok(artifact_report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflicted(local: Option<&str>, remote: Option<&str>) -> scm::ConflictedFile {
        scm::ConflictedFile {
            path: "artifacts/plugins/known_marketplaces.json".to_string(),
            base: None,
            local: local.map(|text| text.as_bytes().to_vec()),
            remote: remote.map(|text| text.as_bytes().to_vec()),
        }
    }

    #[test]
    fn a_conflict_settles_without_asking_only_when_the_dates_alone_differ() {
        let filter = FilterConfig::default();
        let cases = [
            (
                "dates alone differ",
                conflicted(
                    Some("\"lastUpdated\": \"2026-10-01T06:00:01.741Z\""),
                    Some("\"lastUpdated\": \"2026-10-01T06:50:50.567Z\""),
                ),
                scm::ConflictChoice::WriteMerged(
                    b"\"lastUpdated\": \"2026-10-01T06:50:50.567Z\"".to_vec(),
                ),
            ),
            (
                "content differs too",
                conflicted(
                    Some("a 2026-10-01T06:00:01Z"),
                    Some("b 2026-10-01T06:00:02Z"),
                ),
                scm::ConflictChoice::AbortMerge,
            ),
            (
                "deleted on one side",
                conflicted(Some("a"), None),
                scm::ConflictChoice::AbortMerge,
            ),
        ];

        for (name, file, expected) in cases {
            let choice = settle_conflict(&file, false, &filter).unwrap();
            assert_eq!(choice, expected, "{name}");
        }
    }
}
