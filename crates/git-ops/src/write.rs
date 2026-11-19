use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path};

use crate::cli::{CommandOutput, GitCli, OpsError, Result, validate_non_flag};

/// Upstream synchronization status for the repository's current `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchSyncStatus {
    /// Current branch tracks an upstream branch.
    Synced {
        branch: String,
        upstream: String,
        ahead: usize,
        behind: usize,
    },
    /// Current local branch has no upstream tracking branch configured yet.
    NoUpstream {
        branch: String,
        /// Recommended default remote (`origin` if present, otherwise first alphabetically).
        default_remote: Option<String>,
    },
    /// `HEAD` is detached and cannot be synced directly.
    DetachedHead,
}

/// Force-push mode passed to [`GitCli::push_branch`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PushForceMode {
    #[default]
    None,
    ForceWithLease,
    Force,
}

impl PushForceMode {
    fn flag(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::ForceWithLease => Some("--force-with-lease"),
            Self::Force => Some("--force"),
        }
    }
}

/// Options controlling [`GitCli::push_branch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOptions {
    pub local_branch: String,
    pub remote: String,
    pub remote_branch: String,
    pub set_upstream: bool,
    pub push_tags: bool,
    pub force: PushForceMode,
}

/// Summary of a completed [`GitCli::sync_branch`] operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    pub branch: String,
    pub upstream: String,
    pub pulled: bool,
    pub pushed: bool,
}

/// Reset strategy passed to [`GitCli::reset_to_commit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

impl ResetMode {
    fn flag(self) -> &'static str {
        match self {
            Self::Soft => "--soft",
            Self::Mixed => "--mixed",
            Self::Hard => "--hard",
        }
    }
}

impl GitCli {
    // -------------------------------------------------------------------------
    // Branches
    // -------------------------------------------------------------------------

    /// Switches `HEAD` to an existing local branch.
    pub fn checkout_branch(&self, repo: impl AsRef<Path>, branch: &str) -> Result<()> {
        validate_non_flag(branch, "branch")?;
        self.run(repo, ["checkout", branch])?;
        Ok(())
    }

    /// Checks out a specific commit in detached `HEAD` state.
    pub fn checkout_commit(&self, repo: impl AsRef<Path>, hash: &str) -> Result<()> {
        validate_non_flag(hash, "commit hash")?;
        self.run(repo, ["checkout", "--detach", hash])?;
        Ok(())
    }

    /// Creates a new branch, optionally at `start_point`, and optionally checks it out.
    pub fn create_branch(
        &self,
        repo: impl AsRef<Path>,
        name: &str,
        start_point: Option<&str>,
        checkout: bool,
    ) -> Result<()> {
        validate_non_flag(name, "branch name")?;
        if let Some(sp) = start_point {
            validate_non_flag(sp, "start point")?;
        }

        let mut args: Vec<&str> = if checkout {
            vec!["checkout", "-b", name]
        } else {
            vec!["branch", name]
        };
        if let Some(sp) = start_point {
            args.push(sp);
        }
        self.run(repo, args)?;
        Ok(())
    }

    /// Deletes a local branch (`-D` when `force` is true, `-d` otherwise).
    pub fn delete_branch(&self, repo: impl AsRef<Path>, name: &str, force: bool) -> Result<()> {
        validate_non_flag(name, "branch name")?;
        let flag = if force { "-D" } else { "-d" };
        self.run(repo, ["branch", flag, name])?;
        Ok(())
    }

    /// Renames a local branch from `old` to `new`.
    pub fn rename_branch(&self, repo: impl AsRef<Path>, old: &str, new: &str) -> Result<()> {
        validate_non_flag(old, "old branch name")?;
        validate_non_flag(new, "new branch name")?;
        self.run(repo, ["branch", "-m", old, new])?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Tags
    // -------------------------------------------------------------------------

    /// Creates a lightweight tag (when `message` is `None`) or an annotated tag.
    pub fn create_tag(
        &self,
        repo: impl AsRef<Path>,
        name: &str,
        target: &str,
        message: Option<&str>,
    ) -> Result<()> {
        validate_non_flag(name, "tag name")?;
        validate_non_flag(target, "target")?;

        match message {
            Some(msg) => {
                self.run(repo, ["tag", "-a", name, "-m", msg, target])?;
            }
            None => {
                self.run(repo, ["tag", name, target])?;
            }
        }
        Ok(())
    }

    /// Deletes a local tag.
    pub fn delete_tag(&self, repo: impl AsRef<Path>, name: &str) -> Result<()> {
        validate_non_flag(name, "tag name")?;
        self.run(repo, ["tag", "-d", name])?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Commit operations
    // -------------------------------------------------------------------------

    /// Cherry-picks the given commit onto the current `HEAD`.
    pub fn cherry_pick(&self, repo: impl AsRef<Path>, hash: &str) -> Result<()> {
        validate_non_flag(hash, "commit hash")?;
        self.run(repo, ["cherry-pick", hash])?;
        Ok(())
    }

    /// Reverts the given commit without opening an interactive editor.
    pub fn revert_commit(&self, repo: impl AsRef<Path>, hash: &str) -> Result<()> {
        validate_non_flag(hash, "commit hash")?;
        self.run(repo, ["revert", "--no-edit", hash])?;
        Ok(())
    }

    /// Resets the current `HEAD` to `hash` using the specified [`ResetMode`].
    pub fn reset_to_commit(
        &self,
        repo: impl AsRef<Path>,
        hash: &str,
        mode: ResetMode,
    ) -> Result<()> {
        validate_non_flag(hash, "commit hash")?;
        self.run(repo, ["reset", mode.flag(), hash])?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Stash
    // -------------------------------------------------------------------------

    /// Pushes current local modifications onto the stash stack.
    pub fn stash_push(
        &self,
        repo: impl AsRef<Path>,
        message: Option<&str>,
        include_untracked: bool,
    ) -> Result<()> {
        let mut args = vec!["stash", "push"];
        if include_untracked {
            args.push("-u");
        }
        if let Some(msg) = message {
            args.push("-m");
            args.push(msg);
        }
        self.run(repo, args)?;
        Ok(())
    }

    /// Applies and removes `stash@{index}`.
    pub fn stash_pop(&self, repo: impl AsRef<Path>, index: usize) -> Result<()> {
        self.stash_ref_op(repo.as_ref(), "pop", index)
    }

    /// Applies `stash@{index}` without dropping it.
    pub fn stash_apply(&self, repo: impl AsRef<Path>, index: usize) -> Result<()> {
        self.stash_ref_op(repo.as_ref(), "apply", index)
    }

    /// Drops `stash@{index}` from the stash list.
    pub fn stash_drop(&self, repo: impl AsRef<Path>, index: usize) -> Result<()> {
        self.stash_ref_op(repo.as_ref(), "drop", index)
    }

    fn stash_ref_op(&self, repo: &Path, subcommand: &str, index: usize) -> Result<()> {
        let stash_ref = format!("stash@{{{index}}}");
        self.run(repo, ["stash", subcommand, &stash_ref])?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Remotes
    // -------------------------------------------------------------------------

    /// Fetches updates from `remote` (or the default remote when `None`).
    pub fn fetch(&self, repo: impl AsRef<Path>, remote: Option<&str>, prune: bool) -> Result<()> {
        if let Some(r) = remote {
            validate_non_flag(r, "remote")?;
        }
        let mut args = vec!["fetch"];
        if prune {
            args.push("--prune");
        }
        if let Some(r) = remote {
            args.push(r);
        }
        self.run(repo, args)?;
        Ok(())
    }

    /// Pulls upstream changes into the current branch.
    pub fn pull(&self, repo: impl AsRef<Path>) -> Result<()> {
        self.run(repo, ["pull"])?;
        Ok(())
    }

    /// Pushes commits to `remote` / `branch`, optionally using `--force-with-lease`.
    pub fn push(
        &self,
        repo: impl AsRef<Path>,
        remote: Option<&str>,
        branch: Option<&str>,
        force_with_lease: bool,
    ) -> Result<()> {
        if let Some(r) = remote {
            validate_non_flag(r, "remote")?;
        }
        if let Some(b) = branch {
            validate_non_flag(b, "branch")?;
        }

        let mut args = vec!["push"];
        if force_with_lease {
            args.push("--force-with-lease");
        }
        match (remote, branch) {
            (Some(r), Some(b)) => {
                args.push(r);
                args.push(b);
            }
            (None, Some(b)) => {
                // `git push <branch>` treats the first positional arg as a remote name.
                args.push("origin");
                args.push(b);
            }
            (Some(r), None) => {
                args.push(r);
            }
            (None, None) => {}
        }
        self.run(repo, args)?;
        Ok(())
    }

    /// Lists configured remote names for `repo`, sorted alphabetically.
    pub fn list_remotes(&self, repo: impl AsRef<Path>) -> Result<Vec<String>> {
        let out = self.run(repo, ["remote"])?;
        let mut remotes: Vec<String> = out
            .stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        remotes.sort();
        remotes.dedup();
        Ok(remotes)
    }

    /// Returns the configured upstream tracking branch (e.g. `origin/main`) for `branch`, if any.
    pub fn branch_upstream(&self, repo: impl AsRef<Path>, branch: &str) -> Result<Option<String>> {
        let repo = repo.as_ref();
        validate_non_flag(branch, "branch")?;

        let spec = format!("{branch}@{{upstream}}");
        let (status, stdout) = self.probe(
            repo,
            ["rev-parse", "--abbrev-ref", "--symbolic-full-name", &spec],
        )?;
        if status.success() {
            let upstream = stdout.trim();
            if !upstream.is_empty() {
                return Ok(Some(upstream.to_string()));
            }
        } else {
            // Surface non-repo / fatal errors instead of masking them as missing upstream.
            self.run(repo, ["rev-parse", "--git-dir"])?;
        }
        Ok(None)
    }

    /// Inspects the current branch's upstream and ahead/behind commit counts.
    pub fn branch_sync_status(&self, repo: impl AsRef<Path>) -> Result<BranchSyncStatus> {
        let repo = repo.as_ref();
        let (sym_status, sym_out) = self.probe(repo, ["symbolic-ref", "--short", "-q", "HEAD"])?;
        if !sym_status.success() {
            if sym_status.code() != Some(1) {
                self.run(repo, ["rev-parse", "--git-dir"])?;
            }
            return Ok(BranchSyncStatus::DetachedHead);
        }

        let branch = sym_out.trim().to_string();
        if branch.is_empty() {
            return Ok(BranchSyncStatus::DetachedHead);
        }

        let (up_status, up_out) = self.probe(
            repo,
            [
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
        )?;
        if !up_status.success() {
            let remotes = self.list_remotes(repo)?;
            let default_remote = remotes
                .iter()
                .find(|r| r.as_str() == "origin")
                .cloned()
                .or_else(|| remotes.into_iter().next());
            return Ok(BranchSyncStatus::NoUpstream {
                branch,
                default_remote,
            });
        }

        let upstream = up_out.trim().to_string();
        let (head_status, _) = self.probe(repo, ["rev-parse", "--verify", "-q", "HEAD"])?;
        if !head_status.success() {
            return Ok(BranchSyncStatus::Synced {
                branch,
                upstream,
                ahead: 0,
                behind: 0,
            });
        }

        let counts = self.run(
            repo,
            ["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        )?;
        let (ahead, behind) = parse_ahead_behind(&counts.stdout)?;
        Ok(BranchSyncStatus::Synced {
            branch,
            upstream,
            ahead,
            behind,
        })
    }

    /// Pushes a local branch to a remote branch with explicit refspec, upstream, tag, and force options.
    pub fn push_branch(
        &self,
        repo: impl AsRef<Path>,
        options: &PushOptions,
    ) -> Result<CommandOutput> {
        validate_non_flag(&options.local_branch, "local branch")?;
        validate_non_flag(&options.remote, "remote")?;
        validate_non_flag(&options.remote_branch, "remote branch")?;

        let refspec = format!(
            "{}:refs/heads/{}",
            options.local_branch, options.remote_branch
        );
        let mut args = vec!["push"];
        if options.set_upstream {
            args.push("-u");
        }
        if options.push_tags {
            args.push("--tags");
        }
        if let Some(flag) = options.force.flag() {
            args.push(flag);
        }
        args.push(&options.remote);
        args.push(&refspec);

        self.run(repo, args)
    }

    /// Publishes `branch` to `remote` under the same name and sets it as the upstream (`-u`).
    pub fn publish_branch(
        &self,
        repo: impl AsRef<Path>,
        remote: &str,
        branch: &str,
    ) -> Result<CommandOutput> {
        self.push_branch(
            repo,
            &PushOptions {
                local_branch: branch.to_string(),
                remote: remote.to_string(),
                remote_branch: branch.to_string(),
                set_upstream: true,
                push_tags: false,
                force: PushForceMode::None,
            },
        )
    }

    /// Performs a VS Code-style pull-then-push synchronization for the current branch.
    ///
    /// Runs `git pull` using the repository/user's configured pull strategy (without forcing
    /// `--rebase`) and aborts immediately on pull failure without pushing.
    pub fn sync_branch(&self, repo: impl AsRef<Path>) -> Result<SyncOutcome> {
        let repo = repo.as_ref();
        let (branch, upstream) = match self.branch_sync_status(repo)? {
            BranchSyncStatus::Synced {
                branch, upstream, ..
            } => (branch, upstream),
            BranchSyncStatus::DetachedHead => {
                return Err(OpsError::InvalidInput(
                    "cannot sync in detached HEAD state".into(),
                ));
            }
            BranchSyncStatus::NoUpstream { branch, .. } => {
                return Err(OpsError::InvalidInput(format!(
                    "branch '{branch}' has no upstream tracking branch configured"
                )));
            }
        };

        let head_before = self.head_oid(repo)?;
        self.run(repo, ["pull"])?;
        let head_after = self.head_oid(repo)?;
        let pulled = head_before != head_after;

        let pushed = match self.branch_sync_status(repo)? {
            BranchSyncStatus::Synced {
                branch: post_branch,
                ahead,
                ..
            } if ahead > 0 => {
                self.push_to_configured_upstream(repo, &post_branch)?;
                true
            }
            _ => false,
        };

        Ok(SyncOutcome {
            branch,
            upstream,
            pulled,
            pushed,
        })
    }

    fn head_oid(&self, repo: &Path) -> Result<Option<String>> {
        let (status, out) = self.probe(repo, ["rev-parse", "--verify", "-q", "HEAD"])?;
        if status.success() {
            Ok(Some(out.trim().to_string()))
        } else {
            Ok(None)
        }
    }

    fn push_to_configured_upstream(&self, repo: &Path, branch: &str) -> Result<()> {
        let remote_key = format!("branch.{branch}.remote");
        let merge_key = format!("branch.{branch}.merge");
        let (remote_ok, remote_out) = self.probe(repo, ["config", "--get", &remote_key])?;
        let (merge_ok, merge_out) = self.probe(repo, ["config", "--get", &merge_key])?;

        let remote = remote_out.trim();
        let merge = merge_out.trim();
        if remote_ok.success()
            && merge_ok.success()
            && !remote.is_empty()
            && remote != "."
            && !merge.is_empty()
        {
            let refspec = format!("{branch}:{merge}");
            self.run(repo, ["push", remote, &refspec])?;
        } else {
            self.run(repo, ["push"])?;
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Single-file working tree operations
    // -------------------------------------------------------------------------

    /// Stages a single file path into the index.
    pub fn stage_file(&self, repo: impl AsRef<Path>, path: impl AsRef<Path>) -> Result<()> {
        let rel = validate_repo_rel_path(path.as_ref())?;
        let args = [OsString::from("add"), OsString::from("--"), rel.into()];
        self.run(repo, args)?;
        Ok(())
    }

    /// Unstages a single file path from the index without modifying the working tree.
    pub fn unstage_file(&self, repo: impl AsRef<Path>, path: impl AsRef<Path>) -> Result<()> {
        let repo = repo.as_ref();
        let rel = validate_repo_rel_path(path.as_ref())?;

        // `git reset HEAD -- <path>` fails on an unborn HEAD; fall back to `git rm --cached`.
        let (head_status, _) = self.probe(repo, ["rev-parse", "--verify", "-q", "HEAD"])?;
        if head_status.success() {
            let args = [
                OsString::from("reset"),
                OsString::from("-q"),
                OsString::from("HEAD"),
                OsString::from("--"),
                rel.into(),
            ];
            self.run(repo, args)?;
        } else {
            let args = [
                OsString::from("rm"),
                OsString::from("--cached"),
                OsString::from("-q"),
                OsString::from("--"),
                rel.into(),
            ];
            self.run(repo, args)?;
        }
        Ok(())
    }

    /// Discards unstaged working tree modifications (or removes an untracked file).
    pub fn discard_file(&self, repo: impl AsRef<Path>, path: impl AsRef<Path>) -> Result<()> {
        let repo = repo.as_ref();
        let rel = validate_repo_rel_path(path.as_ref())?;

        let status = self.run(
            repo,
            [
                OsString::from("status"),
                OsString::from("--porcelain=v1"),
                OsString::from("--"),
                rel.into(),
            ],
        )?;

        if status.stdout.trim().is_empty() {
            return Ok(());
        }

        if status.stdout.lines().all(|line| line.starts_with("?? ")) {
            let full = repo.join(rel);
            if full.is_dir() {
                fs::remove_dir_all(full)?;
            } else if full.exists() {
                fs::remove_file(full)?;
            }
            return Ok(());
        }

        let args = [OsString::from("checkout"), OsString::from("--"), rel.into()];
        self.run(repo, args)?;
        Ok(())
    }
}

fn validate_repo_rel_path(path: &Path) -> Result<&Path> {
    if path.as_os_str().is_empty() {
        return Err(OpsError::InvalidInput("file path cannot be empty".into()));
    }
    if path.is_absolute() {
        return Err(OpsError::InvalidInput(format!(
            "expected repository-relative path, got absolute path: {}",
            path.display()
        )));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(OpsError::InvalidInput(format!(
            "path cannot contain '..': {}",
            path.display()
        )));
    }
    Ok(path)
}

fn parse_ahead_behind(stdout: &str) -> Result<(usize, usize)> {
    let mut parts = stdout.split_whitespace();
    let ahead = parts
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(|| {
            OpsError::InvalidInput(format!(
                "unexpected `rev-list --left-right --count` output: {}",
                stdout.trim()
            ))
        })?;
    let behind = parts
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(|| {
            OpsError::InvalidInput(format!(
                "unexpected `rev-list --left-right --count` output: {}",
                stdout.trim()
            ))
        })?;
    Ok((ahead, behind))
}
