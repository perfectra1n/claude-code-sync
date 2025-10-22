//! # claude-sync
//!
//! A command-line tool for synchronizing Claude Code conversation history with Git repositories.
//!
//! ## Overview
//!
//! `claude-sync` enables you to back up, version control, and share your Claude Code conversation
//! history across multiple machines. It works by syncing conversation files (stored as JSONL) from
//! your local Claude Code projects directory (`~/.claude/projects/`) to a Git repository, which
//! can be pushed to a remote server for backup or collaboration.
//!
//! ## Key Features
//!
//! - **Bidirectional sync**: Push local conversations to a Git repository and pull remote changes
//! - **Conflict detection**: Automatically detects when the same conversation has diverged
//! - **Interactive resolution**: Provides a TUI for resolving conflicts interactively
//! - **Filtering**: Exclude attachments, old conversations, or specific projects
//! - **Operation history**: Track all sync operations with automatic snapshots for undo
//! - **Cross-platform**: Supports Linux, macOS, and Windows with platform-specific config directories
//!
//! ## Architecture
//!
//! The library is organized into modules that handle different aspects of the sync process:
//!
//! - Configuration and state management ([`config`], [`filter`])
//! - Git operations ([`git`])
//! - Conversation parsing and analysis ([`parser`])
//! - Conflict detection and resolution ([`conflict`])
//! - Operation tracking and undo ([`history`], [`undo`])
//! - User interface and reporting ([`onboarding`], [`report`], [`logger`])
//! - Core synchronization logic ([`sync`])

/// Platform-agnostic configuration directory management for claude-sync.
///
/// Provides utilities for locating and managing configuration files and directories
/// following platform conventions (XDG on Linux, Application Support on macOS,
/// AppData on Windows).
pub mod config;

/// Conflict detection and resolution for conversation synchronization.
///
/// Detects when the same conversation has diverged between local and remote copies
/// by comparing content hashes. Provides strategies for resolving conflicts including
/// keeping both versions (with automatic renaming), keeping local, or keeping remote.
pub mod conflict;

/// Interactive terminal-based conflict resolution interface.
///
/// Provides a user-friendly TUI for resolving sync conflicts interactively. Users can
/// view detailed comparisons of conflicting conversations and choose resolution strategies
/// (keep local, keep remote, or keep both) on a per-conflict basis.
pub mod interactive_conflict;

/// File filtering configuration for selective synchronization.
///
/// Controls which conversation files are included in sync operations based on
/// criteria such as file age, path patterns, file size, and file type (e.g.,
/// excluding attachments to sync only JSONL conversation files).
pub mod filter;

/// Git repository operations for conversation history management.
///
/// Provides a high-level interface to libgit2 for common Git operations including
/// initializing repositories, cloning, committing, pushing, pulling, and fetching.
/// Handles authentication via Git credential helpers and SSH agents.
pub mod git;

/// Operation history tracking and persistence.
///
/// Records all sync operations (push and pull) with metadata about affected
/// conversations. Maintains a rolling history of recent operations with automatic
/// rotation. Each operation record includes a snapshot path for undo functionality.
pub mod history;

/// Logging configuration and utilities.
///
/// Sets up dual logging to both console (configurable via `RUST_LOG` environment
/// variable) and a persistent log file in the config directory. Includes automatic
/// log rotation when files exceed size limits.
pub mod logger;

/// Smart merge functionality for combining divergent conversation branches.
///
/// Provides intelligent merging of conversation sessions by analyzing message UUIDs,
/// parent relationships, and timestamps. Can handle non-overlapping messages, edited
/// messages (resolved by timestamp), conversation branches (all branches preserved),
/// and entries without UUIDs (merged by timestamp).
pub mod merge;

/// Interactive onboarding flow for first-time setup.
///
/// Guides users through initial configuration including repository setup (clone vs local),
/// remote URL configuration, and filter preferences. Uses terminal UI prompts to collect
/// user preferences and validates inputs before saving configuration.
pub mod onboarding;

/// JSONL conversation file parsing and serialization.
///
/// Parses Claude Code conversation files (JSONL format) into structured data.
/// Each conversation session contains multiple entries (user messages, assistant responses,
/// file snapshots, etc.) with metadata like timestamps, UUIDs, and session IDs.
pub mod parser;

/// Conflict report generation and formatting.
///
/// Generates detailed reports of sync conflicts in multiple formats (JSON, Markdown, console).
/// Reports include information about diverged conversations, message counts, timestamps,
/// and resolution strategies applied during the last sync operation.
pub mod report;

/// Core synchronization logic for pushing and pulling conversation history.
///
/// Implements the main sync operations:
/// - **Push**: Copies local conversations to the sync repository and commits changes
/// - **Pull**: Merges remote conversations into local storage with conflict detection
/// - **Sync**: Bidirectional operation that pulls then pushes for full synchronization
///
/// Includes state management, session discovery, conflict handling, and operation tracking.
pub mod sync;

/// Snapshot-based undo functionality for sync operations.
///
/// Creates point-in-time snapshots of conversation files before sync operations.
/// Snapshots enable undoing pull operations (by restoring files) and push operations
/// (by resetting Git commits). Includes validation and security checks for safe restoration.
pub mod undo;
