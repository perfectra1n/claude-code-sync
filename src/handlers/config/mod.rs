//! Configuration command handlers.
//!
//! Split by concern rather than by command: `fields` holds the pure value logic
//! (parsing, formatting) with no I/O, `prompts` holds the terminal interaction
//! the two editing modes share, and the remaining modules are one command each.
//!
//! `interactive` and `wizard` deliberately keep separate prompt flows — they ask
//! for the same six settings in genuinely different ways — but they no longer
//! keep separate copies of the logic that turns what the user typed into a value.

mod export;
mod fields;
mod interactive;
mod prompts;
mod repo_select;
mod wizard;

pub use export::handle_config_export;
pub use interactive::handle_config_interactive;
pub use repo_select::handle_repo_selector;
pub use wizard::handle_config_wizard;
