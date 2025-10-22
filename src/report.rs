use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::conflict::{Conflict, ConflictResolution};

/// Report of sync conflicts encountered during Claude Code synchronization
///
/// This structure contains a summary of all conflicts detected when syncing
/// conversation files between local and remote storage. It provides metadata
/// about when the conflicts were detected and details about each individual conflict.
#[derive(Debug, Serialize, Deserialize)]
pub struct ConflictReport {
    /// ISO 8601 timestamp indicating when this report was generated
    ///
    /// Generated using `chrono::Utc::now().to_rfc3339()` at the time of report creation.
    pub timestamp: String,

    /// Total number of conflicts detected in this sync operation
    ///
    /// This count matches the length of the `conflicts` vector.
    pub total_conflicts: usize,

    /// Detailed information about each individual conflict
    ///
    /// Each entry provides comprehensive information about a specific conflict,
    /// including file paths, message counts, timestamps, and resolution status.
    pub conflicts: Vec<ConflictDetail>,
}

/// Detailed information about a specific conflict between local and remote conversation files
///
/// This structure captures all relevant information about a conflict, including the session
/// identifier, file paths, message counts, timestamps, and the resolution strategy applied
/// or pending. It is used to track and report on conflicts during synchronization operations.
#[derive(Debug, Serialize, Deserialize)]
pub struct ConflictDetail {
    /// Unique identifier for the Claude Code conversation session
    ///
    /// This ID links the conflicting files to their original conversation session
    /// and is used to identify which conversation has diverged between local and remote.
    pub session_id: String,

    /// Path to the local conversation file
    ///
    /// This is the absolute path to the conversation file stored on the local machine,
    /// represented as a string for serialization purposes.
    pub local_file: String,

    /// Path to the remote conversation file
    ///
    /// This is the absolute path to the conversation file stored in remote storage,
    /// represented as a string for serialization purposes.
    pub remote_file: String,

    /// Number of messages in the local conversation file
    ///
    /// This count helps determine which version has more recent activity
    /// and can inform conflict resolution decisions.
    pub local_messages: usize,

    /// Number of messages in the remote conversation file
    ///
    /// This count helps determine which version has more recent activity
    /// and can inform conflict resolution decisions.
    pub remote_messages: usize,

    /// Last modification timestamp of the local conversation file
    ///
    /// ISO 8601 formatted timestamp string, or "unknown" if the timestamp
    /// could not be determined from the file metadata.
    pub local_timestamp: String,

    /// Last modification timestamp of the remote conversation file
    ///
    /// ISO 8601 formatted timestamp string, or "unknown" if the timestamp
    /// could not be determined from the file metadata.
    pub remote_timestamp: String,

    /// The resolution strategy applied or pending for this conflict
    ///
    /// Possible values include:
    /// - "Keep both (remote renamed to `<path>`)" - Both versions preserved with remote renamed
    /// - "Keep local" - Local version kept, remote discarded
    /// - "Keep remote" - Remote version kept, local overwritten
    /// - "Pending" - No resolution applied yet, user intervention required
    pub resolution: String,
}

impl ConflictReport {
    /// Create a new conflict report from detected conflicts
    pub fn from_conflicts(conflicts: &[Conflict]) -> Self {
        let conflict_details = conflicts
            .iter()
            .map(|c| ConflictDetail {
                session_id: c.session_id.clone(),
                local_file: c.local_file.display().to_string(),
                remote_file: c.remote_file.display().to_string(),
                local_messages: c.local_message_count,
                remote_messages: c.remote_message_count,
                local_timestamp: c
                    .local_timestamp
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                remote_timestamp: c
                    .remote_timestamp
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                resolution: match &c.resolution {
                    ConflictResolution::SmartMerge { stats, .. } => {
                        format!(
                            "Smart merged ({} messages, {} branches)",
                            stats.merged_messages, stats.branches_detected
                        )
                    }
                    ConflictResolution::KeepBoth {
                        renamed_remote_file,
                    } => {
                        format!(
                            "Keep both (remote renamed to {})",
                            renamed_remote_file.display()
                        )
                    }
                    ConflictResolution::KeepLocal => "Keep local".to_string(),
                    ConflictResolution::KeepRemote => "Keep remote".to_string(),
                    ConflictResolution::Pending => "Pending".to_string(),
                },
            })
            .collect();

        ConflictReport {
            timestamp: chrono::Utc::now().to_rfc3339(),
            total_conflicts: conflicts.len(),
            conflicts: conflict_details,
        }
    }

    /// Generate a markdown report
    pub fn to_markdown(&self) -> String {
        let mut output = String::new();

        output.push_str("# Claude Code Sync Conflict Report\n\n");
        output.push_str(&format!("**Generated:** {}\n", self.timestamp));
        output.push_str(&format!(
            "**Total Conflicts:** {}\n\n",
            self.total_conflicts
        ));

        if self.conflicts.is_empty() {
            output.push_str("No conflicts detected.\n");
            return output;
        }

        output.push_str("## Conflicts\n\n");

        for (i, conflict) in self.conflicts.iter().enumerate() {
            output.push_str(&format!(
                "### {}. Session: `{}`\n\n",
                i + 1,
                conflict.session_id
            ));
            output.push_str(&format!("- **Resolution:** {}\n", conflict.resolution));
            output.push_str(&format!("- **Local File:** `{}`\n", conflict.local_file));
            output.push_str(&format!("  - Messages: {}\n", conflict.local_messages));
            output.push_str(&format!("  - Last Updated: {}\n", conflict.local_timestamp));
            output.push_str(&format!("- **Remote File:** `{}`\n", conflict.remote_file));
            output.push_str(&format!("  - Messages: {}\n", conflict.remote_messages));
            output.push_str(&format!(
                "  - Last Updated: {}\n",
                conflict.remote_timestamp
            ));
            output.push('\n');
        }

        output
    }

    /// Generate a JSON report
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("Failed to serialize report to JSON")
    }

    /// Print a colored console summary
    pub fn print_summary(&self) {
        println!("\n{}", "=== Conflict Report ===".bold().cyan());
        println!("{}: {}", "Timestamp".bold(), self.timestamp);
        println!(
            "{}: {}",
            "Total Conflicts".bold(),
            self.total_conflicts.to_string().yellow()
        );

        if self.conflicts.is_empty() {
            println!("\n{}", "No conflicts detected!".green());
            return;
        }

        println!("\n{}", "Conflicts:".bold());
        for (i, conflict) in self.conflicts.iter().enumerate() {
            println!(
                "\n{}. {}: {}",
                (i + 1).to_string().cyan(),
                "Session".bold(),
                conflict.session_id.yellow()
            );
            println!(
                "   {}: {}",
                "Resolution".bold(),
                conflict.resolution.green()
            );
            println!("   {}", "Local:".bold());
            println!("     File: {}", conflict.local_file);
            println!("     Messages: {}", conflict.local_messages);
            println!("     Updated: {}", conflict.local_timestamp);
            println!("   {}", "Remote:".bold());
            println!("     File: {}", conflict.remote_file);
            println!("     Messages: {}", conflict.remote_messages);
            println!("     Updated: {}", conflict.remote_timestamp);
        }
        println!();
    }

    /// Save report to file
    pub fn save(&self, path: &Path, format: &str) -> Result<()> {
        let content = match format.to_lowercase().as_str() {
            "json" => self.to_json()?,
            "markdown" | "md" => self.to_markdown(),
            _ => return Err(anyhow::anyhow!("Unsupported format: {format}")),
        };

        fs::write(path, content)
            .with_context(|| format!("Failed to write report to {}", path.display()))?;

        println!(
            "{} {}",
            "Report saved to:".green().bold(),
            path.display().to_string().cyan()
        );

        Ok(())
    }
}

/// Generate and output a conflict report
pub fn generate_report(format: &str, output: Option<&Path>) -> Result<()> {
    // Load the latest conflict report from the sync state
    // For now, we'll create a placeholder implementation
    let report = load_latest_report()?;

    if let Some(output_path) = output {
        report.save(output_path, format)?;
    } else {
        match format.to_lowercase().as_str() {
            "json" => println!("{}", report.to_json()?),
            "markdown" | "md" => println!("{}", report.to_markdown()),
            _ => report.print_summary(),
        }
    }

    Ok(())
}

/// Load the latest conflict report from the sync state
pub fn load_latest_report() -> Result<ConflictReport> {
    let sync_state_path = get_sync_state_dir()?;
    let report_path = sync_state_path.join("latest-conflict-report.json");

    if !report_path.exists() {
        // Return empty report if no conflicts have been recorded
        return Ok(ConflictReport {
            timestamp: chrono::Utc::now().to_rfc3339(),
            total_conflicts: 0,
            conflicts: Vec::new(),
        });
    }

    let content = fs::read_to_string(&report_path)
        .with_context(|| format!("Failed to read report from {}", report_path.display()))?;

    let report: ConflictReport =
        serde_json::from_str(&content).context("Failed to parse conflict report")?;

    Ok(report)
}

/// Save a conflict report to the sync state
pub fn save_conflict_report(report: &ConflictReport) -> Result<()> {
    let sync_state_path = get_sync_state_dir()?;
    fs::create_dir_all(&sync_state_path).context("Failed to create sync state directory")?;

    let report_path = sync_state_path.join("latest-conflict-report.json");
    let content = report.to_json()?;

    fs::write(&report_path, content)
        .with_context(|| format!("Failed to write report to {}", report_path.display()))?;

    Ok(())
}

/// Get the sync state directory
fn get_sync_state_dir() -> Result<std::path::PathBuf> {
    crate::config::ConfigManager::config_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_report() {
        let report = ConflictReport::from_conflicts(&[]);
        assert_eq!(report.total_conflicts, 0);
        assert!(report.conflicts.is_empty());
    }

    #[test]
    fn test_markdown_generation() {
        let report = ConflictReport {
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            total_conflicts: 0,
            conflicts: Vec::new(),
        };

        let markdown = report.to_markdown();
        assert!(markdown.contains("# Claude Code Sync Conflict Report"));
        assert!(markdown.contains("No conflicts detected"));
    }

    #[test]
    fn test_json_generation() {
        let report = ConflictReport {
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            total_conflicts: 0,
            conflicts: Vec::new(),
        };

        let json = report.to_json().unwrap();
        assert!(json.contains("total_conflicts"));
    }
}
