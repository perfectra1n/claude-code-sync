# claude-code-sync

[![CI](https://github.com/perfectra1n/claude-code-sync/actions/workflows/ci.yml/badge.svg)](https://github.com/perfectra1n/claude-code-sync/actions/workflows/ci.yml)
[![Release](https://github.com/perfectra1n/claude-code-sync/actions/workflows/release.yml/badge.svg)](https://github.com/perfectra1n/claude-code-sync/actions/workflows/release.yml)
[![Documentation](https://github.com/perfectra1n/claude-code-sync/actions/workflows/docs.yml/badge.svg)](https://github.com/perfectra1n/claude-code-sync/actions/workflows/docs.yml)

A Rust CLI tool for syncing Claude Code conversation history across machines using git repositories.

![Demo](demo1.svg)

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
| **Artifact Sync** | Carry settings, skills, agents, commands, rules, hooks, plugin manifests, plans, todos, and prompt history across machines |
| **Project Map** | Pin a project to its path per machine, so renamed or relocated checkouts stay one project |
| **Neutral Paths** | Synced config files store `__HOME__` / `__CLAUDE_DIR__` instead of this machine's absolute paths |
| **Deletion Mirroring** | Deleting a skill, agent, command, rule or hook propagates, guarded so a fresh machine can never wipe the repo |
| **External Merge Tool** | Resolve a differing file in a real three-way merge window instead of picking a side |
| **Transcript Retention** | `purge` old conversations from the machine and the repo together, never sooner than Claude Code would |
| **Secrets Guard** | Hardcoded never-sync denylist plus a managed ignore block in the sync repo |
| **Bidirectional Sync** | Pull and push changes in one command with `sync` |
| **Interactive Onboarding** | First-time setup wizard guides you through configuration |
| **Non-Interactive Init** | Config file support for CI/CD and automation |
| **Smart Conflict Resolution** | Interactive TUI for resolving conflicts with preview |
| **Selective Sync** | Filter by project, date, or exclude attachments |
| **Git LFS Support** | Efficiently store large conversation files with Git LFS |
| **Mercurial Support** | Use Mercurial (hg) as an alternative to Git |
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
2. Copies them to a git repository (plus any enabled artifact categories — see [Artifact Sync](#artifact-sync))
3. Commits and optionally pushes to a remote
4. On pull, merges remote changes with local history
5. Detects conflicts (same session modified on different machines)
6. Resolves conflicts by keeping both versions with renamed files

## Artifact Sync

Conversation history is only part of a Claude Code environment. Artifact sync
carries the rest of `~/.claude` across machines, per category:

| Category | What it covers | Notes |
|----------|----------------|-------|
| `settings` | `settings.json`, `keybindings.json` | `settings.local.json` is never synced |
| `memory` | `~/.claude/CLAUDE.md` | Global user memory |
| `skills` | `~/.claude/skills/` | Custom skills, recursively |
| `agents` | `~/.claude/agents/` | Custom subagent definitions |
| `commands` | `~/.claude/commands/` | Custom slash commands |
| `rules` | `~/.claude/rules/` | Shared rule files projects import |
| `hooks` | `~/.claude/hooks/` | The scripts `settings.json` points at; they arrive executable |
| `plugins` | `installed_plugins.json`, `known_marketplaces.json` | Only the manifests — plugin caches never sync |
| `plans` | `~/.claude/plans/` | Plan-mode documents (may contain sensitive prose) |
| `todos` | `~/.claude/todos/` | Session task lists (churny; consider leaving off) |
| `prompt-history` | `~/.claude/history.jsonl` | Union-merged in both directions, so machines converge to the superset |
| attachments | non-`.jsonl` files in `~/.claude/projects/` | Images, PDFs, and per-project `memory/` dirs; governed by `--exclude-attachments`, not a toggle |

Enable categories per machine (all default **off** for existing configs; the
first-run wizard pre-selects them for new setups, except `hooks`):

```bash
# Enable everything except hooks
claude-code-sync config --enable-artifacts all

# Or pick categories
claude-code-sync config --enable-artifacts settings,skills,agents,commands,plugins,prompt-history

# Turn one off again
claude-code-sync config --disable-artifacts todos
```

`hooks` is only ever switched on by name, never by `all` or the wizard's
defaults (see below):
`claude-code-sync config --enable-artifacts hooks`.

Of a file's permissions only the executable bit travels — it is the one git
records — and it travels on its own: a `chmod +x` with no content change is
still a change to sync. It is only ever granted, never taken away, so a
repository written before this existed cannot disarm a script here, and a
`chmod -x` has to be repeated per machine. Nothing else about a mode is copied:
a pull never widens a local file, and a file a pull creates starts private.
Carrying the bit at all depends on `core.fileMode` being true in the sync
repository, which is git's default everywhere except Windows checkouts.

**Hooks are executed code.** Unlike skills and rules, which are text the model
reads, everything under `~/.claude/hooks/` runs automatically, and the
`settings.json` that registers a hook syncs alongside it. An interactive pull
confirms making a file executable, but a file the repository adds is still
written without a prompt, so enable this category only for a repository you
control.

### What is NEVER synced

A hardcoded denylist is enforced on every copy, in both directions, and cannot
be overridden by any configuration: `.credentials.json`,
`settings.local.json`, `.claude.json`, `*.pem`, `*.key`, `.env*`, `daemon*`,
`stats-cache.json`, and machine-local directories (`shell-snapshots/`,
`session-env/`, `file-history/`, `paste-cache/`, `cache/`, `debug/`,
`statsig/`, `backups/`, `sessions/`). Pull refuses these paths even if they
appear inside the sync repository, and every push maintains a managed guard
block in the repo's `.gitignore` (or `.hgignore`) as defense in depth.

### Sync repository layout

```
<sync_repo>/
  .gitignore              # managed never-sync guard block
  projects/               # conversation transcripts + attachments
  artifacts/
    settings/  memory/  skills/  agents/  commands/  rules/
    hooks/     plugins/ plans/   todos/   prompt-history/
```

### Conflict policy and undo

Artifact pulls are **remote-wins**: a file is only written when its bytes
differ, every overwritten local file is snapshotted first, and files the pull
creates are recorded so `claude-code-sync undo pull` is an exact inverse
(restores overwritten bytes, deletes created files). Interactive pulls
(`pull --interactive`) confirm each overwrite per file. `history.jsonl` is
never overwritten — both push and pull merge the union of lines, so prompt
history only ever grows.

> **Note (Git LFS):** if your `lfs_patterns` include `*.jsonl`, the repo copy
> of `history.jsonl` is LFS-tracked; content is materialized on checkout, so
> union merging still works.

## Machines that do not look alike

### Project map: machines that keep projects elsewhere

Claude Code names a project directory after its absolute path, so the same
project is `-home-user-work-app` on one machine and `-Users-someone-src-app-renamed`
on another. `use_project_name_only` collapses that to the folder name, which
loses renames and gives up whenever two checkouts share a name.

Map the project once per machine instead:

```bash
# on the Linux box
claude-code-sync config --map-project app=/home/user/work/app

# on the Mac, where the same project lives elsewhere and is named differently
claude-code-sync config --map-project app=/Users/someone/src/app-renamed

# drop a mapping
claude-code-sync config --unmap-project app
```

The repo then stores that project under `projects/app/`, and each machine
materializes it back into its own directory. Unmapped projects are untouched:
they keep `use_project_name_only` or their encoded path, exactly as before. Ids
are plain directory names, paths must be absolute and spelled exactly as the
project's own path (a trailing slash encodes differently), and two ids pointing
at one directory are refused.

A machine that has not mapped an id yet skips those files rather than writing
them to a directory Claude Code would never read; run `--map-project` there and
pull again. All of a pull's misses are reported in one warning that names every
unmapped project and how many files it holds. To get a line per file instead:

```bash
claude-code-sync config --warn-each-skipped-file true
```

### Machine-neutral paths in config files

Config categories (`settings`, `plugins`) are stored with this machine's
absolute paths replaced by `__CLAUDE_DIR__` and `__HOME__`, and rendered back on
pull. A hook command, a status line, anything that names a path travels
correctly — the rewrite is not tied to any particular key, so new settings need
no new code. Only text files are touched; other categories are stored verbatim.

### Deletions that actually propagate

Each machine records which artifact paths it last synced (in
`~/.claude/.claude-code-sync-tracked.json`, per sync repo, never itself synced).
A file that machine received before and no longer has is a deletion: push
removes it from the repo, pull removes it locally. This applies to the curated
directories only — `skills`, `agents`, `commands`, `rules`, `hooks` — never to
transcripts, attachments, plans, todos or prompt history.

Three guards keep it from destroying anything:

- A machine with no record deletes nothing, so a fresh clone can never wipe the repo.
- A category whose directory is missing locally is skipped on push: "I do not
  have `skills/`" says nothing about what the other machines hold.
- A category the repo does not have at all is skipped on pull, so an older
  branch cannot read as "everything was deleted". Deleting the *last* file of a
  category still propagates: each synced category keeps a `.synced` marker in
  the repo, because git does not track empty directories and the category would
  otherwise disappear along with its last file.

`undo push` also forgets this machine's record for that repository, since the
repo has just been rewound underneath it; the next sync re-learns.

Deleted files are snapshotted first, so `claude-code-sync undo pull` brings them
back, and `pull --interactive` asks before each one.

### Memory indexes merge instead of overwriting

`MEMORY.md` files inside a project's `memory/` directory are union-merged by
link target in both directions, the way prompt history is. A machine that knows
about fewer memories can no longer orphan the ones another machine wrote. Your
file keeps its own shape — headings, blank lines, prose and entry order stay put
— and entries the other side has and yours does not are appended. Only entry
lines travel: a heading or a note you write reaches the other machine only if
your file is the one that created its copy.

### Purging old transcripts

Syncing keeps every conversation forever, on every machine and in the repo.
`purge` removes the ones past a retention window from **both** sides at once —
removing them only here would bring them back on the next pull, and only in the
repo would send them back up from the next machine that still has them:

```bash
claude-code-sync purge --dry-run      # what would go, and how much space
claude-code-sync purge                # asks first
claude-code-sync purge --yes          # for scripts; required outside a terminal
claude-code-sync purge --older-than 365
```

The default window is the **longer of six months and this machine's own Claude
Code retention** (`cleanupPeriodDays` in `~/.claude/settings.json`, 30 days by
default) — Claude Code deletes those transcripts by itself anyway, so the sync
repo is never the shorter-lived copy. That is also the minimum: a window of your
own can only be longer. Set one, and let a sync do it for you, with:

```bash
claude-code-sync config --purge-older-than 365
claude-code-sync config --purge-after-sync true   # runs between pull and push
```

A transcript's age is its **last message**, not the file's timestamp: a
conversation pulled onto a new machine today is still as old as it was. A
transcript with no readable timestamp is never purged. Each removal takes what
Claude Code keeps beside it — the session's `subagents/` and `tool-results/`
directories and any superseded or orphaned copies — and lands in the repo as its
own commit, so `git` still has everything until the history itself is rewritten.

### External merge tool

When a pulled file differs from the local one, `pull --interactive` offers to
open a real three-way merge instead of only choosing a side:

```bash
claude-code-sync config --merge-tool "phpstorm merge"
```

The tool is invoked as `<merge_tool> <local> <remote> <base> <output>` (the
JetBrains argument order); whatever it writes to `<output>` is what lands. The
base pane is empty — an artifact has no recorded common ancestor. Set
`CLAUDE_CODE_SYNC_MERGE_TIMEOUT_SECONDS` to change the 15-minute wait, or pass
an empty string to `--merge-tool` to go back to the terminal picker.

### Binaries per tag

Pushing a tag builds every platform on its own runner and attaches the archives
and checksums to that tag's GitHub release:

```bash
git tag v0.4.0 && git push origin v0.4.0
```

A tag release-please creates works the same way; a tag you push by hand gets a
release created for it with generated notes. Grab the asset for your platform
from the [releases page](https://github.com/perfectra1n/claude-code-sync/releases).

To build all of them locally instead — one Linux or macOS host, no Apple
hardware and no Windows — run `bin/release.sh` (needs `cargo-zigbuild`, zig and
python3); the binaries land in `dist/`. Two details make that possible, both
documented in the script: `chrono`'s `clock` feature is off (it links
CoreFoundation on macOS, so local time for log lines comes from `time`
instead), and the `synchronization` import library Rust's Windows std needs is
generated from zig's own API-set definition.

## Installation

### Prebuilt Binaries (Recommended)

Download the latest release binary for your platform directly from GitHub:

```bash
# Linux (x86_64)
curl -fsSL https://github.com/perfectra1n/claude-code-sync/releases/latest/download/claude-code-sync-linux-x86_64.tar.gz | tar xz
sudo mv claude-code-sync /usr/local/bin/

# macOS (Apple Silicon)
curl -fsSL https://github.com/perfectra1n/claude-code-sync/releases/latest/download/claude-code-sync-macos-aarch64.tar.gz | tar xz
sudo mv claude-code-sync /usr/local/bin/

# macOS (Intel)
curl -fsSL https://github.com/perfectra1n/claude-code-sync/releases/latest/download/claude-code-sync-macos-x86_64.tar.gz | tar xz
sudo mv claude-code-sync /usr/local/bin/
```

Each release asset ships with a `.sha256` checksum file alongside it — see the
[releases page](https://github.com/perfectra1n/claude-code-sync/releases) for
all assets and versions.

**To update:** re-run the same command; it always fetches the latest release.

### Using Cargo (Build from GitHub)

If you have a Rust toolchain installed, you can build and install straight from
this repository without cloning it:

```bash
cargo install --locked --git https://github.com/perfectra1n/claude-code-sync
```

**To update:** re-run the same command. Cargo tracks the commit it built from,
so it rebuilds and reinstalls whenever new commits land on the default branch.

### From Source

```bash
git clone https://github.com/perfectra1n/claude-code-sync
cd claude-code-sync
cargo install --locked --path .
claude-code-sync --help
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
- `--config <PATH>`: Load configuration from TOML file (for non-interactive init)

**Example:**
```bash
claude-code-sync init --repo ~/claude-backup --remote git@github.com:user/claude-history.git
```

#### Non-Interactive Initialization (CI/CD)

For automation and headless environments, use a config file:

```bash
# Use explicit config file
claude-code-sync init --config /path/to/init-config.toml

# Or use default config file locations (checked in order):
# 1. $CLAUDE_CODE_SYNC_INIT_CONFIG environment variable
# 2. ~/.claude-code-sync-init.toml
# 3. <config-dir>/init.toml
claude-code-sync init
```

**Example config file (`~/.claude-code-sync-init.toml`):**
```toml
repo_path = "~/claude-history-sync"
remote_url = "https://github.com/user/repo.git"
clone = true
exclude_attachments = true
enable_lfs = true
scm_backend = "git"
sync_subdirectory = "projects"
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
- `--fetch-remote <BOOL>`: Pull from remote before merging (default: true).
  A remote that cannot be reached or whose changes conflict with the local sync
  repository stops the pull — diverged branches are merged automatically, and a
  real conflict is reported and undone. Pass `--fetch-remote false` to merge
  only what is already in the local sync repository.
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
- `--enable-lfs <true|false>`: Enable Git LFS for large files
- `--lfs-patterns <PATTERNS>`: File patterns to track with LFS (comma-separated, default: `*.jsonl`)
- `--scm-backend <BACKEND>`: SCM backend to use: `git` or `mercurial` (default: `git`)
- `--sync-subdirectory <DIR>`: Subdirectory within sync repo for projects (default: `projects`)
- `--enable-artifacts <NAMES>`: Enable artifact categories (comma-separated, or `all`)
- `--disable-artifacts <NAMES>`: Disable artifact categories (comma-separated, or `all`)
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

# Enable Git LFS for large files
claude-code-sync config --enable-lfs true --lfs-patterns "*.jsonl,*.png"

# Use Mercurial instead of Git
claude-code-sync config --scm-backend mercurial

# Store projects in a custom subdirectory
claude-code-sync config --sync-subdirectory "claude-history"

# Sync settings, skills, and prompt history alongside conversations
claude-code-sync config --enable-artifacts settings,skills,prompt-history

# Show current config
claude-code-sync config --show
```

### `purge`

Delete transcripts past the retention window from this machine **and** the sync
repository. See [Purging old transcripts](#purging-old-transcripts).

```bash
# What would go, and how much space it frees
claude-code-sync purge --dry-run

# Ask, then delete (a terminal prompts; elsewhere --yes is required)
claude-code-sync purge
claude-code-sync purge --yes

# Override the window for this run
claude-code-sync purge --older-than 365
```

**Options:**
- `--older-than <DAYS>`: retention window for this run (default and minimum: the
  longer of 180 days and this machine's Claude Code `cleanupPeriodDays`)
- `--dry-run`: show the plan and stop
- `-y, --yes`: delete without asking

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

# Exclude file attachments (images, PDFs, etc.)
exclude_attachments = false

# Enable Git LFS for large files
enable_lfs = false

# File patterns to track with LFS
lfs_patterns = ["*.jsonl"]

# SCM backend: "git" or "mercurial"
scm_backend = "git"

# Subdirectory within sync repo for projects
sync_subdirectory = "projects"

# Delete transcripts older than this, here and in the repo (see Purging old
# transcripts). Unset means the longer of 180 days and this machine's own
# Claude Code cleanupPeriodDays.
purge_older_than_days = 365

# Purge as part of every sync, between the pull and the push
purge_after_sync = false

# Warn once per file a pull cannot place, instead of one combined warning
warn_each_skipped_file = false

# External three-way merge command offered when a pulled file differs
merge_tool = "phpstorm merge"

# Artifact categories to sync alongside conversation history
# (all default to false; see the Artifact Sync section)
[sync_artifacts]
settings = true
memory = true
skills = true
agents = true
commands = true
rules = true
hooks = true
plugins = true
plans = false
todos = false
prompt_history = true

# Canonical project id -> this machine's path for it (see Project map)
[project_map]
shop = "/home/user/work/shop-web"
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
- **scm/**: SCM abstraction layer supporting multiple backends
  - **mod.rs**: `Scm` trait and factory functions
  - **git.rs**: Git backend via CLI commands
  - **hg.rs**: Mercurial backend via CLI commands
  - **lfs.rs**: Git LFS support
- **sync/**: Core sync engine with push/pull logic and snapshot integration
- **conflict.rs**: Conflict detection and resolution
- **interactive_conflict.rs**: Interactive TUI for conflict resolution
- **filter.rs**: Configuration and filtering system
- **report.rs**: Conflict reporting in JSON/Markdown formats
- **history/**: Operation history tracking and management
- **undo/**: Snapshot-based undo functionality for pull/push operations
- **onboarding.rs**: Interactive first-time setup wizard with config file support
- **logger.rs**: Enhanced logging system with file and console output
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
- `rstest`: Parameterized testing (dev dependency)

**Note:** Git/Mercurial operations are performed via CLI commands, not library bindings. This ensures compatibility with git hooks, LFS, and credential helpers.

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
