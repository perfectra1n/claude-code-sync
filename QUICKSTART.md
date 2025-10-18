# Quick Start Guide

Get started with `claude-sync` in 5 minutes!

## Installation

### Option 1: Build from Source

```bash
cd claude-sync
cargo build --release
sudo cp target/release/claude-sync /usr/local/bin/
```

### Option 2: Install with Cargo

```bash
cargo install --path .
```

### Verify Installation

```bash
claude-sync --version
# Output: claude-sync 0.1.0
```

## First Time Setup (5 Steps)

### 1. Initialize Local Backup

```bash
claude-sync init --repo ~/claude-backup
```

**What this does:**
- Creates a git repository at `~/claude-backup`
- Saves configuration to `~/.claude-sync/state.json`

### 2. Push Your History

```bash
claude-sync push
```

**What this does:**
- Discovers all conversations in `~/.claude/projects/`
- Copies them to `~/claude-backup/projects/`
- Creates a git commit

### 3. Check Status

```bash
claude-sync status
```

**Example output:**
```
=== Claude Code Sync Status ===

Repository:
  Path: /home/user/claude-backup
  Remote: Not configured
  Branch: main
  Uncommitted changes: No

Sessions:
  Local: 15
  Sync repo: 15
```

### 4. (Optional) Add Remote

```bash
# Create a private GitHub repository first, then:
cd ~/claude-backup
git remote add origin git@github.com:username/claude-history-private.git
git push -u origin main
```

### 5. Test Pull

```bash
claude-sync pull
```

## Multi-Machine Setup

### On First Machine

```bash
# 1. Initialize with remote
claude-sync init \
  --repo ~/claude-backup \
  --remote git@github.com:username/claude-history.git

# 2. Push
claude-sync push
```

### On Second Machine

```bash
# 1. Initialize with same remote
claude-sync init \
  --repo ~/claude-backup \
  --remote git@github.com:username/claude-history.git

# 2. Pull
claude-sync pull
```

## Daily Workflow

### Before Starting Work

```bash
claude-sync pull  # Get latest from other machines
```

### After Finishing Work

```bash
claude-sync push  # Backup your work
```

## Common Commands

### View Help

```bash
claude-sync --help
claude-sync push --help
claude-sync pull --help
```

### Custom Commit Message

```bash
claude-sync push -m "Completed AI project analysis"
```

### Check for Conflicts

```bash
claude-sync status --show-conflicts
```

### View Last Sync Report

```bash
claude-sync report
```

### Configure Filters

```bash
# Only sync recent conversations (last 30 days)
claude-sync config --exclude-older-than 30

# Exclude test projects
claude-sync config --exclude-projects "*test*,*temp*"

# View config
claude-sync config --show
```

## Troubleshooting

### "Sync not initialized"

**Solution:** Run `claude-sync init --repo ~/claude-backup` first.

### "Failed to push to remote"

**Possible causes:**
1. SSH key not set up
2. Wrong remote URL
3. No network connection

**Solution:**
```bash
# Check remote
cd ~/claude-backup
git remote -v

# Test SSH
ssh -T git@github.com

# Set up SSH key
ssh-keygen -t ed25519
# Add public key to GitHub
```

### Conflicts Detected

**What happened:** Same conversation was continued on two machines.

**What to do:**
1. Both versions are kept (remote renamed with `-conflict-` suffix)
2. Review both files in `~/.claude/projects/`
3. Manually merge if needed
4. Delete the conflict file when done

**Example:**
```bash
# View conflicts
claude-sync report

# Files will be at:
# ~/.claude/projects/my-project/session-123.jsonl (local)
# ~/.claude/projects/my-project/session-123-conflict-20250117-143022.jsonl (remote)
```

## Automation

### Daily Backup (Cron)

Add to crontab (`crontab -e`):

```bash
# Backup Claude Code history daily at 11 PM
0 23 * * * /usr/local/bin/claude-sync push --message "Daily backup $(date +\%Y-\%m-\%d)" >> ~/claude-sync.log 2>&1
```

### Backup on System Shutdown

See `EXAMPLES.md` for systemd service configuration.

## File Locations

| What | Where |
|------|-------|
| Claude Code history | `~/.claude/projects/` |
| Sync repository | Configured via `--repo` (e.g., `~/claude-backup`) |
| Configuration | `~/.claude-sync.toml` |
| Sync state | `~/.claude-sync/state.json` |
| Conflict reports | `~/.claude-sync/latest-conflict-report.json` |

## Next Steps

- Read `README.md` for detailed documentation
- See `EXAMPLES.md` for advanced use cases
- Check `TEST_COVERAGE.md` for development info

## Getting Help

```bash
# General help
claude-sync --help

# Command-specific help
claude-sync init --help
claude-sync push --help
claude-sync pull --help
claude-sync config --help
claude-sync report --help
```

## Safety Tips

1. **Use private repositories** - Your conversations may contain sensitive info
2. **Regular backups** - Set up automated daily pushes
3. **Test on non-critical data first** - Try with a test project
4. **Keep SSH keys secure** - Use passphrase-protected keys
5. **Review conflicts carefully** - Don't blindly delete conflict files

## Performance Notes

- Syncing 100 sessions (~50MB): ~2-5 seconds
- Initial push with 1000+ sessions: ~30 seconds
- Pull with no changes: <1 second
- Push with no changes: <1 second

## What Gets Synced

✅ **Included:**
- All `.jsonl` conversation files
- All projects under `~/.claude/projects/`
- File structure and organization

❌ **Not Included:**
- Claude Code settings
- SSH keys or credentials
- Local configuration
- Cache files

## Quick Reference Card

```
┌─────────────────────────────────────────────────┐
│           claude-sync Quick Reference           │
├─────────────────────────────────────────────────┤
│ Setup                                           │
│   init --repo PATH [--remote URL]              │
│                                                 │
│ Daily Use                                       │
│   push                    # Backup              │
│   pull                    # Restore/Merge       │
│   status                  # Check status        │
│                                                 │
│ Configuration                                   │
│   config --show                                 │
│   config --exclude-older-than DAYS             │
│   config --include-projects PATTERNS           │
│   config --exclude-projects PATTERNS           │
│                                                 │
│ Conflicts                                       │
│   status --show-conflicts                       │
│   report                  # View details        │
│   report --format json -o file.json            │
├─────────────────────────────────────────────────┤
│ Files                                           │
│   ~/.claude/projects/     # Claude history      │
│   ~/.claude-sync.toml     # Configuration       │
│   ~/.claude-sync/         # State & reports     │
└─────────────────────────────────────────────────┘
```

---

**Ready to go?** Start with `claude-sync init --repo ~/claude-backup` and you're all set!
