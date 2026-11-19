//! 单个打开的本地仓库 Tab 状态模型及后台异步加载任务。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use ely_git::Stretch;
use git_graph_core::api::{Engine, INDEX, WORKING_TREE};
use git_graph_core::types::{
    GitAuthor, GitCommit, GitCommitDetails, GitCommitStash, GitFileChange, GitLineCounts,
    GitRepoInfo, UNCOMMITTED,
};
use git_graph_core::{ErrorKind, details, diff, status};
use git_graph_layout::GraphLayout;
use git_graph_ops::{BranchSyncStatus, GitCli, RepoWatcher};
use gpui::{Image, SharedString, Task, UniformListScrollHandle};

use crate::diff_parser::{ParsedDiff, parse_unified_diff, stretches_from_texts};
use crate::state::{
    FilterCriteria, commit_to_node, compute_graph_layout, filter_commit_indices,
    image_format_for_path, matches_author_filter, matches_commit_query, repo_display_name,
};

/// 详情面板当前选中的文件所属分区。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSection {
    Committed,
    Staged,
    Unstaged,
}

/// 详情面板当前选中的文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedFile {
    pub path: String,
    pub old_path: String,
    pub section: FileSection,
}

/// 右侧 Diff 面板的渲染载荷。
#[derive(Clone)]
pub enum FileDiffContent {
    Text {
        stretches: Vec<Stretch>,
        added: usize,
        removed: usize,
    },
    Picture {
        old: Option<Arc<Image>>,
        new: Option<Arc<Image>>,
    },
    Binary {
        old_size: Option<u64>,
        new_size: Option<u64>,
    },
    Empty,
}

/// 后台拉取的单仓库图谱快照。
pub struct RepoSnapshot {
    pub info: GitRepoInfo,
    pub authors: Vec<GitAuthor>,
    pub commits: Vec<GitCommit>,
    pub head_hash: Option<String>,
    pub more_available: bool,
    pub scm_changes: Vec<status::ScmChange>,
    pub uncommitted_count: usize,
    pub sync_status: Option<BranchSyncStatus>,
}

/// 后台全历史搜索输出结果。
#[derive(Debug, Clone)]
pub struct HistorySearchOutput {
    pub commits: Vec<GitCommit>,
    pub used_literal_fallback: bool,
}

/// 单个打开的仓库 Tab 全部状态。
pub struct RepoTabState {
    pub id: u64,
    pub path: PathBuf,
    pub name: String,
    pub engine: Engine,
    pub load_serial: u64,
    pub detail_serial: u64,
    pub line_count_serial: u64,
    pub diff_serial: u64,
    pub search_serial: u64,

    pub info: Option<GitRepoInfo>,
    pub authors: Vec<GitAuthor>,
    pub all_commits: Vec<GitCommit>,
    pub head_hash: Option<String>,
    pub more_available: bool,
    pub max_commits: u32,
    pub filter: FilterCriteria,
    pub filtered_indices: Vec<usize>,
    pub layout: GraphLayout,
    pub uncommitted_count: usize,

    pub search_loading: bool,
    pub search_results: Option<Vec<GitCommit>>,
    pub search_error: Option<String>,
    pub search_match_count: Option<usize>,
    pub search_used_literal_fallback: bool,

    pub selected_hash: Option<String>,
    pub details: Option<Result<GitCommitDetails, String>>,
    pub line_counts: BTreeMap<String, GitLineCounts>,
    pub scm_changes: Vec<status::ScmChange>,
    pub selected_file: Option<SelectedFile>,
    pub file_view: Option<Result<FileDiffContent, String>>,

    pub loading_graph: bool,
    pub loading_more: bool,
    pub loading_details: bool,
    pub loading_diff: bool,
    pub sync_status: Option<BranchSyncStatus>,
    pub syncing: bool,
    pub sync_action_label: Option<SharedString>,
    pub error: Option<String>,

    pub graph_scroll: UniformListScrollHandle,
    /// 图形列水平滚动偏移（px，>= 0，整列共享以保持跨行泳道连线严格对齐）。
    pub graph_scroll_x: f32,
    pub _watcher: Option<RepoWatcher>,
    pub _watch_task: Option<Task<()>>,
}

impl RepoTabState {
    /// 创建一个新的仓库 Tab（尚未完成首次异步加载）。
    pub fn new(id: u64, engine: Engine) -> Self {
        let path = engine.root().to_path_buf();
        let name = repo_display_name(&path);
        Self {
            id,
            path,
            name,
            engine,
            load_serial: 0,
            detail_serial: 0,
            line_count_serial: 0,
            diff_serial: 0,
            search_serial: 0,
            info: None,
            authors: Vec::new(),
            all_commits: Vec::new(),
            head_hash: None,
            more_available: false,
            max_commits: 300,
            filter: FilterCriteria::default(),
            filtered_indices: Vec::new(),
            layout: GraphLayout::default(),
            uncommitted_count: 0,
            search_loading: false,
            search_results: None,
            search_error: None,
            search_match_count: None,
            search_used_literal_fallback: false,
            selected_hash: None,
            details: None,
            line_counts: BTreeMap::new(),
            scm_changes: Vec::new(),
            selected_file: None,
            file_view: None,
            loading_graph: true,
            loading_more: false,
            loading_details: false,
            loading_diff: false,
            sync_status: None,
            syncing: false,
            sync_action_label: None,
            error: None,
            graph_scroll: UniformListScrollHandle::new(),
            graph_scroll_x: 0.0,
            _watcher: None,
            _watch_task: None,
        }
    }

    /// 当前是否处于非空搜索状态。
    pub fn is_search_active(&self) -> bool {
        !self.filter.search_query.trim().is_empty()
    }

    /// 清空全历史搜索状态并恢复常规图谱列表。
    pub fn clear_search_state(&mut self) {
        self.search_loading = false;
        self.search_results = None;
        self.search_error = None;
        self.search_match_count = None;
        self.search_used_literal_fallback = false;
        self.recompute_visible_rows();
    }

    /// 当前可见行数量（搜索结果就绪时取 `search_results`，否则取 `filtered_indices`）。
    pub fn visible_commit_count(&self) -> usize {
        if self.is_search_active()
            && let Some(results) = &self.search_results
        {
            return results.len();
        }
        self.filtered_indices.len()
    }

    /// 获取当前可见提交列表快照（用于虚拟列表渲染）。
    pub fn visible_commits(&self) -> Vec<GitCommit> {
        if self.is_search_active()
            && let Some(results) = &self.search_results
        {
            return results.clone();
        }
        self.filtered_indices
            .iter()
            .filter_map(|&idx| self.all_commits.get(idx).cloned())
            .collect()
    }

    /// 当前视图是否需要截断指向列表外部的悬空连线（搜索、分支/作者筛选或已加载至底端）。
    pub fn should_truncate_missing_parents(&self) -> bool {
        self.is_search_active()
            || self.filter.branch.is_some()
            || self.filter.author.is_some()
            || !self.more_available
    }

    /// 应用搜索关键词过滤并重算可见行的泳道布局。
    pub fn recompute_visible_rows(&mut self) {
        let truncate = self.should_truncate_missing_parents();
        if self.is_search_active() {
            if let Some(results) = &self.search_results {
                let nodes: Vec<_> = results.iter().map(commit_to_node).collect();
                self.layout = compute_graph_layout(&nodes, self.head_hash.as_deref(), true);
                return;
            }
            // 后台全历史搜索尚未返回时，先基于已加载页即时预过滤并保留可见父子连线、截断悬空线
            let indices = filter_commit_indices(&self.all_commits, &self.filter.search_query);
            let nodes: Vec<_> = indices
                .iter()
                .filter_map(|&idx| self.all_commits.get(idx).map(commit_to_node))
                .collect();
            self.filtered_indices = indices;
            self.layout = compute_graph_layout(&nodes, self.head_hash.as_deref(), true);
            return;
        }

        let indices = filter_commit_indices(&self.all_commits, "");
        let nodes: Vec<_> = indices
            .iter()
            .filter_map(|&idx| self.all_commits.get(idx).map(commit_to_node))
            .collect();
        self.filtered_indices = indices;
        self.layout = compute_graph_layout(&nodes, self.head_hash.as_deref(), truncate);
    }

    /// 查找指定 hash 在当前可见行中的索引。
    pub fn visible_index_of(&self, hash: &str) -> Option<usize> {
        if self.is_search_active()
            && let Some(results) = &self.search_results
        {
            return results.iter().position(|c| c.hash == hash);
        }
        self.filtered_indices
            .iter()
            .position(|&idx| self.all_commits.get(idx).is_some_and(|c| c.hash == hash))
    }

    /// 通过 hash 查找提交（优先从已加载页查找，若不在则查找全历史搜索结果）。
    pub fn find_commit(&self, hash: &str) -> Option<&GitCommit> {
        self.all_commits.iter().find(|c| c.hash == hash).or_else(|| {
            self.search_results
                .as_ref()
                .and_then(|results| results.iter().find(|c| c.hash == hash))
        })
    }

    /// 当前选中的提交在 `all_commits` 中是否已携带 stash 元信息。
    pub fn selected_stash(&self) -> Option<GitCommitStash> {
        let hash = self.selected_hash.as_deref()?;
        self.find_commit(hash).and_then(|c| c.stash.clone())
    }
}

fn is_potential_hash(query: &str) -> bool {
    (4..=40).contains(&query.len()) && query.bytes().all(|b| b.is_ascii_hexdigit())
}

fn subject_from_body(body: &str) -> String {
    body.lines()
        .next()
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

fn commit_from_details(details: GitCommitDetails) -> GitCommit {
    let message = subject_from_body(&details.body);
    GitCommit {
        hash: details.hash,
        parents: details.parents,
        author: details.author,
        email: details.author_email,
        date: details.author_date,
        message,
        heads: Vec::new(),
        tags: Vec::new(),
        remotes: Vec::new(),
        stash: None,
    }
}

/// 在后台线程执行全历史正则搜索、无效正则转义兜底、短 Hash 直达解析、本地匹配融合去重与作者筛选。
pub fn execute_history_search(
    engine: &Engine,
    query: &str,
    loaded_commits: &[GitCommit],
    author_filter: Option<&str>,
) -> Result<HistorySearchOutput, String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(HistorySearchOutput {
            commits: Vec::new(),
            used_literal_fallback: false,
        });
    }

    // 1. 全历史正则搜索（非法正则语法自动转义为字面量重试）
    let mut used_literal_fallback = false;
    let history_matches = match engine.search_history(trimmed) {
        Ok(matches) => matches,
        Err(err) if err.kind == ErrorKind::InvalidArgument => {
            used_literal_fallback = true;
            let escaped = regex::escape(trimmed);
            engine
                .search_history(&escaped)
                .map_err(|e| e.to_string())?
        }
        Err(err) => return Err(err.to_string()),
    };

    let loaded_by_hash: HashMap<&str, &GitCommit> = loaded_commits
        .iter()
        .map(|c| (c.hash.as_str(), c))
        .collect();

    let mut seen_hashes = HashSet::new();
    let mut direct_hit: Option<GitCommit> = None;

    // 2. 4~40 位十六进制短 Hash 全历史直达解析
    if is_potential_hash(trimmed) {
        let needle_lower = trimmed.to_ascii_lowercase();
        let already_matched_hash = history_matches
            .iter()
            .any(|m| m.hash.to_ascii_lowercase().starts_with(&needle_lower));
        if !already_matched_hash && let Ok(details) = engine.commit(trimmed) {
            let commit = loaded_by_hash
                .get(details.hash.as_str())
                .map(|c| (*c).clone())
                .unwrap_or_else(|| commit_from_details(details));
            if matches_author_filter(&commit, author_filter) {
                seen_hashes.insert(commit.hash.clone());
                direct_hit = Some(commit);
            }
        }
    }

    // 3. 融合全历史命中与已加载页本地匹配（去重并复用已加载页的完整 refs/parents/stash）
    let mut merged = Vec::new();
    for m in history_matches {
        if seen_hashes.contains(&m.hash) {
            continue;
        }
        let commit = loaded_by_hash
            .get(m.hash.as_str())
            .map(|c| (*c).clone())
            .unwrap_or(GitCommit {
                hash: m.hash,
                parents: Vec::new(),
                author: m.author,
                email: String::new(),
                date: m.date,
                message: m.message,
                heads: Vec::new(),
                tags: Vec::new(),
                remotes: Vec::new(),
                stash: None,
            });
        if matches_author_filter(&commit, author_filter) {
            seen_hashes.insert(commit.hash.clone());
            merged.push(commit);
        }
    }

    let needle_lower = trimmed.to_lowercase();
    for commit in loaded_commits {
        if seen_hashes.contains(&commit.hash) {
            continue;
        }
        if matches_commit_query(commit, &needle_lower)
            && matches_author_filter(commit, author_filter)
        {
            seen_hashes.insert(commit.hash.clone());
            merged.push(commit.clone());
        }
    }

    // 未提交更改行置顶，其余提交按提交时间倒序稳定排列
    merged.sort_by(|a, b| {
        let a_uncommitted = a.hash == UNCOMMITTED;
        let b_uncommitted = b.hash == UNCOMMITTED;
        b_uncommitted
            .cmp(&a_uncommitted)
            .then_with(|| b.date.cmp(&a.date))
    });

    let mut commits = Vec::with_capacity(merged.len() + usize::from(direct_hit.is_some()));
    if let Some(hit) = direct_hit {
        commits.push(hit);
    }
    commits.extend(merged);

    Ok(HistorySearchOutput {
        commits,
        used_literal_fallback,
    })
}

/// 在后台线程加载仓库图谱、分支/作者信息、工作区与远程同步状态。
pub fn load_repo_snapshot(
    engine: &Engine,
    cli: Option<&GitCli>,
    filter: &FilterCriteria,
    max_commits: u32,
) -> Result<RepoSnapshot, String> {
    let options = filter.to_graph_options(max_commits);
    let info = engine.info(&options).map_err(|e| e.to_string())?;
    let authors = engine.authors().unwrap_or_default();
    let data = engine.graph(&options).map_err(|e| e.to_string())?;
    if let Some(err) = &data.error {
        return Err(err.clone());
    }
    let scm_changes = engine.status().unwrap_or_default();
    let uncommitted_count = status::count_changes(engine.repo(), true).unwrap_or(scm_changes.len());
    let sync_status = cli
        .and_then(|c| c.branch_sync_status(engine.root()).ok())
        .or_else(|| {
            GitCli::new()
                .ok()
                .and_then(|c| c.branch_sync_status(engine.root()).ok())
        });

    Ok(RepoSnapshot {
        info,
        authors,
        commits: data.commits,
        head_hash: data.head,
        more_available: data.more_commits_available,
        scm_changes,
        uncommitted_count,
        sync_status,
    })
}

/// 在后台线程读取提交详情（普通提交 / Stash / 未提交行）。
pub fn load_commit_details(
    engine: &Engine,
    hash: &str,
    stash: Option<&GitCommitStash>,
) -> Result<GitCommitDetails, String> {
    if hash == UNCOMMITTED {
        return details::uncommitted_details(engine.repo()).map_err(|e| e.to_string());
    }
    if let Some(stash) = stash {
        return details::stash_details(engine.repo(), hash, stash).map_err(|e| e.to_string());
    }
    details::commit_details(engine.repo(), hash).map_err(|e| e.to_string())
}

/// 在后台线程批量计算变更文件的 `+N/-M` 行数统计。
pub fn load_file_line_counts(
    engine: &Engine,
    from: Option<&str>,
    hash: &str,
    paths: &[String],
) -> Result<BTreeMap<String, GitLineCounts>, String> {
    if hash == UNCOMMITTED || paths.is_empty() {
        return Ok(BTreeMap::new());
    }
    diff::line_counts(engine.repo(), from, hash, paths).map_err(|e| e.to_string())
}

/// 在后台线程加载单文件 Diff（支持普通提交、Stash、未提交 Staged/Unstaged，支持图片与二进制对比）。
pub fn load_file_diff(
    engine: &Engine,
    hash: &str,
    stash: Option<&GitCommitStash>,
    first_parent: Option<&str>,
    file: &SelectedFile,
    file_change: Option<&GitFileChange>,
) -> Result<FileDiffContent, String> {
    let image_fmt = image_format_for_path(&file.path);

    if hash == UNCOMMITTED {
        return load_uncommitted_file_diff(engine, file, image_fmt);
    }

    if let Some(stash) = stash {
        return load_stash_file_diff(engine, hash, stash, file, file_change, image_fmt);
    }

    // 普通提交：若是图片，读取父提交与当前提交字节做 BeforeAfter 对比
    if let Some(fmt) = image_fmt {
        let old_bytes = match first_parent {
            Some(parent) => git_graph_core::blob::commit_file_bytes(
                engine.repo(),
                parent,
                &file.old_path,
            )
            .map_err(|e| e.to_string())?,
            None => None,
        };
        let new_bytes =
            git_graph_core::blob::commit_file_bytes(engine.repo(), hash, &file.path)
                .map_err(|e| e.to_string())?;
        return Ok(FileDiffContent::Picture {
            old: old_bytes.map(|b| Arc::new(Image::from_bytes(fmt, b))),
            new: new_bytes.map(|b| Arc::new(Image::from_bytes(fmt, b))),
        });
    }

    // 普通提交文本：调用 git_graph_core::blob::commit_file_diff 并解析为 Stretch + mark_words
    let raw_diff = engine
        .file_diff(hash, &file.path)
        .map_err(|e| e.to_string())?;
    match parse_unified_diff(&raw_diff) {
        ParsedDiff::Text {
            stretches,
            added,
            removed,
        } => Ok(FileDiffContent::Text {
            stretches,
            added,
            removed,
        }),
        ParsedDiff::Binary => {
            let old_size = match first_parent {
                Some(parent) => git_graph_core::blob::commit_file_bytes(
                    engine.repo(),
                    parent,
                    &file.old_path,
                )
                .ok()
                .flatten()
                .map(|b| b.len() as u64),
                None => None,
            };
            let new_size = git_graph_core::blob::commit_file_bytes(engine.repo(), hash, &file.path)
                .ok()
                .flatten()
                .map(|b| b.len() as u64);
            Ok(FileDiffContent::Binary { old_size, new_size })
        }
        ParsedDiff::Empty => Ok(FileDiffContent::Empty),
    }
}

fn load_uncommitted_file_diff(
    engine: &Engine,
    file: &SelectedFile,
    image_fmt: Option<gpui::ImageFormat>,
) -> Result<FileDiffContent, String> {
    let read_side_bytes = |rev: &str, path: &str| -> Option<Vec<u8>> {
        if rev == WORKING_TREE {
            let full = engine.root().join(path);
            std::fs::read(full).ok()
        } else if rev == INDEX {
            git_graph_core::blob::index_file_bytes(engine.repo(), path)
                .ok()
                .flatten()
        } else {
            git_graph_core::blob::commit_file_bytes(engine.repo(), rev, path)
                .ok()
                .flatten()
        }
    };

    let (old_rev, new_rev) = match file.section {
        FileSection::Staged => ("HEAD", INDEX),
        FileSection::Unstaged | FileSection::Committed => (INDEX, WORKING_TREE),
    };

    if let Some(fmt) = image_fmt {
        let old_bytes = read_side_bytes(old_rev, &file.old_path);
        let new_bytes = read_side_bytes(new_rev, &file.path);
        return Ok(FileDiffContent::Picture {
            old: old_bytes.map(|b| Arc::new(Image::from_bytes(fmt, b))),
            new: new_bytes.map(|b| Arc::new(Image::from_bytes(fmt, b))),
        });
    }

    let old_file = engine.file(old_rev, &file.old_path).ok();
    let new_file = engine.file(new_rev, &file.path).ok();

    let is_binary = old_file.as_ref().is_some_and(|f| f.binary)
        || new_file.as_ref().is_some_and(|f| f.binary);
    if is_binary {
        let old_size = read_side_bytes(old_rev, &file.old_path).map(|b| b.len() as u64);
        let new_size = read_side_bytes(new_rev, &file.path).map(|b| b.len() as u64);
        return Ok(FileDiffContent::Binary { old_size, new_size });
    }

    let old_text = old_file.as_ref().and_then(|f| f.contents.as_deref());
    let new_text = new_file.as_ref().and_then(|f| f.contents.as_deref());
    Ok(match stretches_from_texts(old_text, new_text) {
        ParsedDiff::Text {
            stretches,
            added,
            removed,
        } => FileDiffContent::Text {
            stretches,
            added,
            removed,
        },
        ParsedDiff::Binary => FileDiffContent::Binary {
            old_size: None,
            new_size: None,
        },
        ParsedDiff::Empty => FileDiffContent::Empty,
    })
}

fn load_stash_file_diff(
    engine: &Engine,
    stash_hash: &str,
    stash: &GitCommitStash,
    file: &SelectedFile,
    file_change: Option<&GitFileChange>,
    image_fmt: Option<gpui::ImageFormat>,
) -> Result<FileDiffContent, String> {
    let is_untracked = file_change
        .is_some_and(|c| c.kind == git_graph_core::types::GitFileStatus::Untracked);
    let new_rev = if is_untracked {
        stash
            .untracked_files_hash
            .as_deref()
            .unwrap_or(stash_hash)
    } else {
        stash_hash
    };

    if let Some(fmt) = image_fmt {
        let old_bytes = if is_untracked {
            None
        } else {
            git_graph_core::blob::commit_file_bytes(
                engine.repo(),
                &stash.base_hash,
                &file.old_path,
            )
            .ok()
            .flatten()
        };
        let new_bytes =
            git_graph_core::blob::commit_file_bytes(engine.repo(), new_rev, &file.path)
                .ok()
                .flatten();
        return Ok(FileDiffContent::Picture {
            old: old_bytes.map(|b| Arc::new(Image::from_bytes(fmt, b))),
            new: new_bytes.map(|b| Arc::new(Image::from_bytes(fmt, b))),
        });
    }

    let old_file = if is_untracked {
        None
    } else {
        engine.file(&stash.base_hash, &file.old_path).ok()
    };
    let new_file = engine.file(new_rev, &file.path).ok();

    let is_binary = old_file.as_ref().is_some_and(|f| f.binary)
        || new_file.as_ref().is_some_and(|f| f.binary);
    if is_binary {
        let old_size = if is_untracked {
            None
        } else {
            git_graph_core::blob::commit_file_bytes(
                engine.repo(),
                &stash.base_hash,
                &file.old_path,
            )
            .ok()
            .flatten()
            .map(|b| b.len() as u64)
        };
        let new_size =
            git_graph_core::blob::commit_file_bytes(engine.repo(), new_rev, &file.path)
                .ok()
                .flatten()
                .map(|b| b.len() as u64);
        return Ok(FileDiffContent::Binary { old_size, new_size });
    }

    let old_text = old_file.as_ref().and_then(|f| f.contents.as_deref());
    let new_text = new_file.as_ref().and_then(|f| f.contents.as_deref());
    Ok(match stretches_from_texts(old_text, new_text) {
        ParsedDiff::Text {
            stretches,
            added,
            removed,
        } => FileDiffContent::Text {
            stretches,
            added,
            removed,
        },
        ParsedDiff::Binary => FileDiffContent::Binary {
            old_size: None,
            new_size: None,
        },
        ParsedDiff::Empty => FileDiffContent::Empty,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use git_graph_core::api::GraphOptions;
    use git_graph_core::types::GitCommitTag;

    use super::*;

    fn git(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
        let mut cmd = Command::new("git");
        cmd.current_dir(dir).args(args);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("git command should execute");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_test_repo() -> (tempfile::TempDir, Engine, Vec<String>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        git(root, &["init", "-b", "main"], &[]);
        git(root, &["config", "user.name", "Alice"], &[]);
        git(root, &["config", "user.email", "alice@example.com"], &[]);

        let commits_spec = [
            (
                "Alice",
                "alice@example.com",
                "2025-01-01T10:00:00Z",
                "chore: initial bootstrap\n\nDeep body token: legacy-needle-42\nAlso literal [WIP tag and unclosed( pattern",
            ),
            (
                "Bob",
                "bob@example.com",
                "2025-01-02T10:00:00Z",
                "feat: add search engine\n\nBody mentions legacy-needle-42 for Bob",
            ),
            (
                "Alice",
                "alice@example.com",
                "2025-01-03T10:00:00Z",
                "fix(ui): repair toolbar alignment",
            ),
            (
                "Bob",
                "bob@example.com",
                "2025-01-04T10:00:00Z",
                "docs: update readme",
            ),
        ];

        let mut hashes = Vec::new();
        for (idx, (author, email, date, msg)) in commits_spec.iter().enumerate() {
            std::fs::write(root.join("file.txt"), format!("step {idx}\n")).unwrap();
            git(root, &["add", "file.txt"], &[]);
            git(
                root,
                &["commit", "-m", msg],
                &[
                    ("GIT_AUTHOR_NAME", author),
                    ("GIT_AUTHOR_EMAIL", email),
                    ("GIT_COMMITTER_NAME", author),
                    ("GIT_COMMITTER_EMAIL", email),
                    ("GIT_AUTHOR_DATE", date),
                    ("GIT_COMMITTER_DATE", date),
                ],
            );
            hashes.push(git(root, &["rev-parse", "HEAD"], &[]));
        }

        // 给最新提交打一个 tag，验证已加载提交去重后仍保留完整 refs
        git(root, &["tag", "v0.9.0", &hashes[2]], &[]);

        let engine = Engine::open(root).expect("open engine");
        (dir, engine, hashes)
    }

    #[test]
    fn history_search_finds_unloaded_commits_including_body() {
        let (_dir, engine, hashes) = init_test_repo();

        // 只加载最近 2 条到 all_commits（hashes[3], hashes[2]）
        let loaded = engine
            .graph(&GraphOptions {
                max_commits: 2,
                ..Default::default()
            })
            .unwrap()
            .commits;
        assert_eq!(loaded.len(), 2);
        assert!(!loaded.iter().any(|c| c.hash == hashes[0]));

        let out = execute_history_search(&engine, "legacy-needle-42", &loaded, None).unwrap();
        assert!(!out.used_literal_fallback);
        let found_hashes: Vec<&str> = out.commits.iter().map(|c| c.hash.as_str()).collect();
        assert!(found_hashes.contains(&hashes[1].as_str()));
        assert!(found_hashes.contains(&hashes[0].as_str()));
        assert_eq!(out.commits.len(), 2);
    }

    #[test]
    fn history_search_supports_regex_and_literal_fallback_on_invalid_regex() {
        let (_dir, engine, hashes) = init_test_repo();
        let loaded = engine
            .graph(&GraphOptions {
                max_commits: 2,
                ..Default::default()
            })
            .unwrap()
            .commits;

        // 合法正则查询：^feat:|fix\(
        let regex_out = execute_history_search(&engine, r"^feat:|fix\(", &loaded, None).unwrap();
        assert!(!regex_out.used_literal_fallback);
        let regex_hashes: Vec<&str> = regex_out.commits.iter().map(|c| c.hash.as_str()).collect();
        assert!(regex_hashes.contains(&hashes[2].as_str()));
        assert!(regex_hashes.contains(&hashes[1].as_str()));
        assert_eq!(regex_hashes.len(), 2);

        // 非法正则字符（未闭合 [WIP 与 unclosed(）应自动字面量转义兜底
        let bracket_out = execute_history_search(&engine, "[WIP", &loaded, None).unwrap();
        assert!(bracket_out.used_literal_fallback);
        assert_eq!(bracket_out.commits.len(), 1);
        assert_eq!(bracket_out.commits[0].hash, hashes[0]);

        let paren_out = execute_history_search(&engine, "unclosed(", &loaded, None).unwrap();
        assert!(paren_out.used_literal_fallback);
        assert_eq!(paren_out.commits.len(), 1);
        assert_eq!(paren_out.commits[0].hash, hashes[0]);
    }

    #[test]
    fn history_search_resolves_short_hash_and_dedups_preserving_loaded_refs() {
        let (_dir, engine, hashes) = init_test_repo();
        let loaded = engine
            .graph(&GraphOptions {
                max_commits: 2,
                ..Default::default()
            })
            .unwrap()
            .commits;

        // 1. 历史深处未加载提交的 7 位短 Hash 直达解析
        let early_short = &hashes[0][..7];
        let direct_out = execute_history_search(&engine, early_short, &loaded, None).unwrap();
        assert_eq!(direct_out.commits.len(), 1);
        assert_eq!(direct_out.commits[0].hash, hashes[0]);
        assert!(direct_out.commits[0].message.contains("initial bootstrap"));

        // 2. 已加载提交按 tag 或 message 搜索时保留 refs 且去重
        let mut loaded_with_tag = loaded.clone();
        if let Some(c) = loaded_with_tag.iter_mut().find(|c| c.hash == hashes[2])
            && c.tags.is_empty()
        {
            c.tags.push(GitCommitTag {
                name: "v0.9.0".to_string(),
                annotated: false,
            });
        }
        let dedup_out = execute_history_search(&engine, "repair toolbar", &loaded_with_tag, None).unwrap();
        assert_eq!(dedup_out.commits.len(), 1);
        assert_eq!(dedup_out.commits[0].hash, hashes[2]);
        assert!(
            dedup_out.commits[0]
                .tags
                .iter()
                .any(|t| t.name == "v0.9.0")
        );
        assert!(!dedup_out.commits[0].parents.is_empty());

        // 3. 本地匹配 ref（v0.9.0 不在 commit message 中，仅在 tag 中）也能融合进结果
        let tag_out = execute_history_search(&engine, "v0.9.0", &loaded_with_tag, None).unwrap();
        assert_eq!(tag_out.commits.len(), 1);
        assert_eq!(tag_out.commits[0].hash, hashes[2]);
    }

    #[test]
    fn history_search_respects_author_filter() {
        let (_dir, engine, hashes) = init_test_repo();
        let loaded = engine
            .graph(&GraphOptions {
                max_commits: 2,
                ..Default::default()
            })
            .unwrap()
            .commits;

        // "legacy-needle-42" 同时出现在 hashes[0] (Alice) 和 hashes[1] (Bob)
        let alice_only =
            execute_history_search(&engine, "legacy-needle-42", &loaded, Some("Alice")).unwrap();
        assert_eq!(alice_only.commits.len(), 1);
        assert_eq!(alice_only.commits[0].hash, hashes[0]);
        assert_eq!(alice_only.commits[0].author, "Alice");

        let bob_only =
            execute_history_search(&engine, "legacy-needle-42", &loaded, Some("Bob")).unwrap();
        assert_eq!(bob_only.commits.len(), 1);
        assert_eq!(bob_only.commits[0].hash, hashes[1]);
        assert_eq!(bob_only.commits[0].author, "Bob");
    }

    #[test]
    fn repo_tab_search_layout_and_lookup_work_across_pages() {
        let (_dir, engine, hashes) = init_test_repo();
        let loaded = engine
            .graph(&GraphOptions {
                max_commits: 2,
                ..Default::default()
            })
            .unwrap()
            .commits;

        let mut tab = RepoTabState::new(1, engine.clone());
        tab.all_commits = loaded.clone();
        tab.head_hash = Some(hashes[3].clone());
        tab.recompute_visible_rows();
        assert_eq!(tab.visible_commit_count(), 2);

        let out = execute_history_search(&engine, "legacy-needle-42", &loaded, None).unwrap();
        tab.filter.search_query = "legacy-needle-42".to_string();
        tab.search_match_count = Some(out.commits.len());
        tab.search_results = Some(out.commits);
        tab.recompute_visible_rows();

        // 搜索态命中了 hashes[1] 与 hashes[0]；当搜索结果携带真实 parents 时应保留父子连线，并截断列表外悬空线
        assert_eq!(tab.visible_commit_count(), 2);
        assert!(tab.find_commit(&hashes[0]).is_some());
        assert!(tab.visible_index_of(&hashes[0]).is_some());
        assert!(tab.should_truncate_missing_parents());

        // 验证预过滤或带真实父子关系的筛选态：保留可见父子连线且截断指向列表外提交的悬空线
        tab.search_results = Some(
            engine
                .graph(&GraphOptions {
                    max_commits: 4,
                    ..Default::default()
                })
                .unwrap()
                .commits
                .into_iter()
                .filter(|c| c.hash == hashes[2] || c.hash == hashes[1])
                .collect(),
        );
        tab.recompute_visible_rows();
        assert_eq!(tab.layout.rows.len(), 2);
        // hashes[2] -> hashes[1] 连线应完好保留，hashes[1] -> hashes[0]（不在可见列表）应被截断
        assert!(
            tab.layout.rows[0]
                .segments
                .iter()
                .any(|s| s.kind == git_graph_layout::SegmentKind::NodeToBottom)
        );
        assert!(
            tab.layout.rows[1]
                .segments
                .iter()
                .any(|s| s.kind == git_graph_layout::SegmentKind::TopToNode)
        );
        assert!(
            !tab.layout.rows[1]
                .segments
                .iter()
                .any(|s| s.kind == git_graph_layout::SegmentKind::NodeToBottom)
        );

        // 清空搜索恢复常规分页图谱
        tab.filter.search_query.clear();
        tab.clear_search_state();
        assert_eq!(tab.visible_commit_count(), 2);
        assert!(tab.find_commit(&hashes[0]).is_none());

        // 分支/作者筛选或 !more_available 均会开启 truncate_missing_parents
        tab.more_available = true;
        assert!(!tab.should_truncate_missing_parents());
        tab.filter.branch = Some("main".to_string());
        assert!(tab.should_truncate_missing_parents());
        tab.filter.branch = None;
        tab.filter.author = Some("Alice".to_string());
        assert!(tab.should_truncate_missing_parents());
    }
}
