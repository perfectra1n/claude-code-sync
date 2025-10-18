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
pub fn push_history(commit_message: Option<&str>, push_remote: bool, branch: Option<&str>, exclude_attachments: bool) -> Result<()> {
    println!("{}", "Pushing Claude Code history...".cyan().bold());

    let state = SyncState::load()?;
    let git_manager = GitManager::open(&state.sync_repo_path)?;
    let mut filter = FilterConfig::load()?;

    // Override exclude_attachments if specified in command
    if exclude_attachments {
        filter.exclude_attachments = true;
    }
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
        let branch_name = branch
            .map(|s| s.to_string())
            .or_else(|| git_manager.current_branch().ok())
            .unwrap_or_else(|| "main".to_string());

        match git_manager.push("origin", &branch_name) {
            Ok(_) => println!("  {} Pushed to origin/{}", "✓".green(), branch_name),
            Err(e) => eprintln!("{} Failed to push: {}", "Warning:".yellow(), e),
        }
    }

    println!("{}", "Push complete!".green().bold());
    Ok(())
}

/// Pull and merge history from sync repository
pub fn pull_history(fetch_remote: bool, branch: Option<&str>) -> Result<()> {
    println!("{}", "Pulling Claude Code history...".cyan().bold());

    let state = SyncState::load()?;
    let git_manager = GitManager::open(&state.sync_repo_path)?;
    let filter = FilterConfig::load()?;
    let claude_dir = claude_projects_dir()?;

    // Fetch from remote if configured
    if fetch_remote && state.has_remote {
        println!("  {} from remote...", "Fetching".cyan());
        let branch_name = branch
            .map(|s| s.to_string())
            .or_else(|| git_manager.current_branch().ok())
            .unwrap_or_else(|| "main".to_string());

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

/// Show current remote configuration
pub fn show_remote() -> Result<()> {
    let state = SyncState::load()?;
    let git_manager = GitManager::open(&state.sync_repo_path)?;

    println!("{}", "=== Git Remote Configuration ===".bold().cyan());
    println!();

    // Show sync repository directory
    println!("{} {}", "Sync Directory:".bold(), state.sync_repo_path.display().to_string().cyan());

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

        println!("{} Updated remote '{}' to: {}",
            "✓".green().bold(),
            name.cyan(),
            url
        );
    } else {
        // Create new remote
        repo.remote(name, url)
            .with_context(|| format!("Failed to create remote '{}'", name))?;

        println!("{} Created remote '{}': {}",
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
pub fn sync_bidirectional(commit_message: Option<&str>, branch: Option<&str>, exclude_attachments: bool) -> Result<()> {
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
    println!("  {} Your local and remote histories are now in sync", "✓".green());

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
        };
        
        // Create state directory
        let state_path = dirs::home_dir().unwrap().join(".claude-sync");
        std::fs::create_dir_all(&state_path).unwrap();
        
        let state_file = state_path.join("state.json");
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
