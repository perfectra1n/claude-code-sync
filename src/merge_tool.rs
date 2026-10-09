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

use crate::scm::{ConflictChoice, ConflictedFile};

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
const KEEP_BOTH: &str = "Keep both — save the sync repo version next to your local one";
// A conflict inside the sync repository itself: "ours" is this machine's
// last commit, "theirs" what the other machine pushed.
const TAKE_THEIRS: &str = "Take the other machine's version";
const KEEP_OURS: &str = "Keep this machine's version";
const STOP_THE_PULL: &str = "Stop the pull (undo the merge)";

/// What the user (or the merge tool) decided about one differing file.
pub enum ResolutionOutcome {
    /// Write these bytes (a take-remote or a saved merge result).
    Write(Vec<u8>),
    /// The user explicitly chose to keep the local file — a DECISION,
    /// which callers make durable.
    KeepLocal,
    /// The merge-tool session was abandoned (closed without applying):
    /// no decision was made, and callers must not record one.
    Abandoned,
    /// The user chose to preserve both versions. `rename` is the
    /// conflict-copy path (Syncthing-style
    /// `<stem>.sync-conflict-<date>-<time>-<host>.<ext>`) where the
    /// remote bytes land. The local file is left untouched; callers
    /// record the repository version as the base, as after a merge, so
    /// the next sync publishes the kept local file.
    ConflictCopy { rename: PathBuf },
}

/// Ask what to do about one differing file. `KeepLocal` and `Abandoned`
/// both leave the local file untouched, but they are NOT the same: only
/// an explicit keep is a decision the caller may record durably.
pub fn resolve_overwrite(
    merge_tool: &str,
    prefer_merge_tool: bool,
    local_path: &Path,
    remote_bytes: &[u8],
) -> Result<ResolutionOutcome> {
    let file_name = crate::artifacts::engine::prompt_file_name(local_path);

    let mut options = vec![TAKE_REMOTE, KEEP_LOCAL];
    if !merge_tool.trim().is_empty() {
        options.push(MERGE_EXTERNALLY);
    }
    // KEEP_BOTH is always offered: it has no dependency on a merge tool
    // and preserves both versions (Syncthing's documented pattern), so
    // even users without a configured merge tool get the option to keep
    // their local edit and inspect the remote's version alongside it.
    options.push(KEEP_BOTH);

    // EOF or a rendering failure is NOT a keep: mapping it to KeepLocal
    // would make the caller durably record the repo bytes as the base —
    // permanently dirty files off an input accident. No decision was made.
    let question = format!("'{file_name}' differs from the sync repo:");
    let Some(choice) = ask(&question, options, prefer_merge_tool) else {
        return Ok(ResolutionOutcome::Abandoned);
    };

    match choice {
        TAKE_REMOTE => Ok(ResolutionOutcome::Write(remote_bytes.to_vec())),
        MERGE_EXTERNALLY => {
            let local_bytes = std::fs::read(local_path)
                .with_context(|| format!("Failed to read {}", local_path.display()))?;
            let no_recorded_ancestor: &[u8] = b"";
            let resolution = merge(
                merge_tool,
                local_path,
                &local_bytes,
                remote_bytes,
                no_recorded_ancestor,
                configured_timeout(),
            )?;
            match resolution {
                Resolution::Written(merged) => Ok(ResolutionOutcome::Write(merged)),
                Resolution::Abandoned => Ok(ResolutionOutcome::Abandoned),
            }
        }
        KEEP_BOTH => Ok(ResolutionOutcome::ConflictCopy {
            rename: build_conflict_path(local_path),
        }),
        _ => Ok(ResolutionOutcome::KeepLocal),
    }
}

/// Build the next free Syncthing-style conflict-copy path: sibling of
/// `local_path` named `<stem>.sync-conflict-<YYYYMMDD>-<HHMMSS>-<host>.<ext>`
/// (UTC). A collision with an existing file takes the smallest free `-1`,
/// `-2`, ... suffix.
pub fn build_conflict_path(local_path: &Path) -> PathBuf {
    let parent = local_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = local_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "artifact".to_string());
    let extension = local_path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let base_name = format!(
        "{stem}{}{}-{}",
        crate::artifacts::denylist::CONFLICT_COPY_INFIX,
        chrono::Utc::now().format("%Y%m%d-%H%M%S"),
        host_name()
    );

    let candidate = parent.join(format!("{base_name}{extension}"));
    if !candidate.exists() {
        return candidate;
    }
    (1..)
        .map(|n| parent.join(format!("{base_name}-{n}{extension}")))
        .find(|candidate| !candidate.exists())
        .expect("an unbounded range always yields a free name")
}

/// This machine's name for a conflict-copy suffix, best effort: the usual
/// environment variables, then `/etc/hostname`, then `local`.
fn host_name() -> String {
    ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .chain(std::fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_string())
        .find(|name| !name.is_empty() && !name.contains(['/', '\\']))
        .unwrap_or_else(|| "local".to_string())
}

/// Ask which version of a file both machines changed the sync repository keeps.
pub fn resolve_conflict(
    merge_tool: &str,
    prefer_merge_tool: bool,
    file: &ConflictedFile,
) -> Result<ConflictChoice> {
    let mut options = vec![TAKE_THEIRS, KEEP_OURS];
    let both_machines_have_it = file.local.is_some() && file.remote.is_some();
    let has_merge_tool = !merge_tool.trim().is_empty();
    if both_machines_have_it && has_merge_tool {
        options.push(MERGE_EXTERNALLY);
    }
    options.push(STOP_THE_PULL);

    let question = format!("'{}' {}:", file.path, describe_conflict(file));
    let Some(choice) = ask(&question, options, prefer_merge_tool) else {
        return Ok(ConflictChoice::AbortMerge);
    };

    match choice {
        KEEP_OURS => Ok(ConflictChoice::KeepLocal),
        TAKE_THEIRS => Ok(ConflictChoice::TakeRemote),
        MERGE_EXTERNALLY => {
            let resolution = merge(
                merge_tool,
                Path::new(&file.path),
                file.local.as_deref().unwrap_or_default(),
                file.remote.as_deref().unwrap_or_default(),
                file.base.as_deref().unwrap_or_default(),
                configured_timeout(),
            )?;
            match resolution {
                Resolution::Written(merged) => Ok(ConflictChoice::WriteMerged(merged)),
                Resolution::Abandoned => Ok(ConflictChoice::AbortMerge),
            }
        }
        _ => Ok(ConflictChoice::AbortMerge),
    }
}

fn describe_conflict(file: &ConflictedFile) -> &'static str {
    match (&file.local, &file.remote) {
        (None, None) => "renamed on both sides",
        (None, _) => "deleted locally, changed remotely",
        (_, None) => "changed locally, deleted remotely",
        _ => "changed on both sides",
    }
}

/// `None` when no choice was made (Esc, EOF, a rendering failure).
fn ask(
    question: &str,
    options: Vec<&'static str>,
    prefer_merge_tool: bool,
) -> Option<&'static str> {
    let merge_position = options
        .iter()
        .position(|option| *option == MERGE_EXTERNALLY);
    let starting_cursor = match merge_position {
        Some(position) if prefer_merge_tool => position,
        _ => 0,
    };

    Select::new(question, options)
        .with_starting_cursor(starting_cursor)
        .prompt()
        .ok()
}

/// Run the configured tool on the local and remote versions of `path`, with
/// `base_bytes` as the version both started from.
pub fn merge(
    merge_tool: &str,
    path: &Path,
    local_bytes: &[u8],
    remote_bytes: &[u8],
    base_bytes: &[u8],
    timeout: Duration,
) -> Result<Resolution> {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "txt".to_string());
    let workspace = tempfile::tempdir()?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "artifact".to_string());

    let pane =
        |side: &str| -> PathBuf { workspace.path().join(format!("{stem}.{side}.{extension}")) };
    let local_pane = pane("local");
    let remote_pane = pane("remote");
    let base_pane = pane("base");
    let output = pane("merged");

    std::fs::write(&local_pane, local_bytes)?;
    std::fs::write(&remote_pane, remote_bytes)?;
    std::fs::write(&base_pane, base_bytes)?;
    std::fs::write(&output, local_bytes)?;
    let before_any_save = std::time::SystemTime::now() - Duration::from_secs(2);
    std::fs::File::options()
        .write(true)
        .open(&output)?
        .set_modified(before_any_save)?;
    let output_written_at = read_modified_time(&output);

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
    let mut previous = local_bytes.to_vec();

    let outcome = loop {
        std::thread::sleep(Duration::from_millis(250));

        let current = std::fs::read(&output).unwrap_or_default();
        let saved = current != local_bytes || read_modified_time(&output) != output_written_at;

        if handed_off {
            // Nothing left to wait on but the file: the editor that owns the
            // merge is another process. Saved and then settled is done.
            if saved && current == previous {
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
                        "  Waiting for the merge of '{}'. Save the merged file to continue \
                         (giving up in {}).",
                        path.display(),
                        describe(timeout)
                    );
                    if saved && current == previous {
                        break Resolution::Written(current);
                    }
                }
                Some(status) if status.success() && saved => break Resolution::Written(current),
                Some(status) => {
                    log::warn!(
                        "The merge tool exited ({status}) without saving; nothing from it is applied"
                    );
                    break Resolution::Abandoned;
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

fn read_modified_time(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
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

    #[test]
    fn a_conflict_copy_sits_beside_the_file_never_syncs_and_never_collides() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("deploy.md");
        std::fs::write(&local, "mine").unwrap();

        let first = build_conflict_path(&local);
        assert_eq!(first.parent(), Some(dir.path()));
        let name = first.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("deploy.sync-conflict-"), "{name}");
        assert!(name.ends_with(".md"), "{name}");
        assert!(crate::artifacts::denylist::is_denied(Path::new(&name)));

        std::fs::write(&first, "theirs").unwrap();
        let second = build_conflict_path(&local);
        assert_ne!(second, first, "an existing copy is never overwritten");
        assert!(!second.exists());
    }

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

        let resolution = merge(
            &tool,
            &path,
            b"local side\n",
            b"remote side\n",
            b"",
            Duration::from_secs(10),
        )
        .unwrap();

        match resolution {
            Resolution::Written(bytes) => assert_eq!(bytes, b"remote side\n".to_vec()),
            Resolution::Abandoned => panic!("expected the merged output to be picked up"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_merge_that_settles_on_the_local_version_is_applied() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = local_file("local side\n");
        let script = dir.path().join("take-local.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 6\ncat \"$1\" > \"$4\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let resolution = merge(
            &script.to_string_lossy(),
            &path,
            b"local side\n",
            b"remote side\n",
            b"",
            Duration::from_secs(30),
        )
        .unwrap();

        assert_eq!(resolution, Resolution::Written(b"local side\n".to_vec()));
    }

    #[test]
    #[cfg(unix)]
    fn an_editor_that_saves_the_local_version_after_the_handoff_is_applied() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = local_file("local side\n");
        let script = dir.path().join("handoff-take-local.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n(sleep 1; cat \"$1\" > \"$4\") &\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let resolution = merge(
            &script.to_string_lossy(),
            &path,
            b"local side\n",
            b"remote side\n",
            b"",
            Duration::from_secs(10),
        )
        .unwrap();

        assert_eq!(resolution, Resolution::Written(b"local side\n".to_vec()));
    }

    #[test]
    #[cfg(unix)]
    fn a_tool_closed_without_saving_applies_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = local_file("local side\n");
        let script = dir.path().join("closed-unsaved.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 6\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let resolution = merge(
            &script.to_string_lossy(),
            &path,
            b"local side\n",
            b"remote side\n",
            b"",
            Duration::from_secs(30),
        )
        .unwrap();

        assert_eq!(resolution, Resolution::Abandoned);
    }

    #[test]
    #[cfg(unix)]
    fn the_base_pane_holds_the_version_both_sides_started_from() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = local_file("local side\n");
        let script = dir.path().join("take-base.sh");
        std::fs::write(&script, "#!/bin/sh\ncat \"$3\" > \"$4\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let resolution = merge(
            &script.to_string_lossy(),
            &path,
            b"local side\n",
            b"remote side\n",
            b"common start\n",
            Duration::from_secs(10),
        )
        .unwrap();

        assert_eq!(resolution, Resolution::Written(b"common start\n".to_vec()));
    }

    #[test]
    #[cfg(unix)]
    fn a_tool_that_writes_nothing_leaves_the_local_file_alone() {
        let (_dir, path) = local_file("local side\n");

        let resolution = merge(
            "true",
            &path,
            b"local side\n",
            b"remote\n",
            b"",
            Duration::from_secs(1),
        )
        .unwrap();

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

        let resolution = merge(
            &tool,
            &path,
            b"local side\n",
            b"remote\n",
            b"",
            Duration::from_secs(30),
        )
        .unwrap();

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
            b"local side\n",
            b"remote\n",
            b"",
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
            b"local side\n",
            b"remote\n",
            b"",
            Duration::from_secs(1),
        );
        assert!(attempt.is_err());
    }
}
