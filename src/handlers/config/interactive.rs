//! Interactive configuration menu: pick the settings to change, then edit only
//! those.
//!
//! Note the empty-input handling: here, submitting an empty value *clears* the
//! setting and says so. The wizard deliberately behaves differently — see
//! `wizard.rs`.

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Confirm, MultiSelect, Text};

use super::fields::{format_patterns, format_size_mb, parse_file_size_mb, parse_patterns};
use super::prompts::{current_age, display_config_summary, prompt_artifact_toggle_selection};
use crate::filter::FilterConfig;

/// Handle interactive configuration menu
///
/// Shows all configuration options and allows user to select which ones to modify
pub fn handle_config_interactive() -> Result<()> {
    println!("{}", "Interactive Configuration".cyan().bold());
    println!("{}", "=".repeat(80).cyan());
    println!();

    let current_config = FilterConfig::load().context("Failed to load current configuration")?;

    println!("{}", "Current Settings:".bold());
    display_config_summary(&current_config);
    println!();

    let options = vec![
        "Exclude older than (days)",
        "Include patterns",
        "Exclude patterns",
        "Exclude attachments",
        "Max file size",
        "Artifact sync categories",
    ];

    let selections = MultiSelect::new(
        "Select settings to modify (Space to select, Enter to confirm):",
        options,
    )
    .with_help_message("Use arrow keys to navigate, Space to select/deselect, Enter when done")
    .prompt()
    .context("Failed to get user selections")?;

    if selections.is_empty() {
        println!(
            "{}",
            "No settings selected. Configuration unchanged.".yellow()
        );
        return Ok(());
    }

    println!();
    println!("{}", "Modifying selected settings:".cyan().bold());
    println!();

    let mut modified_config = current_config.clone();

    for selection in selections {
        match selection {
            "Exclude older than (days)" => {
                let input = Text::new("Exclude older than (days):")
                    .with_help_message(&format!(
                        "Current: {}. Enter a number or leave empty to unset",
                        current_age(&modified_config)
                    ))
                    .prompt()?;

                if input.trim().is_empty() {
                    modified_config.exclude_older_than_days = None;
                    println!("  {} Unset exclude_older_than_days", "✓".green());
                } else {
                    let days: u32 = input
                        .trim()
                        .parse()
                        .context("Invalid number. Must be a positive integer.")?;
                    modified_config.exclude_older_than_days = Some(days);
                    println!(
                        "  {} Set exclude_older_than_days to {} days",
                        "✓".green(),
                        days
                    );
                }
            }

            "Include patterns" => {
                let input = Text::new("Include patterns (comma-separated):")
                    .with_help_message(&format!(
                        "Current: {}. Glob patterns like '*work*' or '/path/to/project'",
                        format_patterns(&modified_config.include_patterns, "None")
                    ))
                    .prompt()?;

                if input.trim().is_empty() {
                    modified_config.include_patterns = Vec::new();
                    println!("  {} Cleared include patterns", "✓".green());
                } else {
                    modified_config.include_patterns = parse_patterns(&input);
                    println!(
                        "  {} Set include patterns: {:?}",
                        "✓".green(),
                        modified_config.include_patterns
                    );
                }
            }

            "Exclude patterns" => {
                let input = Text::new("Exclude patterns (comma-separated):")
                    .with_help_message(&format!(
                        "Current: {}. Glob patterns like '*test*' or '/tmp/*'",
                        format_patterns(&modified_config.exclude_patterns, "None")
                    ))
                    .prompt()?;

                if input.trim().is_empty() {
                    modified_config.exclude_patterns = Vec::new();
                    println!("  {} Cleared exclude patterns", "✓".green());
                } else {
                    modified_config.exclude_patterns = parse_patterns(&input);
                    println!(
                        "  {} Set exclude patterns: {:?}",
                        "✓".green(),
                        modified_config.exclude_patterns
                    );
                }
            }

            "Exclude attachments" => {
                let current = modified_config.exclude_attachments;

                let exclude = Confirm::new("Exclude attachments (images, PDFs, etc.)?")
                    .with_default(current)
                    .with_help_message(&format!(
                        "Current: {current}. If yes, only .jsonl files will be synced"
                    ))
                    .prompt()?;

                modified_config.exclude_attachments = exclude;
                println!("  {} Set exclude_attachments to {}", "✓".green(), exclude);
            }

            "Artifact sync categories" => {
                modified_config.sync_artifacts =
                    prompt_artifact_toggle_selection(&modified_config.sync_artifacts)?;
            }

            "Max file size" => {
                let input = Text::new("Max file size (MB):")
                    .with_default(&format_size_mb(modified_config.max_file_size_bytes))
                    .with_help_message("Maximum size for individual files (e.g., 10 for 10MB)")
                    .prompt()?;

                modified_config.max_file_size_bytes = parse_file_size_mb(&input)?;
                println!(
                    "  {} Set max_file_size to {} MB",
                    "✓".green(),
                    format_size_mb(modified_config.max_file_size_bytes)
                );
            }

            _ => {}
        }
        println!();
    }

    println!("{}", "New Configuration:".cyan().bold());
    display_config_summary(&modified_config);
    println!();

    let confirm = Confirm::new("Save this configuration?")
        .with_default(true)
        .prompt()?;

    if confirm {
        modified_config
            .save()
            .context("Failed to save configuration")?;
        println!("\n{} Configuration saved successfully!", "✓".green().bold());
    } else {
        println!("\n{}", "Configuration not saved.".yellow());
    }

    Ok(())
}
