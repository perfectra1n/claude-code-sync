//! Command handler modules
//!
//! This module contains all command handler functions extracted from main.rs,
//! organized by functionality area.

pub mod cleanup;
pub mod config;
pub mod history;
pub mod onboarding;
pub mod undo;

// Re-export all public handler functions for convenient use
pub use cleanup::handle_cleanup_snapshots;
pub use config::{handle_config_interactive, handle_config_wizard};
pub use history::{handle_history_clear, handle_history_last, handle_history_list, handle_history_review};
pub use onboarding::{is_initialized, run_onboarding_flow};
pub use undo::{handle_undo_pull, handle_undo_push};
