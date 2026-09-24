//! External three-way merge for a differing file.
//!
//! The terminal picker can only choose a side. When `merge_tool` is configured,
//! the prompt gains "merge in the editor": the tool runs as
//! `<merge_tool> <local> <remote> <base> <output>` (the JetBrains argument
//! order) and whatever it writes to the output pane is what lands.
//!
//! A tool that keeps its window open until the merge is done (meld, kdiff3,
//! vimdiff) is waited for, and its exit code decides whether the output pane
//! applies. Editor launchers instead hand the request to an already-running
//! process and exit successfully before the window is drawn; for those the
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

        if handed_off {
            // Nothing left to wait on but the file: the editor that owns the
            // merge is another process. Rewritten and then settled is done.
            if rewritten && current == previous {
                break Resolution::Written(current);
            }
        } else {
            match child.try_wait().ok().flatten() {
                // A tool that owns its window (meld, kdiff3, vimdiff) is done
                // when it exits, not at its first save: the user may save
                // partway through a merge. Its exit code says whether to apply.
                None => {}
                Some(status) if status.success() && started.elapsed() <= handoff_grace => {
                    // Or a launcher that handed the request to an already
                    // running editor and exited straight away.
                    handed_off = true;
                    println!(
                        "  Waiting for the merge of '{}'. Save the merged file to continue, \
                         or press Ctrl-C to keep the local version (giving up in {}).",
                        local_path.display(),
                        describe(timeout)
                    );
                    if rewritten && current == previous {
                        break Resolution::Written(current);
                    }
                }
                Some(status) => {
                    break if status.success() && rewritten {
                        Resolution::Written(current)
                    } else {
                        Resolution::Abandoned
                    };
                }
            }
        }
        previous = current;

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

    /// A stand-in for a tool that owns its window: saves a partial merge,
    /// keeps going, then saves the final one and exits.
    #[cfg(unix)]
    fn tool_that_saves_twice(dir: &Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("slow-merge-tool.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf 'half\\n' > \"$4\"\nsleep 6\nprintf 'final\\n' > \"$4\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().to_string()
    }

    #[test]
    #[cfg(unix)]
    fn a_tool_still_open_after_a_save_is_waited_for() {
        let (dir, path) = local_file("local side\n");
        let tool = tool_that_saves_twice(dir.path());

        let resolution = merge(&tool, &path, b"remote\n", Duration::from_secs(30)).unwrap();

        assert_eq!(resolution, Resolution::Written(b"final\n".to_vec()));
    }

    #[test]
    #[cfg(unix)]
    fn a_tool_that_exits_with_a_failure_after_saving_applies_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = local_file("local side\n");
        let script = dir.path().join("cancelled-merge-tool.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf 'partial\\n' > \"$4\"\nsleep 6\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let resolution = merge(
            &script.to_string_lossy(),
            &path,
            b"remote\n",
            Duration::from_secs(30),
        )
        .unwrap();

        assert_eq!(resolution, Resolution::Abandoned);
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
