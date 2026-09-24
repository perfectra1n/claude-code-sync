use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Represents a single line/entry in the JSONL conversation file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationEntry {
    /// The type of this entry (e.g., "user", "assistant", "file-history-snapshot")
    ///
    /// This field identifies what kind of entry this is in the conversation.
    /// Common types include user messages, assistant responses, and system events.
    #[serde(rename = "type")]
    pub entry_type: String,

    /// Unique identifier for this conversation entry
    ///
    /// Each entry may have its own UUID to uniquely identify it within the conversation.
    /// Not all entry types require a UUID, hence this is optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,

    /// UUID of the parent entry in the conversation thread
    ///
    /// This links entries together in a conversation tree, allowing for branching
    /// and threading of messages. If present, it references the UUID of the entry
    /// that this entry is responding to or following from.
    #[serde(rename = "parentUuid", skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,

    /// Session identifier grouping related conversation entries together
    ///
    /// All entries within a single conversation session share the same session ID.
    /// This is used to associate entries across multiple files or to reconstruct
    /// conversation context. If not present in the entry, the filename may be used.
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// ISO 8601 timestamp indicating when this entry was created
    ///
    /// Format is typically "YYYY-MM-DDTHH:MM:SS.sssZ" (e.g., "2025-01-01T00:00:00.000Z").
    /// Used for sorting entries chronologically and determining the latest activity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,

    /// The actual message content as a JSON value
    ///
    /// Contains the text and structured data of the user or assistant message.
    /// Stored as a generic JSON value to accommodate different message formats
    /// and structures without strict schema requirements.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,

    /// Current working directory at the time this entry was created
    ///
    /// Stores the filesystem path of the working directory, providing context
    /// about where the conversation or command was executed. Useful for
    /// reproducing environments and understanding file references.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Version string of the Claude Code CLI that created this entry
    ///
    /// Records which version of the tool generated this conversation entry,
    /// helpful for debugging compatibility issues and tracking feature support.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Git branch name active when this entry was created
    ///
    /// Captures the current git branch context, allowing conversation entries
    /// to be associated with specific branches in version control. Useful for
    /// tracking which branch work was performed on.
    #[serde(rename = "gitBranch", skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,

    /// Catch-all field for additional JSON properties not explicitly defined
    ///
    /// Preserves any extra fields in the JSON that aren't part of the explicit schema.
    /// This allows forward compatibility - newer versions can add fields without breaking
    /// older parsers. The flattened serde attribute merges these fields at the same level
    /// as the named fields when serializing/deserializing.
    #[serde(flatten)]
    pub extra: Value,
}

/// One conversation session, summarized.
///
/// The entries themselves are deliberately NOT held. Discovery reads every
/// transcript under a tree, and a machine with a few gigabytes of history
/// would need tens of gigabytes of memory to keep every parsed message of
/// every session alive at once — while sync only ever compares identity,
/// recency, size and content hash. The messages of the one session being
/// merged are read back from the file with [`ConversationSession::load_entries`].
#[derive(Debug, Clone)]
pub struct ConversationSession {
    /// Unique identifier for this conversation session
    ///
    /// Either extracted from the first entry that contains a sessionId field,
    /// or derived from the filename (without extension) if no entries contain
    /// a session ID. Used to group related conversation entries together.
    pub session_id: String,

    /// Path to the JSONL file this session was loaded from
    ///
    /// Stores the filesystem path of the source file, used for tracking the
    /// origin of the conversation data and for reading the messages back. Kept
    /// as a path, not a string: a project directory whose name is not valid
    /// UTF-8 would otherwise come back with replacement characters and no
    /// longer name a file that exists.
    pub file_path: PathBuf,

    latest_timestamp: Option<String>,
    message_count: usize,
    content_hash: String,
    project_name: Option<String>,

    /// How much of the file the summary covers: the offset just past the last
    /// message read. Anything after it arrived while the file was being read.
    summarized_bytes: u64,
}

impl ConversationSession {
    /// Summarize a JSONL transcript in a single streaming pass, keeping the
    /// per-session facts and dropping every message as it goes.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let mut entry_session_id = None;
        let mut latest_timestamp: Option<String> = None;
        let mut message_count = 0;
        let mut first_cwd: Option<String> = None;
        let mut hasher = ContentHasher::new();

        let summarized_bytes = for_each_entry(path, |entry| {
            // Remember the first interior sessionId, used only as a fallback below.
            if entry_session_id.is_none() {
                if let Some(ref sid) = entry.session_id {
                    entry_session_id = Some(sid.clone());
                }
            }

            if let Some(ref timestamp) = entry.timestamp {
                let is_later = latest_timestamp
                    .as_ref()
                    .is_none_or(|latest| timestamp > latest);
                if is_later {
                    latest_timestamp = Some(timestamp.clone());
                }
            }

            if is_message(&entry) {
                message_count += 1;
            }

            if first_cwd.is_none() {
                first_cwd.clone_from(&entry.cwd);
            }

            hash_entry(&entry, &mut hasher);

            Ok(())
        })?;

        // Identity is the filename stem, which Claude Code guarantees is unique per
        // transcript: `<uuid>.jsonl` for a session and `agent-<hash>.jsonl` for each
        // of its subagents. The interior `sessionId` field is NOT unique — subagent
        // sidechain transcripts carry their *parent* session's id, so keying identity
        // on it collapses every subagent onto the parent and makes sync drop the
        // parent's main transcript. Fall back to the interior id only if the path has
        // no usable stem.
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .or(entry_session_id)
            .with_context(|| {
                format!(
                    "No session ID found in file or filename: {}",
                    path.display()
                )
            })?;

        // The project is the working directory of the first entry that names
        // one, whatever the later entries say.
        let project_name = first_cwd
            .as_deref()
            .and_then(|cwd| Path::new(cwd).file_name())
            .and_then(|name| name.to_str())
            .map(|name| name.to_string());

        Ok(ConversationSession {
            session_id,
            file_path: path.to_path_buf(),
            latest_timestamp,
            message_count,
            content_hash: hasher.finish(),
            project_name,
            summarized_bytes,
        })
    }

    /// Read the session's messages back from its file. Only a session being
    /// merged needs them, and one transcript at a time is what keeps sync's
    /// memory flat however much history a machine has.
    pub fn load_entries(&self) -> Result<Vec<ConversationEntry>> {
        let mut entries = Vec::new();
        for_each_entry(&self.file_path, |entry| {
            entries.push(entry);
            Ok(())
        })?;
        Ok(entries)
    }

    /// Copy the transcript verbatim to another path, creating its directory.
    /// Byte-for-byte: a re-serialized transcript is a rewritten transcript.
    ///
    /// The copy holds exactly the messages this summary was taken from.
    /// Claude Code appends to a live session while sync runs, and a message
    /// that arrived since — possibly half-written — would make the copy parse
    /// differently, or not at all. It travels with the next sync instead.
    pub fn copy_to<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();

        // Copying a file onto itself truncates it: the destination is opened
        // for writing first, and that is the same inode.
        if is_same_file(&self.file_path, path) {
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        std::fs::copy(&self.file_path, path).with_context(|| {
            format!(
                "Failed to copy {} to {}",
                self.file_path.display(),
                path.display()
            )
        })?;

        trim_to_summarized_length(path, self.summarized_bytes)
            .with_context(|| format!("Failed to finish copying to {}", path.display()))?;

        Ok(())
    }

    /// The latest timestamp seen in the conversation.
    pub fn latest_timestamp(&self) -> Option<&str> {
        self.latest_timestamp.as_deref()
    }

    /// The number of messages (user + assistant) in the conversation.
    pub fn message_count(&self) -> usize {
        self.message_count
    }

    /// The project name from the first entry's `cwd` path.
    pub fn project_name(&self) -> Option<&str> {
        self.project_name.as_deref()
    }

    /// A hash of the conversation content, for telling two copies apart.
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }
}

/// The largest line buffer a parse keeps between lines, 1 MB.
const MAX_RETAINED_LINE_BUFFER: usize = 1024 * 1024;

/// Whether an entry is a message, as opposed to a snapshot, a summary or
/// whatever else Claude Code records in a transcript.
pub fn is_message(entry: &ConversationEntry) -> bool {
    entry.entry_type == "user" || entry.entry_type == "assistant"
}

/// Whether two paths name the same file on disk.
fn is_same_file(source: &Path, destination: &Path) -> bool {
    let (Ok(source), Ok(destination)) = (source.canonicalize(), destination.canonicalize()) else {
        return false;
    };
    source == destination
}

/// Cut a freshly copied transcript back to the messages that were summarized,
/// dropping whatever was appended to the original while it was being read.
fn trim_to_summarized_length(path: &Path, summarized_bytes: u64) -> Result<()> {
    let copied_bytes = std::fs::metadata(path)?.len();
    if copied_bytes <= summarized_bytes {
        return Ok(());
    }

    // The copy carries the original's permissions, and a read-only transcript
    // would leave a destination neither this run nor the next one can write.
    let permissions = std::fs::metadata(path)?.permissions();
    if permissions.readonly() {
        allow_writing(path, permissions)?;
    }

    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_len(summarized_bytes)?;

    Ok(())
}

/// Give the file's owner permission to write it, leaving the rest alone.
#[cfg(unix)]
fn allow_writing(path: &Path, permissions: std::fs::Permissions) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = permissions;
    permissions.set_mode(permissions.mode() | 0o200);
    std::fs::set_permissions(path, permissions)?;

    Ok(())
}

/// Clear the file's read-only flag.
#[cfg(not(unix))]
// The lint warns that this makes a file world-writable on Unix; this is the
// non-Unix branch, where it only clears the read-only attribute.
#[allow(clippy::permissions_set_readonly_false)]
fn allow_writing(path: &Path, permissions: std::fs::Permissions) -> Result<()> {
    let mut permissions = permissions;
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions)?;

    Ok(())
}

/// The content hash of a transcript: FNV-1a over the serialized messages.
///
/// Written out here rather than taken from `std::collections::hash_map::
/// DefaultHasher`, whose value std explicitly does not keep stable across Rust
/// releases: a hashing rule nobody can state is one nobody can test, and this
/// one decides whether a conversation counts as changed. FNV-1a is nine lines,
/// needs no dependency, is measurably faster on the many small writes
/// serialization produces, and is pinned by `the_content_hash_never_changes`.
///
/// The hash is always 16 hex digits, which is what the conflict screen prints.
///
/// Bytes are fed in as they are serialized, so hashing a message never builds
/// its JSON text in memory first: a message can be megabytes, and every core
/// is summarizing a different transcript at the same time.
struct ContentHasher {
    state: u64,
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Separates one message from the next, so two transcripts cannot hash alike
/// by moving a byte across a message boundary. Never appears in the input:
/// serialized JSON is UTF-8, and 0xff is not.
const ENTRY_TERMINATOR: u8 = 0xff;

impl ContentHasher {
    fn new() -> Self {
        ContentHasher {
            state: FNV_OFFSET_BASIS,
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.state ^= u64::from(*byte);
            self.state = self.state.wrapping_mul(FNV_PRIME);
        }
    }

    /// The hash so far, as the fixed 16 hex digits callers slice and print.
    fn finish(&self) -> String {
        format!("{:016x}", self.state)
    }
}

impl std::io::Write for ContentHasher {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_bytes(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Hash one entry's JSON, then the terminator.
fn hash_entry(entry: &ConversationEntry, hasher: &mut ContentHasher) {
    let serialized = serde_json::to_writer(&mut *hasher, entry);
    hasher.write_bytes(&[ENTRY_TERMINATOR]);

    if let Err(error) = serialized {
        log::warn!("Hashing an entry that would not serialize: {error}");
    }
}

/// Parse a JSONL transcript line by line, handing each entry to `visit` and
/// dropping it again unless the caller keeps it. The entry is handed over by
/// value, so a caller that wants to keep it never copies it.
///
/// Returns the offset just past the last message read, which is how much of
/// the file the caller has actually seen.
fn for_each_entry<F>(path: &Path, mut visit: F) -> Result<u64>
where
    F: FnMut(ConversationEntry) -> Result<()>,
{
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut reader = BufReader::new(file);

    // One buffer for the whole file, reused line by line: a transcript is read
    // in a few kilobytes at a time however large it is, and the thread that
    // reads it allocates once instead of once per message.
    let mut line = String::new();
    let mut line_num = 0;
    let mut read_bytes = 0u64;
    let mut parsed_bytes = 0u64;

    loop {
        line.clear();
        // Clearing keeps the capacity, which is the point — except after a
        // multi-megabyte tool result, where holding that buffer for the rest of
        // the file on every core is exactly the memory this avoids elsewhere.
        if line.capacity() > MAX_RETAINED_LINE_BUFFER {
            line.shrink_to(MAX_RETAINED_LINE_BUFFER);
        }
        let bytes_read = reader.read_line(&mut line).with_context(|| {
            format!("Failed to read line {} in {}", line_num + 1, path.display())
        })?;
        if bytes_read == 0 {
            break;
        }
        line_num += 1;
        read_bytes += bytes_read as u64;

        if line.trim().is_empty() {
            continue;
        }

        let entry: ConversationEntry = serde_json::from_str(&line).with_context(|| {
            format!(
                "Failed to parse JSON at line {} in {}",
                line_num,
                path.display()
            )
        })?;

        visit(entry)?;
        parsed_bytes = read_bytes;
    }

    Ok(parsed_bytes)
}

/// Write conversation entries to a JSONL file, creating its directory. Used
/// for the one transcript a merge rewrites; everything else is copied.
pub fn write_entries_to_file<P: AsRef<Path>>(path: P, entries: &[ConversationEntry]) -> Result<()> {
    let path = path.as_ref();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    let mut file =
        File::create(path).with_context(|| format!("Failed to create file: {}", path.display()))?;

    for entry in entries {
        let json =
            serde_json::to_string(entry).context("Failed to serialize conversation entry")?;
        writeln!(file, "{json}")
            .with_context(|| format!("Failed to write to file: {}", path.display()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_conversation_entry() {
        let json =
            r#"{"type":"user","uuid":"123","sessionId":"abc","timestamp":"2025-01-01T00:00:00Z"}"#;
        let entry: ConversationEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.entry_type, "user");
        assert_eq!(entry.uuid.unwrap(), "123");
    }

    #[test]
    fn test_read_write_session() {
        use std::fs::File;
        use tempfile::TempDir;

        // Identity comes from the filename stem, so use a controlled filename.
        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("test-123.jsonl");
        let mut file = File::create(&session_path).unwrap();
        writeln!(file, r#"{{"type":"user","sessionId":"test-123","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","sessionId":"test-123","uuid":"2","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        drop(file);

        let session = ConversationSession::from_file(&session_path).unwrap();
        assert_eq!(session.session_id, "test-123");
        assert_eq!(session.load_entries().unwrap().len(), 2);
        assert_eq!(session.message_count(), 2);

        // Round-trip: copying then re-reading the same path preserves content and id.
        let output_dir = TempDir::new().unwrap();
        let output_path = output_dir.path().join("test-123.jsonl");
        session.copy_to(&output_path).unwrap();

        let reloaded = ConversationSession::from_file(&output_path).unwrap();
        assert_eq!(reloaded.session_id, session.session_id);
        assert_eq!(reloaded.content_hash(), session.content_hash());
        assert_eq!(
            std::fs::read(&output_path).unwrap(),
            std::fs::read(&session_path).unwrap(),
            "a copied transcript is the same bytes, not a re-serialized one"
        );
    }

    #[test]
    fn copying_a_transcript_onto_itself_keeps_it() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("self-copy.jsonl");
        let mut file = File::create(&session_path).unwrap();
        writeln!(
            file,
            r#"{{"type":"user","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        drop(file);

        let session = ConversationSession::from_file(&session_path).unwrap();
        let before = std::fs::read(&session_path).unwrap();

        // A sync repository inside ~/.claude makes source and destination the
        // same file; copying it onto itself would empty it.
        session.copy_to(&session_path).unwrap();

        assert_eq!(std::fs::read(&session_path).unwrap(), before);
    }

    #[test]
    fn a_last_message_written_without_a_newline_still_travels() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("no-trailing-newline.jsonl");
        let mut file = File::create(&session_path).unwrap();
        writeln!(
            file,
            r#"{{"type":"user","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        write!(
            file,
            r#"{{"type":"assistant","uuid":"2","timestamp":"2025-01-01T00:01:00Z"}}"#
        )
        .unwrap();
        drop(file);

        let session = ConversationSession::from_file(&session_path).unwrap();
        assert_eq!(session.message_count(), 2);

        let copy_path = temp_dir
            .path()
            .join("copy")
            .join("no-trailing-newline.jsonl");
        session.copy_to(&copy_path).unwrap();

        assert_eq!(
            std::fs::read(&copy_path).unwrap(),
            std::fs::read(&session_path).unwrap(),
            "a complete last message is not a half-written one"
        );
    }

    #[test]
    fn a_read_only_transcript_can_still_be_copied() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("read-only.jsonl");
        let mut file = File::create(&session_path).unwrap();
        writeln!(
            file,
            r#"{{"type":"user","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        drop(file);

        let mut permissions = std::fs::metadata(&session_path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&session_path, permissions).unwrap();

        let session = ConversationSession::from_file(&session_path).unwrap();
        let copy_path = temp_dir.path().join("copy").join("read-only.jsonl");
        session.copy_to(&copy_path).unwrap();

        assert_eq!(
            ConversationSession::from_file(&copy_path)
                .unwrap()
                .content_hash(),
            session.content_hash()
        );
    }

    #[test]
    fn a_message_still_being_written_is_left_out_of_the_copy() {
        use std::fs::OpenOptions;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("live.jsonl");
        let mut file = File::create(&session_path).unwrap();
        writeln!(
            file,
            r#"{{"type":"user","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        drop(file);

        let session = ConversationSession::from_file(&session_path).unwrap();

        // Claude Code keeps appending while sync runs.
        let mut appending = OpenOptions::new().append(true).open(&session_path).unwrap();
        write!(appending, r#"{{"type":"assistant","uuid":"2","time"#).unwrap();
        drop(appending);

        let copy_path = temp_dir.path().join("copy").join("live.jsonl");
        session.copy_to(&copy_path).unwrap();

        let copied = ConversationSession::from_file(&copy_path).unwrap();
        assert_eq!(copied.load_entries().unwrap().len(), 1);
        assert_eq!(copied.content_hash(), session.content_hash());
    }

    #[test]
    fn test_session_id_from_filename() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_file = temp_dir
            .path()
            .join("248a0cdf-1466-48a7-b3d0-00f9e8e6e4ee.jsonl");

        // Create file with entries that don't have sessionId field
        let mut file = File::create(&session_file).unwrap();
        writeln!(file, r#"{{"type":"file-history-snapshot","messageId":"abc","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"file-history-snapshot","messageId":"def","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();

        // Parse should succeed using filename as session ID
        let session = ConversationSession::from_file(&session_file).unwrap();
        assert_eq!(session.session_id, "248a0cdf-1466-48a7-b3d0-00f9e8e6e4ee");
        assert_eq!(session.load_entries().unwrap().len(), 2);
    }

    #[test]
    fn test_session_id_from_filename_preferred() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_file = temp_dir.path().join("filename-uuid.jsonl");

        // Create file with a (different) sessionId in entries
        let mut file = File::create(&session_file).unwrap();
        writeln!(file, r#"{{"type":"user","sessionId":"entry-uuid","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();

        // Identity must come from the filename, not the interior sessionId.
        let session = ConversationSession::from_file(&session_file).unwrap();
        assert_eq!(session.session_id, "filename-uuid");
    }

    #[test]
    fn test_subagent_does_not_inherit_parent_session_id() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        // Regression: Claude Code subagent transcripts are named `agent-<hash>.jsonl`
        // but every entry carries the *parent* session's id with isSidechain=true.
        // Keying on the interior id collapsed all subagents onto the parent and made
        // sync drop the parent's main transcript. Identity must be the filename stem.
        let temp_dir = TempDir::new().unwrap();
        let subagent_file = temp_dir.path().join("agent-a16263cbf10e1ad0b.jsonl");

        let mut file = File::create(&subagent_file).unwrap();
        writeln!(file, r#"{{"type":"user","sessionId":"56d02190-2a2d-4a55-9ec1-38e34fb25e84","isSidechain":true,"uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();

        let session = ConversationSession::from_file(&subagent_file).unwrap();
        assert_eq!(session.session_id, "agent-a16263cbf10e1ad0b");
        assert_ne!(session.session_id, "56d02190-2a2d-4a55-9ec1-38e34fb25e84");
    }

    #[test]
    fn test_mixed_entries_with_and_without_session_id() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_file = temp_dir.path().join("test-session.jsonl");

        // Create file with mix of entries
        let mut file = File::create(&session_file).unwrap();
        writeln!(file, r#"{{"type":"file-history-snapshot","messageId":"abc","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"user","sessionId":"test-123","uuid":"1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();

        // Identity comes from the filename stem regardless of interior ids.
        let session = ConversationSession::from_file(&session_file).unwrap();
        assert_eq!(session.session_id, "test-session");
        assert_eq!(session.load_entries().unwrap().len(), 2);
    }

    fn session_from_line(line: &str) -> (tempfile::TempDir, ConversationSession) {
        use std::fs::File;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_file = temp_dir.path().join("test.jsonl");
        let mut file = File::create(&session_file).unwrap();
        writeln!(file, "{line}").unwrap();
        drop(file);

        let session = ConversationSession::from_file(&session_file).unwrap();
        (temp_dir, session)
    }

    #[test]
    fn test_project_name_from_cwd() {
        let (_dir, session) =
            session_from_line(r#"{"type":"user","uuid":"1","cwd":"/Users/abc/my-cool-project"}"#);

        assert_eq!(session.project_name(), Some("my-cool-project"));
    }

    #[test]
    fn the_first_working_directory_names_the_project() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("two-cwds.jsonl");
        let mut file = File::create(&session_path).unwrap();
        writeln!(file, r#"{{"type":"user","uuid":"1"}}"#).unwrap();
        writeln!(
            file,
            r#"{{"type":"user","uuid":"2","cwd":"/home/me/first"}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"user","uuid":"3","cwd":"/home/me/second"}}"#
        )
        .unwrap();
        drop(file);

        let session = ConversationSession::from_file(&session_path).unwrap();

        assert_eq!(session.project_name(), Some("first"));
    }

    #[test]
    fn test_project_name_no_cwd() {
        let (_dir, session) = session_from_line(r#"{"type":"user","uuid":"1"}"#);

        assert_eq!(session.project_name(), None);
    }

    #[test]
    fn the_content_hash_never_changes() {
        let (_dir, session) = session_from_line(
            r#"{"type":"assistant","uuid":"1","message":{"text":"hello"},"custom":[1,2]}"#,
        );

        // Pinned on purpose. If a change to hashing, to serialization or to the
        // compiler moves this value, every conversation on every machine looks
        // modified at once and the next push rewrites the whole repository.
        assert_eq!(session.content_hash(), "65439596bed5d900");
        assert_eq!(
            session.content_hash().len(),
            16,
            "the conflict screen prints the first 16 digits of it"
        );
    }

    #[test]
    fn two_transcripts_that_differ_hash_differently() {
        let (_first_dir, first) = session_from_line(r#"{"type":"user","uuid":"1","cwd":"/a"}"#);
        let (_second_dir, second) = session_from_line(r#"{"type":"user","uuid":"2","cwd":"/a"}"#);

        assert_ne!(first.content_hash(), second.content_hash());
    }

    #[test]
    fn a_session_summary_does_not_grow_with_its_transcript() {
        use std::fs::File;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_file = temp_dir.path().join("big.jsonl");
        let mut file = File::create(&session_file).unwrap();
        let padding = "x".repeat(10_000);
        for index in 0..500 {
            writeln!(
                file,
                r#"{{"type":"user","uuid":"{index}","timestamp":"2025-01-01T00:00:00Z","message":{{"text":"{padding}"}}}}"#
            )
            .unwrap();
        }
        drop(file);

        let transcript_bytes = std::fs::metadata(&session_file).unwrap().len();
        assert!(
            transcript_bytes > 5_000_000,
            "a big transcript to summarize"
        );

        let (session, summary_bytes) =
            bytes_retained_by(|| ConversationSession::from_file(&session_file).unwrap());
        assert_eq!(session.message_count(), 500);
        assert!(
            summary_bytes < 10_000,
            "summarizing a {transcript_bytes}-byte transcript kept {summary_bytes} bytes; \
             a summary is ids and counts, never the messages"
        );

        // Control: the same meter does see messages that are held on purpose.
        let (entries, entries_bytes) = bytes_retained_by(|| session.load_entries().unwrap());
        assert_eq!(entries.len(), 500);
        assert!(
            entries_bytes > transcript_bytes as isize,
            "loading the messages back kept only {entries_bytes} bytes, so the measurement is broken"
        );
    }

    /// Runs `work` and reports how many bytes it allocated and did not free.
    ///
    /// Nothing else can tell a summary apart from a struct that keeps every
    /// message: both are the same handful of bytes wide, and only the heap
    /// behind them differs.
    fn bytes_retained_by<T>(work: impl FnOnce() -> T) -> (T, isize) {
        let before = LIVE_BYTES.with(|live| live.get());
        let produced = work();
        let after = LIVE_BYTES.with(|live| live.get());
        (produced, after - before)
    }

    #[global_allocator]
    static COUNTING_ALLOCATOR: CountingAllocator = CountingAllocator;

    /// The system allocator, counting what the calling thread holds.
    struct CountingAllocator;

    thread_local! {
        static LIVE_BYTES: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    }

    unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            let _ = LIVE_BYTES.try_with(|live| live.set(live.get() + layout.size() as isize));
            std::alloc::System.alloc(layout)
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
            let _ = LIVE_BYTES.try_with(|live| live.set(live.get() - layout.size() as isize));
            std::alloc::System.dealloc(pointer, layout)
        }
    }
}
