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

use super::discovery::{
    claude_home_dir, claude_projects_dir, discover_sessions, find_colliding_projects,
};
use super::state::SyncState;
use super::MAX_CONVERSATIONS_TO_DISPLAY;

/// One session's planned copy into the sync repository.
#[derive(Debug, Clone)]
pub struct PlannedSessionPush {
    /// Index into the discovered sessions slice this entry refers to
    pub session_index: usize,
    /// Destination path relative to the sync repo's projects directory
    pub relative_path: PathBuf,
    /// How the session differs from what the sync repository already holds
    pub operation: SyncOperation,
}

/// Read-only classification of a push: which sessions land where, and how they
/// differ from what the sync repository already contains.
#[derive(Debug, Default)]
pub struct PushPlan {
    pub entries: Vec<PlannedSessionPush>,
    pub added: usize,
    pub modified: usize,
    pub unchanged: usize,
    pub skipped_no_cwd: usize,
}

/// Outcome counts of a completed push, returned to callers and tests.
#[allow(dead_code)] // fields are read via the library target; the bin compiles this module separately
#[derive(Debug, Default)]
pub struct PushReport {
    pub added: usize,
    pub modified: usize,
    pub unchanged: usize,
    pub skipped_no_cwd: usize,
    /// Per-category artifact outcomes (empty when no category is enabled).
    pub artifacts: crate::artifacts::engine::ArtifactReport,
}

/// Compute a session's destination path relative to the projects directory,
/// respecting `use_project_name_only`. Returns None when the session lacks the
/// `cwd` needed for project-name mapping.
fn compute_relative_path(
    session: &crate::parser::ConversationSession,
    claude_dir: &Path,
    filter: &FilterConfig,
) -> Option<PathBuf> {
    if filter.use_project_name_only {
        let full_relative = Path::new(&session.file_path)
            .strip_prefix(claude_dir)
            .unwrap_or(Path::new(&session.file_path));

        let filename = full_relative.file_name()?;
        let project_name = session.project_name()?;
        Some(PathBuf::from(project_name).join(filename))
    } else {
        Some(
            Path::new(&session.file_path)
                .strip_prefix(claude_dir)
                .unwrap_or(Path::new(&session.file_path))
                .to_path_buf(),
        )
    }
}

/// Classify every discovered session against the sync repository's current
/// contents without writing anything. Sessions are keyed by their identity
/// (filename stem), so sibling files sharing an interior sessionId — subagent
/// sidechains, resumed sessions — classify independently (issue #68).
pub fn plan_push(
    sessions: &[crate::parser::ConversationSession],
    claude_dir: &Path,
    projects_dir: &Path,
    filter: &FilterConfig,
) -> Result<PushPlan> {
    let existing_sessions = if projects_dir.exists() {
        discover_sessions(projects_dir, filter)?
    } else {
        Vec::new()
    };
    let existing_map: HashMap<_, _> = existing_sessions
        .iter()
        .map(|s| (s.session_id.clone(), s))
        .collect();

    let mut plan = PushPlan::default();

    for (session_index, session) in sessions.iter().enumerate() {
        let relative_path = match compute_relative_path(session, claude_dir, filter) {
            Some(path) => path,
            None => {
                plan.skipped_no_cwd += 1;
                log::debug!("Skipping session {} (no cwd)", session.session_id);
                continue;
            }
        };

        let operation = if let Some(existing) = existing_map.get(&session.session_id) {
            if existing.content_hash() == session.content_hash() {
                plan.unchanged += 1;
                SyncOperation::Unchanged
            } else {
                plan.modified += 1;
                SyncOperation::Modified
            }
        } else {
            plan.added += 1;
            SyncOperation::Added
        };

        plan.entries.push(PlannedSessionPush {
            session_index,
            relative_path,
            operation,
        });
    }

    Ok(plan)
}

/// Push local Claude Code history to sync repository
pub fn push_history(
    commit_message: Option<&str>,
    push_remote: bool,
    branch: Option<&str>,
    exclude_attachments: bool,
    interactive: bool,
    verbosity: crate::VerbosityLevel,
) -> Result<PushReport> {
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

    // Classify every session against the sync repo, then apply the plan
    println!("  {} sessions to sync repository...", "Copying".cyan());
    let plan = plan_push(&sessions, &claude_dir, &projects_dir, &filter)?;
    let added_count = plan.added;
    let modified_count = plan.modified;
    let unchanged_count = plan.unchanged;
    let skipped_no_cwd = plan.skipped_no_cwd;

    // Track pushed conversations for operation record
    let mut pushed_conversations: Vec<ConversationSummary> = Vec::new();

    for entry in &plan.entries {
        let session = &sessions[entry.session_index];
        let dest_path = projects_dir.join(&entry.relative_path);

        // Write the session file
        session.write_to_file(&dest_path)?;

        // Track this session in pushed conversations
        let relative_path_str = entry.relative_path.to_string_lossy().to_string();
        match ConversationSummary::new(
            session.session_id.clone(),
            relative_path_str.clone(),
            session.latest_timestamp(),
            session.message_count(),
            entry.operation,
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
    // COPY ARTIFACTS (settings, skills, agents, ...) AND WRITE IGNORE GUARD
    // ============================================================================
    let artifact_report = crate::artifacts::engine::push_artifacts(
        &claude_home_dir()?,
        &state.sync_repo_path,
        &filter,
    )?;
    crate::artifacts::engine::ensure_ignore_files(&state.sync_repo_path, filter.backend()?)?;

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
        if !artifact_report.counts.is_empty() {
            println!(
                "  {} Artifacts: {} added, {} modified, {} unchanged",
                "•".cyan(),
                artifact_report.total_added(),
                artifact_report.total_modified(),
                artifact_report.total_unchanged()
            );
        }
        println!();
    }

    // Show detailed file list in verbose mode
    if verbosity == VerbosityLevel::Verbose {
        println!("{}", "Files to be pushed:".bold());
        for (idx, entry) in plan.entries.iter().enumerate().take(20) {
            let status = match entry.operation {
                SyncOperation::Unchanged => "unchanged".dimmed(),
                SyncOperation::Modified => "modified".yellow(),
                _ => "new".green(),
            };

            println!(
                "  {}. {} [{}]",
                idx + 1,
                entry.relative_path.display(),
                status
            );
        }
        if plan.entries.len() > 20 {
            println!("  ... and {} more", plan.entries.len() - 20);
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
            return Ok(PushReport::default());
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
        operation_record.artifact_counts = artifact_report.counts.clone();

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

    Ok(PushReport {
        added: added_count,
        modified: modified_count,
        unchanged: unchanged_count,
        skipped_no_cwd,
        artifacts: artifact_report,
    })
}
