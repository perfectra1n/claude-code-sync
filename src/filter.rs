use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Filter configuration for syncing Claude Code history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterConfig {
    /// Exclude projects older than N days
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_older_than_days: Option<u32>,

    /// Include only these project path patterns (glob-style)
    #[serde(default)]
    pub include_patterns: Vec<String>,

    /// Exclude these project path patterns (glob-style)
    #[serde(default)]
    pub exclude_patterns: Vec<String>,

    /// Maximum file size in bytes (default: 10MB)
    #[serde(default = "default_max_file_size")]
    pub max_file_size_bytes: u64,

    /// Exclude file attachments (images, PDFs, etc.)
    #[serde(default)]
    pub exclude_attachments: bool,
}

fn default_max_file_size() -> u64 {
    10 * 1024 * 1024 // 10MB
}

impl Default for FilterConfig {
    fn default() -> Self {
        FilterConfig {
            exclude_older_than_days: None,
            include_patterns: Vec::new(),
            exclude_patterns: Vec::new(),
            max_file_size_bytes: default_max_file_size(),
            exclude_attachments: false,
        }
    }
}

impl FilterConfig {
    /// Load configuration from file
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;

        if !config_path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

        let config: FilterConfig =
            toml::from_str(&content).context("Failed to parse config file")?;

        Ok(config)
    }

    /// Save configuration to file
    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory: {}", parent.display())
            })?;
        }

        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;

        fs::write(&config_path, content)
            .with_context(|| format!("Failed to write config file: {}", config_path.display()))?;

        Ok(())
    }

    /// Get the path to the config file
    fn config_path() -> Result<PathBuf> {
        crate::config::ConfigManager::filter_config_path()
    }

    /// Check if a file should be included based on filters
    pub fn should_include(&self, file_path: &Path) -> bool {
        // Only process .jsonl files (exclude attachments if configured)
        if self.exclude_attachments {
            if let Some(ext) = file_path.extension() {
                if ext != "jsonl" {
                    // This is an attachment (image, PDF, etc.)
                    return false;
                }
            }
        }

        // Check file size
        if let Ok(metadata) = fs::metadata(file_path) {
            if metadata.len() > self.max_file_size_bytes {
                return false;
            }
        }

        let path_str = file_path.to_string_lossy();

        // Check exclude patterns first
        if !self.exclude_patterns.is_empty() {
            for pattern in &self.exclude_patterns {
                if glob_match(pattern, &path_str) {
                    return false;
                }
            }
        }

        // Check include patterns (if any are specified)
        if !self.include_patterns.is_empty() {
            let mut matches_include = false;
            for pattern in &self.include_patterns {
                if glob_match(pattern, &path_str) {
                    matches_include = true;
                    break;
                }
            }
            if !matches_include {
                return false;
            }
        }

        // Check age filter
        if let Some(max_days) = self.exclude_older_than_days {
            if let Ok(metadata) = fs::metadata(file_path) {
                if let Ok(modified) = metadata.modified() {
                    let age = std::time::SystemTime::now()
                        .duration_since(modified)
                        .unwrap_or_default();

                    let max_age = std::time::Duration::from_secs((max_days as u64) * 24 * 60 * 60);
                    if age > max_age {
                        return false;
                    }
                }
            }
        }

        true
    }
}

/// Simple glob pattern matching
fn glob_match(pattern: &str, text: &str) -> bool {
    // Simple implementation - for production, use the `glob` crate
    if pattern.contains('*') {
        let parts: Vec<_> = pattern.split('*').collect();
        if parts.len() == 2 {
            text.starts_with(parts[0]) && text.ends_with(parts[1])
        } else {
            // Simplified multi-wildcard support
            let mut pos = 0;
            for (i, part) in parts.iter().enumerate() {
                if part.is_empty() {
                    continue;
                }
                if i == 0 {
                    if !text[pos..].starts_with(part) {
                        return false;
                    }
                    pos += part.len();
                } else if i == parts.len() - 1 {
                    return text[pos..].ends_with(part);
                } else if let Some(idx) = text[pos..].find(part) {
                    pos += idx + part.len();
                } else {
                    return false;
                }
            }
            true
        }
    } else {
        text.contains(pattern)
    }
}

/// Update the filter configuration
pub fn update_config(
    exclude_older_than: Option<u32>,
    include_projects: Option<String>,
    exclude_projects: Option<String>,
    exclude_attachments: Option<bool>,
) -> Result<()> {
    let mut config = FilterConfig::load()?;

    if let Some(days) = exclude_older_than {
        config.exclude_older_than_days = Some(days);
        println!(
            "{}",
            format!("Set exclude_older_than_days to {days} days").green()
        );
    }

    if let Some(includes) = include_projects {
        config.include_patterns = includes
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        println!(
            "{}",
            format!("Set include patterns: {:?}", config.include_patterns).green()
        );
    }

    if let Some(excludes) = exclude_projects {
        config.exclude_patterns = excludes
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        println!(
            "{}",
            format!("Set exclude patterns: {:?}", config.exclude_patterns).green()
        );
    }

    if let Some(exclude_att) = exclude_attachments {
        config.exclude_attachments = exclude_att;
        println!(
            "{}",
            format!("Exclude attachments: {exclude_att}").green()
        );
    }

    config.save()?;
    println!("{}", "Configuration saved successfully!".green().bold());

    Ok(())
}

/// Show the current filter configuration
pub fn show_config() -> Result<()> {
    let config = FilterConfig::load()?;

    println!("{}", "Current Filter Configuration:".bold());
    println!(
        "  {}: {}",
        "Exclude older than".cyan(),
        config
            .exclude_older_than_days
            .map(|d| format!("{d} days"))
            .unwrap_or_else(|| "Not set".to_string())
    );
    println!(
        "  {}: {}",
        "Include patterns".cyan(),
        if config.include_patterns.is_empty() {
            "None (all included)".to_string()
        } else {
            config.include_patterns.join(", ")
        }
    );
    println!(
        "  {}: {}",
        "Exclude patterns".cyan(),
        if config.exclude_patterns.is_empty() {
            "None".to_string()
        } else {
            config.exclude_patterns.join(", ")
        }
    );
    println!(
        "  {}: {} bytes ({:.2} MB)",
        "Max file size".cyan(),
        config.max_file_size_bytes,
        config.max_file_size_bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "  {}: {}",
        "Exclude attachments".cyan(),
        if config.exclude_attachments {
            "Yes (only .jsonl files)".green()
        } else {
            "No (all files)".yellow()
        }
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*test*", "this is a test"));
        assert!(glob_match("test*", "testing"));
        assert!(glob_match("*test", "this is a test"));
        assert!(!glob_match("test*", "no match"));
    }

    #[test]
    fn test_filter_config_default() {
        let config = FilterConfig::default();
        assert_eq!(config.exclude_older_than_days, None);
        assert!(config.include_patterns.is_empty());
        assert!(config.exclude_patterns.is_empty());
        assert!(!config.exclude_attachments);
    }

    #[test]
    fn test_exclude_attachments_filter() {
        use std::path::PathBuf;

        // Config with exclude_attachments = false (default)
        let config_include_all = FilterConfig::default();

        // Should include .jsonl files
        assert!(config_include_all.should_include(&PathBuf::from("session.jsonl")));

        // Should also include other files when exclude_attachments is false
        assert!(config_include_all.should_include(&PathBuf::from("image.png")));
        assert!(config_include_all.should_include(&PathBuf::from("document.pdf")));

        // Config with exclude_attachments = true
        let config_exclude = FilterConfig {
            exclude_attachments: true,
            ..Default::default()
        };

        // Should include .jsonl files
        assert!(config_exclude.should_include(&PathBuf::from("session.jsonl")));

        // Should exclude non-.jsonl files
        assert!(!config_exclude.should_include(&PathBuf::from("image.png")));
        assert!(!config_exclude.should_include(&PathBuf::from("image.jpg")));
        assert!(!config_exclude.should_include(&PathBuf::from("document.pdf")));
        assert!(!config_exclude.should_include(&PathBuf::from("archive.zip")));
    }

    #[test]
    fn test_exclude_attachments_with_patterns() {
        use std::path::PathBuf;

        let config = FilterConfig {
            exclude_attachments: true,
            exclude_patterns: vec!["*test*".to_string()],
            ..Default::default()
        };

        // Should exclude based on attachment filter
        assert!(!config.should_include(&PathBuf::from("image.png")));

        // Should exclude based on pattern even for .jsonl
        assert!(!config.should_include(&PathBuf::from("/path/test/session.jsonl")));

        // Should include .jsonl that doesn't match exclude pattern
        assert!(config.should_include(&PathBuf::from("/path/prod/session.jsonl")));
    }

    #[test]
    fn test_filter_config_serialization() {
        let config = FilterConfig {
            exclude_attachments: true,
            exclude_older_than_days: Some(30),
            ..Default::default()
        };

        // Test that it can be serialized
        let serialized = toml::to_string(&config).unwrap();
        assert!(serialized.contains("exclude_attachments"));

        // Test that it can be deserialized
        let deserialized: FilterConfig = toml::from_str(&serialized).unwrap();
        assert!(deserialized.exclude_attachments);
        assert_eq!(deserialized.exclude_older_than_days, Some(30));
    }
}
