//! Git SCM backend using CLI commands.

use anyhow::{anyhow, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{ConflictChoice, ConflictResolver, ConflictedFile, Scm};

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

    fn settle_conflicts(&self, resolve_conflict: ConflictResolver) -> Result<bool> {
        for file in self.read_conflicted_files()? {
            let choice = resolve_conflict(&file)?;
            match choice {
                ConflictChoice::KeepLocal => {
                    self.take_side("--ours", file.local.is_some(), &file.path)?;
                }
                ConflictChoice::TakeRemote => {
                    self.take_side("--theirs", file.remote.is_some(), &file.path)?;
                }
                ConflictChoice::WriteMerged(bytes) => {
                    std::fs::write(self.workdir.join(&file.path), bytes)
                        .with_context(|| format!("Failed to write the merged {}", file.path))?;
                    self.run_git_ok(&["--literal-pathspecs", "add", "--", &file.path])?;
                }
                ConflictChoice::AbortMerge => return Ok(false),
            }
        }

        self.run_git_ok(&["commit", "--no-edit"])?;
        Ok(true)
    }

    fn take_side(&self, side: &str, side_has_file: bool, path: &str) -> Result<()> {
        if !side_has_file {
            return self.run_git_ok(&[
                "--literal-pathspecs",
                "rm",
                "--quiet",
                "--force",
                "--",
                path,
            ]);
        }
        self.run_git_ok(&["--literal-pathspecs", "checkout", side, "--", path])?;
        self.run_git_ok(&["--literal-pathspecs", "add", "--", path])
    }

    fn read_conflicted_files(&self) -> Result<Vec<ConflictedFile>> {
        const COMMON_ANCESTOR_STAGE: &str = "1";
        const LOCAL_STAGE: &str = "2";
        const REMOTE_STAGE: &str = "3";

        let output = self.git_output(&["ls-files", "--unmerged", "-z"])?;
        let listed = output.status.success();
        if !listed {
            return Err(anyhow!(
                "git ls-files --unmerged failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let listing = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut files: BTreeMap<String, ConflictedFile> = BTreeMap::new();
        for entry in listing.split('\0').filter(|entry| !entry.is_empty()) {
            let (stage_entry, path) = entry
                .split_once('\t')
                .with_context(|| format!("Unexpected ls-files entry: {entry}"))?;
            let mut fields = stage_entry.split(' ');
            let object = fields.nth(1).context("ls-files entry without an object")?;
            let stage = fields.next().context("ls-files entry without a stage")?;
            let content = Some(self.read_blob(object, path)?);

            let file = files
                .entry(path.to_string())
                .or_insert_with(|| ConflictedFile {
                    path: path.to_string(),
                    ..Default::default()
                });
            match stage {
                COMMON_ANCESTOR_STAGE => file.base = content,
                LOCAL_STAGE => file.local = content,
                REMOTE_STAGE => file.remote = content,
                _ => return Err(anyhow!("Unexpected merge stage {stage} for {path}")),
            }
        }

        Ok(files.into_values().collect())
    }

    fn read_blob(&self, object: &str, path: &str) -> Result<Vec<u8>> {
        let path_argument = format!("--path={path}");
        let output = self.git_output(&["cat-file", "--filters", &path_argument, object])?;
        let read = output.status.success();
        if !read {
            return Err(anyhow!(
                "Failed to read {path} from the merge: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(output.stdout)
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

    /// The file that marks a merge in the sync repository as started by this
    /// tool, so a later pull may undo it if an interrupt left it unfinished.
    fn merge_marker(&self) -> PathBuf {
        let git_dir = self
            .run_git(&["rev-parse", "--git-dir"])
            .unwrap_or_else(|_| ".git".to_string());
        self.workdir.join(git_dir).join("claude-code-sync-merge")
    }

    /// Merge the fetched branch, settling conflicts through `resolve_conflict`;
    /// an unsettled merge is undone.
    fn merge_fetched(
        &self,
        resolve_conflict: ConflictResolver,
        remote: &str,
        branch: &str,
    ) -> Result<()> {
        let merge = self.git_output(&["merge", "--no-edit", "FETCH_HEAD"])?;
        if merge.status.success() {
            return Ok(());
        }

        // A merge git refused to start — uncommitted work in the sync
        // repository, unrelated histories — has nothing to abort and changed
        // nothing.
        let merge_started = self.has_unfinished_merge();
        let settled = if merge_started {
            self.settle_conflicts(resolve_conflict)
        } else {
            Ok(false)
        };
        if let Ok(true) = settled {
            return Ok(());
        }

        let details = match settled {
            Err(error) => format!("{error:#}"),
            _ => format!(
                "{}{}",
                String::from_utf8_lossy(&merge.stdout),
                String::from_utf8_lossy(&merge.stderr)
            ),
        };
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
}

impl Scm for GitScm {
    fn current_branch(&self) -> Result<String> {
        self.run_git(&["branch", "--show-current"])
    }

    fn current_commit_hash(&self) -> Result<String> {
        self.run_git(&["rev-parse", "HEAD"])
    }

    fn stage_all(&self) -> Result<()> {
        let merge_is_unfinished = self.has_unfinished_merge();
        if merge_is_unfinished {
            return Err(anyhow!(
                "The sync repository has an unfinished merge. If an interrupted pull \
                 left it, run `claude-code-sync pull` to undo it; if you are resolving \
                 it by hand, finish it with `git commit` first."
            ));
        }
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
    fn pull(&self, remote: &str, branch: &str, resolve_conflict: ConflictResolver) -> Result<()> {
        if self.has_unfinished_merge() {
            // Only a merge this tool started (and an interrupt cut short) is
            // ours to undo. Any other is someone resolving conflicts by hand,
            // and aborting it would throw their resolution away.
            if !self.merge_marker().is_file() {
                return Err(anyhow!(
                    "The sync repository {} has a merge in progress that claude-code-sync \
                     did not start. Finish it (`git commit`) or undo it (`git merge --abort`) \
                     there, then pull again.",
                    self.workdir.display()
                ));
            }
            self.run_git_ok(&["merge", "--abort"])
                .context("Failed to undo the merge an interrupted pull left unfinished")?;
            let _ = std::fs::remove_file(self.merge_marker());
            log::warn!("Undid the merge an interrupted pull left unfinished; merging again");
        }

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

        // Marks the merge as this tool's while it may be left unfinished
        // (conflicts waiting on a prompt that an interrupt can cut short).
        std::fs::write(self.merge_marker(), b"")
            .context("Failed to mark the merge as started by claude-code-sync")?;
        let outcome = self.merge_fetched(resolve_conflict, remote, branch);
        if !self.has_unfinished_merge() {
            let _ = std::fs::remove_file(self.merge_marker());
        }
        outcome
    }

    fn has_unfinished_merge(&self) -> bool {
        self.git_succeeds(&["rev-parse", "--verify", "--quiet", "MERGE_HEAD"])
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

    fn stop_at_conflicts(_file: &ConflictedFile) -> Result<ConflictChoice> {
        Ok(ConflictChoice::AbortMerge)
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

        machine.pull("origin", &branch, &stop_at_conflicts).unwrap();

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
            .pull("origin", &branch, &stop_at_conflicts)
            .expect_err("a conflict nobody settled stops the pull")
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
    fn a_conflict_settled_per_file_completes_the_merge_with_the_chosen_version() {
        let (_root, machine, workdir, branch) = two_diverged_machines(Some("both-touched.txt"));
        let before = machine.current_commit_hash().unwrap();
        let offered = std::cell::RefCell::new(Vec::new());
        let take_the_remote = |file: &ConflictedFile| {
            offered.borrow_mut().push(format!(
                "{} base={:?} local={:?} remote={:?}",
                file.path,
                file.base.as_deref().map(String::from_utf8_lossy),
                file.local.as_deref().map(String::from_utf8_lossy),
                file.remote.as_deref().map(String::from_utf8_lossy),
            ));
            Ok(ConflictChoice::WriteMerged(file.remote.clone().unwrap()))
        };

        machine.pull("origin", &branch, &take_the_remote).unwrap();

        assert_eq!(
            offered.into_inner(),
            vec![
                "both-touched.txt base=None local=Some(\"from the first\\n\") \
                 remote=Some(\"from the second\\n\")"
                    .to_string()
            ]
        );
        assert_eq!(
            std::fs::read_to_string(workdir.join("both-touched.txt")).unwrap(),
            "from the second\n"
        );
        assert!(!machine.has_changes().unwrap(), "the merge is committed");
        let merge_parents = machine.run_git(&["rev-parse", "HEAD^1", "HEAD^2"]).unwrap();
        assert!(
            merge_parents.starts_with(&before),
            "a merge commit on top of this machine's history: {merge_parents}"
        );
    }

    #[test]
    fn a_file_deleted_on_one_machine_and_changed_on_the_other_can_be_deleted() {
        let (root, machine, workdir, branch) = two_diverged_machines(None);
        let second = root.path().join("second");
        let machine_second = GitScm::open(&second).unwrap();
        git_in(&second, &["rm", "--quiet", "shared-start.txt"]);
        machine_second.commit("remove shared-start.txt").unwrap();
        machine_second.push("origin", &branch).unwrap();
        commit_file(&machine, &workdir, "shared-start.txt", "changed here\n");
        let offered = std::cell::RefCell::new(Vec::new());
        let take_the_remote = |file: &ConflictedFile| {
            offered.borrow_mut().push((
                file.path.clone(),
                file.base.is_some(),
                file.local.is_some(),
                file.remote.is_some(),
            ));
            Ok(ConflictChoice::TakeRemote)
        };

        machine.pull("origin", &branch, &take_the_remote).unwrap();

        assert_eq!(
            offered.into_inner(),
            vec![("shared-start.txt".to_string(), true, true, false)]
        );
        assert!(!workdir.join("shared-start.txt").exists());
        assert!(workdir.join("only-second.txt").is_file());
        assert!(!machine.has_changes().unwrap(), "the merge is committed");
    }

    #[test]
    fn a_merge_an_interrupted_pull_left_blocks_a_push_and_is_redone_by_the_next_pull() {
        let (_root, machine, workdir, branch) = two_diverged_machines(Some("both-touched.txt"));
        git_in(&workdir, &["fetch", "--quiet", "origin", &branch]);
        // What a pull interrupted at the conflict prompt leaves behind: its
        // marker, and the merge stopped at the conflict.
        std::fs::write(machine.merge_marker(), b"").unwrap();
        let interrupted = machine.git_output(&["merge", "FETCH_HEAD"]).unwrap();
        assert!(
            !interrupted.status.success(),
            "the merge stops at the conflict"
        );

        let push_error = machine
            .stage_all()
            .expect_err("conflict markers must not be committed")
            .to_string();
        assert!(push_error.contains("pull"), "unexpected: {push_error}");

        let keep_local = |_file: &ConflictedFile| Ok(ConflictChoice::KeepLocal);
        machine.pull("origin", &branch, &keep_local).unwrap();

        assert_eq!(
            std::fs::read_to_string(workdir.join("both-touched.txt")).unwrap(),
            "from the first\n"
        );
        assert!(!machine.has_unfinished_merge());
        assert!(!machine.has_changes().unwrap(), "the merge is committed");
        assert!(!machine.merge_marker().exists(), "the marker goes with it");
    }

    #[test]
    fn a_merge_someone_is_resolving_by_hand_is_left_alone() {
        let (_root, machine, workdir, branch) = two_diverged_machines(Some("both-touched.txt"));
        git_in(&workdir, &["fetch", "--quiet", "origin", &branch]);
        let manual = machine.git_output(&["merge", "FETCH_HEAD"]).unwrap();
        assert!(!manual.status.success(), "the merge stops at the conflict");
        std::fs::write(workdir.join("both-touched.txt"), "resolved by hand\n").unwrap();

        let take_remote = |_file: &ConflictedFile| Ok(ConflictChoice::TakeRemote);
        let error = machine
            .pull("origin", &branch, &take_remote)
            .expect_err("a merge this tool did not start is not its to undo")
            .to_string();

        assert!(error.contains("git commit"), "unexpected: {error}");
        assert!(
            machine.has_unfinished_merge(),
            "the merge is still in progress"
        );
        assert_eq!(
            std::fs::read_to_string(workdir.join("both-touched.txt")).unwrap(),
            "resolved by hand\n",
            "the hand resolution is untouched"
        );
    }

    #[test]
    fn a_settled_or_undone_pull_leaves_no_marker() {
        let (_root, machine, _workdir, branch) = two_diverged_machines(Some("both-touched.txt"));
        machine
            .pull("origin", &branch, &stop_at_conflicts)
            .expect_err("nobody settled the conflict");
        assert!(!machine.merge_marker().exists());

        let keep_local = |_file: &ConflictedFile| Ok(ConflictChoice::KeepLocal);
        machine.pull("origin", &branch, &keep_local).unwrap();
        assert!(!machine.merge_marker().exists());
    }

    #[test]
    fn a_log_both_machines_wrote_to_is_merged_rather_than_refused() {
        // A transcript and the prompt history only ever grow, so both sides'
        // lines are kept instead of stopping the pull with a conflict.
        let (_root, machine, workdir, branch) = two_diverged_machines(Some("history.jsonl"));

        machine.pull("origin", &branch, &stop_at_conflicts).unwrap();

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

        machine.pull("origin", &branch, &stop_at_conflicts).unwrap();

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

        machine.pull("origin", &branch, &stop_at_conflicts).unwrap();

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

        machine.pull("origin", "main", &stop_at_conflicts).unwrap();

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

        assert!(machine.pull("origin", "main", &stop_at_conflicts).is_err());
    }

    #[test]
    fn a_pull_git_refuses_to_start_says_the_repository_was_left_alone() {
        let (_root, machine, workdir, branch) = two_diverged_machines(None);

        // An interrupted push leaves the sync repository dirty, and git will
        // not begin a merge that would overwrite uncommitted work.
        std::fs::write(workdir.join("only-second.txt"), "half a push\n").unwrap();

        let error = machine
            .pull("origin", &branch, &stop_at_conflicts)
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
