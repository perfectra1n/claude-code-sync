use anyhow::{anyhow, Context, Result};
use colored::Colorize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::conflict::ConflictDetector;
use crate::filter::FilterConfig;
use crate::git::GitManager;
use crate::history::{
    ConversationSummary, OperationHistory, OperationRecord, OperationType, SyncOperation,
};
use crate::parser::ConversationSession;
use crate::report::{save_conflict_report, ConflictReport};
use crate::undo::Snapshot;

/// Maximum number of conversations to display per project in pull summary
const MAX_CONVERSATIONS_TO_DISPLAY: usize = 10;

/// Sync state and configuration
///
/// This struct stores the persistent state of the Claude Code sync system.
/// It tracks where the sync repository is located, whether it's configured
/// with a remote, and whether it was originally cloned from a remote URL.
///
/// The state is serialized to JSON and stored in the user's configuration
/// directory, allowing the sync system to remember its configuration across
/// multiple command invocations.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SyncState {
    /// Path to the local git repository used for syncing Claude Code conversations
    ///
    /// This is the directory where all conversation sessions are stored in git format.
    /// The conversations are organized under a `projects/` subdirectory within this path.
    /// This repository can be a local-only git repository or one that's synchronized
    /// with a remote origin.
    pub sync_repo_path: PathBuf,

    /// Whether the sync repository has a remote configured
    ///
    /// When `true`, the repository has a remote (typically named "origin") configured,
    /// allowing push and pull operations to synchronize with a remote git service
    /// (e.g., GitHub, GitLab). When `false`, the repository is local-only and cannot
    /// push to or pull from remote servers.
    pub has_remote: bool,

    /// Whether the repository was cloned from a remote URL
    ///
    /// This field distinguishes between repositories that were:
    /// - Cloned from an existing remote repository (`true`)
    /// - Initialized locally and optionally had a remote added later (`false`)
    ///
    /// This affects certain initialization behaviors, as cloned repositories
    /// may already have existing content and history.
    #[serde(default)]
    pub is_cloned_repo: bool,
}

impl SyncState {
    /// Loads the sync state from the user's configuration directory
    ///
    /// This function reads the persisted sync configuration from disk and deserializes
    /// it into a `SyncState` instance. The state file is stored in the user's Claude
    /// configuration directory and contains information about the sync repository location,
    /// remote configuration, and initialization method.
    ///
    /// # Returns
    ///
    /// Returns a `Result` containing the loaded `SyncState` on success.
    ///
    /// # Errors
    ///
    /// This function returns an error if:
    /// - The sync system has not been initialized (state file doesn't exist)
    /// - The state file cannot be read (permission errors, I/O errors)
    /// - The state file contains invalid JSON or cannot be deserialized
    ///
    /// If the sync is not initialized, the error message will instruct the user
    /// to run `claude-sync init` first.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use claude_sync::sync::SyncState;
    ///
    /// match SyncState::load() {
    ///     Ok(state) => {
    ///         println!("Sync repo: {}", state.sync_repo_path.display());
    ///         println!("Has remote: {}", state.has_remote);
    ///     }
    ///     Err(e) => eprintln!("Failed to load sync state: {}", e),
    /// }
    /// ```
    pub fn load() -> Result<Self> {
        let state_path = Self::state_file_path()?;

        if !state_path.exists() {
            return Err(anyhow!(
                "Sync not initialized. Run 'claude-sync init' first."
            ));
        }

        let content = fs::read_to_string(&state_path).context("Failed to read sync state")?;

        let state: SyncState =
            serde_json::from_str(&content).context("Failed to parse sync state")?;

        Ok(state)
    }

    fn save(&self) -> Result<()> {
        let state_path = Self::state_file_path()?;

        if let Some(parent) = state_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content =
            serde_json::to_string_pretty(self).context("Failed to serialize sync state")?;

        fs::write(&state_path, content).context("Failed to write sync state")?;

        Ok(())
    }

    fn state_file_path() -> Result<PathBuf> {
        crate::config::ConfigManager::state_file_path()
    }
}

/// Get the Claude Code projects directory
fn claude_projects_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to get home directory")?;
    Ok(home.join(".claude").join("projects"))
}

/// Initialize sync repository from onboarding config
pub fn init_from_onboarding(
    repo_path: &Path,
    remote_url: Option<&str>,
    is_cloned: bool,
) -> Result<()> {
    crate::config::ConfigManager::ensure_config_dir()?;

    // If this is a cloned repo and it already exists, just open it
    // Otherwise, initialize a new one
    let git_manager = if repo_path.exists() && repo_path.join(".git").exists() {
        GitManager::open(repo_path)?
    } else {
        GitManager::init(repo_path)?
    };

    // Add remote if specified
    let has_remote = if let Some(url) = remote_url {
        if !git_manager.has_remote("origin") {
            git_manager.add_remote("origin", url)?;
        }
        true
    } else {
        false
    };

    // Save sync state
    let state = SyncState {
        sync_repo_path: repo_path.to_path_buf(),
        has_remote,
        is_cloned_repo: is_cloned,
    };
    state.save()?;

    Ok(())
}

/// Initialize a new sync repository
pub fn init_sync_repo(repo_path: &Path, remote_url: Option<&str>) -> Result<()> {
    println!(
        "{}",
        "Initializing Claude Code sync repository...".cyan().bold()
    );

    // Create/open the git repository
    let git_manager = if repo_path.exists() && repo_path.join(".git").exists() {
        println!(
            "  {} existing repository at {}",
            "Using".green(),
            repo_path.display()
        );
        GitManager::open(repo_path)?
    } else {
        println!(
            "  {} new repository at {}",
            "Creating".green(),
            repo_path.display()
        );
        GitManager::init(repo_path)?
    };

    // Add remote if specified
    let has_remote = if let Some(url) = remote_url {
        if !git_manager.has_remote("origin") {
            git_manager.add_remote("origin", url)?;
            println!("  {} remote 'origin' -> {}", "Added".green(), url);
        } else {
            println!("  {} Remote 'origin' already exists", "Note:".yellow());
        }
        true
    } else {
        false
    };

    // Save sync state
    let state = SyncState {
        sync_repo_path: repo_path.to_path_buf(),
        has_remote,
        is_cloned_repo: false,
    };
    state.save()?;

    println!(
        "{}",
        "Sync repository initialized successfully!".green().bold()
    );
    println!("\n{} claude-sync push", "Next steps:".cyan().bold());

    Ok(())
}

/// Discover all conversation sessions in Claude Code history
fn discover_sessions(base_path: &Path, filter: &FilterConfig) -> Result<Vec<ConversationSession>> {
    let mut sessions = Vec::new();

    for entry in WalkDir::new(base_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            if !filter.should_include(path) {
                continue;
            }

            match ConversationSession::from_file(path) {
                Ok(session) => sessions.push(session),
                Err(e) => {
                    eprintln!(
                        "{} Failed to parse {}: {}",
                        "Warning:".yellow(),
                        path.display(),
                        e
                    );
                }
            }
        }
    }

    Ok(sessions)
}

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
            Err(e) => eprintln!(
                "{} Failed to create summary for {}: {}",
                "Warning:".yellow(),
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

        // Create snapshot of sync repository state before push
        // Note: Snapshot creation failure is fatal because we need to ensure users can
        // safely undo this push operation if issues occur. Without a snapshot,
        // there would be no way to restore the previous repository state.
        let snapshot = Snapshot::create(
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
                Err(e) => eprintln!("{} Failed to push: {}", "Warning:".yellow(), e),
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
                eprintln!(
                    "{} Failed to load operation history: {}",
                    "Warning:".yellow(),
                    e
                );
                eprintln!("  Creating new history...");
                OperationHistory::default()
            }
        };

        if let Err(e) = history.add_operation(operation_record) {
            eprintln!(
                "{} Failed to save operation to history: {}",
                "Warning:".yellow(),
                e
            );
            eprintln!("  Push completed successfully, but history was not updated.");
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
        format!("{}", added_count).green(),
        format!("{}", modified_count).cyan(),
        format!("{}", unchanged_count).dimmed(),
    );
    println!("{}", stats_msg);
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
    Ok(())
}

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
                eprintln!("{} Failed to pull: {}", "Warning:".yellow(), e);
                eprintln!("  Continuing with local sync repository state...");
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

    // Create snapshot of current state (before any changes)
    // Note: Snapshot creation failure is fatal because we need to ensure users can
    // safely undo this pull operation if conflicts or issues occur. Without a snapshot,
    // there would be no way to restore the previous state.
    let snapshot = Snapshot::create(
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

        // Check if we can run interactively
        let use_interactive = crate::interactive_conflict::is_interactive();

        let renames = if use_interactive {
            // Interactive conflict resolution
            println!(
                "\n{} Running in interactive mode",
                "→".cyan()
            );

            let conflicts = detector.conflicts_mut();
            let resolution_result = crate::interactive_conflict::resolve_conflicts_interactive(conflicts)?;

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
            // Non-interactive mode: use default "keep both" strategy
            println!(
                "\n{} Using automatic conflict resolution (keep both versions)",
                "→".cyan()
            );

            // Resolve conflicts using "keep both" strategy
            let renames = detector.resolve_all_keep_both()?;

            // Save conflict report
            let report = ConflictReport::from_conflicts(detector.conflicts());
            save_conflict_report(&report)?;

            println!("\n{}", "Conflict Resolution:".yellow().bold());
            for (_original, renamed) in &renames {
                let relative_renamed = renamed.strip_prefix(&claude_dir).unwrap_or(renamed);
                println!(
                    "  {} remote version saved as: {}",
                    "→".yellow(),
                    relative_renamed.display().to_string().cyan()
                );
            }

            // Apply renames and copy files
            for (original_path, renamed_path) in &renames {
                // Find the remote session that corresponds to this original path
                if let Some(session) = remote_sessions.iter().find(|s| {
                    let remote_path = remote_projects_dir.join(&s.file_path);
                    &remote_path == original_path
                }) {
                    // Write the session to the renamed destination path
                    session.write_to_file(renamed_path)?;
                }
            }

            renames
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
                    Err(e) => eprintln!(
                        "{} Failed to create summary for conflict {}: {}",
                        "Warning:".yellow(),
                        relative_path,
                        e
                    ),
                }
            }
        }

        println!("\n{} View details with: claude-sync report", "Hint:".cyan());
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
            Err(e) => eprintln!(
                "{} Failed to create summary for {}: {}",
                "Warning:".yellow(),
                relative_path_str,
                e
            ),
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
            eprintln!(
                "{} Failed to load operation history: {}",
                "Warning:".yellow(),
                e
            );
            eprintln!("  Creating new history...");
            OperationHistory::default()
        }
    };

    if let Err(e) = history.add_operation(operation_record) {
        eprintln!(
            "{} Failed to save operation to history: {}",
            "Warning:".yellow(),
            e
        );
        eprintln!("  Pull completed successfully, but history was not updated.");
    }

    // ============================================================================
    // DISPLAY SUMMARY TO USER
    // ============================================================================
    println!("\n{}", "=== Pull Summary ===".bold().cyan());

    // Show operation statistics
    let conflict_count = detector.conflict_count();
    let stats_msg = format!(
        "  {} Added    {} Modified    {} Conflicts    {} Unchanged",
        format!("{}", added_count).green(),
        format!("{}", modified_count).cyan(),
        format!("{}", conflict_count).yellow(),
        format!("{}", unchanged_count).dimmed(),
    );
    println!("{}", stats_msg);
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

    Ok(())
}

/// Show sync status
pub fn show_status(show_conflicts: bool, show_files: bool) -> Result<()> {
    let state = SyncState::load()?;
    let git_manager = GitManager::open(&state.sync_repo_path)?;
    let filter = FilterConfig::load()?;
    let claude_dir = claude_projects_dir()?;

    println!("{}", "=== Claude Code Sync Status ===".bold().cyan());
    println!();

    // Repository info
    println!("{}", "Repository:".bold());
    println!("  Path: {}", state.sync_repo_path.display());
    println!(
        "  Remote: {}",
        if state.has_remote {
            "Configured".green()
        } else {
            "Not configured".yellow()
        }
    );

    if let Ok(branch) = git_manager.current_branch() {
        println!("  Branch: {}", branch.cyan());
    }

    if let Ok(has_changes) = git_manager.has_changes() {
        println!(
            "  Uncommitted changes: {}",
            if has_changes {
                "Yes".yellow()
            } else {
                "No".green()
            }
        );
    }

    // Session counts
    println!();
    println!("{}", "Sessions:".bold());
    let local_sessions = discover_sessions(&claude_dir, &filter)?;
    println!("  Local: {}", local_sessions.len().to_string().cyan());

    let remote_projects_dir = state.sync_repo_path.join("projects");
    if remote_projects_dir.exists() {
        let remote_sessions = discover_sessions(&remote_projects_dir, &filter)?;
        println!("  Sync repo: {}", remote_sessions.len().to_string().cyan());
    }

    // Show files if requested
    if show_files {
        println!();
        println!("{}", "Local session files:".bold());
        for session in local_sessions.iter().take(20) {
            let relative = Path::new(&session.file_path)
                .strip_prefix(&claude_dir)
                .unwrap_or(Path::new(&session.file_path));
            println!(
                "  {} ({} messages)",
                relative.display(),
                session.message_count()
            );
        }
        if local_sessions.len() > 20 {
            println!("  ... and {} more", local_sessions.len() - 20);
        }
    }

    // Show conflicts if requested
    if show_conflicts {
        println!();
        if let Ok(report) = crate::report::load_latest_report() {
            if report.total_conflicts > 0 {
                report.print_summary();
            } else {
                println!("{}", "No conflicts in last sync".green());
            }
        }
    }

    Ok(())
}

/// Show current remote configuration
pub fn show_remote() -> Result<()> {
    let state = SyncState::load()?;
    let git_manager = GitManager::open(&state.sync_repo_path)?;

    println!("{}", "=== Git Remote Configuration ===".bold().cyan());
    println!();

    // Show sync repository directory
    println!(
        "{} {}",
        "Sync Directory:".bold(),
        state.sync_repo_path.display().to_string().cyan()
    );

    // Show current branch
    if let Ok(branch) = git_manager.current_branch() {
        println!("{} {}", "Current Branch:".bold(), branch.cyan());
    }

    println!();

    // Get repository
    let repo = git2::Repository::open(&state.sync_repo_path)?;

    // List all remotes
    let remotes = repo.remotes()?;

    if remotes.is_empty() {
        println!("{}", "No remotes configured".yellow());
        println!("\n{} claude-sync remote set origin <url>", "Hint:".cyan());
        return Ok(());
    }

    for remote_name in remotes.iter() {
        if let Some(name) = remote_name {
            if let Ok(remote) = repo.find_remote(name) {
                println!("{} {}", "Remote:".bold(), name.cyan());

                if let Some(url) = remote.url() {
                    println!("  URL: {}", url);
                } else {
                    println!("  URL: {}", "None".yellow());
                }

                if let Some(push_url) = remote.pushurl() {
                    println!("  Push URL: {}", push_url);
                }

                println!();
            }
        }
    }

    Ok(())
}

/// Set or update remote URL
pub fn set_remote(name: &str, url: &str) -> Result<()> {
    let state = SyncState::load()?;
    let repo = git2::Repository::open(&state.sync_repo_path)?;

    // Validate URL format
    if !url.starts_with("http://") && !url.starts_with("https://") && !url.starts_with("git@") {
        return Err(anyhow!(
            "Invalid URL format: {}\n\
            \n\
            URL must start with:\n\
            - https:// (e.g., https://github.com/user/repo.git)\n\
            - http:// (e.g., http://gitlab.com/user/repo.git)\n\
            - git@ (e.g., git@github.com:user/repo.git)",
            url
        ));
    }

    // Check if remote exists
    let remote_exists = repo.find_remote(name).is_ok();

    if remote_exists {
        // Update existing remote
        repo.remote_set_url(name, url)
            .with_context(|| format!("Failed to update remote '{}' URL", name))?;

        println!(
            "{} Updated remote '{}' to: {}",
            "✓".green().bold(),
            name.cyan(),
            url
        );
    } else {
        // Create new remote
        repo.remote(name, url)
            .with_context(|| format!("Failed to create remote '{}'", name))?;

        println!(
            "{} Created remote '{}': {}",
            "✓".green().bold(),
            name.cyan(),
            url
        );
    }

    // Update state if this is the origin remote
    if name == "origin" {
        let mut state = state;
        state.has_remote = true;
        state.save()?;
    }

    println!("\n{} claude-sync push", "Next:".cyan());

    Ok(())
}

/// Remove a remote
pub fn remove_remote(name: &str) -> Result<()> {
    let state = SyncState::load()?;
    let repo = git2::Repository::open(&state.sync_repo_path)?;

    // Check if remote exists
    if repo.find_remote(name).is_err() {
        return Err(anyhow!("Remote '{}' not found", name));
    }

    // Remove the remote
    repo.remote_delete(name)
        .with_context(|| format!("Failed to remove remote '{}'", name))?;

    println!("{} Removed remote '{}'", "✓".green().bold(), name.cyan());

    // Update state if this was the origin remote
    if name == "origin" {
        let mut state = state;
        state.has_remote = false;
        state.save()?;
    }

    Ok(())
}

/// Bidirectional sync: pull remote changes, then push local changes
pub fn sync_bidirectional(
    commit_message: Option<&str>,
    branch: Option<&str>,
    exclude_attachments: bool,
) -> Result<()> {
    println!("{}", "=== Bidirectional Sync ===".bold().cyan());
    println!();

    // First, pull remote changes
    println!("{}", "Step 1: Pulling remote changes...".bold());
    pull_history(true, branch)?;

    println!();

    // Then, push local changes
    println!("{}", "Step 2: Pushing local changes...".bold());
    push_history(commit_message, true, branch, exclude_attachments)?;

    println!();
    println!("{}", "=== Sync Complete ===".green().bold());
    println!(
        "  {} Your local and remote histories are now in sync",
        "✓".green()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_url_validation() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path().join("test-repo");

        // Initialize a test repo
        GitManager::init(&repo_path).unwrap();

        // Save a test state
        let state = SyncState {
            sync_repo_path: repo_path.clone(),
            has_remote: false,
            is_cloned_repo: false,
        };

        // Create state directory using ConfigManager
        let state_path = crate::config::ConfigManager::ensure_config_dir().unwrap();
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
    }

    #[test]
    fn test_filter_with_attachments() {
        let mut filter = FilterConfig::default();
        filter.exclude_attachments = true;

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
