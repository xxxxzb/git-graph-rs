use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use git_graph_ops::{
    BranchSyncStatus, GitCli, OpsError, PushForceMode, PushOptions, RepoWatcher, ResetMode,
    SafetyRisk, SyncOutcome, WatchEvent, check_checkout_safety, check_reset_safety,
};
use tempfile::TempDir;

fn init_repo() -> (TempDir, GitCli) {
    let dir = TempDir::new().expect("create tempdir");
    let cli = GitCli::new().expect("locate git");

    cli.run(dir.path(), ["init", "-b", "main"])
        .expect("git init");
    cli.run(dir.path(), ["config", "user.name", "Test User"])
        .expect("config user.name");
    cli.run(dir.path(), ["config", "user.email", "test@example.com"])
        .expect("config user.email");
    cli.run(dir.path(), ["config", "commit.gpgsign", "false"])
        .expect("config gpgsign");

    (dir, cli)
}

fn commit_file(cli: &GitCli, repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    fs::write(repo.join(filename), content).expect("write file");
    cli.stage_file(repo, filename).expect("stage file");
    cli.run(repo, ["commit", "-m", msg]).expect("commit");
    cli.run(repo, ["rev-parse", "HEAD"])
        .expect("rev-parse HEAD")
        .stdout
        .trim()
        .to_string()
}

fn wait_for_event(
    rx: &mpsc::Receiver<WatchEvent>,
    expected: WatchEvent,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(ev) if ev == expected => return true,
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return false;
            }
        }
    }
    false
}

#[test]
fn cli_detection_and_error_handling() {
    let cli = GitCli::new().expect("detect git");
    assert!(cli.git_path().is_file());

    let explicit = GitCli::from_path(cli.git_path()).expect("construct from explicit path");
    assert_eq!(explicit.git_path(), cli.git_path());

    let bad_path = GitCli::from_path("/nonexistent/path/to/git");
    assert!(matches!(bad_path, Err(OpsError::GitNotFound(_))));

    let (dir, cli) = init_repo();
    let err = cli
        .run(dir.path(), ["rev-parse", "--verify", "does-not-exist"])
        .expect_err("expected failure");
    match err {
        OpsError::CommandFailed { args, code, stderr } => {
            assert_eq!(args, vec!["rev-parse", "--verify", "does-not-exist"]);
            assert!(code.is_some());
            assert_ne!(code, Some(0));
            assert!(!stderr.is_empty());
        }
        other => panic!("unexpected error variant: {other:?}"),
    }

    let flag_err = cli
        .checkout_branch(dir.path(), "--orphan")
        .expect_err("leading dash must be rejected");
    assert!(matches!(flag_err, OpsError::InvalidInput(_)));
}

#[test]
fn branch_lifecycle() {
    let (dir, cli) = init_repo();
    let repo = dir.path();
    let c1 = commit_file(&cli, repo, "a.txt", "one\n", "initial");

    // Create without checkout
    cli.create_branch(repo, "feature", None, false)
        .expect("create feature branch");
    let head_branch = cli
        .run(repo, ["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap()
        .stdout;
    assert_eq!(head_branch.trim(), "main");

    // Checkout branch
    cli.checkout_branch(repo, "feature")
        .expect("checkout feature");
    let head_branch = cli
        .run(repo, ["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap()
        .stdout;
    assert_eq!(head_branch.trim(), "feature");

    // Rename branch
    cli.rename_branch(repo, "feature", "feature-renamed")
        .expect("rename branch");
    let head_branch = cli
        .run(repo, ["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap()
        .stdout;
    assert_eq!(head_branch.trim(), "feature-renamed");

    // Make an unmerged commit on feature-renamed
    let c2 = commit_file(&cli, repo, "b.txt", "two\n", "feature work");

    // Create + checkout from explicit start_point
    cli.create_branch(repo, "from-c1", Some(&c1), true)
        .expect("create from start_point");
    let current = cli.run(repo, ["rev-parse", "HEAD"]).unwrap().stdout;
    assert_eq!(current.trim(), c1);

    // Safe delete fails on unmerged branch, force delete succeeds
    cli.checkout_branch(repo, "main").unwrap();
    assert!(matches!(
        cli.delete_branch(repo, "feature-renamed", false),
        Err(OpsError::CommandFailed { .. })
    ));
    cli.delete_branch(repo, "feature-renamed", true)
        .expect("force delete unmerged branch");

    // Checkout commit enters detached HEAD
    cli.checkout_commit(repo, &c2).expect("checkout commit");
    let sym = cli.run(repo, ["symbolic-ref", "-q", "HEAD"]);
    assert!(sym.is_err());
    let head = cli.run(repo, ["rev-parse", "HEAD"]).unwrap().stdout;
    assert_eq!(head.trim(), c2);
}

#[test]
fn tag_lifecycle() {
    let (dir, cli) = init_repo();
    let repo = dir.path();
    let c1 = commit_file(&cli, repo, "a.txt", "v1\n", "init");

    cli.create_tag(repo, "v0.1.0", &c1, None)
        .expect("lightweight tag");
    cli.create_tag(repo, "v0.2.0", "HEAD", Some("Release 0.2.0"))
        .expect("annotated tag");

    let tags = cli.run(repo, ["tag", "--list"]).unwrap().stdout;
    assert!(tags.lines().any(|l| l == "v0.1.0"));
    assert!(tags.lines().any(|l| l == "v0.2.0"));

    let tag_type = cli.run(repo, ["cat-file", "-t", "v0.2.0"]).unwrap().stdout;
    assert_eq!(tag_type.trim(), "tag");

    cli.delete_tag(repo, "v0.1.0").expect("delete tag");
    let tags_after = cli.run(repo, ["tag", "--list"]).unwrap().stdout;
    assert!(!tags_after.lines().any(|l| l == "v0.1.0"));
    assert!(tags_after.lines().any(|l| l == "v0.2.0"));
}

#[test]
fn cherry_pick_and_revert() {
    let (dir, cli) = init_repo();
    let repo = dir.path();
    commit_file(&cli, repo, "base.txt", "base\n", "base");

    cli.create_branch(repo, "side", None, true).unwrap();
    let side_commit = commit_file(&cli, repo, "side.txt", "from side\n", "add side.txt");

    cli.checkout_branch(repo, "main").unwrap();
    assert!(!repo.join("side.txt").exists());

    cli.cherry_pick(repo, &side_commit).expect("cherry-pick");
    assert_eq!(
        fs::read_to_string(repo.join("side.txt")).unwrap(),
        "from side\n"
    );

    let picked_head = cli.run(repo, ["rev-parse", "HEAD"]).unwrap().stdout;
    cli.revert_commit(repo, picked_head.trim())
        .expect("revert commit");
    assert!(!repo.join("side.txt").exists());
}

#[test]
fn reset_and_safety_checks() {
    let (dir, cli) = init_repo();
    let repo = dir.path();
    let c1 = commit_file(&cli, repo, "file.txt", "v1\n", "c1");
    let c2 = commit_file(&cli, repo, "file.txt", "v2\n", "c2");

    // Clean worktree: hard reset safety check is None
    assert_eq!(
        check_reset_safety(&cli, repo, ResetMode::Hard).unwrap(),
        None
    );

    // Modify tracked file: Soft/Mixed are safe, Hard warns (not recoverable via reflog)
    fs::write(repo.join("file.txt"), "dirty\n").unwrap();
    assert_eq!(
        check_reset_safety(&cli, repo, ResetMode::Soft).unwrap(),
        None
    );
    assert_eq!(
        check_reset_safety(&cli, repo, ResetMode::Mixed).unwrap(),
        None
    );
    let warn = check_reset_safety(&cli, repo, ResetMode::Hard)
        .unwrap()
        .expect("expected hard reset warning");
    assert_eq!(warn.risk, SafetyRisk::UncommittedChanges);
    assert!(!warn.recoverable_via_reflog);

    // Hard reset discards dirty edit
    cli.reset_to_commit(repo, &c2, ResetMode::Hard)
        .expect("hard reset");
    assert_eq!(fs::read_to_string(repo.join("file.txt")).unwrap(), "v2\n");

    // Soft reset moves HEAD back to c1 while keeping c2 changes staged
    cli.reset_to_commit(repo, &c1, ResetMode::Soft)
        .expect("soft reset");
    let head = cli.run(repo, ["rev-parse", "HEAD"]).unwrap().stdout;
    assert_eq!(head.trim(), c1);
    let staged = cli
        .run(repo, ["diff", "--cached", "--name-only"])
        .unwrap()
        .stdout;
    assert_eq!(staged.trim(), "file.txt");

    // Mixed reset unstages changes while keeping working tree
    cli.reset_to_commit(repo, &c1, ResetMode::Mixed)
        .expect("mixed reset");
    let staged_after = cli
        .run(repo, ["diff", "--cached", "--name-only"])
        .unwrap()
        .stdout;
    assert!(staged_after.trim().is_empty());
    assert_eq!(fs::read_to_string(repo.join("file.txt")).unwrap(), "v2\n");

    // Restore clean state before testing checkout safety
    cli.reset_to_commit(repo, &c1, ResetMode::Hard).unwrap();

    // On a branch -> safe
    assert_eq!(check_checkout_safety(&cli, repo).unwrap(), None);

    // Detached HEAD pointing to a commit on main -> safe
    cli.checkout_commit(repo, &c1).unwrap();
    assert_eq!(check_checkout_safety(&cli, repo).unwrap(), None);

    // Detached HEAD with a new unreferenced commit -> warns and is recoverable via reflog
    let orphan_hash = commit_file(&cli, repo, "orphan.txt", "orphan\n", "orphan commit");
    let checkout_warn = check_checkout_safety(&cli, repo)
        .unwrap()
        .expect("expected detached HEAD warning");
    assert_eq!(
        checkout_warn.risk,
        SafetyRisk::OrphanedDetachedHead {
            head_commit: orphan_hash,
            orphaned_commits: 1,
        }
    );
    assert!(checkout_warn.recoverable_via_reflog);
}

#[test]
fn stash_full_lifecycle() {
    let (dir, cli) = init_repo();
    let repo = dir.path();
    commit_file(&cli, repo, "tracked.txt", "initial\n", "init");

    // Modify tracked file + create untracked file
    fs::write(repo.join("tracked.txt"), "modified\n").unwrap();
    fs::write(repo.join("untracked.txt"), "hello\n").unwrap();

    cli.stash_push(repo, Some("wip changes"), true)
        .expect("stash push");
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "initial\n"
    );
    assert!(!repo.join("untracked.txt").exists());

    let list = cli.run(repo, ["stash", "list"]).unwrap().stdout;
    assert!(list.contains("wip changes"));

    // Apply keeps stash entry
    cli.stash_apply(repo, 0).expect("stash apply");
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "modified\n"
    );
    assert!(repo.join("untracked.txt").exists());
    assert!(
        !cli.run(repo, ["stash", "list"])
            .unwrap()
            .stdout
            .trim()
            .is_empty()
    );

    // Reset and pop consumes stash entry
    cli.reset_to_commit(repo, "HEAD", ResetMode::Hard).unwrap();
    fs::remove_file(repo.join("untracked.txt")).unwrap();

    cli.stash_pop(repo, 0).expect("stash pop");
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "modified\n"
    );
    assert!(
        cli.run(repo, ["stash", "list"])
            .unwrap()
            .stdout
            .trim()
            .is_empty()
    );

    // Push another and drop it
    cli.stash_push(repo, None, true).expect("stash push 2");
    cli.stash_drop(repo, 0).expect("stash drop");
    assert!(
        cli.run(repo, ["stash", "list"])
            .unwrap()
            .stdout
            .trim()
            .is_empty()
    );
}

#[test]
fn working_tree_single_file_ops() {
    let (dir, cli) = init_repo();
    let repo = dir.path();

    // Unborn HEAD: stage & unstage
    fs::write(repo.join("first.txt"), "hello\n").unwrap();
    cli.stage_file(repo, "first.txt").expect("stage on unborn");
    cli.unstage_file(repo, "first.txt")
        .expect("unstage on unborn");
    let status = cli.run(repo, ["status", "--porcelain"]).unwrap().stdout;
    assert_eq!(status.trim(), "?? first.txt");

    // Discard untracked file removes it
    cli.discard_file(repo, "first.txt")
        .expect("discard untracked");
    assert!(!repo.join("first.txt").exists());

    // Normal repo with commit: stage, unstage, discard tracked edit
    commit_file(&cli, repo, "tracked.txt", "original\n", "init");
    fs::write(repo.join("tracked.txt"), "edited\n").unwrap();

    cli.stage_file(repo, "tracked.txt").unwrap();
    assert_eq!(
        cli.run(repo, ["diff", "--cached", "--name-only"])
            .unwrap()
            .stdout
            .trim(),
        "tracked.txt"
    );

    cli.unstage_file(repo, "tracked.txt").unwrap();
    assert!(
        cli.run(repo, ["diff", "--cached", "--name-only"])
            .unwrap()
            .stdout
            .trim()
            .is_empty()
    );

    cli.discard_file(repo, "tracked.txt").unwrap();
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "original\n"
    );
}

#[test]
fn remote_fetch_pull_push() {
    let remote_dir = TempDir::new().unwrap();
    let cli = GitCli::new().unwrap();
    cli.run(remote_dir.path(), ["init", "--bare", "-b", "main"])
        .expect("init bare remote");

    let (dir1, _) = init_repo();
    let repo1 = dir1.path();
    let remote_url = remote_dir.path().to_str().unwrap();
    cli.run(repo1, ["remote", "add", "origin", remote_url])
        .unwrap();
    commit_file(&cli, repo1, "readme.md", "hello\n", "init");
    cli.run(repo1, ["push", "-u", "origin", "main"]).unwrap();

    // Clone into repo2
    let dir2 = TempDir::new().unwrap();
    let repo2 = dir2.path().join("clone");
    cli.run(dir2.path(), ["clone", remote_url, repo2.to_str().unwrap()])
        .expect("clone");
    cli.run(&repo2, ["config", "user.name", "Clone User"])
        .unwrap();
    cli.run(&repo2, ["config", "user.email", "clone@example.com"])
        .unwrap();
    cli.run(&repo2, ["config", "commit.gpgsign", "false"])
        .unwrap();

    // Push a new commit from repo1
    commit_file(&cli, repo1, "second.txt", "two\n", "second");
    cli.push(repo1, Some("origin"), Some("main"), true)
        .expect("push with force_with_lease");

    // Fetch and pull in repo2
    cli.fetch(&repo2, Some("origin"), true).expect("fetch");
    cli.pull(&repo2).expect("pull");
    assert_eq!(
        fs::read_to_string(repo2.join("second.txt")).unwrap(),
        "two\n"
    );
}

#[test]
fn repo_watcher_emits_events_and_stops_on_drop() {
    let (dir, cli) = init_repo();
    let repo = dir.path();
    commit_file(&cli, repo, "init.txt", "init\n", "init");

    // Let initial filesystem events settle before starting the watcher.
    thread::sleep(Duration::from_millis(250));

    let (tx, rx) = mpsc::channel();
    let watcher = RepoWatcher::new(repo, tx).expect("start RepoWatcher");

    // Allow FSEvents stream to register cleanly on macOS.
    thread::sleep(Duration::from_millis(200));
    while rx.try_recv().is_ok() {}

    // 1. Working tree edit -> WorktreeChanged
    fs::write(repo.join("init.txt"), "changed\n").unwrap();
    assert!(
        wait_for_event(&rx, WatchEvent::WorktreeChanged, Duration::from_secs(5)),
        "expected WatchEvent::WorktreeChanged"
    );

    // 2. Branch creation -> RefsOrIndexChanged
    cli.create_branch(repo, "watch-branch", None, true)
        .expect("create branch");
    assert!(
        wait_for_event(&rx, WatchEvent::RefsOrIndexChanged, Duration::from_secs(5)),
        "expected WatchEvent::RefsOrIndexChanged"
    );

    // 3. Dropping watcher disconnects sender cleanly without panicking
    drop(watcher);
    // Drain any buffered events, then verify channel is disconnected.
    while rx.try_recv().is_ok() {}
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
}

fn setup_bare_and_two_clones() -> (TempDir, TempDir, TempDir, GitCli) {
    let remote_dir = TempDir::new().unwrap();
    let cli = GitCli::new().unwrap();
    cli.run(remote_dir.path(), ["init", "--bare", "-b", "main"])
        .expect("init bare remote");

    let (dir1, _) = init_repo();
    let repo1 = dir1.path();
    let remote_url = remote_dir.path().to_str().unwrap();
    cli.run(repo1, ["remote", "add", "origin", remote_url])
        .unwrap();
    cli.run(repo1, ["config", "pull.rebase", "false"]).unwrap();
    commit_file(&cli, repo1, "base.txt", "base\n", "initial commit");
    cli.publish_branch(repo1, "origin", "main")
        .expect("publish initial main");

    let dir2 = TempDir::new().unwrap();
    let repo2 = dir2.path();
    cli.run(repo2, ["clone", remote_url, "."]).expect("clone");
    cli.run(repo2, ["config", "user.name", "Clone User"])
        .unwrap();
    cli.run(repo2, ["config", "user.email", "clone@example.com"])
        .unwrap();
    cli.run(repo2, ["config", "commit.gpgsign", "false"])
        .unwrap();
    cli.run(repo2, ["config", "pull.rebase", "false"]).unwrap();

    (remote_dir, dir1, dir2, cli)
}

#[test]
fn branch_sync_status_publish_and_ahead_behind() {
    // 1. Unborn repo without remotes -> NoUpstream { default_remote: None }
    let (local_only, cli) = init_repo();
    assert_eq!(
        cli.branch_sync_status(local_only.path()).unwrap(),
        BranchSyncStatus::NoUpstream {
            branch: "main".to_string(),
            default_remote: None,
        }
    );
    assert_eq!(
        cli.list_remotes(local_only.path()).unwrap(),
        Vec::<String>::new()
    );

    // Add remotes without `origin` -> alphabetical first remote selected
    cli.run(
        local_only.path(),
        ["remote", "add", "zebra", "https://example.com/z.git"],
    )
    .unwrap();
    cli.run(
        local_only.path(),
        ["remote", "add", "alpha", "https://example.com/a.git"],
    )
    .unwrap();
    assert_eq!(
        cli.list_remotes(local_only.path()).unwrap(),
        vec!["alpha".to_string(), "zebra".to_string()]
    );
    assert_eq!(
        cli.branch_sync_status(local_only.path()).unwrap(),
        BranchSyncStatus::NoUpstream {
            branch: "main".to_string(),
            default_remote: Some("alpha".to_string()),
        }
    );

    // Adding `origin` prefers `origin` over `alpha`
    cli.run(
        local_only.path(),
        ["remote", "add", "origin", "https://example.com/o.git"],
    )
    .unwrap();
    assert_eq!(
        cli.branch_sync_status(local_only.path()).unwrap(),
        BranchSyncStatus::NoUpstream {
            branch: "main".to_string(),
            default_remote: Some("origin".to_string()),
        }
    );

    // 2. Real bare remote: new branch NoUpstream -> publish_branch -> Synced { 0, 0 }
    let (_remote_dir, dir1, dir2, cli) = setup_bare_and_two_clones();
    let repo1 = dir1.path();
    let repo2 = dir2.path();

    cli.create_branch(repo1, "feature", None, true)
        .expect("create feature");
    assert_eq!(cli.branch_upstream(repo1, "feature").unwrap(), None);
    assert_eq!(
        cli.branch_sync_status(repo1).unwrap(),
        BranchSyncStatus::NoUpstream {
            branch: "feature".to_string(),
            default_remote: Some("origin".to_string()),
        }
    );

    cli.publish_branch(repo1, "origin", "feature")
        .expect("publish feature");
    assert_eq!(
        cli.branch_upstream(repo1, "feature").unwrap(),
        Some("origin/feature".to_string())
    );
    assert_eq!(
        cli.branch_sync_status(repo1).unwrap(),
        BranchSyncStatus::Synced {
            branch: "feature".to_string(),
            upstream: "origin/feature".to_string(),
            ahead: 0,
            behind: 0,
        }
    );

    // 3. Switch back to main: 2 local commits -> ahead == 2; clone pushes 1 commit -> fetch -> behind == 1, ahead == 2
    cli.checkout_branch(repo1, "main").unwrap();
    commit_file(&cli, repo1, "local1.txt", "1\n", "local 1");
    commit_file(&cli, repo1, "local2.txt", "2\n", "local 2");
    assert_eq!(
        cli.branch_sync_status(repo1).unwrap(),
        BranchSyncStatus::Synced {
            branch: "main".to_string(),
            upstream: "origin/main".to_string(),
            ahead: 2,
            behind: 0,
        }
    );

    commit_file(&cli, repo2, "remote1.txt", "r1\n", "remote 1");
    cli.push(repo2, Some("origin"), Some("main"), false)
        .unwrap();

    cli.fetch(repo1, Some("origin"), false).unwrap();
    assert_eq!(
        cli.branch_sync_status(repo1).unwrap(),
        BranchSyncStatus::Synced {
            branch: "main".to_string(),
            upstream: "origin/main".to_string(),
            ahead: 2,
            behind: 1,
        }
    );

    // 4. Detached HEAD -> DetachedHead
    let head_oid = cli
        .run(repo1, ["rev-parse", "HEAD"])
        .unwrap()
        .stdout
        .trim()
        .to_string();
    cli.checkout_commit(repo1, &head_oid).unwrap();
    assert_eq!(
        cli.branch_sync_status(repo1).unwrap(),
        BranchSyncStatus::DetachedHead
    );
}

#[test]
fn sync_branch_pulls_merges_and_pushes_cleanly() {
    let (_remote_dir, dir1, dir2, cli) = setup_bare_and_two_clones();
    let repo1 = dir1.path();
    let repo2 = dir2.path();

    // Diverge cleanly on separate files
    commit_file(&cli, repo1, "left1.txt", "l1\n", "left 1");
    commit_file(&cli, repo1, "left2.txt", "l2\n", "left 2");

    commit_file(&cli, repo2, "right1.txt", "r1\n", "right 1");
    cli.publish_branch(repo2, "origin", "main").unwrap();

    let outcome = cli.sync_branch(repo1).expect("sync_branch should succeed");
    assert_eq!(
        outcome,
        SyncOutcome {
            branch: "main".to_string(),
            upstream: "origin/main".to_string(),
            pulled: true,
            pushed: true,
        }
    );
    assert_eq!(
        cli.branch_sync_status(repo1).unwrap(),
        BranchSyncStatus::Synced {
            branch: "main".to_string(),
            upstream: "origin/main".to_string(),
            ahead: 0,
            behind: 0,
        }
    );

    // repo2 pulls and observes both local commits from repo1
    let outcome2 = cli.sync_branch(repo2).expect("repo2 sync_branch");
    assert_eq!(
        outcome2,
        SyncOutcome {
            branch: "main".to_string(),
            upstream: "origin/main".to_string(),
            pulled: true,
            pushed: false,
        }
    );
    assert_eq!(
        cli.branch_sync_status(repo2).unwrap(),
        BranchSyncStatus::Synced {
            branch: "main".to_string(),
            upstream: "origin/main".to_string(),
            ahead: 0,
            behind: 0,
        }
    );
    assert!(repo2.join("left1.txt").exists());
    assert!(repo2.join("left2.txt").exists());
    assert!(repo1.join("right1.txt").exists());

    // Error cases: NoUpstream and DetachedHead
    cli.create_branch(repo1, "untracked", None, true).unwrap();
    assert!(matches!(
        cli.sync_branch(repo1),
        Err(OpsError::InvalidInput(_))
    ));
    let head = cli.run(repo1, ["rev-parse", "HEAD"]).unwrap().stdout;
    cli.checkout_commit(repo1, head.trim()).unwrap();
    assert!(matches!(
        cli.sync_branch(repo1),
        Err(OpsError::InvalidInput(_))
    ));
}

#[test]
fn push_branch_supports_renaming_tags_and_force_with_lease() {
    let (remote_dir, dir1, _dir2, cli) = setup_bare_and_two_clones();
    let repo1 = dir1.path();

    // Create a local branch with a tag and push it to a different remote branch name
    cli.create_branch(repo1, "local-wip", None, true).unwrap();
    let c1 = commit_file(&cli, repo1, "feat.txt", "v1\n", "feat v1");
    cli.create_tag(repo1, "v1.0.0", &c1, Some("release v1"))
        .unwrap();

    cli.push_branch(
        repo1,
        &PushOptions {
            local_branch: "local-wip".to_string(),
            remote: "origin".to_string(),
            remote_branch: "remote-topic".to_string(),
            set_upstream: true,
            push_tags: true,
            force: PushForceMode::None,
        },
    )
    .expect("push_branch to renamed remote branch with tags");

    assert_eq!(
        cli.branch_upstream(repo1, "local-wip").unwrap(),
        Some("origin/remote-topic".to_string())
    );
    let remote_refs = cli.run(remote_dir.path(), ["show-ref"]).unwrap().stdout;
    assert!(remote_refs.contains("refs/heads/remote-topic"));
    assert!(remote_refs.contains("refs/tags/v1.0.0"));

    // Amend commit locally -> normal push rejected, force-with-lease succeeds
    fs::write(repo1.join("feat.txt"), "v1-amended\n").unwrap();
    cli.stage_file(repo1, "feat.txt").unwrap();
    cli.run(repo1, ["commit", "--amend", "--no-edit"])
        .expect("amend commit");

    let rejected = cli.push_branch(
        repo1,
        &PushOptions {
            local_branch: "local-wip".to_string(),
            remote: "origin".to_string(),
            remote_branch: "remote-topic".to_string(),
            set_upstream: false,
            push_tags: false,
            force: PushForceMode::None,
        },
    );
    assert!(matches!(rejected, Err(OpsError::CommandFailed { .. })));

    cli.push_branch(
        repo1,
        &PushOptions {
            local_branch: "local-wip".to_string(),
            remote: "origin".to_string(),
            remote_branch: "remote-topic".to_string(),
            set_upstream: false,
            push_tags: false,
            force: PushForceMode::ForceWithLease,
        },
    )
    .expect("force-with-lease push");

    let new_local_oid = cli
        .run(repo1, ["rev-parse", "HEAD"])
        .unwrap()
        .stdout
        .trim()
        .to_string();
    let remote_topic_oid = cli
        .run(remote_dir.path(), ["rev-parse", "refs/heads/remote-topic"])
        .unwrap()
        .stdout
        .trim()
        .to_string();
    assert_eq!(remote_topic_oid, new_local_oid);

    // Input validation rejects empty or flag-like names
    assert!(matches!(
        cli.push_branch(
            repo1,
            &PushOptions {
                local_branch: "".to_string(),
                remote: "origin".to_string(),
                remote_branch: "main".to_string(),
                set_upstream: false,
                push_tags: false,
                force: PushForceMode::None,
            },
        ),
        Err(OpsError::InvalidInput(_))
    ));
}

#[test]
fn sync_branch_aborts_on_pull_conflict_without_pushing() {
    let (remote_dir, dir1, dir2, cli) = setup_bare_and_two_clones();
    let repo1 = dir1.path();
    let repo2 = dir2.path();

    // Both clones edit base.txt differently
    commit_file(&cli, repo2, "base.txt", "from repo2\n", "repo2 change");
    cli.push(repo2, Some("origin"), Some("main"), false)
        .unwrap();
    let remote_oid_before = cli
        .run(remote_dir.path(), ["rev-parse", "refs/heads/main"])
        .unwrap()
        .stdout
        .trim()
        .to_string();

    commit_file(
        &cli,
        repo1,
        "base.txt",
        "from repo1\n",
        "repo1 conflicting change",
    );

    let err = cli
        .sync_branch(repo1)
        .expect_err("sync_branch must fail on merge conflict");
    assert!(matches!(err, OpsError::CommandFailed { .. }));

    // Remote must NOT have received any push from repo1
    let remote_oid_after = cli
        .run(remote_dir.path(), ["rev-parse", "refs/heads/main"])
        .unwrap()
        .stdout
        .trim()
        .to_string();
    assert_eq!(remote_oid_after, remote_oid_before);

    // Working tree is in conflict state (UNMERGED) rather than pushed
    let status = cli.run(repo1, ["status", "--porcelain"]).unwrap().stdout;
    assert!(status.contains("UU base.txt"));
}
