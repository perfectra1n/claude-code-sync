use anyhow::{Context, Result};
use colored::Colorize;
use inquire::Confirm;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::filter::FilterConfig;
use crate::history::{
    ConversationSummary, OperationHistory, OperationRecord, OperationType, SyncOperation,
};
use crate::interactive_conflict;
use crate::scm;

use super::discovery::{claude_projects_dir, discover_sessions, find_colliding_projects};
use super::state::SyncState;
use super::MAX_CONVERSATIONS_TO_DISPLAY;

/// Push local Claude Code history to sync repository
pub fn push_history(
    commit_message: Option<&str>,
    push_remote: bool,
    branch: Option<&str>,
    exclude_attachments: bool,
    interactive: bool,
    verbosity: crate::VerbosityLevel,
) -> Result<()> {
    use crate::VerbosityLevel;

    if verbosity != VerbosityLevel::Quiet {
        println!("{}", "Pushing Claude Code history...".cyan().bold());
    }

    let state = SyncState::load()?;
    let repo = scm::open(&state.sync_repo_path)?;
    let mut filter = FilterConfig::load()?;

    // Override exclude_attachments if specified in command
    if exclude_attachments {
        filter.exclude_attachments = true;
    }

    // Set up LFS if enabled
    if filter.enable_lfs {
        if verbosity != VerbosityLevel::Quiet {
            println!("  {} Git LFS...", "Configuring".cyan());
        }
        scm::lfs::setup(&state.sync_repo_path, &filter.lfs_patterns)
            .context("Failed to set up Git LFS")?;
    }

    let claude_dir = claude_projects_dir()?;

    // Get the current branch name for operation record
    let branch_name = branch
        .map(|s| s.to_string())
        .or_else(|| repo.current_branch().ok())
        .unwrap_or_else(|| "main".to_string());

    // Discover all sessions
    println!("  {} conversation sessions...", "Discovering".cyan());
    let sessions = discover_sessions(&claude_dir, &filter)?;
    println!("  {} {} sessions", "Found".green(), sessions.len());

    // Check for project name collisions when using project-name-only mode
    if filter.use_project_name_only {
        let collisions = find_colliding_projects(&claude_dir);
        if !collisions.is_empty() {
            println!();
            println!(
                "{}",
                "Warning: Multiple projects map to the same name:".yellow().bold()
            );
            for (name, paths) in &collisions {
                println!("  {} -> {} locations:", name.cyan(), paths.len());
                for path in paths.iter().take(3) {
                    let display_path = path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown");
                    println!("    - {}", display_path);
                }
                if paths.len() > 3 {
                    println!("    ... and {} more", paths.len() - 3);
                }
            }
            println!();
            println!(
                "{}",
                "Sessions from colliding projects will be merged into the same directory.".yellow()
            );
            println!();
        }
    }

    // ============================================================================
    // COPY SESSIONS AND TRACK CHANGES
    // ============================================================================
    let projects_dir = state.sync_repo_path.join(&filter.sync_subdirectory);
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

    // Track sessions skipped due to missing cwd
    let mut skipped_no_cwd = 0;

    // Closure to compute the relative path for a session, respecting use_project_name_only
    let compute_relative_path =
        |session: &crate::parser::ConversationSession| -> Option<PathBuf> {
            if filter.use_project_name_only {
                let full_relative = Path::new(&session.file_path)
                    .strip_prefix(&claude_dir)
                    .unwrap_or(Path::new(&session.file_path));

                let filename = full_relative.file_name()?;
                let project_name = session.project_name()?;
                Some(PathBuf::from(project_name).join(filename))
            } else {
                Some(
                    Path::new(&session.file_path)
                        .strip_prefix(&claude_dir)
                        .unwrap_or(Path::new(&session.file_path))
                        .to_path_buf(),
                )
            }
        };

    for session in &sessions {
        let relative_path = match compute_relative_path(session) {
            Some(path) => path,
            None => {
                skipped_no_cwd += 1;
                log::debug!("Skipping session {} (no cwd)", session.session_id);
                continue;
            }
        };

        let dest_path = projects_dir.join(&relative_path);

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
    // SHOW SUMMARY AND INTERACTIVE CONFIRMATION
    // ============================================================================
    if verbosity != VerbosityLevel::Quiet {
        println!();
        println!("{}", "Push Summary:".bold().cyan());
        println!("  {} Added: {}", "•".green(), added_count);
        println!("  {} Modified: {}", "•".yellow(), modified_count);
        println!("  {} Unchanged: {}", "•".dimmed(), unchanged_count);
        let total_with_cwd = sessions.len().saturating_sub(skipped_no_cwd);
        println!("  {} Skipped (no cwd): {}", "•".dimmed(), skipped_no_cwd);
        println!(
            "  {} Sessions (with project context): {}",
            "•".cyan(),
            total_with_cwd
        );
        println!();
    }

    // Show detailed file list in verbose mode
    if verbosity == VerbosityLevel::Verbose {
        println!("{}", "Files to be pushed:".bold());
        for (idx, session) in sessions.iter().enumerate().take(20) {
            let Some(relative_path) = compute_relative_path(session) else {
                continue;
            };

            let status = if let Some(existing) = existing_map.get(&session.session_id) {
                if existing.content_hash() == session.content_hash() {
                    "unchanged".dimmed()
                } else {
                    "modified".yellow()
                }
            } else {
                "new".green()
            };

            println!("  {}. {} [{}]", idx + 1, relative_path.display(), status);
        }
        if sessions.len() > 20 {
            println!("  ... and {} more", sessions.len() - 20);
        }
        println!();
    }

    // Interactive confirmation
    if interactive && interactive_conflict::is_interactive() {
        let confirm = Confirm::new("Do you want to proceed with pushing these changes?")
            .with_default(true)
            .with_help_message("This will commit and push to the sync repository")
            .prompt()
            .context("Failed to get confirmation")?;

        if !confirm {
            println!("\n{}", "Push cancelled.".yellow());
            return Ok(());
        }
    }

    // ============================================================================
    // COMMIT AND PUSH CHANGES
    // ============================================================================
    repo.stage_all()?;

    let has_changes = repo.has_changes()?;
    if has_changes {
        // Get the current commit hash before making any changes
        // This allows us to undo the push later by resetting to this commit
        // Note: We don't create file snapshots for push - git already has history!
        // Undo push simply does `git reset` to this commit.
        // On a brand new repo with no commits, this will be None (no undo available for first push)
        let commit_before_push = repo.current_commit_hash().ok();

        if let Some(ref hash) = commit_before_push {
            if verbosity != VerbosityLevel::Quiet {
                println!(
                    "  {} Recorded commit {} for undo",
                    "✓".green(),
                    &hash[..8]
                );
            }
        } else if verbosity != VerbosityLevel::Quiet {
            println!(
                "  {} First push - no previous commit to undo to",
                "ℹ".cyan()
            );
        }

        let default_message = format!(
            "Sync {} sessions at {}",
            sessions.len(),
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        );
        let message = commit_message.unwrap_or(&default_message);

        println!("  {} changes...", "Committing".cyan());
        repo.commit(message)?;
        println!("  {} Committed: {}", "✓".green(), message);

        // Push to remote if configured
        if push_remote && state.has_remote {
            println!("  {} to remote...", "Pushing".cyan());

            match repo.push("origin", &branch_name) {
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

        // Store commit hash for undo (no file snapshot needed - git has history)
        // On first push (no prior commits), this will be None
        operation_record.commit_hash = commit_before_push;

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

    if verbosity == VerbosityLevel::Quiet {
        println!("Push complete");
    } else {
        println!("\n{}", "Push complete!".green().bold());
    }

    // Clean up old snapshots automatically
    if let Err(e) = crate::undo::cleanup_old_snapshots(None, false) {
        log::warn!("Failed to cleanup old snapshots: {}", e);
    }

    Ok(())
}
