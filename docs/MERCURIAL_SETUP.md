# Mercurial Setup Guide

This guide explains how to use `claude-code-sync` with Mercurial (hg) instead of Git.

## Overview

`claude-code-sync` supports both Git and Mercurial as SCM backends. While Git is the default, you can switch to Mercurial if you prefer its workflow or already use it for your projects.

---

## Prerequisites

### Install Mercurial

**macOS:**
```bash
brew install mercurial
```

**Ubuntu/Debian:**
```bash
sudo apt-get update
sudo apt-get install mercurial
```

**Fedora/RHEL:**
```bash
sudo dnf install mercurial
```

**Windows:**
Download from [https://www.mercurial-scm.org/](https://www.mercurial-scm.org/)

### Verify Installation

```bash
hg --version
```

You should see output like:
```
Mercurial Distributed SCM (version 6.x.x)
```

---

## Quick Start

### 1. Configure Backend

```bash
# Set Mercurial as the SCM backend
claude-code-sync config --scm-backend mercurial
```

### 2. Initialize Repository

```bash
# Create a new Mercurial repository
claude-code-sync init --repo ~/claude-backup-hg
```

### 3. Push Your History

```bash
claude-code-sync push
```

---

## Using Remote Repositories

### With Bitbucket

```bash
# Initialize with Bitbucket remote
claude-code-sync init \
  --repo ~/claude-backup-hg \
  --remote https://bitbucket.org/username/claude-history

# Push
claude-code-sync push
```

### With Self-Hosted Mercurial

```bash
# Initialize with self-hosted server
claude-code-sync init \
  --repo ~/claude-backup-hg \
  --remote ssh://hg@yourserver.com/repos/claude-history
```

---

## Authentication

### HTTPS Authentication

Mercurial uses `~/.hgrc` for authentication. Add your credentials:

```ini
# ~/.hgrc
[auth]
bb.prefix = https://bitbucket.org
bb.username = your-username
bb.password = your-app-password
```

**Security Note:** For better security, use SSH or app passwords instead of your main account password.

### SSH Authentication

1. **Generate SSH key** (if needed):
   ```bash
   ssh-keygen -t ed25519 -C "your_email@example.com"
   ```

2. **Add to SSH agent:**
   ```bash
   eval "$(ssh-agent -s)"
   ssh-add ~/.ssh/id_ed25519
   ```

3. **Add public key to Bitbucket/server:**
   ```bash
   cat ~/.ssh/id_ed25519.pub
   # Copy and add to your Mercurial hosting service
   ```

4. **Use SSH URL:**
   ```bash
   claude-code-sync init \
     --repo ~/claude-backup-hg \
     --remote ssh://hg@bitbucket.org/username/claude-history
   ```

---

## Mercurial-Specific Behavior

### Command Mapping

| claude-code-sync | Mercurial Command |
|------------------|-------------------|
| `init` | `hg init` |
| `push` (stage) | `hg addremove` |
| `push` (commit) | `hg commit -m` |
| `push` (remote) | `hg push` |
| `pull` | `hg pull -u` |
| `status` | `hg status` |
| `undo push` | `hg update -r` |

### Branch Handling

- Mercurial uses named branches differently than Git
- The default branch is `default` (not `main` or `master`)
- `claude-code-sync` uses `hg branch` to detect the current branch

### Reset Behavior

When undoing a push operation:
- Git: Soft reset preserves working directory changes
- Mercurial: `hg update` to a previous revision removes files from newer revisions

This is a semantic difference between the two SCMs.

---

## Configuration File

When using Mercurial, your `~/.claude-code-sync.toml` might look like:

```toml
exclude_older_than_days = 30
include_patterns = []
exclude_patterns = ["*test*"]
max_file_size_bytes = 10485760
exclude_attachments = false
scm_backend = "mercurial"
sync_subdirectory = "projects"
```

---

## Non-Interactive Init with Mercurial

Create `~/.claude-code-sync-init.toml`:

```toml
repo_path = "~/claude-history-hg"
remote_url = "ssh://hg@bitbucket.org/user/claude-history"
clone = true
scm_backend = "mercurial"
exclude_attachments = true
```

Then run:
```bash
claude-code-sync init
```

---

## Limitations

### No LFS Support

Git LFS is not available with Mercurial. If you try to enable LFS with Mercurial:

```bash
claude-code-sync config --scm-backend mercurial --enable-lfs true
```

You'll get an error:
```
Error: Git LFS is only supported with the 'git' backend. Current backend: 'mercurial'
```

**Workaround:** If you have large files, consider:
- Using the Git backend with LFS
- Excluding large attachments: `--exclude-attachments true`
- Using Mercurial's largefiles extension (manual setup required)

### Platform Support

Mercurial support has been tested on:
- Linux (Ubuntu)
- macOS
- Windows (with Mercurial installed)

---

## Switching Between Backends

You can switch backends, but note:
- Each backend uses different repository formats (`.git` vs `.hg`)
- Switching requires reinitializing the repository
- Your sync history will start fresh with the new backend

```bash
# Switch from Git to Mercurial
claude-code-sync config --scm-backend mercurial

# Reinitialize (creates new .hg repository)
claude-code-sync init --repo ~/new-hg-backup

# Push history to new repo
claude-code-sync push
```

---

## Troubleshooting

### "hg: command not found"

**Solution:** Install Mercurial (see Prerequisites above)

### "abort: repository ... not found"

**Solution:** Check the remote URL:
```bash
# Verify URL is accessible
hg identify https://bitbucket.org/user/repo
```

### "abort: authorization required"

**Solution:** Configure authentication in `~/.hgrc`:
```ini
[auth]
default.prefix = *
default.username = your-username
```

### Push fails with "nothing to push"

This means your local repository is up to date with the remote. This is normal if you've already pushed.

---

## Getting Help

- Mercurial documentation: [https://www.mercurial-scm.org/wiki/](https://www.mercurial-scm.org/wiki/)
- Bitbucket documentation: [https://support.atlassian.com/bitbucket-cloud/](https://support.atlassian.com/bitbucket-cloud/)
- `claude-code-sync` issues: [https://github.com/perfectra1n/claude-code-sync/issues](https://github.com/perfectra1n/claude-code-sync/issues)
