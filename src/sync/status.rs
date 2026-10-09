use anyhow::Result;
use colored::Colorize;
use std::path::Path;

use crate::filter::FilterConfig;
use crate::scm;

use super::discovery::{claude_projects_dir, discover_sessions};
use super::state::SyncState;

/// Show sync status
pub fn show_status(show_conflicts: bool, show_files: bool, held_back: bool) -> Result<()> {
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
    let mut skipped_by_project = crate::project_map::SkippedByProject::new();
    if remote_projects_dir.exists() {
        let remote_sessions = discover_sessions(&remote_projects_dir, &filter)?;
        println!("  Sync repo: {}", remote_sessions.len().to_string().cyan());
        skipped_by_project = sessions_without_local_project(
            &filter,
            &claude_dir,
            &remote_projects_dir,
            &remote_sessions,
        );
    }

    // Artifact categories: enabled state and local-vs-repo drift
    println!();
    println!("{}", "Artifacts:".bold());
    if filter.sync_artifacts.any_enabled() || !filter.exclude_attachments {
        let claude_home = super::discovery::claude_home_dir()?;
        let plan =
            crate::artifacts::engine::plan_pull(&claude_home, &state.sync_repo_path, &filter)?;
        crate::project_map::merge_skipped(&mut skipped_by_project, &plan.unmapped_projects);
        for desc in crate::artifacts::registry::REGISTRY {
            if !crate::artifacts::engine::is_category_enabled(desc, &filter) {
                println!("  {}: {}", desc.name, "disabled".dimmed());
                continue;
            }
            let differing = plan
                .overwrites
                .iter()
                .chain(plan.date_settles.iter())
                .chain(plan.local_only.iter())
                .chain(plan.deleted_here.iter())
                .chain(plan.creates.iter())
                .chain(plan.unions.iter())
                .chain(plan.mode_fixes.iter())
                .chain(plan.kept_local.iter())
                .filter(|w| w.category == desc.id)
                .count()
                + plan
                    .deletes
                    .iter()
                    .chain(plan.kept_local_deletes.iter())
                    .filter(|d| d.category == desc.id)
                    .count();
            if differing == 0 {
                println!("  {}: {}", desc.name, "in sync".green());
            } else {
                println!(
                    "  {}: {}",
                    desc.name,
                    format!("{differing} file(s) differ from sync repo").yellow()
                );
            }
        }
        // Skipped files (over the size limit, unreadable) are outside sync
        // but permanently divergent — "in sync" everywhere would hide that.
        if plan.skipped > 0 {
            println!(
                "  {}: {}",
                "skipped".yellow(),
                format!(
                    // Unmapped-project files DO increment plan.skipped
                    // (and are also warned per project) — the taxonomy
                    // must match the number.
                    "{} file(s) skipped (size limit, unreadable, refused name, or unmapped project) — see logs",
                    plan.skipped
                )
                .yellow()
            );
        }
    } else {
        println!(
            "  {}",
            "All categories disabled — enable with: claude-code-sync config --enable-artifacts <names|all>"
                .dimmed()
        );
    }

    for line in crate::project_map::skipped_project_warnings(
        &skipped_by_project,
        filter.warn_each_skipped_file,
    ) {
        log::warn!("{line}");
    }

    // Show files if requested
    if show_files {
        println!();
        println!("{}", "Local session files:".bold());
        for session in local_sessions.iter().take(20) {
            let relative = session
                .file_path
                .strip_prefix(&claude_dir)
                .unwrap_or(&session.file_path);
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

    // Held-back files (5ff1d62 push guard): only printed when explicitly
    // requested. Drives the `push --resurrect <path>` discovery flow
    // without performing a push. Read-only — does not touch the tracked
    // record or the repo.
    if held_back {
        println!();
        println!("{}", "Files the 5ff1d62 push guard would refuse:".bold());
        let held = crate::artifacts::engine::plan_held_back_remote_lost(
            &claude_dir,
            &state.sync_repo_path,
            &filter,
        )?;
        if held.is_empty() {
            println!("  (none)");
        } else {
            // Group by category for readability. HashMap (not BTreeMap)
            // because CategoryId does not implement Ord.
            use std::collections::HashMap;
            let mut by_cat: HashMap<crate::artifacts::registry::CategoryId, Vec<&Path>> =
                HashMap::new();
            for (cat, path) in &held {
                by_cat.entry(*cat).or_default().push(path.as_path());
            }
            for (cat, paths) in &by_cat {
                println!("  {}:", format!("{:?}", cat).bold());
                for path in paths {
                    println!("    {}", path.display());
                }
            }
            println!();
            println!(
                "To republish any of these on the next push, run:\n  \
                 claude-code-sync push --resurrect <path>"
            );
        }
    }

    Ok(())
}

/// The sync-repo transcripts a pull could not place on this machine, grouped
/// by the project they came from. `status` reports the same misses a pull
/// would warn about, so the two agree before anything is written.
fn sessions_without_local_project(
    filter: &FilterConfig,
    claude_dir: &Path,
    remote_projects_dir: &Path,
    remote_sessions: &[crate::parser::ConversationSession],
) -> crate::project_map::SkippedByProject {
    let mut skipped = crate::project_map::SkippedByProject::new();

    for session in remote_sessions {
        let relative = session
            .file_path
            .strip_prefix(remote_projects_dir)
            .unwrap_or(&session.file_path);
        let Some((project, _)) = crate::project_map::split_project_path(relative) else {
            continue;
        };
        let local_project = crate::project_map::local_project_dir(filter, claude_dir, project);
        if local_project.is_none() {
            skipped
                .entry(project.to_string())
                .or_default()
                .push(session.file_path.clone());
        }
    }

    skipped
}
