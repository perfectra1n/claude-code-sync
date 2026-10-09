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
const KEEP_BOTH: &str = "Keep both — save the sync repo version next to your local one";

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
    /// remote bytes land. The local file is left untouched. Callers
    /// do NOT call `record_base` for this outcome: the local file
    /// is dirty by user-edit definition, and the conflict copy is
    /// a side artifact, not a synced value.
    ConflictCopy { rename: PathBuf },
}

/// Ask what to do about one differing file. `KeepLocal` and `Abandoned`
/// both leave the local file untouched, but they are NOT the same: only
/// an explicit keep is a decision the caller may record durably.
pub fn resolve_overwrite(
    merge_tool: &str,
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

    let choice = match Select::new(
        &format!("'{file_name}' differs from the sync repo:"),
        options,
    )
    .prompt()
    {
        Ok(choice) => choice,
        // EOF or a rendering failure is NOT a keep: mapping it to
        // KeepLocal would make the caller durably record the repo bytes
        // as the base — permanently dirty files off an input accident.
        // No decision was made.
        Err(_) => return Ok(ResolutionOutcome::Abandoned),
    };

    match choice {
        TAKE_REMOTE => Ok(ResolutionOutcome::Write(remote_bytes.to_vec())),
        MERGE_EXTERNALLY => {
            match merge(merge_tool, local_path, remote_bytes, configured_timeout())? {
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
/// `local_path` named `<stem>.sync-conflict-<YYYYMMDD>-<HHMMSS>-<host>.<ext>`.
/// Collisions with an existing file pick the smallest non-colliding `-1`,
/// `-2`, ... suffix. The `host` is `hostname` (best-effort; falls back to
/// `local` when the call fails).
pub fn build_conflict_path(local_path: &Path) -> PathBuf {
    let parent = local_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let stem = local_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "artifact".to_string());
    let ext = local_path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let host = std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // Best-effort hostname lookup. Avoid pulling in the `hostname`
            // crate for this single use: an empty env is fine, and the
            // toolchain's `gethostname(2)` is a single libc call.
            #[cfg(unix)]
            unsafe {
                let mut buf = [0u8; 256];
                let ret = libc_gethostname(buf.as_mut_ptr() as *mut _, buf.len());
                if ret == 0 {
                    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    if let Ok(s) = std::str::from_utf8(&buf[..len]) {
                        if !s.is_empty() {
                            return Some(s.to_string());
                        }
                    }
                }
                None
            }
            #[cfg(not(unix))]
            {
                None
            }
        })
        .unwrap_or_else(|| "local".to_string());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // YYYYMMDD-HHMMSS without external chrono dep: simple time math from
    // a known epoch anchor. Good enough for a unique-enough suffix.
    let (date_part, time_part) = epoch_seconds_to_ymd_hms(now);
    let base_name = format!("{stem}.sync-conflict-{date_part}-{time_part}-{host}");
    let mut candidate: PathBuf = if ext.is_empty() {
        parent.join(format!("{base_name}"))
    } else {
        parent.join(format!("{base_name}.{ext}"))
    };
    if !candidate.exists() {
        return candidate;
    }
    for n in 1.. {
        candidate = if ext.is_empty() {
            parent.join(format!("{base_name}-{n}"))
        } else {
            parent.join(format!("{base_name}-{n}.{ext}"))
        };
        if !candidate.exists() {
            return candidate;
        }
        // Guard against pathological directory with thousands of pre-existing
        // conflict copies; 10000 is plenty for any sane user.
        if n > 10_000 {
            return candidate;
        }
    }
    unreachable!()
}

/// Convert UNIX seconds to (YYYYMMDD, HHMMSS) using the Gregorian calendar.
/// Inline implementation to avoid pulling in a `chrono`/`time` dep just
/// for a 6-line conflict-copy suffix. The math is the standard
/// "days-since-1970-01-01 then year/month/day" recipe.
fn epoch_seconds_to_ymd_hms(secs: u64) -> (String, String) {
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    // 1970-01-01 is a Thursday (day 0 = Thursday in the proleptic Gregorian
    // calendar: 1970-01-01 = day 0, day-of-week (0..6) with 0=Thursday).
    // Shift days so 0 maps to Monday: offset by 3.
    let dow0 = ((days + 3).rem_euclid(7)) as u32; // 0 = Mon
    let (y, m, d) = days_to_ymd(days);
    // Unused: dow0 is for the test below; we don't actually need it
    // for the suffix but keep the variable to avoid the unused-let
    // warning that would otherwise surface.
    let _ = dow0;
    (
        format!("{y:04}{m:02}{d:02}"),
        format!("{hour:02}{minute:02}{second:02}"),
    )
}

fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's date algorithm: civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

#[cfg(unix)]
unsafe extern "C" {
    fn gethostname(
        name: *mut std::os::raw::c_char,
        len: usize,
    ) -> std::os::raw::c_int;
}

#[cfg(unix)]
unsafe fn libc_gethostname(
    name: *mut std::os::raw::c_char,
    len: usize,
) -> std::os::raw::c_int {
    gethostname(name, len)
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
