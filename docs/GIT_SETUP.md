# Git Authentication Setup Guide

This guide explains how to set up git authentication for `claude-code-sync` to work with remote repositories.

## Overview

`claude-code-sync` uses git's built-in credential system, which means it works with whatever git authentication you already have configured on your system.

---

## HTTPS Authentication

### Option 1: Git Credential Helper (Recommended)

This is the easiest method and works across all platforms.

```bash
# Configure git to remember credentials
git config --global credential.helper store

# Test with your repository
cd ~/claude-backup
git push origin main
# You'll be prompted for username and password/token once
# Credentials will be saved for future use
```

**What this does:**
- Saves credentials to `~/.git-credentials` (plain text - see security note below)
- Automatically used by `claude-code-sync` for all operations
- No additional setup needed

### Option 2: Git Credential Cache (More Secure)

Stores credentials in memory for a specified time.

```bash
# Cache credentials for 1 hour (3600 seconds)
git config --global credential.helper 'cache --timeout=3600'

# Or cache for 8 hours (28800 seconds)
git config --global credential.helper 'cache --timeout=28800'
```

**What this does:**
- Stores credentials in memory only
- Expires after the timeout period
- You'll need to re-enter credentials after timeout

### Option 3: GitHub Personal Access Token (Most Secure for GitHub)

1. **Create a Personal Access Token**
   - Go to https://github.com/settings/tokens
   - Click "Generate new token" → "Generate new token (classic)"
   - Select scopes: `repo` (full control of private repositories)
   - Click "Generate token"
   - **Copy the token** (you won't see it again!)

2. **Configure git to use the token**
   ```bash
   # Set up credential helper
   git config --global credential.helper store

   # Test push with token as password
   cd ~/claude-backup
   git push origin main
   # Username: your-github-username
   # Password: paste-your-token-here
   ```

3. **Now claude-code-sync will work automatically**
   ```bash
   claude-code-sync push
   ```

### Troubleshooting HTTPS

If you get "Authentication failed":

```bash
# Check current credential helper
git config --global credential.helper

# Remove cached credentials
git credential reject <<EOF
protocol=https
host=github.com
EOF

# Try again - you'll be prompted for credentials
cd ~/claude-backup
git push origin main
```

---

## SSH Authentication

SSH is more secure and doesn't require entering credentials.

### Setup SSH Keys

1. **Generate SSH key** (if you don't have one)
   ```bash
   ssh-keygen -t ed25519 -C "your_email@example.com"
   # Press Enter to accept default location (~/.ssh/id_ed25519)
   # Optionally set a passphrase
   ```

2. **Start SSH agent**
   ```bash
   eval "$(ssh-agent -s)"
   ssh-add ~/.ssh/id_ed25519
   ```

3. **Add SSH key to GitHub/GitLab**

   **For GitHub:**
   ```bash
   # Copy your public key
   cat ~/.ssh/id_ed25519.pub
   # Copy the output

   # Then go to:
   # https://github.com/settings/ssh/new
   # Paste your key and save
   ```

   **For GitLab:**
   ```bash
   cat ~/.ssh/id_ed25519.pub
   # Then go to:
   # https://gitlab.com/-/profile/keys
   # Paste your key and save
   ```

4. **Test SSH connection**
   ```bash
   # For GitHub
   ssh -T git@github.com
   # Should see: "Hi username! You've successfully authenticated..."

   # For GitLab
   ssh -T git@gitlab.com
   # Should see: "Welcome to GitLab, @username!"
   ```

5. **Use SSH URL when initializing**
   ```bash
   claude-code-sync init \
     --repo ~/claude-backup \
     --remote git@github.com:username/claude-history.git
   ```

### Troubleshooting SSH

If you get "Permission denied (publickey)":

```bash
# Check if SSH agent is running
ssh-add -l

# If "Could not open a connection to your authentication agent"
eval "$(ssh-agent -s)"
ssh-add ~/.ssh/id_ed25519

# Test connection again
ssh -T git@github.com
```

---

## Platform-Specific Instructions

### Windows (WSL)

```bash
# Using HTTPS with credential manager
git config --global credential.helper "/mnt/c/Program\ Files/Git/mingw64/bin/git-credential-manager.exe"

# Or use credential store
git config --global credential.helper store
```

### macOS

```bash
# Use macOS Keychain
git config --global credential.helper osxkeychain

# Test
cd ~/claude-backup
git push origin main
# Credentials will be saved to Keychain
```

### Linux

```bash
# Use libsecret for secure storage
# First, install libsecret
sudo apt-get install libsecret-1-0 libsecret-1-dev  # Debian/Ubuntu
sudo dnf install libsecret-devel  # Fedora/RHEL

# Configure git to use it
git config --global credential.helper /usr/share/doc/git/contrib/credential/libsecret/git-credential-libsecret

# Or use simple store
git config --global credential.helper store
```

---

## Security Considerations

### HTTPS Token/Password Storage

**credential.helper store:**
- ✅ Convenient - no need to re-enter credentials
- ❌ Stores in plain text at `~/.git-credentials`
- ⚠️ Anyone with access to your account can read credentials

**credential.helper cache:**
- ✅ Stores in memory only
- ✅ Expires after timeout
- ❌ Need to re-enter periodically

**Best Practice:**
1. Use SSH keys (most secure)
2. Use credential cache (temporary storage)
3. Use credential store only if necessary (convenience vs security trade-off)

### SSH Key Security

**Best Practices:**
- Always use a passphrase for SSH keys
- Use ed25519 (modern, secure) or RSA 4096-bit keys
- Never share your private key
- Store keys in `~/.ssh/` with proper permissions (600)

---

## Verification

After setup, verify everything works:

```bash
# 1. Initialize claude-code-sync
claude-code-sync init --repo ~/claude-backup --remote YOUR_REPO_URL

# 2. Push (should not prompt for credentials)
claude-code-sync push

# 3. Check status
claude-code-sync status

# 4. If successful, you'll see:
# ✓ Pushed to origin/main
```

---

## Common Error Messages and Solutions

### "Failed to push: authentication required"

**Solution:**
```bash
# Set up credential helper
git config --global credential.helper store

# Test git push manually first
cd ~/claude-backup
git push origin main

# Enter credentials when prompted
# Then claude-code-sync will work
```

### "Failed to push: could not read Username"

**Solution:**
```bash
# For HTTPS: Configure credentials
git config --global credential.helper store

# For SSH: Use SSH URL instead
claude-code-sync init --repo ~/backup --remote git@github.com:user/repo.git
```

### "Failed to push: network error"

**Solution:**
```bash
# Check network connectivity
ping github.com

# Check if remote is reachable
git ls-remote YOUR_REPO_URL

# Verify remote URL
cd ~/claude-backup
git remote -v
```

### "Failed to push: branch protection rules"

**Solution:**
- Go to repository settings on GitHub/GitLab
- Disable branch protection for the branch you're pushing to
- Or push to a different branch:
  ```bash
  cd ~/claude-backup
  git checkout -b claude-code-sync-backup
  git push origin claude-code-sync-backup
  ```

---

## Quick Reference

```bash
# HTTPS with token
git config --global credential.helper store
cd ~/claude-backup && git push origin main
# Enter username and token when prompted

# SSH
ssh-keygen -t ed25519 -C "email@example.com"
cat ~/.ssh/id_ed25519.pub  # Copy to GitHub/GitLab
ssh -T git@github.com  # Test

# Initialize with remote
claude-code-sync init --repo ~/backup --remote git@github.com:user/repo.git

# Push
claude-code-sync push

# Check if it worked
claude-code-sync status
```

---

## Getting Help

If you're still having issues:

1. **Test git directly first:**
   ```bash
   cd ~/claude-backup
   git push origin main
   ```

2. **Check git configuration:**
   ```bash
   git config --list | grep credential
   ```

3. **Enable debug output:**
   ```bash
   GIT_TRACE=1 GIT_CURL_VERBOSE=1 claude-code-sync push
   ```

4. **Check the full error message** - `claude-code-sync` now provides detailed error messages with suggestions

---

## Additional Resources

- [GitHub: Managing your credentials](https://docs.github.com/en/get-started/getting-started-with-git/caching-your-github-credentials-in-git)
- [Git Credential Storage](https://git-scm.com/book/en/v2/Git-Tools-Credential-Storage)
- [GitHub: Personal Access Tokens](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/creating-a-personal-access-token)
- [SSH Key Setup](https://docs.github.com/en/authentication/connecting-to-github-with-ssh)
