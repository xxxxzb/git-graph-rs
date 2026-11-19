use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::cli::Result;

/// High-level repository change events emitted by [`RepoWatcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEvent {
    /// Repository references, `HEAD`, index, or in-progress state files changed.
    RefsOrIndexChanged,
    /// One or more files in the working tree (outside `.git/`) changed.
    WorktreeChanged,
}

/// Watches a Git repository's working tree and `.git/` metadata directory.
///
/// Stopping the watcher is automatic when `RepoWatcher` is dropped.
pub struct RepoWatcher {
    _watcher: RecommendedWatcher,
    repo_root: PathBuf,
}

impl std::fmt::Debug for RepoWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoWatcher")
            .field("repo_root", &self.repo_root)
            .finish_non_exhaustive()
    }
}

impl RepoWatcher {
    /// Starts watching `repo` and sends filtered [`WatchEvent`]s to `sender`.
    pub fn new(repo: impl AsRef<Path>, sender: Sender<WatchEvent>) -> Result<Self> {
        Self::with_handler(repo, move |event| {
            let _ = sender.send(event);
        })
    }

    /// Starts watching `repo` and invokes `handler` for each filtered [`WatchEvent`].
    pub fn with_handler<F>(repo: impl AsRef<Path>, mut handler: F) -> Result<Self>
    where
        F: FnMut(WatchEvent) + Send + 'static,
    {
        let raw_root = repo.as_ref().to_path_buf();
        let canonical_root = std::fs::canonicalize(&raw_root).unwrap_or_else(|_| raw_root.clone());

        let watch_root = canonical_root.clone();
        let raw_for_cb = raw_root.clone();
        let canonical_for_cb = canonical_root.clone();

        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| match res {
                Ok(event) => {
                    if matches!(event.kind, EventKind::Access(_)) {
                        return;
                    }

                    let mut emit_refs = false;
                    let mut emit_worktree = false;

                    for path in &event.paths {
                        match classify_event_path(&canonical_for_cb, &raw_for_cb, path) {
                            Some(WatchEvent::RefsOrIndexChanged) => emit_refs = true,
                            Some(WatchEvent::WorktreeChanged) => emit_worktree = true,
                            None => {}
                        }
                    }

                    if emit_refs {
                        handler(WatchEvent::RefsOrIndexChanged);
                    }
                    if emit_worktree {
                        handler(WatchEvent::WorktreeChanged);
                    }
                }
                Err(err) => {
                    log::warn!("repository watcher error: {err}");
                }
            })?;

        watcher.watch(&watch_root, RecursiveMode::Recursive)?;

        Ok(Self {
            _watcher: watcher,
            repo_root: canonical_root,
        })
    }

    /// Returns the canonicalized repository root being watched.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }
}

fn classify_event_path(canonical_root: &Path, raw_root: &Path, path: &Path) -> Option<WatchEvent> {
    let rel = path
        .strip_prefix(canonical_root)
        .or_else(|_| path.strip_prefix(raw_root))
        .ok()?;
    classify_relative_path(rel)
}

/// Classifies a repository-relative path into a [`WatchEvent`], filtering out high-noise paths
/// such as `.git/objects/**` and temporary `*.lock` files.
pub(crate) fn classify_relative_path(rel: &Path) -> Option<WatchEvent> {
    let mut components = rel.components();
    let first = components.next()?;

    if first == Component::Normal(OsStr::new(".git")) {
        let git_subpath: PathBuf = components.collect();
        return classify_git_subpath(&git_subpath);
    }

    if rel.file_name() == Some(OsStr::new(".DS_Store")) {
        return None;
    }

    Some(WatchEvent::WorktreeChanged)
}

fn classify_git_subpath(subpath: &Path) -> Option<WatchEvent> {
    if subpath.as_os_str().is_empty() {
        return None;
    }

    // Ignore temporary lock files created during git transactions (e.g. index.lock).
    if subpath.extension() == Some(OsStr::new("lock")) {
        return None;
    }

    let first = subpath.components().next()?;
    let Component::Normal(name) = first else {
        return None;
    };

    let name_str = name.to_str()?;
    match name_str {
        "HEAD" | "index" | "packed-refs" | "refs" | "MERGE_HEAD" | "REBASE_HEAD"
        | "CHERRY_PICK_HEAD" | "REVERT_HEAD" | "ORIG_HEAD" | "BISECT_LOG" | "rebase-merge"
        | "rebase-apply" => Some(WatchEvent::RefsOrIndexChanged),
        // High-frequency or non-graph noise: objects/, logs/, hooks/, info/, etc.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_filters_noise_and_distinguishes_events() {
        assert_eq!(
            classify_relative_path(Path::new(".git/HEAD")),
            Some(WatchEvent::RefsOrIndexChanged)
        );
        assert_eq!(
            classify_relative_path(Path::new(".git/index")),
            Some(WatchEvent::RefsOrIndexChanged)
        );
        assert_eq!(
            classify_relative_path(Path::new(".git/refs/heads/main")),
            Some(WatchEvent::RefsOrIndexChanged)
        );
        assert_eq!(
            classify_relative_path(Path::new(".git/refs/stash")),
            Some(WatchEvent::RefsOrIndexChanged)
        );
        assert_eq!(
            classify_relative_path(Path::new(".git/packed-refs")),
            Some(WatchEvent::RefsOrIndexChanged)
        );

        // Filtered noise inside .git
        assert_eq!(classify_relative_path(Path::new(".git")), None);
        assert_eq!(
            classify_relative_path(Path::new(".git/objects/ab/cdef012345")),
            None
        );
        assert_eq!(classify_relative_path(Path::new(".git/index.lock")), None);
        assert_eq!(
            classify_relative_path(Path::new(".git/refs/heads/main.lock")),
            None
        );
        assert_eq!(classify_relative_path(Path::new(".git/logs/HEAD")), None);

        // Working tree
        assert_eq!(classify_relative_path(Path::new("")), None);
        assert_eq!(classify_relative_path(Path::new(".DS_Store")), None);
        assert_eq!(
            classify_relative_path(Path::new("src/main.rs")),
            Some(WatchEvent::WorktreeChanged)
        );
    }
}
