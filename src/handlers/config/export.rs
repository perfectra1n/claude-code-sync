//! `claude-code-sync config --export`: write the active configuration back out
//! as a `claude-code-sync-init.toml`.

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::PathBuf;

use crate::filter::FilterConfig;
use crate::onboarding::InitConfig;
use crate::sync::{MultiRepoState, SyncState};

/// Export current config as a claude-code-sync-init.toml in the current directory.
///
/// Reads the active sync state and filter config, builds an InitConfig, and writes
/// it to `./claude-code-sync-init.toml`.
pub fn handle_config_export() -> Result<()> {
    let filter = FilterConfig::load().context("Failed to load filter configuration")?;

    let repo_path = match SyncState::load() {
        Ok(state) => state.sync_repo_path.to_string_lossy().to_string(),
        Err(e) => {
            eprintln!(
                "{} Could not load sync state ({}), using default repo_path",
                "!".yellow(),
                e
            );
            "~/claude-code-sync-repo".to_string()
        }
    };

    let remote_url = match MultiRepoState::load() {
        Ok(ms) => match ms.repos.get(&ms.active_repo) {
            Some(repo) => repo.remote_url.clone(),
            None => {
                eprintln!(
                    "{} Active repo '{}' not found in multi-repo state, remote_url will be unset",
                    "!".yellow(),
                    ms.active_repo
                );
                None
            }
        },
        Err(e) => {
            eprintln!(
                "{} Could not load multi-repo state ({}), remote_url will be unset",
                "!".yellow(),
                e
            );
            None
        }
    };

    // InitConfig has no home for these, so say so rather than dropping them silently.
    if !filter.include_patterns.is_empty() {
        eprintln!(
            "{} include_patterns are not included in the export and will need to be reconfigured",
            "!".yellow()
        );
    }
    if !filter.exclude_patterns.is_empty() {
        eprintln!(
            "{} exclude_patterns are not included in the export and will need to be reconfigured",
            "!".yellow()
        );
    }

    let init_config = InitConfig {
        repo_path,
        remote_url: remote_url.clone(),
        clone: remote_url.is_some(),
        exclude_attachments: filter.exclude_attachments,
        exclude_older_than_days: filter.exclude_older_than_days,
        enable_lfs: filter.enable_lfs,
        scm_backend: filter.scm_backend,
        sync_subdirectory: filter.sync_subdirectory,
        use_project_name_only: filter.use_project_name_only,
        sync_artifacts: filter.sync_artifacts.clone(),
    };

    let content =
        toml::to_string_pretty(&init_config).context("Failed to serialize init config")?;

    let output_path = PathBuf::from("claude-code-sync-init.toml");
    std::fs::write(&output_path, content)
        .with_context(|| format!("Failed to write {}", output_path.display()))?;

    println!(
        "{} Exported to {}",
        "✓".green().bold(),
        output_path.display()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::RepoConfig;
    use serial_test::serial;
    use std::collections::HashMap;
    use tempfile::TempDir;

    /// Isolates a test from the developer's real config and working directory.
    ///
    /// `handle_config_export` writes to a *relative* path, so exercising it means
    /// mutating two pieces of process-global state: the config-dir override and
    /// the current directory. Restoring them in `Drop` rather than at the end of
    /// the test body means an assertion failure can't leave the whole harness
    /// pointed at a deleted temp dir.
    ///
    /// `CLAUDE_CODE_SYNC_CONFIG_DIR` is used rather than `XDG_CONFIG_HOME`
    /// because macOS ignores the latter.
    struct ExportEnv {
        config_dir: TempDir,
        work_dir: TempDir,
        original_dir: PathBuf,
    }

    impl ExportEnv {
        fn new() -> Self {
            let config_dir = TempDir::new().unwrap();
            let work_dir = TempDir::new().unwrap();
            let original_dir = std::env::current_dir().unwrap();

            std::env::set_var("CLAUDE_CODE_SYNC_CONFIG_DIR", config_dir.path());
            std::env::set_current_dir(work_dir.path()).unwrap();

            Self {
                config_dir,
                work_dir,
                original_dir,
            }
        }

        /// Write a v2 MultiRepoState to the config dir's state.json
        fn with_multi_repo_state(self, repo_path: &str, remote_url: Option<&str>) -> Self {
            let remote_url = remote_url.map(str::to_string);
            let mut repos = HashMap::new();
            repos.insert(
                "default".to_string(),
                RepoConfig {
                    name: "default".to_string(),
                    sync_repo_path: PathBuf::from(repo_path),
                    has_remote: remote_url.is_some(),
                    is_cloned_repo: false,
                    remote_url,
                    description: None,
                },
            );
            let state = MultiRepoState {
                version: 2,
                active_repo: "default".to_string(),
                repos,
            };

            let dir = self.claude_dir();
            std::fs::write(
                dir.join("state.json"),
                serde_json::to_string_pretty(&state).unwrap(),
            )
            .unwrap();
            self
        }

        /// Write a FilterConfig to the config dir's config.toml
        fn with_filter_config(self, filter: &FilterConfig) -> Self {
            let dir = self.claude_dir();
            std::fs::write(
                dir.join("config.toml"),
                toml::to_string_pretty(filter).unwrap(),
            )
            .unwrap();
            self
        }

        fn claude_dir(&self) -> PathBuf {
            let dir = self.config_dir.path().join("claude-code-sync");
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        /// Read back the exported TOML from the working directory.
        fn exported(&self) -> InitConfig {
            toml::from_str(&self.exported_raw()).unwrap()
        }

        fn exported_raw(&self) -> String {
            let path = self.work_dir.path().join("claude-code-sync-init.toml");
            assert!(path.exists(), "Exported file should exist");
            std::fs::read_to_string(&path).unwrap()
        }
    }

    impl Drop for ExportEnv {
        fn drop(&mut self) {
            std::env::remove_var("CLAUDE_CODE_SYNC_CONFIG_DIR");
            // Leave the cwd somewhere valid: `work_dir` is about to be deleted.
            let _ = std::env::set_current_dir(&self.original_dir);
        }
    }

    fn filter_with(scm_backend: &str, subdirectory: &str, days: Option<u32>) -> FilterConfig {
        FilterConfig {
            exclude_attachments: true,
            exclude_older_than_days: days,
            scm_backend: scm_backend.to_string(),
            sync_subdirectory: subdirectory.to_string(),
            use_project_name_only: true,
            ..Default::default()
        }
    }

    #[test]
    #[serial]
    fn test_export_with_full_state() {
        let env = ExportEnv::new()
            .with_multi_repo_state("/tmp/test-repo", Some("https://github.com/user/repo.git"))
            .with_filter_config(&FilterConfig {
                enable_lfs: true,
                ..filter_with("git", "my-projects", Some(30))
            });

        handle_config_export().expect("export should succeed");

        let exported = env.exported();
        assert_eq!(exported.repo_path, "/tmp/test-repo");
        assert_eq!(
            exported.remote_url.as_deref(),
            Some("https://github.com/user/repo.git")
        );
        assert!(
            exported.clone,
            "clone should be true when remote_url is set"
        );
        assert!(exported.exclude_attachments);
        assert_eq!(exported.exclude_older_than_days, Some(30));
        assert!(exported.enable_lfs);
        assert_eq!(exported.scm_backend, "git");
        assert_eq!(exported.sync_subdirectory, "my-projects");
        assert!(exported.use_project_name_only);
    }

    #[test]
    #[serial]
    fn test_export_without_remote_url() {
        let env = ExportEnv::new()
            .with_multi_repo_state("/tmp/local-repo", None)
            .with_filter_config(&FilterConfig::default());

        handle_config_export().unwrap();

        let exported = env.exported();
        assert_eq!(exported.repo_path, "/tmp/local-repo");
        assert!(exported.remote_url.is_none());
        assert!(!exported.clone, "clone should be false without remote_url");
    }

    #[test]
    #[serial]
    fn test_export_falls_back_when_no_state() {
        // No state.json: SyncState::load() and MultiRepoState::load() both fail,
        // and the export must still produce a usable file.
        let env = ExportEnv::new().with_filter_config(&FilterConfig::default());

        handle_config_export().expect("export should still succeed with fallback");

        let exported = env.exported();
        assert_eq!(exported.repo_path, "~/claude-code-sync-repo");
        assert!(exported.remote_url.is_none());
        assert!(!exported.clone);
    }

    #[test]
    #[serial]
    fn test_export_defaults_from_empty_filter() {
        // No config.toml: FilterConfig::load() returns defaults.
        let env = ExportEnv::new().with_multi_repo_state("/tmp/test-repo", None);

        handle_config_export().unwrap();

        let exported = env.exported();
        assert!(!exported.exclude_attachments);
        assert!(exported.exclude_older_than_days.is_none());
        assert!(!exported.enable_lfs);
        assert_eq!(exported.scm_backend, "git");
        assert_eq!(exported.sync_subdirectory, "projects");
        assert!(!exported.use_project_name_only);
    }

    #[test]
    #[serial]
    fn test_export_roundtrip_with_init_config() {
        let env = ExportEnv::new()
            .with_multi_repo_state(
                "/home/user/sync-repo",
                Some("git@github.com:user/history.git"),
            )
            .with_filter_config(&filter_with("mercurial", "conversations", Some(90)));

        handle_config_export().unwrap();

        // Parse straight from the raw text: the point is that what we wrote
        // survives a full serialize/deserialize round trip.
        let parsed: InitConfig = toml::from_str(&env.exported_raw()).unwrap();

        assert_eq!(parsed.repo_path, "/home/user/sync-repo");
        assert_eq!(
            parsed.remote_url.as_deref(),
            Some("git@github.com:user/history.git")
        );
        assert!(parsed.clone);
        assert!(parsed.exclude_attachments);
        assert_eq!(parsed.exclude_older_than_days, Some(90));
        assert!(!parsed.enable_lfs);
        assert_eq!(parsed.scm_backend, "mercurial");
        assert_eq!(parsed.sync_subdirectory, "conversations");
        assert!(parsed.use_project_name_only);
    }

    #[test]
    #[serial]
    fn test_export_output_file_is_valid_toml() {
        let env = ExportEnv::new()
            .with_multi_repo_state("/tmp/repo", None)
            .with_filter_config(&FilterConfig::default());

        handle_config_export().unwrap();

        let table: toml::Table = toml::from_str(&env.exported_raw()).unwrap();
        assert!(table.contains_key("repo_path"));
        assert!(table.contains_key("scm_backend"));
        assert!(table.contains_key("sync_subdirectory"));
    }
}
