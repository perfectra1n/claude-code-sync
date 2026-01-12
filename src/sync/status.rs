use anyhow::Result;
use colored::Colorize;
use std::path::Path;

use crate::filter::FilterConfig;
use crate::scm;

use super::discovery::{claude_projects_dir, discover_sessions};
use super::state::SyncState;

/// Show sync status
pub fn show_status(show_conflicts: bool, show_files: bool) -> Result<()> {
    let state = SyncState::load()?;
    let repo = scm::open(&state.sync_repo_path)?;
    let filter = FilterConfig::load()?;
    let claude_dir = claude_projects_dir()?;

    println!("{}", "=== Claude Code Sync Status ===".bold().cyan());
    println!();

    // Repository info
    println!("{}", "Repository:".bold());
    println!("  Path: {}", state.sync_repo_path.display());
    let backend = scm::detect_backend(&state.sync_repo_path)
        .map(|b| format!("{:?}", b))
        .unwrap_or_else(|| "Unknown".to_string());
    println!("  Backend: {}", backend);
    println!(
        "  Remote: {}",
        if state.has_remote {
            "Configured".green()
        } else {
            "Not configured".yellow()
        }
    );

    if let Ok(branch) = repo.current_branch() {
        println!("  Branch: {}", branch.cyan());
    }

    if let Ok(has_changes) = repo.has_changes() {
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

    let remote_projects_dir = state.sync_repo_path.join(&filter.sync_subdirectory);
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
