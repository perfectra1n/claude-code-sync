//! Undo command handlers
//!
//! Handles the undo pull and undo push commands, including preview
//! and confirmation dialogs when running interactively.

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::Confirm;

use crate::interactive_conflict;
use crate::sync;
use crate::undo;

/// Handle undo pull command
pub fn handle_undo_pull() -> Result<()> {
    println!("{}", "Preparing to undo last pull operation...".cyan());

    // Check if we're in an interactive terminal
    let is_interactive = interactive_conflict::is_interactive();

    if is_interactive {
        // Show preview
        let preview = undo::preview_undo_pull(None).context("Failed to preview undo operation")?;

        preview.display();

        // Ask for confirmation
        let confirm = Confirm::new("Do you want to proceed with this undo operation?")
            .with_default(false)
            .with_help_message("This will restore files to their pre-pull state")
            .prompt()
            .context("Failed to get confirmation")?;

        if !confirm {
            println!("\n{}", "Undo operation cancelled.".yellow());
            return Ok(());
        }
    }

    println!("\n{}", "Undoing last pull operation...".cyan());

    // Call undo_pull with None for both history_path and allowed_base_dir
    // This uses the default locations for production use
    let summary = undo::undo_pull(None, None).context("Failed to undo pull operation")?;

    println!("\n{}", "SUCCESS".green().bold());
    println!("{summary}");

    Ok(())
}

/// Handle undo push command
pub fn handle_undo_push() -> Result<()> {
    println!("{}", "Preparing to undo last push operation...".cyan());

    // Load sync state to get repository path
    let state = sync::SyncState::load()
        .context("Sync not initialized. Run 'claude-code-sync init' first.")?;

    // Check if we're in an interactive terminal
    let is_interactive = interactive_conflict::is_interactive();

    if is_interactive {
        // Show preview
        let preview = undo::preview_undo_push(None).context("Failed to preview undo operation")?;

        preview.display();

        // Ask for confirmation
        let confirm = Confirm::new("Do you want to proceed with this undo operation?")
            .with_default(false)
            .with_help_message("This will reset the git repository to the previous commit")
            .prompt()
            .context("Failed to get confirmation")?;

        if !confirm {
            println!("\n{}", "Undo operation cancelled.".yellow());
            return Ok(());
        }
    }

    println!("\n{}", "Undoing last push operation...".cyan());

    // Call undo_push with repository path and default history path
    let summary =
        undo::undo_push(&state.sync_repo_path, None).context("Failed to undo push operation")?;

    println!("\n{}", "SUCCESS".green().bold());
    println!("{summary}");

    Ok(())
}
