# claude-sync

A Rust CLI tool for syncing Claude Code conversation history across machines using git repositories.

## Overview

`claude-sync` helps you backup and synchronize your Claude Code conversation history by pushing it to a git repository. This enables:

- **Backup**: Never lose your Claude Code conversations
- **Multi-machine sync**: Keep conversation history consistent across multiple computers
- **Version control**: Track changes to your conversations over time
- **Conflict resolution**: Automatically handles divergent conversation histories

## How It Works

Claude Code stores conversation history locally in `~/.claude/projects/` as JSONL (JSON Lines) files. Each project has its own directory, and each conversation is a separate `.jsonl` file.

`claude-sync`:
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
cd claude-sync
cargo build --release
sudo cp target/release/claude-sync /usr/local/bin/
```

### Using Cargo

```bash
cargo install --path .
```

## Quick Start

### 1. Initialize Sync Repository

```bash
# Create a local sync repository
claude-sync init --repo ~/claude-history-backup

# Or with a remote git repository
claude-sync init --repo ~/claude-history-backup --remote git@github.com:username/claude-history.git
```

### 2. Push Your History

```bash
# Push all conversation history
claude-sync push

# Push with custom commit message
claude-sync push --message "Backup before system upgrade"

# Push without pushing to remote
claude-sync push --push-remote=false
```

### 3. Pull and Merge (On Another Machine)

```bash
# Initialize on the new machine with the same remote
claude-sync init --repo ~/claude-history-backup --remote git@github.com:username/claude-history.git

# Pull and merge history
claude-sync pull
```

## Commands

### `init`

Initialize a new sync repository.

```bash
claude-sync init --repo <path> [--remote <url>]
```

**Options:**
- `--repo, -r <PATH>`: Path to the git repository for storing history
- `--remote <URL>`: Optional remote git URL for pushing/pulling

**Example:**
```bash
claude-sync init --repo ~/claude-backup --remote git@github.com:user/claude-history.git
```

### `push`

Push local Claude Code history to the sync repository.

```bash
claude-sync push [--message <MSG>] [--push-remote]
```

**Options:**
- `--message, -m <MSG>`: Custom commit message
- `--push-remote`: Push to remote after committing (default: true)

**Example:**
```bash
claude-sync push -m "Weekly backup"
```

### `pull`

Pull and merge history from the sync repository.

```bash
claude-sync pull [--fetch-remote]
```

**Options:**
- `--fetch-remote`: Pull from remote before merging (default: true)

**Example:**
```bash
claude-sync pull
```

### `status`

Show sync status and information.

```bash
claude-sync status [--show-conflicts] [--show-files]
```

**Options:**
- `--show-conflicts`: Show detailed conflict information
- `--show-files`: Show which files would be synced

**Example:**
```bash
claude-sync status --show-conflicts --show-files
```

### `config`

Configure sync filters and settings.

```bash
claude-sync config [OPTIONS] [--show]
```

**Options:**
- `--exclude-older-than <DAYS>`: Exclude projects older than N days
- `--include-projects <PATTERNS>`: Include only specific project paths (comma-separated)
- `--exclude-projects <PATTERNS>`: Exclude specific project paths (comma-separated)
- `--show`: Show current configuration

**Examples:**
```bash
# Exclude conversations older than 30 days
claude-sync config --exclude-older-than 30

# Include only specific projects
claude-sync config --include-projects "*my-project*,*important-work*"

# Exclude test projects
claude-sync config --exclude-projects "*test*,*temp*"

# Show current config
claude-sync config --show
```

### `report`

View conflict reports from previous syncs.

```bash
claude-sync report [--format <FORMAT>] [--output <FILE>]
```

**Options:**
- `--format, -f <FORMAT>`: Output format: `json`, `markdown`, or `text` (default: markdown)
- `--output, -o <FILE>`: Output file (default: print to stdout)

**Examples:**
```bash
# Print markdown report to console
claude-sync report

# Save JSON report to file
claude-sync report --format json --output conflicts.json

# View as markdown
claude-sync report --format markdown | less
```

## Conflict Resolution

When the same conversation session is modified on different machines, `claude-sync` detects this as a conflict.

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

Configuration is stored in `~/.claude-sync.toml`:

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

Sync state is stored in `~/.claude-sync/`:
- `state.json`: Current sync repository configuration
- `latest-conflict-report.json`: Most recent conflict report

## Use Cases

### Daily Backup Workflow

```bash
# At the end of each day
claude-sync push -m "Daily backup $(date +%Y-%m-%d)"
```

### Multi-Machine Development

**On Machine A:**
```bash
claude-sync init --repo ~/claude-backup --remote git@github.com:user/claude-history.git
claude-sync push
```

**On Machine B:**
```bash
claude-sync init --repo ~/claude-backup --remote git@github.com:user/claude-history.git
claude-sync pull
# Work on Machine B
claude-sync push
```

**Back on Machine A:**
```bash
claude-sync pull  # Merges Machine B's changes
```

### Automated Backup (Cron)

Add to your crontab:

```bash
# Backup Claude Code history every night at 2 AM
0 2 * * * /usr/local/bin/claude-sync push --message "Automated backup" >> ~/claude-sync.log 2>&1
```

## Architecture

### Module Overview

- **parser.rs**: JSONL conversation file parser
- **git.rs**: Git operations wrapper (using `git2` crate)
- **sync.rs**: Core sync engine with push/pull logic
- **conflict.rs**: Conflict detection and resolution
- **filter.rs**: Configuration and filtering system
- **report.rs**: Conflict reporting in JSON/Markdown formats
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

## Security Considerations

- Conversation history may contain sensitive information
- Use private git repositories for remote storage
- Consider encrypting the git repository for additional security
- SSH keys or access tokens are recommended for git authentication

## Troubleshooting

### "Sync not initialized"

Run `claude-sync init` first to set up the sync repository.

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

## License

MIT License - See LICENSE file for details

## Acknowledgments

- Built with [Anthropic's Claude Code](https://docs.claude.com/claude-code)
- Inspired by the need for cross-machine conversation continuity

## Roadmap

Future enhancements:
- [ ] Selective session sync (by project, date, etc.)
- [ ] Export conversations to readable formats (Markdown, HTML)
- [ ] Compression for large history files
- [ ] Encryption support
- [ ] Smart merge (combine non-conflicting conversation branches)
- [ ] Web UI for browsing history
- [ ] Integration with Claude Code as a plugin
