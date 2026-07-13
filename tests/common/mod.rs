//! Shared fixtures for the integration test binaries.

// Each `tests/*.rs` file is its own crate and compiles this module separately
// via its own `mod common;`. A helper used by only some of them is therefore
// genuinely unused in the others, and `clippy --all-targets -- -D warnings`
// would reject it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Points `CLAUDE_CODE_SYNC_CONFIG_DIR` at a fresh temp dir for the lifetime of
/// the guard, and unsets it on drop.
///
/// This replaces the previous convention of calling `env::set_var` at the top of
/// a test and `env::remove_var` as the last statement of the body. Those tests
/// return `Result<()>` and use `?` throughout, so *any* early return — not only
/// a panic — skipped the cleanup and left the variable pointing at a `TempDir`
/// that was about to be deleted. Every subsequent test in the same binary then
/// resolved its config against a path that no longer existed, which reads like
/// flaky CI rather than the bug it is. `Drop` runs on the early-return and
/// unwind paths both.
///
/// Note this *removes* the variable rather than restoring a previous value,
/// matching what the hand-written cleanup did.
///
/// `CLAUDE_CODE_SYNC_CONFIG_DIR` is honoured on every platform, unlike
/// `XDG_CONFIG_HOME`, which macOS ignores.
pub struct ConfigEnv {
    temp_dir: TempDir,
}

impl ConfigEnv {
    pub fn new() -> Self {
        let temp_dir = TempDir::new().expect("failed to create temp config dir");
        std::env::set_var("CLAUDE_CODE_SYNC_CONFIG_DIR", temp_dir.path());
        Self { temp_dir }
    }

    /// The isolated root — the value the environment variable points at.
    pub fn path(&self) -> &Path {
        self.temp_dir.path()
    }

    /// A path inside the isolated root.
    pub fn join(&self, name: &str) -> PathBuf {
        self.temp_dir.path().join(name)
    }

    /// `<root>/claude-code-sync`, created on demand — this is where
    /// `ConfigManager` actually reads and writes.
    pub fn config_dir(&self) -> PathBuf {
        let dir = self.temp_dir.path().join("claude-code-sync");
        std::fs::create_dir_all(&dir).expect("failed to create config dir");
        dir
    }

    /// Plant a `state.json` verbatim, for exercising the v1 and v2 on-disk formats.
    pub fn write_state_json(&self, contents: &str) -> PathBuf {
        let path = self.config_dir().join("state.json");
        std::fs::write(&path, contents).expect("failed to write state.json");
        path
    }
}

impl Default for ConfigEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ConfigEnv {
    fn drop(&mut self) {
        std::env::remove_var("CLAUDE_CODE_SYNC_CONFIG_DIR");
    }
}
