//! External three-way merge for a differing file.
//!
//! The terminal picker can only choose a side. When `merge_tool` is configured,
//! the prompt gains "merge in the editor": the tool runs as
//! `<merge_tool> <local> <remote> <base> <output>` (the JetBrains argument
//! order) and whatever it writes to the output pane is what lands.
//!
//! The launcher's exit code is not a result. Editor launchers hand the request
//! to an already-running process and exit before the window is drawn, so the
//! output file being rewritten and then settling is what completion means.

use anyhow::{Context, Result};
use inquire::Select;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// How a merge window ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The tool wrote the merged file.
    Written(Vec<u8>),
    /// The window closed without applying anything, or the wait ran out.
    Abandoned,
}

const KEEP_LOCAL: &str = "Keep the local version";
const TAKE_REMOTE: &str = "Overwrite with the sync repo version";
const MERGE_EXTERNALLY: &str = "Merge both in the configured merge tool";

/// Ask what to do about one differing file, returning the bytes to write, or
/// `None` to keep the local file untouched.
pub fn resolve_overwrite(
    merge_tool: &str,
    local_path: &Path,
    remote_bytes: &[u8],
) -> Result<Option<Vec<u8>>> {
    let file_name = local_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| local_path.display().to_string());

    let mut options = vec![TAKE_REMOTE, KEEP_LOCAL];
    if !merge_tool.trim().is_empty() {
        options.push(MERGE_EXTERNALLY);
    }

    let choice = Select::new(
        &format!("'{file_name}' differs from the sync repo:"),
        options,
    )
    .prompt()
    .unwrap_or(KEEP_LOCAL);

    match choice {
        TAKE_REMOTE => Ok(Some(remote_bytes.to_vec())),
        MERGE_EXTERNALLY => {
            match merge(merge_tool, local_path, remote_bytes, configured_timeout())? {
                Resolution::Written(merged) => Ok(Some(merged)),
                Resolution::Abandoned => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

/// Run the configured tool on `local_path` against `remote_bytes`.
///
/// There is no recorded common ancestor for an artifact, so the base pane is
/// empty: the tool shows both sides in full rather than a misleading diff
/// against a version neither machine had.
pub fn merge(
    merge_tool: &str,
    local_path: &Path,
    remote_bytes: &[u8],
    timeout: Duration,
) -> Result<Resolution> {
    let extension = local_path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "txt".to_string());
    let workspace = tempfile::tempdir()?;
    let stem = local_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "artifact".to_string());

    let pane =
        |side: &str| -> PathBuf { workspace.path().join(format!("{stem}.{side}.{extension}")) };
    let local_pane = pane("local");
    let remote_pane = pane("remote");
    let base_pane = pane("base");
    let output = pane("merged");

    let local_bytes = std::fs::read(local_path)
        .with_context(|| format!("Failed to read {}", local_path.display()))?;
    std::fs::write(&local_pane, &local_bytes)?;
    std::fs::write(&remote_pane, remote_bytes)?;
    std::fs::write(&base_pane, b"")?;
    std::fs::write(&output, &local_bytes)?;

    let mut parts = merge_tool.split_whitespace();
    let program = parts.next().context("merge_tool is empty")?;
    let mut child = Command::new(program)
        .args(parts)
        .arg(&local_pane)
        .arg(&remote_pane)
        .arg(&base_pane)
        .arg(&output)
        .spawn()
        .with_context(|| format!("Failed to start merge tool {program}"))?;

    let started = Instant::now();
    let handoff_grace = Duration::from_secs(5);
    let mut handed_off = false;
    let mut previous = local_bytes.clone();

    let outcome = loop {
        std::thread::sleep(Duration::from_millis(250));

        let current = std::fs::read(&output).unwrap_or_default();
        let rewritten = current != local_bytes;
        let settled = current == previous;
        if rewritten && settled {
            break Resolution::Written(current);
        }
        previous = current;

        // A launcher that hands the request to an already-running editor exits
        // straight away, and successfully; a tool that failed to start exits
        // with a failure and is not worth waiting for.
        let exit_status = child.try_wait().ok().flatten();
        let handed_over_to_an_editor = exit_status
            .is_some_and(|status| status.success() && started.elapsed() <= handoff_grace);
        if handed_over_to_an_editor {
            if !handed_off {
                println!(
                    "  Waiting for the merge of '{}'. Save the merged file to continue, \
                     or press Ctrl-C to keep the local version (giving up in {}).",
                    local_path.display(),
                    describe(timeout)
                );
            }
            handed_off = true;
        } else if exit_status.is_some() && !handed_off {
            break Resolution::Abandoned;
        }

        if started.elapsed() > timeout {
            log::warn!("Merge tool did not finish within the timeout; keeping the local file");
            break Resolution::Abandoned;
        }
    };

    // The launcher usually exited long ago; reap it, and make sure a tool still
    // running for a merge nobody waits for does not outlive this command.
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
    let _ = child.wait();

    Ok(outcome)
}

/// A wait spelled the way a person reads it.
fn describe(timeout: Duration) -> String {
    let seconds = timeout.as_secs();
    if seconds < 60 {
        return format!("{seconds} seconds");
    }
    format!("{} minutes", seconds / 60)
}

/// How long to wait for a merge window, overridable per machine with
/// `CLAUDE_CODE_SYNC_MERGE_TIMEOUT_SECONDS`.
fn configured_timeout() -> Duration {
    let seconds = std::env::var("CLAUDE_CODE_SYNC_MERGE_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(900);
    Duration::from_secs(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_file(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("SKILL.md");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    /// A stand-in merge tool: `<local> <remote> <base> <output>`, writing the
    /// remote pane into the output pane.
    #[cfg(unix)]
    fn tool_that_takes_the_remote_side(dir: &Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("merge-tool.sh");
        std::fs::write(&script, "#!/bin/sh\ncat \"$2\" > \"$4\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().to_string()
    }

    #[test]
    #[cfg(unix)]
    fn a_tool_that_writes_the_output_pane_resolves_the_conflict() {
        let (dir, path) = local_file("local side\n");
        let tool = tool_that_takes_the_remote_side(dir.path());

        let resolution = merge(&tool, &path, b"remote side\n", Duration::from_secs(10)).unwrap();

        match resolution {
            Resolution::Written(bytes) => assert_eq!(bytes, b"remote side\n".to_vec()),
            Resolution::Abandoned => panic!("expected the merged output to be picked up"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_tool_that_writes_nothing_leaves_the_local_file_alone() {
        let (_dir, path) = local_file("local side\n");

        let resolution = merge("true", &path, b"remote\n", Duration::from_secs(1)).unwrap();

        assert_eq!(resolution, Resolution::Abandoned);
        assert_eq!(std::fs::read(&path).unwrap(), b"local side\n".to_vec());
    }

    #[test]
    fn a_missing_tool_is_an_error_not_a_silent_keep() {
        let (_dir, path) = local_file("local side\n");
        let attempt = merge(
            "definitely-not-a-real-merge-tool",
            &path,
            b"remote\n",
            Duration::from_secs(1),
        );
        assert!(attempt.is_err());
    }
}
