use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path};

use crate::cli::{GitCli, OpsError, Result, validate_non_flag};

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
