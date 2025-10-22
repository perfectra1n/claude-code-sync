use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::filter::FilterConfig;
use crate::git::GitManager;
use crate::history::{
    ConversationSummary, OperationHistory, OperationRecord, OperationType, SyncOperation,
};
use crate::undo::Snapshot;

use super::discovery::{claude_projects_dir, discover_sessions, warn_large_files};
use super::state::SyncState;
use super::MAX_CONVERSATIONS_TO_DISPLAY;

/// Push local Claude Code history to sync repository
pub fn push_history(
    commit_message: Option<&str>,
    push_remote: bool,
    branch: Option<&str>,
    exclude_attachments: bool,
) -> Result<()> {
    println!("{}", "Pushing Claude Code history...".cyan().bold());

    let state = SyncState::load()?;
    let git_manager = GitManager::open(&state.sync_repo_path)?;
    let mut filter = FilterConfig::load()?;

    // Override exclude_attachments if specified in command
    if exclude_attachments {
        filter.exclude_attachments = true;
    }
    let claude_dir = claude_projects_dir()?;

    // Get the current branch name for operation record
    let branch_name = branch
        .map(|s| s.to_string())
        .or_else(|| git_manager.current_branch().ok())
        .unwrap_or_else(|| "main".to_string());

    // Discover all sessions
    println!("  {} conversation sessions...", "Discovering".cyan());
    let sessions = discover_sessions(&claude_dir, &filter)?;
    println!("  {} {} sessions", "Found".green(), sessions.len());

    // ============================================================================
    // COPY SESSIONS AND TRACK CHANGES
    // ============================================================================
    let projects_dir = state.sync_repo_path.join("projects");
    fs::create_dir_all(&projects_dir)?;

    // Discover existing sessions in sync repo to determine operation type
    println!("  {} sessions to sync repository...", "Copying".cyan());
    let existing_sessions = discover_sessions(&projects_dir, &filter)?;
    let existing_map: HashMap<_, _> = existing_sessions
        .iter()
        .map(|s| (s.session_id.clone(), s))
        .collect();

    // Track pushed conversations for operation record
    let mut pushed_conversations: Vec<ConversationSummary> = Vec::new();
    let mut added_count = 0;
    let mut modified_count = 0;
    let mut unchanged_count = 0;

    for session in &sessions {
        let relative_path = Path::new(&session.file_path)
            .strip_prefix(&claude_dir)
            .unwrap_or(Path::new(&session.file_path));

        let dest_path = projects_dir.join(relative_path);

        // Determine operation type based on existing state
        let operation = if let Some(existing) = existing_map.get(&session.session_id) {
            if existing.content_hash() == session.content_hash() {
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

        // Write the session file
        session.write_to_file(&dest_path)?;

        // Track this session in pushed conversations
        let relative_path_str = relative_path.to_string_lossy().to_string();
        match ConversationSummary::new(
            session.session_id.clone(),
            relative_path_str.clone(),
            session.latest_timestamp(),
            session.message_count(),
            operation,
        ) {
            Ok(summary) => pushed_conversations.push(summary),
            Err(e) => log::warn!(
                "Failed to create summary for {}: {}",
                relative_path_str,
                e
            ),
        }
    }

    // ============================================================================
    // COMMIT AND PUSH CHANGES
    // ============================================================================
    git_manager.stage_all()?;

    let has_changes = git_manager.has_changes()?;
    if has_changes {
        // ============================================================================
        // SNAPSHOT CREATION: Create a snapshot before committing changes
        // ============================================================================
        println!("  {} snapshot before push...", "Creating".cyan());

        // Get the current commit hash before making any changes
        // This allows us to undo the push later by resetting to this commit
        let commit_before_push = git_manager
            .current_commit_hash()
            .context("Failed to get current commit hash")?;

        // Collect all file paths in the sync repository that will be affected
        // For push operations, we snapshot the sync repository state, not local files
        let sync_repo_files: Vec<PathBuf> = sessions
            .iter()
            .map(|s| {
                let relative_path = Path::new(&s.file_path)
                    .strip_prefix(&claude_dir)
                    .unwrap_or(Path::new(&s.file_path));
                state.sync_repo_path.join("projects").join(relative_path)
            })
            .collect();

        // Check for large conversation files and warn users
        warn_large_files(&sync_repo_files);

        // Create differential snapshot of sync repository state before push
        // Note: Snapshot creation failure is fatal because we need to ensure users can
        // safely undo this push operation if issues occur. Without a snapshot,
        // there would be no way to restore the previous repository state.
        //
        // Using differential snapshots saves disk space by only storing changed files.
        let snapshot = Snapshot::create_differential(
            OperationType::Push,
            sync_repo_files.iter(),
            Some(&git_manager), // Pass git manager to capture commit hash
        )
        .context("Failed to create snapshot before push")?;

        // Save snapshot to disk
        let snapshot_path = snapshot
            .save_to_disk(None)
            .context("Failed to save snapshot to disk")?;

        println!(
            "  {} Snapshot created: {} (commit: {})",
            "✓".green(),
            snapshot_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| snapshot_path.display().to_string()),
            &commit_before_push[..8]
        );

        let default_message = format!(
            "Sync {} sessions at {}",
            sessions.len(),
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        );
        let message = commit_message.unwrap_or(&default_message);

        println!("  {} changes...", "Committing".cyan());
        git_manager.commit(message)?;
        println!("  {} Committed: {}", "✓".green(), message);

        // Push to remote if configured
        if push_remote && state.has_remote {
            println!("  {} to remote...", "Pushing".cyan());

            match git_manager.push("origin", &branch_name) {
                Ok(_) => println!("  {} Pushed to origin/{}", "✓".green(), branch_name),
                Err(e) => log::warn!("Failed to push: {}", e),
            }
        }

        // ============================================================================
        // CREATE AND SAVE OPERATION RECORD
        // ============================================================================
        let mut operation_record = OperationRecord::new(
            OperationType::Push,
            Some(branch_name.clone()),
            pushed_conversations.clone(),
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
            log::info!("Push completed successfully, but history was not updated.");
        }
    } else {
        println!("  {} No changes to commit", "Note:".yellow());
    }

    // ============================================================================
    // DISPLAY SUMMARY TO USER
    // ============================================================================
    println!("\n{}", "=== Push Summary ===".bold().cyan());

    // Show operation statistics
    let stats_msg = format!(
        "  {} Added    {} Modified    {} Unchanged",
        format!("{added_count}").green(),
        format!("{modified_count}").cyan(),
        format!("{unchanged_count}").dimmed(),
    );
    println!("{stats_msg}");
    println!();

    // Group conversations by project (top-level directory)
    let mut by_project: HashMap<String, Vec<&ConversationSummary>> = HashMap::new();
    for conv in &pushed_conversations {
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
        println!("{}", "Pushed Conversations:".bold());

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

    println!("\n{}", "Push complete!".green().bold());

    // Clean up old snapshots automatically
    if let Err(e) = crate::undo::cleanup_old_snapshots(None, false) {
        log::warn!("Failed to cleanup old snapshots: {}", e);
    }

    Ok(())
}
