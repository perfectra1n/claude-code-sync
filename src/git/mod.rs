//! Git repository operations for conversation history management.
//!
//! Provides a high-level interface to libgit2 for common Git operations including
//! initializing repositories, cloning, committing, pushing, pulling, and fetching.
//! Handles authentication via Git credential helpers and SSH agents.

mod branches;
mod credentials;
mod manager;
mod operations;
mod remote;

// Re-export the main GitManager type
pub use manager::GitManager;
