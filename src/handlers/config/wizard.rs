//! Wizard-mode configuration: walk every setting in order, gating each one
//! behind a yes/no question.
//!
//! This is intentionally not the same UX as `interactive.rs`. The wizard asks
//! "Do you want to ...?" first and only prompts for a value on yes, and it has
//! no "empty clears the setting" affordance — an empty pattern list here simply
//! parses to `[]` and is reported as such. Both flows share the value logic in
//! `fields.rs`, not the prompt shape.

use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Confirm, Text};

use super::fields::{format_patterns, format_size_mb, parse_file_size_mb, parse_patterns};
use super::prompts::{current_age, display_config_summary, prompt_artifact_toggle_selection};
use crate::filter::FilterConfig;

/// Handle wizard-mode configuration
///
/// Steps through each configuration option one by one
pub fn handle_config_wizard() -> Result<()> {
    println!("{}", "Configuration Wizard".cyan().bold());
    println!("{}", "=".repeat(80).cyan());
    println!();
    println!(
        "{}",
        "This wizard will walk you through all configuration options.".dimmed()
    );
    println!(
        "{}",
        "Press Enter to keep current value or enter a new value.".dimmed()
    );
    println!();

    let current_config = FilterConfig::load().context("Failed to load current configuration")?;
    let mut modified_config = current_config.clone();

    // 1. Exclude older than
    println!("{}", "1. Age Filter".bold().cyan());
    println!("   Current: {}", current_age(&modified_config).yellow());

    let exclude_old =
        Confirm::new("Do you want to exclude projects older than a certain number of days?")
            .with_default(modified_config.exclude_older_than_days.is_some())
            .prompt()?;

    if exclude_old {
        let default_days = modified_config
            .exclude_older_than_days
            .unwrap_or(30)
            .to_string();
        let input = Text::new("How many days?")
            .with_default(&default_days)
            .prompt()?;

        let days: u32 = input
            .trim()
            .parse()
            .context("Invalid number. Must be a positive integer.")?;
        modified_config.exclude_older_than_days = Some(days);
        println!(
            "  {} Will exclude projects older than {} days\n",
            "✓".green(),
            days
        );
    } else {
        modified_config.exclude_older_than_days = None;
        println!("  {} Age filter disabled\n", "✓".green());
    }

    // 2. Include patterns
    println!("{}", "2. Include Patterns".bold().cyan());
    println!(
        "   Current: {}",
        format_patterns(
            &modified_config.include_patterns,
            "None (all projects included)"
        )
        .yellow()
    );

    let use_include = Confirm::new("Do you want to limit sync to specific project patterns?")
        .with_default(!modified_config.include_patterns.is_empty())
        .with_help_message("Example: *work*, /home/user/important/*")
        .prompt()?;

    if use_include {
        let default = modified_config.include_patterns.join(", ");
        let input = Text::new("Enter include patterns (comma-separated):")
            .with_default(&default)
            .with_help_message("Glob patterns like '*work*' or '/specific/path'")
            .prompt()?;

        modified_config.include_patterns = parse_patterns(&input);
        println!(
            "  {} Include patterns set: {:?}\n",
            "✓".green(),
            modified_config.include_patterns
        );
    } else {
        modified_config.include_patterns = Vec::new();
        println!("  {} All projects will be included\n", "✓".green());
    }

    // 3. Exclude patterns
    println!("{}", "3. Exclude Patterns".bold().cyan());
    println!(
        "   Current: {}",
        format_patterns(&modified_config.exclude_patterns, "None").yellow()
    );

    let use_exclude = Confirm::new("Do you want to exclude specific project patterns?")
        .with_default(!modified_config.exclude_patterns.is_empty())
        .with_help_message("Example: *test*, *tmp*, /temp/*")
        .prompt()?;

    if use_exclude {
        let default = modified_config.exclude_patterns.join(", ");
        let input = Text::new("Enter exclude patterns (comma-separated):")
            .with_default(&default)
            .with_help_message("Glob patterns like '*test*' or '/tmp/*'")
            .prompt()?;

        modified_config.exclude_patterns = parse_patterns(&input);
        println!(
            "  {} Exclude patterns set: {:?}\n",
            "✓".green(),
            modified_config.exclude_patterns
        );
    } else {
        modified_config.exclude_patterns = Vec::new();
        println!("  {} No exclusion patterns\n", "✓".green());
    }

    // 4. Exclude attachments
    println!("{}", "4. File Type Filter".bold().cyan());
    println!(
        "   Current: {}",
        if modified_config.exclude_attachments {
            "Exclude attachments".yellow()
        } else {
            "Include all files".yellow()
        }
    );

    let exclude_attachments = Confirm::new("Exclude attachments (images, PDFs, etc.)?")
        .with_default(modified_config.exclude_attachments)
        .with_help_message("If yes, only .jsonl conversation files will be synced")
        .prompt()?;

    modified_config.exclude_attachments = exclude_attachments;
    println!(
        "  {} Attachments will be {}\n",
        "✓".green(),
        if exclude_attachments {
            "excluded"
        } else {
            "included"
        }
    );

    // 5. Max file size
    println!("{}", "5. File Size Limit".bold().cyan());
    println!(
        "   Current: {} MB",
        format_size_mb(modified_config.max_file_size_bytes)
    );

    let change_size = Confirm::new("Do you want to change the maximum file size limit?")
        .with_default(false)
        .prompt()?;

    if change_size {
        let input = Text::new("Max file size (MB):")
            .with_default(&format_size_mb(modified_config.max_file_size_bytes))
            .prompt()?;

        modified_config.max_file_size_bytes = parse_file_size_mb(&input)?;
        println!(
            "  {} Max file size set to {} MB\n",
            "✓".green(),
            format_size_mb(modified_config.max_file_size_bytes)
        );
    } else {
        println!("  {} Keeping current max file size\n", "✓".green());
    }

    // Artifact sync categories
    let change_artifacts =
        Confirm::new("Configure artifact sync categories (settings, skills, agents, ...)?")
            .with_default(false)
            .prompt()?;
    if change_artifacts {
        modified_config.sync_artifacts =
            prompt_artifact_toggle_selection(&modified_config.sync_artifacts)?;
    }

    // Summary and confirmation
    println!("{}", "=".repeat(80).cyan());
    println!("{}", "Configuration Summary:".bold().cyan());
    println!("{}", "=".repeat(80).cyan());
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
