//! Hardcoded never-sync rules for artifact copies.
//!
//! These are enforced at the single copy chokepoint in both directions: on
//! push nothing denied leaves the machine, and on pull a poisoned sync repo
//! cannot plant a denied file (or escape `~/.claude`). No user configuration
//! is consulted here — there is deliberately no way to override this list.

use std::path::Path;

/// Exact file names that must never sync, regardless of location.
const DENIED_FILE_NAMES: &[&str] = &[
    ".credentials.json",
    "settings.local.json",
    ".claude.json",
    "stats-cache.json",
    ".last-update-result.json",
    "mcp-needs-auth-cache.json",
];

/// File extensions that must never sync (key material).
const DENIED_EXTENSIONS: &[&str] = &["pem", "key"];

/// Name prefixes that must never sync (`.env*`, `daemon`, `daemon.lock`, ...).
const DENIED_NAME_PREFIXES: &[&str] = &[".env", "daemon"];

/// Directory names that are machine-local or cache-like; any path containing
/// one of these components is denied.
const DENIED_DIR_COMPONENTS: &[&str] = &[
    "shell-snapshots",
    "session-env",
    "file-history",
    "paste-cache",
    "cache",
    "debug",
    "statsig",
    "backups",
    "sessions",
];

/// Returns true when a `~/.claude`-relative path must never be copied by
/// artifact sync, checking every path component against the deny rules.
pub fn is_denied(rel_path: &Path) -> bool {
    for component in rel_path.components() {
        let Some(name) = component.as_os_str().to_str() else {
            // Non-UTF-8 component: refuse rather than guess.
            return true;
        };

        if DENIED_FILE_NAMES.contains(&name) || DENIED_DIR_COMPONENTS.contains(&name) {
            return true;
        }

        if DENIED_NAME_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            return true;
        }

        if let Some(ext) = Path::new(name).extension().and_then(|e| e.to_str()) {
            if DENIED_EXTENSIONS
                .iter()
                .any(|denied| ext.eq_ignore_ascii_case(denied))
            {
                return true;
            }
        }
    }

    false
}

/// Returns true when a repo-relative path is unsafe to restore: absolute,
/// or containing `..`/prefix components that could escape the target root.
pub fn is_unsafe_rel_path(rel_path: &Path) -> bool {
    use std::path::Component;

    rel_path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_denies_credential_files_anywhere() {
        assert!(is_denied(Path::new(".credentials.json")));
        assert!(is_denied(Path::new("skills/x/.credentials.json")));
        assert!(is_denied(Path::new("settings.local.json")));
        assert!(is_denied(Path::new("plans/settings.local.json")));
        assert!(is_denied(Path::new(".claude.json")));
    }

    #[test]
    fn test_denies_key_material_by_extension() {
        assert!(is_denied(Path::new("id_rsa.pem")));
        assert!(is_denied(Path::new("skills/deploy/host.key")));
        assert!(is_denied(Path::new("agents/x/cert.PEM"))); // case-insensitive
    }

    #[test]
    fn test_denies_env_and_daemon_prefixes() {
        assert!(is_denied(Path::new(".env")));
        assert!(is_denied(Path::new(".env.production")));
        assert!(is_denied(Path::new("skills/api/.env.local")));
        assert!(is_denied(Path::new("daemon.lock")));
        assert!(is_denied(Path::new("daemon.log")));
        assert!(is_denied(Path::new("daemon.status.json")));
        assert!(is_denied(Path::new("daemon/state.db")));
    }

    #[test]
    fn test_denies_machine_local_dirs_as_any_component() {
        assert!(is_denied(Path::new("shell-snapshots/x.sh")));
        assert!(is_denied(Path::new("session-env/abc/env")));
        assert!(is_denied(Path::new("file-history/f/1.txt")));
        assert!(is_denied(Path::new("paste-cache/p.bin")));
        assert!(is_denied(Path::new("statsig/flags.json")));
        assert!(is_denied(Path::new("plugins/cache/repo/x.md")));
        assert!(is_denied(Path::new("skills/my-skill/cache/blob")));
        assert!(is_denied(Path::new("debug/log.txt")));
        assert!(is_denied(Path::new("backups/old.json")));
        assert!(is_denied(Path::new("sessions/current")));
    }

    #[test]
    fn test_denies_stats_and_update_caches() {
        assert!(is_denied(Path::new("stats-cache.json")));
        assert!(is_denied(Path::new(".last-update-result.json")));
        assert!(is_denied(Path::new("mcp-needs-auth-cache.json")));
    }

    #[test]
    fn test_allows_legitimate_artifacts() {
        assert!(!is_denied(Path::new("settings.json")));
        assert!(!is_denied(Path::new("keybindings.json")));
        assert!(!is_denied(Path::new("CLAUDE.md")));
        assert!(!is_denied(Path::new("skills/my-skill/SKILL.md")));
        assert!(!is_denied(Path::new("skills/my-skill/references/notes.md")));
        assert!(!is_denied(Path::new("agents/reviewer.md")));
        assert!(!is_denied(Path::new("commands/deploy.md")));
        assert!(!is_denied(Path::new("plugins/installed_plugins.json")));
        assert!(!is_denied(Path::new("plugins/known_marketplaces.json")));
        assert!(!is_denied(Path::new("plans/2026-07-12-refactor.md")));
        assert!(!is_denied(Path::new("todos/session-123.json")));
        assert!(!is_denied(Path::new("history.jsonl")));
        // A file merely *containing* a denied word is fine
        assert!(!is_denied(Path::new("skills/env-setup/guide.md")));
        assert!(!is_denied(Path::new("plans/cache-strategy.md")));
        assert!(!is_denied(Path::new("skills/keyboard/keys.md")));
    }

    #[test]
    fn test_unsafe_rel_paths() {
        assert!(is_unsafe_rel_path(Path::new("../escape")));
        assert!(is_unsafe_rel_path(Path::new("skills/../../evil")));
        assert!(is_unsafe_rel_path(Path::new("/absolute/path")));
        assert!(!is_unsafe_rel_path(Path::new("skills/ok/file.md")));
        assert!(!is_unsafe_rel_path(Path::new("settings.json")));
    }
}
