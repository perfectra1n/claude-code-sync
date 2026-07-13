//! The category registry: one row per syncable artifact kind. Everything the
//! engine does — collecting sources, choosing repo destinations, picking a
//! merge strategy — is driven by this data.

use serde::{Deserialize, Serialize};

/// Identifies one syncable artifact category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CategoryId {
    Settings,
    Memory,
    Skills,
    Agents,
    Commands,
    Plugins,
    Plans,
    Todos,
    PromptHistory,
}

/// What to copy, relative to `~/.claude`. Missing sources are treated as
/// empty, never as an error — a machine may simply not have e.g. `todos/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceSpec {
    /// Exact files. Anything not listed is structurally unreachable — this is
    /// how `plugins/` syncs its two manifests but never `cache/`.
    Files(&'static [&'static str]),
    /// A directory walked recursively (symlinks not followed).
    Dir(&'static str),
}

/// How pull reconciles a repo copy with the local file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Remote bytes win (only written when they differ; snapshot first).
    RawOverwrite,
    /// Line-wise union of both sides (append-only JSONL like history.jsonl);
    /// applied on push and pull so machines converge grow-only.
    UnionJsonl,
}

/// One registry row.
#[derive(Debug)]
pub struct CategoryDescriptor {
    pub id: CategoryId,
    /// Config key / CLI name (kebab-case).
    pub name: &'static str,
    /// Subdirectory under the sync repo's `artifacts/` root.
    pub repo_subdir: &'static str,
    pub source: SourceSpec,
    pub merge: MergeStrategy,
    /// Short human description for wizard / config --show.
    pub description: &'static str,
}

/// Root directory inside the sync repository for all artifact categories.
/// Deliberately a constant: a fixed category→path mapping keeps pull
/// deterministic and the deny-guarantee auditable.
pub const ARTIFACTS_SUBDIR: &str = "artifacts";

pub static REGISTRY: &[CategoryDescriptor] = &[
    CategoryDescriptor {
        id: CategoryId::Settings,
        name: "settings",
        repo_subdir: "settings",
        source: SourceSpec::Files(&["settings.json", "keybindings.json"]),
        merge: MergeStrategy::RawOverwrite,
        description: "User settings and keybindings (never settings.local.json)",
    },
    CategoryDescriptor {
        id: CategoryId::Memory,
        name: "memory",
        repo_subdir: "memory",
        source: SourceSpec::Files(&["CLAUDE.md"]),
        merge: MergeStrategy::RawOverwrite,
        description: "Global CLAUDE.md user memory",
    },
    CategoryDescriptor {
        id: CategoryId::Skills,
        name: "skills",
        repo_subdir: "skills",
        source: SourceSpec::Dir("skills"),
        merge: MergeStrategy::RawOverwrite,
        description: "Custom skills",
    },
    CategoryDescriptor {
        id: CategoryId::Agents,
        name: "agents",
        repo_subdir: "agents",
        source: SourceSpec::Dir("agents"),
        merge: MergeStrategy::RawOverwrite,
        description: "Custom subagent definitions",
    },
    CategoryDescriptor {
        id: CategoryId::Commands,
        name: "commands",
        repo_subdir: "commands",
        source: SourceSpec::Dir("commands"),
        merge: MergeStrategy::RawOverwrite,
        description: "Custom slash commands",
    },
    CategoryDescriptor {
        id: CategoryId::Plugins,
        name: "plugins",
        repo_subdir: "plugins",
        source: SourceSpec::Files(&[
            "plugins/installed_plugins.json",
            "plugins/known_marketplaces.json",
        ]),
        merge: MergeStrategy::RawOverwrite,
        description: "Installed-plugin and marketplace manifests (never plugin caches)",
    },
    CategoryDescriptor {
        id: CategoryId::Plans,
        name: "plans",
        repo_subdir: "plans",
        source: SourceSpec::Dir("plans"),
        merge: MergeStrategy::RawOverwrite,
        description: "Plan-mode documents (may contain sensitive prose)",
    },
    CategoryDescriptor {
        id: CategoryId::Todos,
        name: "todos",
        repo_subdir: "todos",
        source: SourceSpec::Dir("todos"),
        merge: MergeStrategy::RawOverwrite,
        description: "Session task lists (changes frequently)",
    },
    CategoryDescriptor {
        id: CategoryId::PromptHistory,
        name: "prompt-history",
        repo_subdir: "prompt-history",
        source: SourceSpec::Files(&["history.jsonl"]),
        merge: MergeStrategy::UnionJsonl,
        description: "Cross-project prompt history (union-merged, grow-only)",
    },
];

/// Per-category enable switches, stored in the `[sync_artifacts]` table of
/// config.toml. Every field defaults to false so configs written by older
/// versions parse unchanged and existing users opt in explicitly.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactToggles {
    #[serde(default)]
    pub settings: bool,
    #[serde(default)]
    pub memory: bool,
    #[serde(default)]
    pub skills: bool,
    #[serde(default)]
    pub agents: bool,
    #[serde(default)]
    pub commands: bool,
    #[serde(default)]
    pub plugins: bool,
    #[serde(default)]
    pub plans: bool,
    #[serde(default)]
    pub todos: bool,
    #[serde(default)]
    pub prompt_history: bool,
}

impl ArtifactToggles {
    /// A toggles value with every category enabled (used by onboarding
    /// defaults and the `all` CLI shorthand).
    pub fn all_enabled() -> Self {
        Self {
            settings: true,
            memory: true,
            skills: true,
            agents: true,
            commands: true,
            plugins: true,
            plans: true,
            todos: true,
            prompt_history: true,
        }
    }

    /// Whether any category is enabled at all.
    pub fn any_enabled(&self) -> bool {
        REGISTRY.iter().any(|d| self.is_enabled(d.id))
    }

    /// Read the switch for one category.
    pub fn is_enabled(&self, id: CategoryId) -> bool {
        match id {
            CategoryId::Settings => self.settings,
            CategoryId::Memory => self.memory,
            CategoryId::Skills => self.skills,
            CategoryId::Agents => self.agents,
            CategoryId::Commands => self.commands,
            CategoryId::Plugins => self.plugins,
            CategoryId::Plans => self.plans,
            CategoryId::Todos => self.todos,
            CategoryId::PromptHistory => self.prompt_history,
        }
    }

    /// Flip the switch for one category.
    pub fn set_enabled(&mut self, id: CategoryId, value: bool) {
        match id {
            CategoryId::Settings => self.settings = value,
            CategoryId::Memory => self.memory = value,
            CategoryId::Skills => self.skills = value,
            CategoryId::Agents => self.agents = value,
            CategoryId::Commands => self.commands = value,
            CategoryId::Plugins => self.plugins = value,
            CategoryId::Plans => self.plans = value,
            CategoryId::Todos => self.todos = value,
            CategoryId::PromptHistory => self.prompt_history = value,
        }
    }
}

/// Look up a descriptor by its CLI/config name (kebab-case).
pub fn find_by_name(name: &str) -> Option<&'static CategoryDescriptor> {
    REGISTRY.iter().find(|d| d.name == name)
}

/// All registry rows whose toggle is on.
pub fn enabled_categories(toggles: &ArtifactToggles) -> Vec<&'static CategoryDescriptor> {
    REGISTRY
        .iter()
        .filter(|d| toggles.is_enabled(d.id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_registry_has_all_nine_categories() {
        assert_eq!(REGISTRY.len(), 9);
        let ids: HashSet<_> = REGISTRY.iter().map(|d| d.id).collect();
        assert_eq!(ids.len(), 9, "every category appears exactly once");
    }

    #[test]
    fn test_registry_names_and_subdirs_unique() {
        let names: HashSet<_> = REGISTRY.iter().map(|d| d.name).collect();
        let subdirs: HashSet<_> = REGISTRY.iter().map(|d| d.repo_subdir).collect();
        assert_eq!(names.len(), REGISTRY.len());
        assert_eq!(subdirs.len(), REGISTRY.len());
    }

    #[test]
    fn test_plugins_category_is_exact_file_allowlist() {
        let plugins = REGISTRY.iter().find(|d| d.id == CategoryId::Plugins).unwrap();
        match plugins.source {
            SourceSpec::Files(files) => {
                assert_eq!(
                    files,
                    [
                        "plugins/installed_plugins.json",
                        "plugins/known_marketplaces.json"
                    ]
                );
            }
            _ => panic!("plugins must be an exact file allowlist, never a dir walk"),
        }
    }

    #[test]
    fn test_prompt_history_uses_union_merge() {
        let ph = REGISTRY
            .iter()
            .find(|d| d.id == CategoryId::PromptHistory)
            .unwrap();
        assert_eq!(ph.merge, MergeStrategy::UnionJsonl);
        assert_eq!(ph.source, SourceSpec::Files(&["history.jsonl"]));
        // Everything else raw-overwrites
        for d in REGISTRY.iter().filter(|d| d.id != CategoryId::PromptHistory) {
            assert_eq!(d.merge, MergeStrategy::RawOverwrite, "{}", d.name);
        }
    }

    #[test]
    fn test_settings_never_includes_local_overrides() {
        let settings = REGISTRY.iter().find(|d| d.id == CategoryId::Settings).unwrap();
        match settings.source {
            SourceSpec::Files(files) => {
                assert!(files.contains(&"settings.json"));
                assert!(files.contains(&"keybindings.json"));
                assert!(!files.contains(&"settings.local.json"));
            }
            _ => panic!("settings must be an exact file allowlist"),
        }
    }

    #[test]
    fn test_toggles_map_every_category() {
        let mut toggles = ArtifactToggles::default();
        assert!(!toggles.any_enabled());
        for d in REGISTRY {
            assert!(!toggles.is_enabled(d.id), "{} defaults off", d.name);
            toggles.set_enabled(d.id, true);
            assert!(toggles.is_enabled(d.id), "{} can be enabled", d.name);
        }
        assert_eq!(toggles, ArtifactToggles::all_enabled());
    }

    #[test]
    fn test_find_by_name_matches_cli_names() {
        for d in REGISTRY {
            let found = find_by_name(d.name).unwrap();
            assert_eq!(found.id, d.id);
        }
        assert!(find_by_name("prompt-history").is_some());
        assert!(find_by_name("nonsense").is_none());
    }

    #[test]
    fn test_old_config_without_toggles_parses_all_off() {
        // Backward compat: a config.toml written before this feature has no
        // [sync_artifacts] table; deserializing must yield all-false.
        let toggles: ArtifactToggles = toml::from_str("").unwrap();
        assert_eq!(toggles, ArtifactToggles::default());
        assert!(!toggles.any_enabled());
    }
}
