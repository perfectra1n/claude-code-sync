//! Config surface tests for artifact sync: CLI toggle plumbing and headless
//! InitConfig support. Uses the post-#89 isolation pattern: a dedicated
//! CLAUDE_CODE_SYNC_CONFIG_DIR per test, serialized because env vars are
//! process-global.

use claude_code_sync::filter::{update_config, FilterConfig};
use claude_code_sync::onboarding::InitConfig;
use serial_test::serial;
use tempfile::TempDir;

struct EnvGuard {
    prev: Option<String>,
}

impl EnvGuard {
    fn set(dir: &TempDir) -> Self {
        let prev = std::env::var("CLAUDE_CODE_SYNC_CONFIG_DIR").ok();
        std::env::set_var("CLAUDE_CODE_SYNC_CONFIG_DIR", dir.path());
        Self { prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var("CLAUDE_CODE_SYNC_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CODE_SYNC_CONFIG_DIR"),
        }
    }
}

fn update_artifacts(enable: Option<&str>, disable: Option<&str>) -> anyhow::Result<()> {
    update_config(
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        enable.map(str::to_string),
        disable.map(str::to_string),
    )
}

#[test]
#[serial]
fn test_enable_and_disable_artifact_categories_via_config() {
    let dir = TempDir::new().unwrap();
    let _guard = EnvGuard::set(&dir);

    update_artifacts(Some("settings,skills,prompt-history"), None).unwrap();
    let config = FilterConfig::load().unwrap();
    assert!(config.sync_artifacts.settings);
    assert!(config.sync_artifacts.skills);
    assert!(config.sync_artifacts.prompt_history);
    assert!(!config.sync_artifacts.plans);

    update_artifacts(None, Some("skills")).unwrap();
    let config = FilterConfig::load().unwrap();
    assert!(config.sync_artifacts.settings, "other toggles untouched");
    assert!(!config.sync_artifacts.skills);
}

#[test]
#[serial]
fn test_enable_all_shorthand() {
    let dir = TempDir::new().unwrap();
    let _guard = EnvGuard::set(&dir);

    update_artifacts(Some("all"), None).unwrap();
    let config = FilterConfig::load().unwrap();
    assert_eq!(
        config.sync_artifacts,
        claude_code_sync::artifacts::registry::ArtifactToggles::all_enabled()
    );
}

#[test]
#[serial]
fn test_unknown_category_name_errors_and_names_valid_ones() {
    let dir = TempDir::new().unwrap();
    let _guard = EnvGuard::set(&dir);

    let err = update_artifacts(Some("settings,nonsense"), None).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("nonsense"), "names the bad input: {msg}");
    assert!(msg.contains("prompt-history"), "lists valid names: {msg}");

    // Nothing was persisted from the failed call.
    let config = FilterConfig::load().unwrap();
    assert!(!config.sync_artifacts.settings);
}

#[test]
fn test_init_config_parses_sync_artifacts_table() {
    let toml_text = r#"
        repo_path = "/tmp/x"

        [sync_artifacts]
        settings = true
        skills = true
    "#;
    let config: InitConfig = toml::from_str(toml_text).unwrap();
    assert!(config.sync_artifacts.settings);
    assert!(config.sync_artifacts.skills);
    assert!(!config.sync_artifacts.todos);
}

#[test]
fn test_init_config_without_table_defaults_all_off() {
    let config: InitConfig = toml::from_str(r#"repo_path = "/tmp/x""#).unwrap();
    assert_eq!(config.sync_artifacts, Default::default());
}
