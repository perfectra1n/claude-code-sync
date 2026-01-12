//! Onboarding and initialization handlers
//!
//! Handles the first-time setup flow including checking initialization
//! status and running the interactive onboarding process.

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::Path;

use crate::config;
use crate::filter;
use crate::onboarding::{self, InitConfig};
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

/// Run initialization from a config file (non-interactive).
///
/// This is used when:
/// - A config file is explicitly provided via `--config`
/// - A config file exists at a default location
/// - The environment variable `CLAUDE_CODE_SYNC_INIT_CONFIG` is set
pub fn run_init_from_config<P: AsRef<Path>>(config_path: Option<P>) -> Result<()> {
    // Load config from explicit path or default locations
    let init_config = if let Some(path) = config_path {
        log::info!("Loading init config from: {}", path.as_ref().display());
        InitConfig::load(path.as_ref())?
    } else {
        InitConfig::load_default()?
            .ok_or_else(|| anyhow::anyhow!("No init config file found"))?
    };

    println!(
        "{}",
        "📄 Initializing from config file...".cyan().bold()
    );

    // Convert to onboarding config
    let onboarding_config = init_config.to_onboarding_config()?;

    // Handle cloning if needed
    if onboarding_config.is_cloned {
        if let Some(ref remote_url) = onboarding_config.remote_url {
            println!("  {} {}", "Cloning from:".cyan(), remote_url);

            scm::clone(remote_url, &onboarding_config.repo_path)
                .context("Failed to clone repository")?;

            println!("{}", "  ✓ Repository cloned".green());
        }
    }

    // Initialize sync state
    sync::init_from_onboarding(
        &onboarding_config.repo_path,
        onboarding_config.remote_url.as_deref(),
        onboarding_config.is_cloned,
    )
    .context("Failed to initialize sync state")?;

    // Save filter configuration with all settings from init config
    let filter_config = filter::FilterConfig {
        exclude_attachments: init_config.exclude_attachments,
        exclude_older_than_days: init_config.exclude_older_than_days,
        enable_lfs: init_config.enable_lfs,
        scm_backend: init_config.scm_backend.clone(),
        sync_subdirectory: init_config.sync_subdirectory.clone(),
        ..Default::default()
    };
    filter_config
        .save()
        .context("Failed to save filter configuration")?;

    println!("{}", "✓ Initialization complete!".green().bold());
    println!("  {} {}", "Repo:".cyan(), onboarding_config.repo_path.display());
    if let Some(ref url) = onboarding_config.remote_url {
        println!("  {} {}", "Remote:".cyan(), url);
    }
    println!("  {} {}", "Backend:".cyan(), init_config.scm_backend);
    if init_config.enable_lfs {
        println!("  {} enabled", "LFS:".cyan());
    }
    println!();

    Ok(())
}

/// Try to run non-interactive initialization if a config file exists.
///
/// Returns Ok(true) if initialization was performed, Ok(false) if no config found.
pub fn try_init_from_config() -> Result<bool> {
    match InitConfig::load_default()? {
        Some(_) => {
            run_init_from_config::<&Path>(None)?;
            Ok(true)
        }
        None => Ok(false),
    }
}
