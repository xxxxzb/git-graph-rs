use std::path::Path;

use crate::cli::{GitCli, Result};
use crate::write::ResetMode;

/// Category of potential data loss detected prior to a destructive Git operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyRisk {
    /// `git reset --hard` will permanently discard uncommitted tracked changes.
    UncommittedChanges,
    /// Leaving a detached `HEAD` that contains commits unreachable from any branch or tag.
    OrphanedDetachedHead {
        head_commit: String,
        orphaned_commits: usize,
    },
}

/// Structured confirmation prompt payload for the GUI before running a risky operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyWarning {
    pub risk: SafetyRisk,
    pub title: String,
    pub reason: String,
    pub recoverable_via_reflog: bool,
}

/// Checks whether `reset_to_commit(..., mode)` would discard uncommitted working tree or index changes.
///
/// Only [`ResetMode::Hard`] triggers a warning; `Soft` and `Mixed` always return `Ok(None)`.
pub fn check_reset_safety(
    cli: &GitCli,
    repo: impl AsRef<Path>,
    mode: ResetMode,
) -> Result<Option<SafetyWarning>> {
    if mode != ResetMode::Hard {
        return Ok(None);
    }

    let status = cli.run(repo, ["status", "--porcelain", "--untracked-files=no"])?;
    let changed_count = status
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();

    if changed_count == 0 {
        return Ok(None);
    }

    Ok(Some(SafetyWarning {
        risk: SafetyRisk::UncommittedChanges,
        title: "Discard uncommitted changes?".into(),
        reason: format!(
            "Hard reset will permanently discard uncommitted changes in {changed_count} tracked file(s). \
             Uncommitted working tree edits cannot be recovered from reflog."
        ),
        recoverable_via_reflog: false,
    }))
}

/// Checks whether switching away from the current `HEAD` would orphan detached commits.
///
/// Returns `Ok(Some(_))` when `HEAD` is detached and points to one or more commits not
/// reachable from any branch or tag.
pub fn check_checkout_safety(
    cli: &GitCli,
    repo: impl AsRef<Path>,
) -> Result<Option<SafetyWarning>> {
    let repo = repo.as_ref();

    // If HEAD points to a branch (symbolic ref), switching branches will not orphan commits.
    let (sym_status, _) = cli.probe(repo, ["symbolic-ref", "-q", "HEAD"])?;
    if sym_status.success() {
        return Ok(None);
    }

    // Check if HEAD resolves to an actual commit (unborn HEAD returns non-zero).
    let (head_status, head_out) = cli.probe(repo, ["rev-parse", "--verify", "-q", "HEAD"])?;
    if !head_status.success() {
        return Ok(None);
    }
    let head_commit = head_out.trim().to_string();

    let count_out = cli.run(
        repo,
        [
            "rev-list",
            "--count",
            "HEAD",
            "--not",
            "--branches",
            "--tags",
            "--remotes",
        ],
    )?;
    let orphaned_commits: usize = count_out.stdout.trim().parse().unwrap_or(0);

    if orphaned_commits == 0 {
        return Ok(None);
    }

    let short_hash = head_commit.get(..7).unwrap_or(&head_commit);
    Ok(Some(SafetyWarning {
        risk: SafetyRisk::OrphanedDetachedHead {
            head_commit: head_commit.clone(),
            orphaned_commits,
        },
        title: "Leave detached HEAD with unreferenced commits?".into(),
        reason: format!(
            "HEAD is detached at {short_hash} with {orphaned_commits} commit(s) not on any branch or tag. \
             Switching away will leave them unreferenced, though they remain recoverable via git reflog until GC."
        ),
        recoverable_via_reflog: true,
    }))
}
