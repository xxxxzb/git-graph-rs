//! 单个打开的本地仓库 Tab 状态模型及后台异步加载任务。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use ely_git::Stretch;
use git_graph_core::api::{Engine, INDEX, WORKING_TREE};
use git_graph_core::types::{
    GitAuthor, GitCommit, GitCommitDetails, GitCommitStash, GitFileChange, GitLineCounts,
    GitRepoInfo, UNCOMMITTED,
};
use git_graph_core::{details, diff, status};
use git_graph_layout::{GraphLayout, compute_layout};
use git_graph_ops::RepoWatcher;
use gpui::{Image, Task, UniformListScrollHandle};

use crate::diff_parser::{ParsedDiff, parse_unified_diff, stretches_from_texts};
use crate::state::{
    FilterCriteria, commit_to_node, filter_commit_indices, image_format_for_path, repo_display_name,
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
    pub fetching: bool,
    pub error: Option<String>,

    pub graph_scroll: UniformListScrollHandle,
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
            fetching: false,
            error: None,
            graph_scroll: UniformListScrollHandle::new(),
            _watcher: None,
            _watch_task: None,
        }
    }

    /// 应用搜索关键词过滤并重算可见行的泳道布局。
    pub fn recompute_visible_rows(&mut self) {
        let indices = filter_commit_indices(&self.all_commits, &self.filter.search_query);
        let nodes: Vec<_> = indices
            .iter()
            .filter_map(|&idx| self.all_commits.get(idx).map(commit_to_node))
            .collect();
        self.filtered_indices = indices;
        self.layout = compute_layout(&nodes, self.head_hash.as_deref());
    }

    /// 查找指定 hash 在当前可见行中的索引。
    pub fn visible_index_of(&self, hash: &str) -> Option<usize> {
        self.filtered_indices
            .iter()
            .position(|&idx| self.all_commits.get(idx).is_some_and(|c| c.hash == hash))
    }

    /// 通过 hash 查找已加载的 `GitCommit`。
    pub fn find_commit(&self, hash: &str) -> Option<&GitCommit> {
        self.all_commits.iter().find(|c| c.hash == hash)
    }

    /// 当前选中的提交在 `all_commits` 中是否已携带 stash 元信息。
    pub fn selected_stash(&self) -> Option<GitCommitStash> {
        let hash = self.selected_hash.as_deref()?;
        self.find_commit(hash).and_then(|c| c.stash.clone())
    }
}

/// 在后台线程加载仓库图谱、分支/作者信息与工作区状态。
pub fn load_repo_snapshot(
    engine: &Engine,
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

    Ok(RepoSnapshot {
        info,
        authors,
        commits: data.commits,
        head_hash: data.head,
        more_available: data.more_commits_available,
        scm_changes,
        uncommitted_count,
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
