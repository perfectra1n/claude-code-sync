use anyhow::{anyhow, Context, Result};
use colored::Colorize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::conflict::ConflictDetector;
use crate::filter::FilterConfig;
use crate::git::GitManager;
use crate::parser::ConversationSession;
use crate::report::{ConflictReport, save_conflict_report};

/// Sync state and configuration
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SyncState {
    sync_repo_path: PathBuf,
    has_remote: bool,
}

impl SyncState {
    fn load() -> Result<Self> {
        let state_path = Self::state_file_path()?;

        if !state_path.exists() {
            return Err(anyhow!("Sync not initialized. Run 'claude-sync init' first."));
        }

        let content = fs::read_to_string(&state_path)
            .context("Failed to read sync state")?;

        let state: SyncState = serde_json::from_str(&content)
            .context("Failed to parse sync state")?;

        Ok(state)
    }

    fn save(&self) -> Result<()> {
        let state_path = Self::state_file_path()?;

        if let Some(parent) = state_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)
            .context("Failed to serialize sync state")?;

        fs::write(&state_path, content)
            .context("Failed to write sync state")?;

        Ok(())
    }

    fn state_file_path() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .context("Failed to get home directory")?;
        Ok(home.join(".claude-sync").join("state.json"))
    }
}

/// Get the Claude Code projects directory
fn claude_projects_dir() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .context("Failed to get home directory")?;
    Ok(home.join(".claude").join("projects"))
}

/// Initialize a new sync repository
pub fn init_sync_repo(repo_path: &Path, remote_url: Option<&str>) -> Result<()> {
    println!("{}", "Initializing Claude Code sync repository...".cyan().bold());

    // Create/open the git repository
    let git_manager = if repo_path.exists() && repo_path.join(".git").exists() {
        println!("  {} existing repository at {}", "Using".green(), repo_path.display());
        GitManager::open(repo_path)?
    } else {
        println!("  {} new repository at {}", "Creating".green(), repo_path.display());
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
    };
    state.save()?;

    println!("{}", "Sync repository initialized successfully!".green().bold());
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
                    eprintln!("{} Failed to parse {}: {}",
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
pub fn push_history(commit_message: Option<&str>, push_remote: bool) -> Result<()> {
    println!("{}", "Pushing Claude Code history...".cyan().bold());

    let state = SyncState::load()?;
    let git_manager = GitManager::open(&state.sync_repo_path)?;
    let filter = FilterConfig::load()?;
    let claude_dir = claude_projects_dir()?;

    // Discover all sessions
    println!("  {} conversation sessions...", "Discovering".cyan());
    let sessions = discover_sessions(&claude_dir, &filter)?;
    println!("  {} {} sessions", "Found".green(), sessions.len());

    // Copy sessions to sync repo
    let projects_dir = state.sync_repo_path.join("projects");
    fs::create_dir_all(&projects_dir)?;

    println!("  {} sessions to sync repository...", "Copying".cyan());
    for session in &sessions {
        let relative_path = Path::new(&session.file_path)
            .strip_prefix(&claude_dir)
            .unwrap_or(Path::new(&session.file_path));

        let dest_path = projects_dir.join(relative_path);
        session.write_to_file(&dest_path)?;
    }

    // Commit changes
    git_manager.stage_all()?;

    if git_manager.has_changes()? {
        let default_message = format!("Sync {} sessions at {}", sessions.len(), chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"));
        let message = commit_message.unwrap_or(&default_message);

        println!("  {} changes...", "Committing".cyan());
        git_manager.commit(message)?;
        println!("  {} Committed: {}", "✓".green(), message);
    } else {
        println!("  {} No changes to commit", "Note:".yellow());
    }

    // Push to remote if configured
    if push_remote && state.has_remote {
        println!("  {} to remote...", "Pushing".cyan());
        let branch = git_manager.current_branch().unwrap_or_else(|_| "main".to_string());
        match git_manager.push("origin", &branch) {
            Ok(_) => println!("  {} Pushed to origin/{}", "✓".green(), branch),
            Err(e) => eprintln!("{} Failed to push: {}", "Warning:".yellow(), e),
        }
    }

    println!("{}", "Push complete!".green().bold());
    Ok(())
}

/// Pull and merge history from sync repository
pub fn pull_history(fetch_remote: bool) -> Result<()> {
    println!("{}", "Pulling Claude Code history...".cyan().bold());

    let state = SyncState::load()?;
    let git_manager = GitManager::open(&state.sync_repo_path)?;
    let filter = FilterConfig::load()?;
    let claude_dir = claude_projects_dir()?;

    // Fetch from remote if configured
    if fetch_remote && state.has_remote {
        println!("  {} from remote...", "Fetching".cyan());
        let branch = git_manager.current_branch().unwrap_or_else(|_| "main".to_string());
        match git_manager.pull("origin", &branch) {
            Ok(_) => println!("  {} Pulled from origin/{}", "✓".green(), branch),
            Err(e) => {
                eprintln!("{} Failed to pull: {}", "Warning:".yellow(), e);
                eprintln!("  Continuing with local sync repository state...");
            }
        }
    }

    // Discover local sessions
    println!("  {} local sessions...", "Discovering".cyan());
    let local_sessions = discover_sessions(&claude_dir, &filter)?;
    println!("  {} {} local sessions", "Found".green(), local_sessions.len());

    // Discover remote sessions
    let remote_projects_dir = state.sync_repo_path.join("projects");
    println!("  {} remote sessions...", "Discovering".cyan());
    let remote_sessions = discover_sessions(&remote_projects_dir, &filter)?;
    println!("  {} {} remote sessions", "Found".green(), remote_sessions.len());

    // Detect conflicts
    println!("  {} conflicts...", "Detecting".cyan());
    let mut detector = ConflictDetector::new();
    detector.detect(&local_sessions, &remote_sessions);

    if detector.has_conflicts() {
        println!("  {} {} conflicts detected", "!".yellow(), detector.conflict_count());

        // Resolve conflicts using "keep both" strategy
        let renames = detector.resolve_all_keep_both()?;

        // Save conflict report
        let report = ConflictReport::from_conflicts(detector.conflicts());
        save_conflict_report(&report)?;

        println!("\n{}", "Conflict Resolution:".yellow().bold());
        for (_original, renamed) in &renames {
            let relative_renamed = renamed
                .strip_prefix(&claude_dir)
                .unwrap_or(renamed);
            println!("  {} remote version saved as: {}",
                "→".yellow(),
                relative_renamed.display().to_string().cyan()
            );
        }

        // Apply renames and copy files
        for (_, renamed_path) in &renames {
            if let Some(session) = remote_sessions.iter().find(|s| {
                Path::new(&s.file_path) == renamed_path.strip_prefix(&claude_dir).unwrap_or(renamed_path)
            }) {
                let dest = claude_dir.join(
                    renamed_path.strip_prefix(&remote_projects_dir).unwrap_or(renamed_path)
                );
                session.write_to_file(&dest)?;
            }
        }

        println!("\n{} View details with: claude-sync report",
            "Hint:".cyan()
        );
    } else {
        println!("  {} No conflicts detected", "✓".green());
    }

    // Copy non-conflicting sessions
    println!("  {} non-conflicting sessions...", "Merging".cyan());
    let local_map: HashMap<_, _> = local_sessions
        .iter()
        .map(|s| (s.session_id.clone(), s))
        .collect();

    let mut merged_count = 0;
    for remote_session in &remote_sessions {
        // Skip if conflicts were detected
        if detector.conflicts().iter().any(|c| c.session_id == remote_session.session_id) {
            continue;
        }

        // Copy if doesn't exist locally or is identical
        if let Some(local) = local_map.get(&remote_session.session_id) {
            if local.content_hash() == remote_session.content_hash() {
                continue; // Already in sync
            }
        }

        let relative_path = Path::new(&remote_session.file_path)
            .strip_prefix(&remote_projects_dir)
            .unwrap_or(Path::new(&remote_session.file_path));

        let dest_path = claude_dir.join(relative_path);
        remote_session.write_to_file(&dest_path)?;
        merged_count += 1;
    }

    println!("  {} Merged {} sessions", "✓".green(), merged_count);
    println!("{}", "Pull complete!".green().bold());

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
    println!("  Remote: {}", if state.has_remote { "Configured".green() } else { "Not configured".yellow() });

    if let Ok(branch) = git_manager.current_branch() {
        println!("  Branch: {}", branch.cyan());
    }

    if let Ok(has_changes) = git_manager.has_changes() {
        println!("  Uncommitted changes: {}", if has_changes { "Yes".yellow() } else { "No".green() });
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
            println!("  {} ({} messages)", relative.display(), session.message_count());
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

