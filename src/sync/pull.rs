use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::conflict::ConflictDetector;
use crate::filter::FilterConfig;
use crate::git::GitManager;
use crate::history::{
    ConversationSummary, OperationHistory, OperationRecord, OperationType, SyncOperation,
};
use crate::parser::ConversationSession;
use crate::report::{save_conflict_report, ConflictReport};
use crate::undo::Snapshot;

use super::discovery::{claude_projects_dir, discover_sessions, warn_large_files};
use super::state::SyncState;
use super::MAX_CONVERSATIONS_TO_DISPLAY;

/// Pull and merge history from sync repository
pub fn pull_history(fetch_remote: bool, branch: Option<&str>) -> Result<()> {
    println!("{}", "Pulling Claude Code history...".cyan().bold());

    let state = SyncState::load()?;
    let git_manager = GitManager::open(&state.sync_repo_path)?;
    let filter = FilterConfig::load()?;
    let claude_dir = claude_projects_dir()?;

    // Get the current branch name for operation record
    let branch_name = branch
        .map(|s| s.to_string())
        .or_else(|| git_manager.current_branch().ok())
        .unwrap_or_else(|| "main".to_string());

    // Fetch from remote if configured
    if fetch_remote && state.has_remote {
        println!("  {} from remote...", "Fetching".cyan());

        match git_manager.pull("origin", &branch_name) {
            Ok(_) => println!("  {} Pulled from origin/{}", "✓".green(), branch_name),
            Err(e) => {
                log::warn!("Failed to pull: {}", e);
                log::info!("Continuing with local sync repository state...");
            }
        }
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
    let remote_projects_dir = state.sync_repo_path.join("projects");
    println!("  {} remote sessions...", "Discovering".cyan());
    let remote_sessions = discover_sessions(&remote_projects_dir, &filter)?;
    println!(
        "  {} {} remote sessions",
        "Found".green(),
        remote_sessions.len()
    );

    // ============================================================================
    // SNAPSHOT CREATION: Create a snapshot before merging any changes
    // ============================================================================
    println!("  {} snapshot before merge...", "Creating".cyan());

    // Collect all local file paths that might be affected
    let local_file_paths: Vec<PathBuf> = local_sessions
        .iter()
        .map(|s| claude_dir.join(&s.file_path))
        .collect();

    // Check for large conversation files and warn users
    warn_large_files(&local_file_paths);

    // Create differential snapshot of current state (before any changes)
    // Note: Snapshot creation failure is fatal because we need to ensure users can
    // safely undo this pull operation if conflicts or issues occur. Without a snapshot,
    // there would be no way to restore the previous state.
    //
    // Using differential snapshots saves disk space by only storing changed files.
    let snapshot = Snapshot::create_differential(
        OperationType::Pull,
        local_file_paths.iter(),
        None, // No git manager needed for pull snapshots
    )
    .context("Failed to create snapshot before pull")?;

    // Save snapshot to disk
    let snapshot_path = snapshot
        .save_to_disk(None)
        .context("Failed to save snapshot to disk")?;

    println!(
        "  {} Snapshot created: {}",
        "✓".green(),
        snapshot_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| snapshot_path.display().to_string())
    );

    // ============================================================================
    // CONFLICT DETECTION AND RESOLUTION
    // ============================================================================
    println!("  {} conflicts...", "Detecting".cyan());
    let mut detector = ConflictDetector::new();
    detector.detect(&local_sessions, &remote_sessions);

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

        let local_map: HashMap<_, _> = local_sessions
            .iter()
            .map(|s| (s.session_id.clone(), s))
            .collect();

        let remote_map: HashMap<_, _> = remote_sessions
            .iter()
            .map(|s| (s.session_id.clone(), s))
            .collect();

        let mut smart_merge_success_count = 0;
        let mut smart_merge_failed_conflicts = Vec::new();

        for conflict in detector.conflicts_mut() {
            // Find local and remote sessions
            if let (Some(local_session), Some(remote_session)) = (
                local_map.get(&conflict.session_id),
                remote_map.get(&conflict.session_id),
            ) {
                // Try smart merge
                match conflict.try_smart_merge(local_session, remote_session) {
                    Ok(()) => {
                        smart_merge_success_count += 1;
                        // Write merged result to local file
                        if let crate::conflict::ConflictResolution::SmartMerge {
                            ref merged_entries,
                            ref stats,
                        } = conflict.resolution
                        {
                            // Create a new session with merged entries
                            let merged_session = ConversationSession {
                                session_id: conflict.session_id.clone(),
                                entries: merged_entries.clone(),
                                file_path: conflict.local_file.to_string_lossy().to_string(),
                            };

                            // Write merged session to local path
                            if let Err(e) = merged_session.write_to_file(&conflict.local_file) {
                                log::warn!(
                                    "Failed to write merged session {}: {}",
                                    conflict.session_id,
                                    e
                                );
                                smart_merge_failed_conflicts.push(conflict.clone());
                            } else {
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
                            session.write_to_file(&renamed_path)?;
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
                let session_file = Path::new(&s.file_path).file_name();
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
                    session.latest_timestamp(),
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
    let local_map: HashMap<_, _> = local_sessions
        .iter()
        .map(|s| (s.session_id.clone(), s))
        .collect();

    let mut merged_count = 0;
    let mut added_count = 0;
    let mut modified_count = 0;
    let mut unchanged_count = 0;

    for remote_session in &remote_sessions {
        // Skip if conflicts were detected
        if detector
            .conflicts()
            .iter()
            .any(|c| c.session_id == remote_session.session_id)
        {
            continue;
        }

        let relative_path = Path::new(&remote_session.file_path)
            .strip_prefix(&remote_projects_dir)
            .ok()
            .unwrap_or_else(|| Path::new(&remote_session.file_path));

        let dest_path = claude_dir.join(relative_path);

        // Determine operation type based on local state
        let operation = if let Some(local) = local_map.get(&remote_session.session_id) {
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
            remote_session.write_to_file(&dest_path)?;
            merged_count += 1;
        }

        // Track all sessions (including unchanged) in affected conversations
        let relative_path_str = relative_path.to_string_lossy().to_string();
        match ConversationSummary::new(
            remote_session.session_id.clone(),
            relative_path_str.clone(),
            remote_session.latest_timestamp(),
            remote_session.message_count(),
            operation,
        ) {
            Ok(summary) => affected_conversations.push(summary),
            Err(e) => log::warn!("Failed to create summary for {}: {}", relative_path_str, e),
        }
    }

    println!("  {} Merged {} sessions", "✓".green(), merged_count);

    // ============================================================================
    // CREATE AND SAVE OPERATION RECORD
    // ============================================================================
    let mut operation_record = OperationRecord::new(
        OperationType::Pull,
        Some(branch_name.clone()),
        affected_conversations.clone(),
    );

    // Attach the snapshot path to the operation record
    operation_record.snapshot_path = Some(snapshot_path);

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

    println!("\n{}", "Pull complete!".green().bold());

    // Clean up old snapshots automatically
    if let Err(e) = crate::undo::cleanup_old_snapshots(None, false) {
        log::warn!("Failed to cleanup old snapshots: {}", e);
    }

    Ok(())
}
