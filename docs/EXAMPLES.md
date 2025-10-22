# claude-code-sync Examples

This document provides practical examples of using `claude-code-sync` in various scenarios.

## Basic Setup and First Sync

### Step 1: Initialize with Local Repository

```bash
# Create a local backup repository
claude-code-sync init --repo ~/claude-backup
```

Output:
```
Initializing Claude Code sync repository...
  Creating new repository at /home/user/claude-backup
Sync repository initialized successfully!

Next steps: claude-code-sync push
```

### Step 2: Push Your History

```bash
# Push all conversation history
claude-code-sync push
```

Output:
```
Pushing Claude Code history...
  Discovering conversation sessions...
  Found 15 sessions
  Copying sessions to sync repository...
  Committing changes...
  ✓ Committed: Sync 15 sessions at 2025-01-17 14:30:22 UTC
Push complete!
```

## Multi-Machine Synchronization

### Scenario: Work laptop and home computer

**On Work Laptop (initial setup):**

```bash
# 1. Initialize with GitHub remote
claude-code-sync init \
  --repo ~/claude-backup \
  --remote git@github.com:yourname/claude-history-private.git

# 2. Push your work conversations
claude-code-sync push
```

**On Home Computer:**

```bash
# 1. Clone the same repository
claude-code-sync init \
  --repo ~/claude-backup \
  --remote git@github.com:yourname/claude-history-private.git

# 2. Pull conversations from work
claude-code-sync pull
```

Output:
```
Pulling Claude Code history...
  Fetching from remote...
  ✓ Pulled from origin/main
  Discovering local sessions...
  Found 8 local sessions
  Discovering remote sessions...
  Found 15 remote sessions
  Detecting conflicts...
  ✓ No conflicts detected
  Merging non-conflicting sessions...
  ✓ Merged 7 sessions
Pull complete!
```

**Back on Work Laptop (after working at home):**

```bash
# Pull changes made at home
claude-code-sync pull
```

## Handling Conflicts

### Example: Same conversation modified on two machines

**Scenario:** You continued the same Claude Code session on both machines.

```bash
# On Machine B, pull from Machine A
claude-code-sync pull
```

Output:
```
Pulling Claude Code history...
  Fetching from remote...
  ✓ Pulled from origin/main
  Discovering local sessions...
  Found 10 local sessions
  Discovering remote sessions...
  Found 10 remote sessions
  Detecting conflicts...
  ! 1 conflicts detected

Conflict Resolution:
  → remote version saved as: -root-repos-my-project/abc-123-conflict-20250117-143022.jsonl

Hint: View details with: claude-code-sync report
```

**View conflict details:**

```bash
claude-code-sync report
```

Output:
```
=== Conflict Report ===
Timestamp: 2025-01-17T14:30:22Z
Total Conflicts: 1

Conflicts:

1. Session: abc-123-def-456
   Resolution: Keep both (remote renamed to .../abc-123-conflict-20250117-143022.jsonl)
   Local:
     File: /home/user/.claude/projects/-root-repos-my-project/abc-123.jsonl
     Messages: 45
     Updated: 2025-01-17T14:00:00Z
   Remote:
     File: /home/user/claude-backup/projects/-root-repos-my-project/abc-123.jsonl
     Messages: 42
     Updated: 2025-01-17T10:30:00Z
```

## Advanced Configuration

### Exclude Old Projects

```bash
# Only sync conversations from the last 30 days
claude-code-sync config --exclude-older-than 30
```

Output:
```
Set exclude_older_than_days to 30 days
Configuration saved successfully!
```

### Selective Project Sync

```bash
# Only sync specific projects
claude-code-sync config --include-projects "*important*,*work*"
```

Output:
```
Set include patterns: ["*important*", "*work*"]
Configuration saved successfully!
```

### Exclude Test Projects

```bash
# Exclude temporary or test projects
claude-code-sync config --exclude-projects "*test*,*temp*,*playground*"
```

Output:
```
Set exclude patterns: ["*test*", "*temp*", "*playground*"]
Configuration saved successfully!
```

### View Current Configuration

```bash
claude-code-sync config --show
```

Output:
```
Current Filter Configuration:
  Exclude older than: 30 days
  Include patterns: *important*, *work*
  Exclude patterns: *test*, *temp*, *playground*
  Max file size: 10485760 bytes (10.00 MB)
```

## Status and Monitoring

### Check Sync Status

```bash
claude-code-sync status
```

Output:
```
=== Claude Code Sync Status ===

Repository:
  Path: /home/user/claude-backup
  Remote: Configured
  Branch: main
  Uncommitted changes: No

Sessions:
  Local: 15
  Sync repo: 15
```

### Detailed Status with Files

```bash
claude-code-sync status --show-files
```

Output:
```
=== Claude Code Sync Status ===

Repository:
  Path: /home/user/claude-backup
  Remote: Configured
  Branch: main
  Uncommitted changes: No

Sessions:
  Local: 15
  Sync repo: 15

Local session files:
  -root-repos-my-project/abc-123.jsonl (45 messages)
  -root-repos-another-project/def-456.jsonl (32 messages)
  -root-repos-work-stuff/ghi-789.jsonl (18 messages)
  ... and 12 more
```

### Check for Conflicts

```bash
claude-code-sync status --show-conflicts
```

## Generating Reports

### Markdown Report

```bash
claude-code-sync report --format markdown
```

Output:
```markdown
# Claude Code Sync Conflict Report

**Generated:** 2025-01-17T14:30:22Z
**Total Conflicts:** 1

## Conflicts

### 1. Session: `abc-123-def-456`

- **Resolution:** Keep both (remote renamed to .../abc-123-conflict-20250117-143022.jsonl)
- **Local File:** `/home/user/.claude/projects/-root-repos-my-project/abc-123.jsonl`
  - Messages: 45
  - Last Updated: 2025-01-17T14:00:00Z
- **Remote File:** `/home/user/claude-backup/projects/-root-repos-my-project/abc-123.jsonl`
  - Messages: 42
  - Last Updated: 2025-01-17T10:30:00Z
```

### JSON Report to File

```bash
claude-code-sync report --format json --output conflicts.json
```

Output:
```
Report saved to: conflicts.json
```

## Automated Workflows

### Daily Backup Script

Create `~/bin/claude-backup.sh`:

```bash
#!/bin/bash
# Daily Claude Code backup script

DATE=$(date +%Y-%m-%d)
LOG_FILE=~/claude-code-sync-backup.log

echo "[$DATE] Starting Claude Code backup..." >> "$LOG_FILE"

if /usr/local/bin/claude-code-sync push --message "Automated backup $DATE" >> "$LOG_FILE" 2>&1; then
    echo "[$DATE] Backup completed successfully" >> "$LOG_FILE"
else
    echo "[$DATE] Backup failed!" >> "$LOG_FILE"
fi
```

Add to crontab:

```bash
# Backup Claude Code history daily at 11 PM
0 23 * * * ~/bin/claude-backup.sh
```

### Pre-shutdown Hook

Create a systemd service to backup before shutdown (Linux):

`/etc/systemd/system/claude-code-sync-shutdown.service`:

```ini
[Unit]
Description=Backup Claude Code history before shutdown
DefaultDependencies=no
Before=shutdown.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/claude-code-sync push --message "Shutdown backup"
TimeoutStartSec=30

[Install]
WantedBy=shutdown.target
```

Enable:
```bash
sudo systemctl enable claude-code-sync-shutdown.service
```

## Troubleshooting Examples

### Example: Sync Not Initialized

```bash
$ claude-code-sync push
Error: Sync not initialized. Run 'claude-code-sync init' first.
```

**Solution:**
```bash
claude-code-sync init --repo ~/claude-backup
```

### Example: Git Authentication Issues

```bash
$ claude-code-sync push
Pushing Claude Code history...
  ...
  Pushing to remote...
Warning: Failed to push: authentication required
```

**Solution:** Set up SSH keys or Git credential manager:
```bash
# Generate SSH key if needed
ssh-keygen -t ed25519 -C "your_email@example.com"

# Add to GitHub
cat ~/.ssh/id_ed25519.pub
# Copy and add to GitHub SSH keys settings
```

### Example: Large History Files

If you have very large conversation files:

```bash
# Set a size limit (5MB)
claude-code-sync config
# Edit ~/.claude-code-sync.toml manually:
# max_file_size_bytes = 5242880
```

## Best Practices

### 1. Regular Backups

Set up automated daily backups:
```bash
# Add to crontab
0 23 * * * /usr/local/bin/claude-code-sync push --message "Daily backup $(date +\%Y-\%m-\%d)"
```

### 2. Before Major System Changes

```bash
# Before OS upgrade
claude-code-sync push --message "Pre-upgrade backup $(date)"
```

### 3. Starting Work on Different Machine

```bash
# Always pull first
claude-code-sync pull

# Work...

# Push when done
claude-code-sync push
```

### 4. Handling Frequent Conflicts

If you often work on the same projects across machines:

- Consider using separate Claude Code projects per machine
- Pull/push more frequently
- Review conflict reports to understand patterns

### 5. Repository Organization

Use a private GitHub/GitLab repository:
- Enable branch protection
- Use meaningful commit messages
- Tag important milestones

```bash
# Example: tag before major project changes
cd ~/claude-backup
git tag -a v1.0-pre-refactor -m "Before major refactoring"
git push origin v1.0-pre-refactor
```
