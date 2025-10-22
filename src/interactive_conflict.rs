use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Confirm, Select};
use std::path::{Path, PathBuf};

use crate::conflict::{Conflict, ConflictResolution};
use crate::parser::ConversationSession;

/// Resolution action chosen by the user
#[derive(Debug, Clone)]
pub enum ResolutionAction {
    /// Intelligently merge both versions (default/recommended)
    SmartMerge,
    /// Keep the local version and discard the remote changes
    KeepLocal,
    /// Keep the remote version and overwrite the local file
    KeepRemote,
    /// Keep both versions by saving the remote file with a conflict suffix
    KeepBoth,
    /// View detailed comparison of the conflicting files (does not resolve the conflict)
    ViewDetails,
}

impl std::fmt::Display for ResolutionAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolutionAction::SmartMerge => {
                write!(f, "Smart Merge (combine both versions - recommended)")
            }
            ResolutionAction::KeepLocal => write!(f, "Keep Local Version (discard remote)"),
            ResolutionAction::KeepRemote => write!(f, "Keep Remote Version (overwrite local)"),
            ResolutionAction::KeepBoth => {
                write!(f, "Keep Both (save remote with conflict suffix)")
            }
            ResolutionAction::ViewDetails => write!(f, "View Detailed Comparison"),
        }
    }
}

/// Result of interactive conflict resolution
#[derive(Debug)]
pub struct ResolutionResult {
    /// Conflicts resolved via smart merge
    pub smart_merge: Vec<Conflict>,
    /// Conflicts that should keep local version (discard remote)
    pub keep_local: Vec<Conflict>,
    /// Conflicts that should keep remote version (overwrite local)
    pub keep_remote: Vec<Conflict>,
    /// Conflicts that should keep both versions (rename remote)
    pub keep_both: Vec<Conflict>,
}

impl ResolutionResult {
    /// Creates a new empty ResolutionResult with all conflict vectors initialized
    pub fn new() -> Self {
        ResolutionResult {
            smart_merge: Vec::new(),
            keep_local: Vec::new(),
            keep_remote: Vec::new(),
            keep_both: Vec::new(),
        }
    }

    /// Total number of conflicts resolved
    pub fn total(&self) -> usize {
        self.smart_merge.len()
            + self.keep_local.len()
            + self.keep_remote.len()
            + self.keep_both.len()
    }
}

/// Check if we're running in an interactive terminal
pub fn is_interactive() -> bool {
    atty::is(atty::Stream::Stdin) && atty::is(atty::Stream::Stdout)
}

/// Display detailed conflict information
fn display_conflict_details(conflict: &Conflict) {
    println!("\n{}", "=".repeat(80).cyan());
    println!("{}", "Conflict Details".bold().cyan());
    println!("{}", "=".repeat(80).cyan());

    println!("\n{} {}", "Session ID:".bold(), conflict.session_id.cyan());

    println!(
        "\n{} {}",
        "Local File:".bold().green(),
        conflict.local_file.display()
    );
    println!(
        "  {} messages",
        conflict.local_message_count.to_string().green()
    );
    if let Some(ts) = &conflict.local_timestamp {
        println!("  Last updated: {}", ts.dimmed());
    }
    println!("  Content hash: {}", &conflict.local_hash[..16].dimmed());

    println!(
        "\n{} {}",
        "Remote File:".bold().yellow(),
        conflict.remote_file.display()
    );
    println!(
        "  {} messages",
        conflict.remote_message_count.to_string().yellow()
    );
    if let Some(ts) = &conflict.remote_timestamp {
        println!("  Last updated: {}", ts.dimmed());
    }
    println!("  Content hash: {}", &conflict.remote_hash[..16].dimmed());

    // Highlight the differences
    let msg_diff = conflict.remote_message_count as i32 - conflict.local_message_count as i32;
    if msg_diff > 0 {
        println!(
            "\n{} Remote has {} more messages",
            "→".yellow(),
            msg_diff.to_string().yellow().bold()
        );
    } else if msg_diff < 0 {
        println!(
            "\n{} Local has {} more messages",
            "→".green(),
            (-msg_diff).to_string().green().bold()
        );
    } else {
        println!(
            "\n{} Both have the same number of messages, but content differs",
            "→".cyan()
        );
    }

    println!("{}", "=".repeat(80).cyan());
}

/// Interactively resolve a single conflict
fn resolve_conflict_interactive(conflict: &Conflict) -> Result<ResolutionAction> {
    loop {
        println!("\n{}", "Conflict Detected!".yellow().bold());
        println!("  {}", conflict.description().dimmed());

        let options = vec![
            ResolutionAction::SmartMerge,
            ResolutionAction::KeepLocal,
            ResolutionAction::KeepRemote,
            ResolutionAction::KeepBoth,
            ResolutionAction::ViewDetails,
        ];

        let action = Select::new("How would you like to resolve this conflict?", options)
            .with_help_message("Use arrow keys to navigate, Enter to select")
            .prompt()
            .context("Failed to get resolution action")?;

        match action {
            ResolutionAction::ViewDetails => {
                display_conflict_details(conflict);
                // Loop back to ask again
                continue;
            }
            _ => return Ok(action),
        }
    }
}

/// Interactively resolve all conflicts
///
/// This function presents each conflict to the user one at a time,
/// allowing them to choose how to resolve it.
///
/// # Arguments
/// * `conflicts` - Mutable slice of conflicts to resolve
/// * `local_sessions` - Optional map of local sessions (for smart merge)
/// * `remote_sessions` - Optional map of remote sessions (for smart merge)
///
/// # Returns
/// A `ResolutionResult` containing the categorized conflicts
pub fn resolve_conflicts_interactive_with_sessions(
    conflicts: &mut [Conflict],
    local_sessions: Option<&std::collections::HashMap<String, &ConversationSession>>,
    remote_sessions: Option<&std::collections::HashMap<String, &ConversationSession>>,
) -> Result<ResolutionResult> {
    if conflicts.is_empty() {
        return Ok(ResolutionResult::new());
    }

    let total_conflicts = conflicts.len();

    println!(
        "\n{}",
        format!("Found {} conflicts to resolve", total_conflicts)
            .yellow()
            .bold()
    );
    println!("{}", "Let's resolve them one by one...".cyan());

    let mut result = ResolutionResult::new();

    for (idx, conflict) in conflicts.iter_mut().enumerate() {
        println!(
            "\n{} Conflict {} of {}",
            ">>>".yellow().bold(),
            (idx + 1).to_string().cyan(),
            total_conflicts.to_string().cyan()
        );

        let action = resolve_conflict_interactive(conflict)?;

        match action {
            ResolutionAction::SmartMerge => {
                // Attempt smart merge
                if let (Some(local_map), Some(remote_map)) = (local_sessions, remote_sessions) {
                    if let (Some(&local_session), Some(&remote_session)) = (
                        local_map.get(&conflict.session_id),
                        remote_map.get(&conflict.session_id),
                    ) {
                        match conflict.try_smart_merge(local_session, remote_session) {
                            Ok(()) => {
                                if let ConflictResolution::SmartMerge { ref stats, .. } =
                                    conflict.resolution
                                {
                                    println!(
                                        "  {} Smart merged ({} local + {} remote = {} total, {} branches)",
                                        "✓".green(),
                                        stats.local_messages,
                                        stats.remote_messages,
                                        stats.merged_messages,
                                        stats.branches_detected
                                    );
                                }
                                result.smart_merge.push(conflict.clone());
                            }
                            Err(e) => {
                                eprintln!("  {} Smart merge failed: {}", "✗".red(), e);
                                eprintln!("  Please choose another resolution method...");
                                // Don't add to result, user will be prompted again
                                continue;
                            }
                        }
                    } else {
                        eprintln!("  {} Cannot find local or remote session", "✗".red());
                        eprintln!("  Please choose another resolution method...");
                        continue;
                    }
                } else {
                    eprintln!("  {} Session maps not provided", "✗".red());
                    eprintln!("  Please choose another resolution method...");
                    continue;
                }
            }
            ResolutionAction::KeepLocal => {
                println!("  {} Keeping local version", "✓".green());
                conflict.resolution = ConflictResolution::KeepLocal;
                result.keep_local.push(conflict.clone());
            }
            ResolutionAction::KeepRemote => {
                println!(
                    "  {} Keeping remote version (will overwrite local)",
                    "✓".yellow()
                );
                conflict.resolution = ConflictResolution::KeepRemote;
                result.keep_remote.push(conflict.clone());
            }
            ResolutionAction::KeepBoth => {
                println!(
                    "  {} Keeping both versions (remote will be saved with conflict suffix)",
                    "✓".cyan()
                );
                // Keep both is handled later with proper renaming
                result.keep_both.push(conflict.clone());
            }
            ResolutionAction::ViewDetails => {
                unreachable!("ViewDetails should be handled in the loop")
            }
        }
    }

    println!("\n{}", "=".repeat(80).green());
    println!("{}", "Resolution Summary".bold().green());
    println!("{}", "=".repeat(80).green());
    println!(
        "  Smart Merge: {}",
        result.smart_merge.len().to_string().cyan()
    );
    println!(
        "  Keep Local:  {}",
        result.keep_local.len().to_string().green()
    );
    println!(
        "  Keep Remote: {}",
        result.keep_remote.len().to_string().yellow()
    );
    println!(
        "  Keep Both:   {}",
        result.keep_both.len().to_string().cyan()
    );
    println!("{}", "=".repeat(80).green());

    // Final confirmation
    let confirm = Confirm::new("Apply these resolutions?")
        .with_default(true)
        .prompt()
        .context("Failed to get confirmation")?;

    if !confirm {
        return Err(anyhow::anyhow!(
            "Resolution cancelled by user. No changes were made."
        ));
    }

    Ok(result)
}

/// Backward-compatible version of resolve_conflicts_interactive without session maps
///
/// This version doesn't support SmartMerge since it requires session data.
/// Use `resolve_conflicts_interactive_with_sessions` for full functionality.
pub fn resolve_conflicts_interactive(conflicts: &mut [Conflict]) -> Result<ResolutionResult> {
    resolve_conflicts_interactive_with_sessions(conflicts, None, None)
}

/// Apply the resolution results by copying/writing files
///
/// # Arguments
/// * `result` - The resolution result containing categorized conflicts
/// * `remote_sessions` - All remote sessions (to find the ones we need)
/// * `claude_dir` - The Claude projects directory
/// * `_remote_projects_dir` - The remote sync repository projects directory (unused)
///
/// # Returns
/// List of (original_path, renamed_path) tuples for conflicts kept as both
pub fn apply_resolutions(
    result: &ResolutionResult,
    remote_sessions: &[ConversationSession],
    claude_dir: &Path,
    _remote_projects_dir: &Path,
) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut renames = Vec::new();

    // Handle "smart merge" - write merged entries to local file
    for conflict in &result.smart_merge {
        if let ConflictResolution::SmartMerge {
            ref merged_entries, ..
        } = conflict.resolution
        {
            // Create a session with merged entries
            let merged_session = ConversationSession {
                session_id: conflict.session_id.clone(),
                entries: merged_entries.clone(),
                file_path: conflict.local_file.to_string_lossy().to_string(),
            };

            // Write to local file
            merged_session
                .write_to_file(&conflict.local_file)
                .with_context(|| {
                    format!(
                        "Failed to write smart merged file: {}",
                        conflict.local_file.display()
                    )
                })?;

            println!(
                "  {} Wrote smart merged conversation: {}",
                "✓".cyan(),
                conflict.local_file.display()
            );
        }
    }

    // Handle "keep remote" - overwrite local with remote
    for conflict in &result.keep_remote {
        // Find the remote session
        if let Some(remote_session) = remote_sessions
            .iter()
            .find(|s| s.session_id == conflict.session_id)
        {
            // Write remote session to local path (overwrite)
            remote_session
                .write_to_file(&conflict.local_file)
                .with_context(|| {
                    format!(
                        "Failed to overwrite local file with remote: {}",
                        conflict.local_file.display()
                    )
                })?;

            println!(
                "  {} Overwrote local with remote: {}",
                "✓".yellow(),
                conflict.local_file.display()
            );
        }
    }

    // Handle "keep both" - save remote with conflict suffix
    for conflict in &result.keep_both {
        let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let conflict_suffix = format!("conflict-{}", timestamp);

        let renamed_path = conflict
            .clone()
            .resolve_keep_both(&conflict_suffix)
            .with_context(|| format!("Failed to resolve keep_both for {}", conflict.session_id))?;

        // Find and write the remote session to the renamed path
        if let Some(remote_session) = remote_sessions
            .iter()
            .find(|s| s.session_id == conflict.session_id)
        {
            remote_session
                .write_to_file(&renamed_path)
                .with_context(|| {
                    format!(
                        "Failed to write remote conflict version: {}",
                        renamed_path.display()
                    )
                })?;

            let relative_renamed = renamed_path
                .strip_prefix(claude_dir)
                .unwrap_or(&renamed_path);
            println!(
                "  {} Saved remote as: {}",
                "✓".cyan(),
                relative_renamed.display()
            );

            renames.push((conflict.remote_file.clone(), renamed_path));
        }
    }

    // "keep local" requires no action - we simply don't copy the remote file

    Ok(renames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolution_result() {
        let result = ResolutionResult::new();
        assert_eq!(result.total(), 0);
        assert_eq!(result.keep_local.len(), 0);
        assert_eq!(result.keep_remote.len(), 0);
        assert_eq!(result.keep_both.len(), 0);
    }

    #[test]
    fn test_display_resolution_action() {
        let action = ResolutionAction::KeepLocal;
        assert_eq!(action.to_string(), "Keep Local Version (discard remote)");

        let action = ResolutionAction::KeepRemote;
        assert_eq!(action.to_string(), "Keep Remote Version (overwrite local)");

        let action = ResolutionAction::KeepBoth;
        assert_eq!(
            action.to_string(),
            "Keep Both (save remote with conflict suffix)"
        );
    }
}
