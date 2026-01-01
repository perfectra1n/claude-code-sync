//! Onboarding and initialization handlers
//!
//! Handles the first-time setup flow including checking initialization
//! status and running the interactive onboarding process.

use anyhow::{Context, Result};
use colored::Colorize;

use crate::config;
use crate::filter;
use crate::onboarding;
use crate::scm;
use crate::sync;

/// Check if claude-code-sync has been initialized
pub fn is_initialized() -> Result<bool> {
    let state_path = config::ConfigManager::state_file_path()?;
    Ok(state_path.exists())
}

/// Run the onboarding flow and initialize the system
pub fn run_onboarding_flow() -> Result<()> {
    // Run the interactive onboarding
    let onboarding_config =
        onboarding::run_onboarding().context("Onboarding cancelled or failed")?;

    // Handle cloning if needed
    if onboarding_config.is_cloned {
        if let Some(ref remote_url) = onboarding_config.remote_url {
            println!();
            println!("{}", "✓ Cloning repository...".cyan());

            scm::clone(remote_url, &onboarding_config.repo_path)
                .context("Failed to clone repository")?;

            println!("{}", "✓ Repository cloned successfully!".green());
        }
    }

    // Initialize sync state
    sync::init_from_onboarding(
        &onboarding_config.repo_path,
        onboarding_config.remote_url.as_deref(),
        onboarding_config.is_cloned,
    )
    .context("Failed to initialize sync state")?;

    // Save filter configuration
    let filter_config = filter::FilterConfig {
        exclude_attachments: onboarding_config.exclude_attachments,
        exclude_older_than_days: onboarding_config.exclude_older_than_days,
        ..Default::default()
    };
    filter_config
        .save()
        .context("Failed to save filter configuration")?;

    println!("{}", "✓ Ready to sync!".green().bold());
    println!();

    Ok(())
}
