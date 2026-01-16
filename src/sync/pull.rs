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
use crate::parser::ConversationSession;
use crate::report::{save_conflict_report, ConflictReport};
use crate::scm;
use crate::undo::Snapshot;

use super::discovery::{claude_projects_dir, discover_sessions, find_local_project_by_name, warn_large_files};
use super::state::SyncState;
use super::MAX_CONVERSATIONS_TO_DISPLAY;

/// Pull and merge history from sync repository
pub fn pull_history(
    fetch_remote: bool,
    branch: Option<&str>,
    interactive: bool,
    verbosity: crate::VerbosityLevel,
) -> Result<()> {
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

    // Fetch from remote if configured
    if fetch_remote && state.has_remote {
        println!("  {} from remote...", "Fetching".cyan());

        match repo.pull("origin", &branch_name) {
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
    let mut detector = ConflictDetector::new();
    detector.detect(&local_sessions, &remote_sessions);

    // ============================================================================
    // SNAPSHOT CREATION: Only backup files that have conflicts
    // ============================================================================
    // Optimization: Only backup local files that have conflicts and will be merged.
    // Files that are new (remote-only) or unchanged don't need backup.
    // This reduces snapshot size from potentially gigabytes to typically <1MB.
    let snapshot_path = if detector.has_conflicts() {
        println!("  {} snapshot of {} conflicting files...", "Creating".cyan(), detector.conflict_count());

        // Only collect paths for files that have conflicts
        let conflicting_file_paths: Vec<PathBuf> = detector
            .conflicts()
            .iter()
            .map(|c| c.local_file.clone())
            .collect();

        // Check for large conversation files and warn users
        warn_large_files(&conflicting_file_paths);

        // Create snapshot of ONLY conflicting files
        let snapshot = Snapshot::create(
            OperationType::Pull,
            conflicting_file_paths.iter(),
            None, // No git manager needed for pull snapshots
        )
        .context("Failed to create snapshot before pull")?;

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
                conflicting_file_paths.len()
            );
        }

        Some(path)
    } else {
        println!("  {} No conflicts - skipping snapshot", "✓".green());
        None
    };

    // ============================================================================
    // SHOW SUMMARY AND INTERACTIVE CONFIRMATION
    // ============================================================================
    if verbosity != VerbosityLevel::Quiet {
        println!();
        println!("{}", "Pull Summary:".bold().cyan());
        println!("  {} Local sessions: {}", "•".cyan(), local_sessions.len());
        println!("  {} Remote sessions: {}", "•".cyan(), remote_sessions.len());
        println!();
    }

    // Show detailed file list in verbose mode
    if verbosity == VerbosityLevel::Verbose {
        println!("{}", "Remote sessions to be pulled:".bold());
        for (idx, session) in remote_sessions.iter().enumerate().take(20) {
            let relative_path = Path::new(&session.file_path)
                .strip_prefix(&remote_projects_dir)
                .unwrap_or(Path::new(&session.file_path));

            println!("  {}. {} ({} messages)", idx + 1, relative_path.display(), session.message_count());
        }
        if remote_sessions.len() > 20 {
            println!("  ... and {} more", remote_sessions.len() - 20);
        }
        println!();
    }

    // Interactive confirmation
    if interactive && interactive_conflict::is_interactive() {
        let confirm = Confirm::new("Do you want to proceed with pulling and merging these changes?")
            .with_default(true)
            .with_help_message("This will merge remote sessions into your local Claude Code history")
            .prompt()
            .context("Failed to get confirmation")?;

        if !confirm {
            println!("\n{}", "Pull cancelled.".yellow());
            return Ok(());
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
    let mut skipped_no_local_match = 0;

    for remote_session in &remote_sessions {
        // Skip if conflicts were detected
        if detector
            .conflicts()
            .iter()
            .any(|c| c.session_id == remote_session.session_id)
        {
            continue;
        }

        let (dest_path, relative_path_for_tracking) = if filter.use_project_name_only {
            // Extract project name and session filename from remote path
            let remote_relative = Path::new(&remote_session.file_path)
                .strip_prefix(&remote_projects_dir)
                .ok()
                .unwrap_or_else(|| Path::new(&remote_session.file_path));

            // Get the project name from the remote path structure
            let project_name = remote_relative
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .unwrap_or("unknown");

            // Find matching local Claude project directory
            if let Some(local_project_dir) = find_local_project_by_name(&claude_dir, project_name) {
                // Get just the session filename
                if let Some(filename) = remote_relative.file_name() {
                    let dest = local_project_dir.join(filename);
                    // Compute relative path for tracking from the destination
                    let tracking_path = dest
                        .strip_prefix(&claude_dir)
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|_| remote_relative.to_path_buf());
                    (dest, tracking_path)
                } else {
                    log::warn!("Could not extract filename from remote path: {:?}", remote_relative);
                    skipped_no_local_match += 1;
                    continue; // Skip this session
                }
            } else {
                log::warn!(
                    "No matching local project found for '{}'. \
                     Open the project with Claude Code locally first, or disable use_project_name_only.",
                    project_name
                );
                skipped_no_local_match += 1;
                continue; // Skip this session - no local match
            }
        } else {
            let relative_path = Path::new(&remote_session.file_path)
                .strip_prefix(&remote_projects_dir)
                .ok()
                .unwrap_or_else(|| Path::new(&remote_session.file_path));
            (claude_dir.join(relative_path), relative_path.to_path_buf())
        };

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
        let relative_path_str = relative_path_for_tracking.to_string_lossy().to_string();
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

    // Attach the snapshot path to the operation record (only if we created one)
    operation_record.snapshot_path = snapshot_path;

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
    if filter.use_project_name_only && skipped_no_local_match > 0 {
        println!(
            "  {} Skipped (no local match): {}",
            "!".yellow(),
            skipped_no_local_match
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

    println!("\n{}", "Pull complete!".green().bold());

    // Clean up old snapshots automatically
    if let Err(e) = crate::undo::cleanup_old_snapshots(None, false) {
        log::warn!("Failed to cleanup old snapshots: {}", e);
    }

    Ok(())
}
