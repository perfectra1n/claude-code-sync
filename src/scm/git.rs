//! Git SCM backend using CLI commands.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::Scm;

/// Git SCM implementation using the git CLI.
pub struct GitScm {
    workdir: PathBuf,
}

impl GitScm {
    /// Open an existing Git repository.
    pub fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        if !path.join(".git").exists() {
            return Err(anyhow!(
                "Not a git repository: '{}' (no .git directory)",
                path.display()
            ));
        }

        Ok(Self { workdir: path })
    }

    /// Initialize a new Git repository.
    pub fn init(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("Failed to create directory '{}'", path.display()))?;

        let output = Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .context("Failed to run 'git init'")?;

        if !output.status.success() {
            return Err(anyhow!(
                "git init failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        // Configure user name and email if not set
        let _ = Command::new("git")
            .args(["config", "user.name", "Claude Code Sync"])
            .current_dir(path)
            .output();
        let _ = Command::new("git")
            .args(["config", "user.email", "claude-code-sync@local"])
            .current_dir(path)
            .output();

        Self::open(path)
    }

    /// Clone a remote repository.
    pub fn clone(url: &str, path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create parent directory for '{}'", path.display())
            })?;
        }

        let output = Command::new("git")
            .args(["clone", url, &path.to_string_lossy()])
            .output()
            .context("Failed to run 'git clone'")?;

        if !output.status.success() {
            return Err(anyhow!(
                "git clone failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        Self::open(path)
    }

    /// Run a git command and return stdout as a string.
    fn run_git(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.workdir)
            .output()
            .with_context(|| format!("Failed to run 'git {}'", args.join(" ")))?;

        if !output.status.success() {
            return Err(anyhow!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Run a git command, returning Ok if it succeeds (ignoring stdout).
    fn run_git_ok(&self, args: &[&str]) -> Result<()> {
        self.run_git(args)?;
        Ok(())
    }

    /// Run a git command and return its raw output, for the callers that read
    /// a failure's own text instead of turning it into an error.
    fn git_output(&self, args: &[&str]) -> Result<std::process::Output> {
        Command::new("git")
            .args(args)
            .current_dir(&self.workdir)
            .output()
            .with_context(|| format!("Failed to run 'git {}'", args.join(" ")))
    }

    /// Whether `remote` has `branch`. Only a definite "no" (exit code 2 from
    /// `ls-remote --exit-code`) counts; an unreachable remote is an error.
    fn remote_has_branch(&self, remote: &str, branch: &str) -> Result<bool> {
        let output = self.git_output(&["ls-remote", "--exit-code", "--heads", remote, branch])?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(2) => Ok(false),
            _ => Err(anyhow!(
                "Failed to reach remote '{remote}': {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
        }
    }

    /// Whether the only uncommitted change is the `.gitattributes` file.
    fn only_sync_attributes_pending(&self) -> Result<bool> {
        // Raw output: `run_git` trims, which would eat the first line's
        // leading status column.
        let output = self.git_output(&["status", "--porcelain", "--untracked-files=all"])?;
        let status = String::from_utf8_lossy(&output.stdout);
        let mut lines = status.lines().filter(|l| !l.trim().is_empty()).peekable();
        Ok(lines.peek().is_some() && lines.all(|l| l.get(3..) == Some(".gitattributes")))
    }

    /// Check if a git command succeeds (exit code 0).
    fn git_succeeds(&self, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(&self.workdir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

impl Scm for GitScm {
    fn current_branch(&self) -> Result<String> {
        self.run_git(&["branch", "--show-current"])
    }

    fn current_commit_hash(&self) -> Result<String> {
        self.run_git(&["rev-parse", "HEAD"])
    }

    fn stage_all(&self) -> Result<()> {
        self.run_git_ok(&["-c", "core.safecrlf=false", "add", "-A"])
    }

    fn stage_renormalized(&self) -> Result<()> {
        self.run_git_ok(&["-c", "core.safecrlf=false", "add", "--renormalize", "."])
    }

    fn commit(&self, message: &str) -> Result<()> {
        self.run_git_ok(&["commit", "-m", message])
    }

    fn has_changes(&self) -> Result<bool> {
        let output = self.run_git(&["status", "--porcelain"])?;
        Ok(!output.is_empty())
    }

    fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        self.run_git_ok(&["remote", "add", name, url])
    }

    fn has_remote(&self, name: &str) -> bool {
        self.git_succeeds(&["remote", "get-url", name])
    }

    fn get_remote_url(&self, name: &str) -> Result<String> {
        self.run_git(&["remote", "get-url", name])
    }

    fn set_remote_url(&self, name: &str, url: &str) -> Result<()> {
        self.run_git_ok(&["remote", "set-url", name, url])
    }

    fn remove_remote(&self, name: &str) -> Result<()> {
        self.run_git_ok(&["remote", "remove", name])
    }

    fn list_remotes(&self) -> Result<Vec<String>> {
        let output = self.run_git(&["remote"])?;
        if output.is_empty() {
            Ok(Vec::new())
        } else {
            Ok(output.lines().map(|s| s.to_string()).collect())
        }
    }

    fn push(&self, remote: &str, branch: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["push", remote, branch])
            .current_dir(&self.workdir)
            .output()
            .context("Failed to run 'git push'")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "Failed to push to remote '{}': {}\n\n\
                Possible causes:\n\
                1. Authentication failed - ensure credentials are configured\n\
                2. No permission to push to this repository\n\
                3. Network connectivity issues\n\
                4. Remote branch protection rules\n\n\
                For HTTPS: Run 'git config --global credential.helper store' and try again\n\
                For SSH: Ensure SSH keys are set up with 'ssh -T git@github.com'",
                remote,
                stderr
            ));
        }

        Ok(())
    }

    /// Fetch and merge the remote branch.
    ///
    /// The merge is spelled out rather than left to `git pull`, which since
    /// git 2.34 refuses to reconcile diverged branches until the machine's
    /// own `pull.rebase` / `pull.ff` is configured — a setting this tool does
    /// not own. Merge, not rebase: undo records point at local commit hashes,
    /// and a rebase rewrites them.
    fn pull(&self, remote: &str, branch: &str) -> Result<()> {
        // A remote nobody has pushed to yet has no branch to fetch. That is
        // the first machine's normal state, not a failure: there is nothing
        // to merge, and the push that follows creates the branch.
        if !self.remote_has_branch(remote, branch)? {
            log::info!("Remote '{remote}' has no branch '{branch}' yet; nothing to pull");
            return Ok(());
        }

        self.run_git_ok(&["fetch", remote, branch])
            .with_context(|| format!("Failed to fetch from remote '{remote}'"))?;

        // A repository `init` just created has no commit of its own, and git
        // refuses to merge into an empty head: take the fetched branch whole.
        // Only when there is nothing to lose — checking it out is a hard reset,
        // and work staged but never committed would go with it.
        if !self.git_succeeds(&["rev-parse", "--verify", "HEAD"]) {
            // `init` writes the sync `.gitattributes` without committing it.
            // It is regenerated after the pull, so it is not work to protect.
            if self.only_sync_attributes_pending()? {
                let _ = self.git_output(&["rm", "--cached", "--quiet", ".gitattributes"]);
                std::fs::remove_file(self.workdir.join(".gitattributes"))
                    .context("Failed to set aside the uncommitted .gitattributes")?;
            }
            if self.has_changes()? {
                return Err(anyhow!(
                    "Failed to check out '{remote}/{branch}': the sync repository has no commit \
                     of its own yet, and taking the remote's history would discard the files \
                     waiting in it. Move them out of {}, then pull again.",
                    self.workdir.display()
                ));
            }
            return self
                .run_git_ok(&["reset", "--hard", "FETCH_HEAD"])
                .with_context(|| format!("Failed to check out '{remote}/{branch}'"));
        }

        let merge = self.git_output(&["merge", "--no-edit", "FETCH_HEAD"])?;
        if merge.status.success() {
            return Ok(());
        }

        // Leave the repository where it was, so the next sync can retry. A
        // merge git refused to start — uncommitted work in the sync repository,
        // unrelated histories — has nothing to abort and changed nothing.
        let merge_started = self.git_succeeds(&["rev-parse", "--verify", "MERGE_HEAD"]);
        let details = format!(
            "{}{}",
            String::from_utf8_lossy(&merge.stdout),
            String::from_utf8_lossy(&merge.stderr)
        );
        let state = if !merge_started {
            "Nothing was merged; the sync repository is as it was."
        } else if self.git_succeeds(&["merge", "--abort"]) {
            "The merge was undone; the sync repository is as it was."
        } else {
            "The merge could not be undone; the sync repository needs attention."
        };

        Err(anyhow!(
            "Failed to merge '{remote}/{branch}' into the sync repository: {}\n{state}",
            details.trim()
        ))
    }

    fn reset_soft(&self, commit: &str) -> Result<()> {
        self.run_git_ok(&["reset", "--soft", commit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_git_init_and_open() {
        let temp = TempDir::new().unwrap();
        let _scm = GitScm::init(temp.path()).unwrap();

        assert!(temp.path().join(".git").exists());

        // Verify we can open the initialized repo
        let _reopened = GitScm::open(temp.path()).unwrap();
    }

    #[test]
    fn test_git_stage_commit() {
        let temp = TempDir::new().unwrap();
        let scm = GitScm::init(temp.path()).unwrap();

        // Initially no changes
        assert!(!scm.has_changes().unwrap());

        // Create a file
        std::fs::write(temp.path().join("test.txt"), "hello").unwrap();
        assert!(scm.has_changes().unwrap());

        // Stage and commit
        scm.stage_all().unwrap();
        scm.commit("Initial commit").unwrap();
        assert!(!scm.has_changes().unwrap());

        // Verify commit hash
        let hash = scm.current_commit_hash().unwrap();
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 40); // Full SHA
    }

    #[test]
    fn test_git_branch() {
        let temp = TempDir::new().unwrap();
        let scm = GitScm::init(temp.path()).unwrap();

        // Create initial commit (needed for branch to exist)
        std::fs::write(temp.path().join("test.txt"), "hello").unwrap();
        scm.stage_all().unwrap();
        scm.commit("Initial commit").unwrap();

        // Check branch (default is master or main depending on git config)
        let branch = scm.current_branch().unwrap();
        assert!(!branch.is_empty());
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit_file(machine: &GitScm, dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
        machine.stage_all().unwrap();
        machine.commit(&format!("add {name}")).unwrap();
    }

    fn use_git_for_windows_line_endings(dir: &Path) {
        git_in(dir, &["config", "core.autocrlf", "true"]);
    }

    /// Two clones of one bare remote, both already one commit ahead of it in
    /// their own way: the shape a sync repository takes when two machines
    /// pushed between pulls.
    fn two_diverged_machines(shared_file: Option<&str>) -> (TempDir, GitScm, PathBuf, String) {
        let root = TempDir::new().unwrap();
        git_in(root.path(), &["init", "--bare", "--quiet", "origin"]);

        let first = root.path().join("first");
        git_in(root.path(), &["clone", "--quiet", "origin", "first"]);
        let machine_first = GitScm::open(&first).unwrap();
        git_in(&first, &["config", "user.name", "First"]);
        git_in(&first, &["config", "user.email", "first@local"]);
        use_git_for_windows_line_endings(&first);
        crate::scm::attributes::ensure_sync_attributes(&first).unwrap();
        commit_file(&machine_first, &first, "shared-start.txt", "start\n");
        let branch = machine_first.current_branch().unwrap();
        machine_first.push("origin", &branch).unwrap();

        let second = root.path().join("second");
        git_in(root.path(), &["clone", "--quiet", "origin", "second"]);
        let machine_second = GitScm::open(&second).unwrap();
        git_in(&second, &["config", "user.name", "Second"]);
        git_in(&second, &["config", "user.email", "second@local"]);
        use_git_for_windows_line_endings(&second);
        let second_file = shared_file.unwrap_or("only-second.txt");
        commit_file(&machine_second, &second, second_file, "from the second\r\n");
        machine_second.push("origin", &branch).unwrap();

        let first_file = shared_file.unwrap_or("only-first.txt");
        commit_file(&machine_first, &first, first_file, "from the first\n");

        (root, machine_first, first, branch)
    }

    #[test]
    fn pull_reconciles_a_repository_that_both_machines_moved() {
        let (_root, machine, workdir, branch) = two_diverged_machines(None);

        machine.pull("origin", &branch).unwrap();

        let only_second = std::fs::read_to_string(workdir.join("only-second.txt")).unwrap();
        let only_first = std::fs::read_to_string(workdir.join("only-first.txt")).unwrap();
        assert_eq!(
            only_second, "from the second\n",
            "the other machine's commit is merged in, with LF line endings"
        );
        assert_eq!(
            only_first, "from the first\n",
            "this machine's own commit survives"
        );
        assert!(!machine.has_changes().unwrap(), "the merge is committed");
    }

    #[test]
    fn a_conflicting_pull_fails_and_leaves_the_repository_as_it_was() {
        let (_root, machine, workdir, branch) = two_diverged_machines(Some("both-touched.txt"));
        let before = machine.current_commit_hash().unwrap();

        let error = machine
            .pull("origin", &branch)
            .expect_err("a content conflict cannot be resolved for the user")
            .to_string();

        assert!(error.contains("both-touched.txt"), "unexpected: {error}");
        assert_eq!(
            machine.current_commit_hash().unwrap(),
            before,
            "a failed pull moves nothing"
        );
        assert!(
            !machine.has_changes().unwrap(),
            "no conflict markers are left in the working tree"
        );
        assert_eq!(
            std::fs::read_to_string(workdir.join("both-touched.txt")).unwrap(),
            "from the first\n"
        );
    }

    #[test]
    fn a_log_both_machines_wrote_to_is_merged_rather_than_refused() {
        // A transcript and the prompt history only ever grow, so both sides'
        // lines are kept instead of stopping the pull with a conflict.
        let (_root, machine, workdir, branch) = two_diverged_machines(Some("history.jsonl"));

        machine.pull("origin", &branch).unwrap();

        let merged = std::fs::read_to_string(workdir.join("history.jsonl")).unwrap();
        assert!(merged.contains("from the first"), "kept: {merged}");
        assert!(merged.contains("from the second"), "kept: {merged}");
        assert!(!merged.contains("<<<<"), "no conflict markers: {merged}");
        let has_carriage_return = merged.contains('\r');
        assert!(!has_carriage_return, "LF line endings only: {merged:?}");
    }

    #[test]
    fn the_first_pull_of_a_repository_with_no_commits_checks_the_branch_out() {
        let (root, _machine, _first, branch) = two_diverged_machines(None);

        // What `init --remote <url>` leaves behind: a repository with a remote
        // and not a single commit of its own.
        let fresh = root.path().join("fresh");
        let machine = GitScm::init(&fresh).unwrap();
        use_git_for_windows_line_endings(&fresh);
        machine
            .add_remote("origin", root.path().join("origin").to_str().unwrap())
            .unwrap();

        machine.pull("origin", &branch).unwrap();

        let shared_start = std::fs::read_to_string(fresh.join("shared-start.txt")).unwrap();
        assert_eq!(
            shared_start, "start\n",
            "the remote's history is checked out with LF line endings"
        );
    }

    #[test]
    fn a_first_pull_is_not_blocked_by_the_attributes_init_wrote() {
        let (root, _machine, _first, branch) = two_diverged_machines(None);

        // `init` writes the sync rules and commits nothing.
        let fresh = root.path().join("fresh");
        let machine = GitScm::init(&fresh).unwrap();
        crate::scm::attributes::ensure_sync_attributes(&fresh).unwrap();
        machine
            .add_remote("origin", root.path().join("origin").to_str().unwrap())
            .unwrap();

        machine.pull("origin", &branch).unwrap();

        assert!(fresh.join("shared-start.txt").is_file());
        assert!(!machine.has_changes().unwrap());
    }

    #[test]
    fn a_pull_from_a_remote_nobody_pushed_to_yet_is_a_no_op() {
        let root = TempDir::new().unwrap();
        git_in(root.path(), &["init", "--bare", "--quiet", "origin"]);
        let fresh = root.path().join("fresh");
        let machine = GitScm::init(&fresh).unwrap();
        crate::scm::attributes::ensure_sync_attributes(&fresh).unwrap();
        machine
            .add_remote("origin", root.path().join("origin").to_str().unwrap())
            .unwrap();

        machine.pull("origin", "main").unwrap();

        assert!(
            fresh.join(".gitattributes").is_file(),
            "nothing was touched"
        );
    }

    #[test]
    fn a_pull_from_an_unreachable_remote_is_an_error() {
        let root = TempDir::new().unwrap();
        let fresh = root.path().join("fresh");
        let machine = GitScm::init(&fresh).unwrap();
        machine
            .add_remote("origin", root.path().join("missing").to_str().unwrap())
            .unwrap();

        assert!(machine.pull("origin", "main").is_err());
    }

    #[test]
    fn a_pull_git_refuses_to_start_says_the_repository_was_left_alone() {
        let (_root, machine, workdir, branch) = two_diverged_machines(None);

        // An interrupted push leaves the sync repository dirty, and git will
        // not begin a merge that would overwrite uncommitted work.
        std::fs::write(workdir.join("only-second.txt"), "half a push\n").unwrap();

        let error = machine
            .pull("origin", &branch)
            .expect_err("a dirty sync repository cannot be merged into")
            .to_string();

        assert!(
            error.contains("Nothing was merged; the sync repository is as it was."),
            "unexpected: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(workdir.join("only-second.txt")).unwrap(),
            "half a push\n",
            "the uncommitted work is untouched"
        );
    }

    #[test]
    fn test_git_remote() {
        let temp = TempDir::new().unwrap();
        let scm = GitScm::init(temp.path()).unwrap();

        assert!(!scm.has_remote("origin"));

        scm.add_remote("origin", "https://github.com/test/repo.git")
            .unwrap();
        assert!(scm.has_remote("origin"));
        assert!(!scm.has_remote("upstream"));
    }
}
