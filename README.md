# claude-code-sync

[![Unit Tests](https://github.com/perfectra1n/claude-code-sync/actions/workflows/unit-tests.yml/badge.svg)](https://github.com/perfectra1n/claude-code-sync/actions/workflows/unit-tests.yml)
[![Integration Tests](https://github.com/perfectra1n/claude-code-sync/actions/workflows/integration-tests.yml/badge.svg)](https://github.com/perfectra1n/claude-code-sync/actions/workflows/integration-tests.yml)
[![Build](https://github.com/perfectra1n/claude-code-sync/actions/workflows/build.yml/badge.svg)](https://github.com/perfectra1n/claude-code-sync/actions/workflows/build.yml)
[![Documentation](https://github.com/perfectra1n/claude-code-sync/actions/workflows/docs.yml/badge.svg)](https://github.com/perfectra1n/claude-code-sync/actions/workflows/docs.yml)

A Rust CLI tool for syncing Claude Code conversation history across machines using git repositories.

## Documentation

📚 **[View API Documentation](https://perfectra1n.github.io/claude-code-sync/)** - Complete API reference and code documentation

To build and view documentation locally:
```bash
# Build and open documentation in your browser
cargo doc --open --no-deps --all-features
```

## Features

| Feature | Description |
|---------|-------------|
| **Smart Merge** | Automatically combines non-conflicting conversation changes |
| **Bidirectional Sync** | Pull and push changes in one command with `sync` |
| **Interactive Onboarding** | First-time setup wizard guides you through configuration |
| **Smart Conflict Resolution** | Interactive TUI for resolving conflicts with preview |
| **Selective Sync** | Filter by project, date, or exclude attachments |
| **Undo Operations** | Rollback pull/push with automatic snapshots |
| **Operation History** | Track and review past sync operations |
| **Branch Management** | Sync to different branches, manage remotes |
| **Detailed Logging** | Console and file logging with configurable levels |
| **Conflict Tracking** | Comprehensive conflict reports in JSON/Markdown |
| **Flexible Configuration** | TOML-based config with CLI overrides |

## Overview

`claude-code-sync` helps you backup and synchronize your Claude Code conversation history by pushing it to a git repository. This enables:

- **Backup**: Never lose your Claude Code conversations
- **Multi-machine sync**: Keep conversation history consistent across multiple computers
- **Version control**: Track changes to your conversations over time
- **Conflict resolution**: Automatically handles divergent conversation histories

## How It Works

Claude Code stores conversation history locally in `~/.claude/projects/` as JSONL (JSON Lines) files. Each project has its own directory, and each conversation is a separate `.jsonl` file.

`claude-code-sync`:
1. Discovers all conversation files in your local Claude Code history
2. Copies them to a git repository
3. Commits and optionally pushes to a remote
4. On pull, merges remote changes with local history
5. Detects conflicts (same session modified on different machines)
6. Resolves conflicts by keeping both versions with renamed files

## Installation

### From Source

```bash
git clone <repository-url>
cd claude-code-sync
cargo build --release
sudo cp target/release/claude-code-sync /usr/local/bin/
```

### Using Cargo

```bash
cargo install --path .
```

## Quick Start

### First-Time Setup (Interactive Onboarding)

When you run `claude-code-sync` for the first time, an interactive onboarding wizard will guide you through setup:

```bash
# Simply run any command - onboarding starts automatically
claude-code-sync sync

# Or explicitly run onboarding
claude-code-sync init
```

The onboarding wizard will ask you:
- Whether to use a remote repository or local directory
- Where to store your sync repository
- Remote URL (for remote repos) or path (for local)
- Whether to exclude file attachments (images, PDFs, etc.)
- How old conversations to sync (e.g., last 30 days)

**Benefits of Interactive Onboarding:**
- ✅ Step-by-step guidance for first-time users
- ✅ Validates Git repository URLs and paths
- ✅ Automatically clones remote repositories
- ✅ Sets up sensible defaults based on your choices
- ✅ No need to remember command-line flags

### Manual Initialization (Advanced)

If you prefer to skip onboarding, you can initialize manually:

```bash
# Create a local sync repository
claude-code-sync init --repo ~/claude-history-backup

# Or with a remote git repository
claude-code-sync init --repo ~/claude-history-backup --remote git@github.com:username/claude-history.git
```

### 2. Sync Your History

```bash
# Bidirectional sync (pull then push) - RECOMMENDED
claude-code-sync sync

# Or manually:
# Push all conversation history
claude-code-sync push

# Pull from remote
claude-code-sync pull
```

### 3. Advanced Usage

```bash
# Exclude attachments (images, PDFs, etc.) - only sync .jsonl files
claude-code-sync push --exclude-attachments

# Push to specific branch
claude-code-sync push --branch main

# Sync with custom message and exclude attachments
claude-code-sync sync --message "Daily backup" --exclude-attachments
```

## Commands

### `init`

Initialize a new sync repository.

```bash
claude-code-sync init --repo <path> [--remote <url>]
```

**Options:**
- `--repo, -r <PATH>`: Path to the git repository for storing history
- `--remote <URL>`: Optional remote git URL for pushing/pulling

**Example:**
```bash
claude-code-sync init --repo ~/claude-backup --remote git@github.com:user/claude-history.git
```

### `sync`

**NEW!** Bidirectional sync (pull remote changes, then push local changes).

```bash
claude-code-sync sync [OPTIONS]
```

**Options:**
- `--message, -m <MSG>`: Custom commit message for push
- `--branch, -b <BRANCH>`: Branch to sync with (default: current branch)
- `--exclude-attachments`: Only sync .jsonl files, exclude images/PDFs/etc.

**Example:**
```bash
claude-code-sync sync -m "Daily sync" --exclude-attachments
```

### `push`

Push local Claude Code history to the sync repository.

```bash
claude-code-sync push [OPTIONS]
```

**Options:**
- `--message, -m <MSG>`: Custom commit message
- `--push-remote`: Push to remote after committing (default: true)
- `--branch, -b <BRANCH>`: Branch to push to (default: current branch)
- `--exclude-attachments`: Only sync .jsonl files, exclude images/PDFs/etc.

**Examples:**
```bash
# Basic push
claude-code-sync push -m "Weekly backup"

# Push to specific branch, excluding attachments
claude-code-sync push --branch backup --exclude-attachments
```

### `pull`

Pull and merge history from the sync repository.

```bash
claude-code-sync pull [OPTIONS]
```

**Options:**
- `--fetch-remote`: Pull from remote before merging (default: true)
- `--branch, -b <BRANCH>`: Branch to pull from (default: current branch)

**Example:**
```bash
claude-code-sync pull --branch main
```

### `status`

Show sync status and information.

```bash
claude-code-sync status [--show-conflicts] [--show-files]
```

**Options:**
- `--show-conflicts`: Show detailed conflict information
- `--show-files`: Show which files would be synced

**Example:**
```bash
claude-code-sync status --show-conflicts --show-files
```

### `config`

Configure sync filters and settings.

```bash
claude-code-sync config [OPTIONS] [--show]
```

**Options:**
- `--exclude-older-than <DAYS>`: Exclude projects older than N days
- `--include-projects <PATTERNS>`: Include only specific project paths (comma-separated)
- `--exclude-projects <PATTERNS>`: Exclude specific project paths (comma-separated)
- `--exclude-attachments <true|false>`: Exclude file attachments (images, PDFs, etc.)
- `--show`: Show current configuration

**Examples:**
```bash
# Exclude conversations older than 30 days
claude-code-sync config --exclude-older-than 30

# Include only specific projects
claude-code-sync config --include-projects "*my-project*,*important-work*"

# Exclude test projects
claude-code-sync config --exclude-projects "*test*,*temp*"

# Permanently exclude attachments from all syncs
claude-code-sync config --exclude-attachments true

# Show current config
claude-code-sync config --show
```

### `report`

View conflict reports from previous syncs.

```bash
claude-code-sync report [--format <FORMAT>] [--output <FILE>]
```

**Options:**
- `--format, -f <FORMAT>`: Output format: `json`, `markdown`, or `text` (default: markdown)
- `--output, -o <FILE>`: Output file (default: print to stdout)

**Examples:**
```bash
# Print markdown report to console
claude-code-sync report

# Save JSON report to file
claude-code-sync report --format json --output conflicts.json

# View as markdown
claude-code-sync report --format markdown | less
```

### `remote`

**NEW!** Manage git remote configuration.

```bash
claude-code-sync remote <COMMAND>
```

**Commands:**
- `show`: Display current remote configuration and sync directory
- `set`: Set or update remote URL
- `remove`: Remove a remote

**Options for `set`:**
- `--name, -n <NAME>`: Remote name (default: origin)
- `url`: Remote URL (e.g., https://github.com/user/repo.git or git@github.com:user/repo.git)

**Options for `remove`:**
- `--name, -n <NAME>`: Remote name (default: origin)

**Examples:**
```bash
# Show current remote and sync directory
claude-code-sync remote show

# Set/update remote URL (HTTPS)
claude-code-sync remote set origin https://github.com/user/claude-history.git

# Set/update remote URL (SSH)
claude-code-sync remote set origin git@github.com:user/claude-history.git

# Remove remote
claude-code-sync remote remove origin
```

**Note:** The remote URL must start with `http://`, `https://`, or `git@` for SSH connections.

### `undo`

**NEW in v0.2.0!** Undo the last sync operation by restoring from automatic snapshots.

```bash
claude-code-sync undo <OPERATION>
```

**Operations:**
- `pull`: Undo the last pull operation (restores local files to pre-pull state)
- `push`: Undo the last push operation (resets git repository to previous commit)

**Examples:**
```bash
# Undo the last pull operation
claude-code-sync undo pull

# Undo the last push operation
claude-code-sync undo push
```

**How it works:**
- Every pull/push operation automatically creates a snapshot before making changes
- Snapshots are stored in `~/.claude-code-sync/snapshots/`
- Undo operations restore files/git state from the snapshot
- After successful undo, the snapshot is automatically deleted
- Operation history is updated to reflect the undo

**Note:** You can only undo the most recent operation of each type. Once you run a new pull/push, the previous snapshot is replaced.

### `history`

**NEW in v0.2.0!** View and manage operation history.

```bash
claude-code-sync history <COMMAND>
```

**Commands:**
- `list`: List recent sync operations
- `last`: Show detailed information about the last operation
- `clear`: Clear all operation history

**Options for `list`:**
- `--limit, -l <N>`: Number of operations to show (default: 10)

**Options for `last`:**
- `--operation-type, -t <TYPE>`: Filter by operation type (`pull` or `push`)

**Examples:**
```bash
# List the last 10 operations
claude-code-sync history list

# List the last 20 operations
claude-code-sync history list --limit 20

# Show details of the last operation (pull or push)
claude-code-sync history last

# Show details of the last pull operation only
claude-code-sync history last -t pull

# Show details of the last push operation only
claude-code-sync history last -t push

# Clear all operation history
claude-code-sync history clear
```

**History Information:**
Each history entry shows:
- Operation type (PULL or PUSH)
- Timestamp
- Branch name
- Number of conversations affected
- Statistics (added, modified, conflicts, unchanged)
- Snapshot availability for undo

**History Storage:**
- Operation history is stored in `~/.claude-code-sync/operation-history.json`
- Up to 5 operations are kept (automatically rotated)
- Each operation includes details about affected conversations

## Conflict Resolution

When the same conversation session is modified on different machines, `claude-code-sync` detects this as a conflict.

### Smart Merge (NEW!)

**Smart merge is now the default conflict resolution strategy!** When conflicts are detected, `claude-code-sync` automatically attempts to intelligently merge both versions by:

- **Analyzing message UUIDs and parent relationships**: Builds a message tree to understand conversation structure
- **Resolving edited messages by timestamp**: If the same message was edited on both machines, keeps the newer version
- **Preserving all conversation branches**: When conversations diverge (same parent, different continuations), keeps all branches intact
- **Handling entries without UUIDs**: Falls back to timestamp-based merging for system events

**Smart merge automatically handles:**
- ✅ Non-overlapping changes (simple merge)
- ✅ Message additions to different parts of the conversation
- ✅ Conversation branches (multiple continuations from the same point)
- ✅ Edited messages (resolved by timestamp)
- ✅ Mixed UUID and non-UUID entries

If smart merge fails (e.g., due to corrupted data), the system falls back to interactive or "keep both" resolution.

### Interactive Conflict Resolution (New!)

When running in an interactive terminal, `claude-code-sync` now provides a **TUI (Text User Interface)** for resolving conflicts:

```bash
# Pull with interactive conflict resolution
claude-code-sync pull

# Or sync (pull + push)
claude-code-sync sync
```

**Interactive Features:**
- 📋 **List all conflicts** with session IDs and project paths
- 🔍 **Preview differences** between local and remote versions
- 📊 **View statistics**: message counts, timestamps, file sizes
- 🎯 **Choose resolution per conflict**:
  - **Smart Merge** (combine both versions - recommended) ✨ NEW
  - Keep Local (discard remote changes)
  - Keep Remote (overwrite local file)
  - Keep Both (save remote with conflict suffix)
  - View Details (show full comparison)

**Example Interactive Flow:**
```
Found 2 conflicts during pull:

! 2 conflicts detected
  Attempting smart merge...
  ✓ Smart merged abc-123 (45 local + 52 remote = 90 total, 2 branches)
  ✓ Smart merged def-456 (30 local + 35 remote = 65 total, 0 branches)
  ✓ Successfully smart merged 2/2 conflicts

Pull complete!
```

**Example: Smart Merge Failure with Interactive Fallback:**
```
Found 1 conflicts during pull:

! 1 conflicts detected
  Attempting smart merge...
  ⚠ Smart merge failed for xyz-789: circular reference detected
  Falling back to manual resolution...
  ! 1 conflicts require manual resolution

→ Running in interactive mode for remaining conflicts

Conflict 1 of 1
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Session ID: xyz-789
Project: my-project
Local:  45 messages, last modified 2 hours ago (15.2 KB)
Remote: 52 messages, last modified 1 hour ago (18.5 KB)

How do you want to resolve this conflict?
❯ Smart Merge (combine both versions - recommended)
  Keep Local Version (discard remote)
  Keep Remote Version (overwrite local)
  Keep Both (save remote with conflict suffix)
  View Detailed Comparison
```

### Automatic Resolution (Non-Interactive)

When not in an interactive terminal (CI/CD, scripts), conflicts are automatically resolved:

**Resolution Strategy:**
- Local version: Kept as-is
- Remote version: Saved with suffix `-conflict-<timestamp>.jsonl`
- A detailed conflict report is generated

**Example:**

If session `abc-123.jsonl` conflicts:
- Local: `~/.claude/projects/my-project/abc-123.jsonl` (unchanged)
- Remote: `~/.claude/projects/my-project/abc-123-conflict-20250117-143022.jsonl` (saved separately)

You can then manually review both versions and decide which to keep.

## Configuration File

Configuration is stored in `~/.claude-code-sync.toml`:

```toml
# Exclude projects older than N days
exclude_older_than_days = 30

# Include only these project path patterns
include_patterns = ["*my-project*", "*work*"]

# Exclude these project path patterns
exclude_patterns = ["*test*", "*temp*"]

# Maximum file size in bytes (10MB default)
max_file_size_bytes = 10485760
```

## Sync State

Sync state is stored in `~/.claude-code-sync/`:
- `state.json`: Current sync repository configuration
- `operation-history.json`: History of sync operations (up to 5 entries)
- `snapshots/`: Directory containing snapshots for undo operations
- `latest-conflict-report.json`: Most recent conflict report

## Use Cases

### Daily Backup Workflow

```bash
# At the end of each day
claude-code-sync push -m "Daily backup $(date +%Y-%m-%d)"
```

### Multi-Machine Development

**On Machine A:**
```bash
claude-code-sync init --repo ~/claude-backup --remote git@github.com:user/claude-history.git
claude-code-sync push
```

**On Machine B:**
```bash
claude-code-sync init --repo ~/claude-backup --remote git@github.com:user/claude-history.git
claude-code-sync pull
# Work on Machine B
claude-code-sync push
```

**Back on Machine A:**
```bash
claude-code-sync pull  # Merges Machine B's changes
```

### Automated Backup (Cron)

Add to your crontab:

```bash
# Backup Claude Code history every night at 2 AM
0 2 * * * /usr/local/bin/claude-code-sync push --message "Automated backup" >> ~/claude-code-sync.log 2>&1
```

## Architecture

### Module Overview

- **parser.rs**: JSONL conversation file parser
- **git.rs**: Git operations wrapper (using `git2` crate)
- **sync.rs**: Core sync engine with push/pull logic and snapshot integration
- **conflict.rs**: Conflict detection and resolution
- **interactive_conflict.rs**: Interactive TUI for conflict resolution (NEW!)
- **filter.rs**: Configuration and filtering system
- **config.rs**: Configuration management and defaults (NEW!)
- **report.rs**: Conflict reporting in JSON/Markdown formats
- **history.rs**: Operation history tracking and management
- **undo.rs**: Snapshot-based undo functionality for pull/push operations
- **onboarding.rs**: Interactive first-time setup wizard (NEW!)
- **logger.rs**: Enhanced logging system with file and console output (NEW!)
- **main.rs**: CLI interface (using `clap`)

### File Format

Claude Code stores conversations in JSONL format:

```json
{"type":"user","uuid":"...","sessionId":"...","timestamp":"...","message":{...}}
{"type":"assistant","uuid":"...","sessionId":"...","timestamp":"...","message":{...}}
{"type":"file-history-snapshot","messageId":"...","snapshot":{...}}
```

Each line is a separate JSON object representing a conversation event.

## Dependencies

- `clap`: CLI argument parsing
- `serde` + `serde_json`: JSON parsing
- `git2`: Git operations
- `toml`: Configuration parsing
- `anyhow`: Error handling
- `chrono`: Timestamp handling
- `walkdir`: Directory traversal
- `colored`: Terminal colors
- `dirs`: Cross-platform directory paths
- `uuid`: Snapshot identification
- `base64`: Binary file encoding in snapshots
- `inquire`: Interactive prompts and TUI menus
- `log`: Logging facade
- `env_logger`: Console logging implementation
- `atty`: Terminal detection for interactive mode

## Security Considerations

- Conversation history may contain sensitive information
- Use private git repositories for remote storage
- Consider encrypting the git repository for additional security
- SSH keys or access tokens are recommended for git authentication

## Logging

`claude-code-sync` provides comprehensive logging to help you track operations and troubleshoot issues.

### Console Logging

Control console output with the `RUST_LOG` environment variable:

```bash
# Show all debug messages
RUST_LOG=debug claude-code-sync sync

# Only show errors
RUST_LOG=error claude-code-sync push

# Only show warnings and errors
RUST_LOG=warn claude-code-sync pull

# Show info, warnings, and errors (default)
claude-code-sync sync

# Disable console output (file logging continues)
RUST_LOG=off claude-code-sync status
```

**Log Levels:**
- `trace` - Everything (very verbose)
- `debug` - Debug information and above
- `info` - Informational messages, warnings, and errors (default)
- `warn` - Warnings and errors only
- `error` - Errors only
- `off` - No console output

### File Logging

All operations are automatically logged to a file, regardless of console settings:

**Log File Locations:**
- **Linux**: `~/.config/claude-code-sync/claude-code-sync.log` or `$XDG_CONFIG_HOME/claude-code-sync/claude-code-sync.log`
- **macOS**: `~/Library/Application Support/claude-code-sync/claude-code-sync.log`
- **Windows**: `%APPDATA%\claude-code-sync\claude-code-sync.log`

**File Logging Features:**
- ✅ Captures all log levels (trace to error)
- ✅ Persists across sessions
- ✅ Useful for debugging and audit trails
- ✅ Automatically rotated to prevent excessive disk usage

**Example:**
```bash
# Run sync silently, check logs later
RUST_LOG=off claude-code-sync sync

# View the log file
cat ~/.config/claude-code-sync/claude-code-sync.log
```

## Troubleshooting

### "Sync not initialized"

Run `claude-code-sync init` first to set up the sync repository, or let the interactive onboarding guide you.

### "Failed to push to remote"

Check:
- Git remote URL is correct
- SSH keys or credentials are configured
- Network connectivity
- Remote repository permissions

### Conflicts on every pull

This may indicate:
- Clock skew between machines
- Different filter configurations
- Same conversations being actively used on multiple machines

## Contributing

Contributions are welcome! Please:
1. Fork the repository
2. Create a feature branch
3. Add tests for new functionality
4. Submit a pull request

## Roadmap

Future enhancements:
- [ ] Export conversations to readable formats (Markdown, HTML)
- [ ] Compression for large history files
- [ ] Encryption support
- [x] **Smart merge (combine non-conflicting conversation branches)** ✅ **COMPLETED!**
- [ ] Web UI for browsing history
- [ ] Integration with Claude Code as a plugin
- [ ] Interactive TUI for configuration management
- [ ] Snapshot cleanup command for managing disk space
