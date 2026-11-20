//! 纯逻辑状态、配置持久化、时间格式化与过滤辅助函数。

use std::path::{Path, PathBuf};

use git_graph_core::api::GraphOptions;
use git_graph_core::types::{CommitOrdering, GitCommit, UNCOMMITTED};
use git_graph_layout::{
    CommitNode, GraphLayout, LayoutOptions, RefKind, RefLabel, compute_layout_with_options,
};
use gpui::ImageFormat;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

/// 默认最近仓库保留上限。
const MAX_RECENT_REPOS: usize = 12;

/// 默认全局 UI 缩放比例（相比标准 16px rem 基准放大 1.20x，降低信息密度）。
pub const DEFAULT_UI_SCALE: f32 = 1.20;
/// 允许的最小 / 最大 UI 缩放比例。
pub const MIN_UI_SCALE: f32 = 0.90;
pub const MAX_UI_SCALE: f32 = 1.60;

fn default_ui_scale() -> f32 {
    DEFAULT_UI_SCALE
}

/// 5 列宽度持久化配置（像素）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColumnWidths {
    pub graph: f32,
    pub graph_manual: bool,
    pub date: f32,
    pub author: f32,
    pub commit: f32,
}

impl Default for ColumnWidths {
    fn default() -> Self {
        Self {
            graph: 160.0,
            graph_manual: false,
            date: 184.0,
            author: 168.0,
            commit: 108.0,
        }
    }
}

/// 持久化到本地应用目录的状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedState {
    pub recent_repos: Vec<PathBuf>,
    pub dark_mode: bool,
    pub column_widths: ColumnWidths,
    pub details_open: bool,
    pub vertical_split: [f32; 2],
    pub horizontal_split: [f32; 2],
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f32,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            recent_repos: Vec::new(),
            dark_mode: false,
            column_widths: ColumnWidths::default(),
            details_open: true,
            vertical_split: [0.58, 0.42],
            horizontal_split: [0.36, 0.64],
            ui_scale: DEFAULT_UI_SCALE,
        }
    }
}

impl PersistedState {
    /// 将仓库路径加入最近打开列表头部（去重并限制数量）。
    pub fn record_recent(&mut self, path: &Path) {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.recent_repos.retain(|existing| existing != &canonical);
        self.recent_repos.insert(0, canonical);
        self.recent_repos.truncate(MAX_RECENT_REPOS);
    }

    /// 从最近列表移除失效或手动删除的仓库。
    pub fn remove_recent(&mut self, path: &Path) {
        self.recent_repos.retain(|existing| existing != path);
    }
}

/// 获取应用持久化配置文件路径。
pub fn state_file_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "git-graph-rs", "git-graph-app")
        .map(|dirs| dirs.config_dir().join("state.json"))
}

/// 从默认路径加载持久化状态（不存在或损坏时回退默认值）。
pub fn load_persisted_state() -> PersistedState {
    match state_file_path() {
        Some(path) => load_state_from(&path).unwrap_or_default(),
        None => PersistedState::default(),
    }
}

/// 保存持久化状态到默认路径。
pub fn save_persisted_state(state: &PersistedState) {
    if let Some(path) = state_file_path()
        && let Err(err) = save_state_to(&path, state)
    {
        log::warn!("failed to save state to {}: {err}", path.display());
    }
}

/// 从指定路径读取持久化 JSON。
pub fn load_state_from(path: &Path) -> anyhow::Result<PersistedState> {
    let raw = std::fs::read_to_string(path)?;
    let mut state: PersistedState = serde_json::from_str(&raw)?;
    state.ui_scale = if state.ui_scale.is_finite() {
        state.ui_scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE)
    } else {
        DEFAULT_UI_SCALE
    };
    // 兼容旧版紧凑默认列宽，升级到放大后的舒适默认值
    if (state.column_widths.date - 148.0).abs() < 0.5
        && (state.column_widths.author - 136.0).abs() < 0.5
        && (state.column_widths.commit - 86.0).abs() < 0.5
    {
        state.column_widths = ColumnWidths::default();
    }
    Ok(state)
}

/// 将持久化状态写入指定路径（自动创建父目录）。
pub fn save_state_to(path: &Path, state: &PersistedState) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// 图谱顶部筛选栏状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterCriteria {
    /// 选中分支名，`None` 表示全部分支。
    pub branch: Option<String>,
    /// 选中作者名，`None` 表示全部作者。
    pub author: Option<String>,
    /// 是否显示远程分支。
    pub show_remote_branches: bool,
    /// 搜索框关键词。
    pub search_query: String,
}

impl Default for FilterCriteria {
    fn default() -> Self {
        Self {
            branch: None,
            author: None,
            show_remote_branches: true,
            search_query: String::new(),
        }
    }
}

impl FilterCriteria {
    /// 转换为 `git_graph_core::api::GraphOptions`。
    pub fn to_graph_options(&self, max_commits: u32) -> GraphOptions {
        GraphOptions {
            branches: self.branch.as_ref().map(|b| vec![b.clone()]),
            authors: self.author.as_ref().map(|a| vec![a.clone()]),
            max_commits,
            show_tags: true,
            show_remote_branches: self.show_remote_branches,
            first_parent_only: false,
            ordering: CommitOrdering::Date,
            hide_remotes: Vec::new(),
            paths: Vec::new(),
        }
    }
}

/// 根据搜索词过滤提交并返回匹配的原始切片索引（支持提交信息、作者名、邮箱、Hash 前缀及分支/Tag 名，大小写不敏感）。
///
/// 空查询返回全部索引；未提交更改行 `*` 仅在无关键词或关键词匹配 `"uncommitted"` / `"未提交"` 时保留。
pub fn filter_commit_indices(commits: &[GitCommit], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return (0..commits.len()).collect();
    }
    commits
        .iter()
        .enumerate()
        .filter_map(|(idx, commit)| matches_commit_query(commit, &needle).then_some(idx))
        .collect()
}

/// 判断单条提交是否匹配小写关键词 `needle`。
pub fn matches_commit_query(commit: &GitCommit, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if commit.hash == UNCOMMITTED {
        return "uncommitted changes".contains(needle_lower) || "未提交".contains(needle_lower);
    }
    commit.hash.to_lowercase().contains(needle_lower)
        || commit.message.to_lowercase().contains(needle_lower)
        || commit.author.to_lowercase().contains(needle_lower)
        || commit.email.to_lowercase().contains(needle_lower)
        || commit
            .heads
            .iter()
            .any(|h| h.to_lowercase().contains(needle_lower))
        || commit
            .tags
            .iter()
            .any(|t| t.name.to_lowercase().contains(needle_lower))
        || commit
            .remotes
            .iter()
            .any(|r| r.name.to_lowercase().contains(needle_lower))
}

/// 判断提交是否符合顶部「作者」筛选条件（大小写不敏感精确匹配作者名）。
pub fn matches_author_filter(commit: &GitCommit, author_filter: Option<&str>) -> bool {
    let Some(author) = author_filter else {
        return true;
    };
    if commit.hash == UNCOMMITTED {
        return false;
    }
    commit.author.to_lowercase() == author.to_lowercase()
}

/// 计算提交图谱布局；当 `truncate_missing_parents` 为 `true` 时截断指向可见列表之外的悬空线，
/// 仅保留当前可见提交之间的真实父子连线。
pub fn compute_graph_layout(
    commits: &[CommitNode],
    head_hash: Option<&str>,
    truncate_missing_parents: bool,
) -> GraphLayout {
    compute_layout_with_options(
        commits,
        head_hash,
        LayoutOptions {
            truncate_missing_parents,
        },
    )
}

/// 将 `GitCommit` 转换为图谱布局所需的 `CommitNode`。
pub fn commit_to_node(commit: &GitCommit) -> CommitNode {
    CommitNode {
        hash: commit.hash.clone(),
        parents: commit.parents.clone(),
        is_stash: commit.stash.is_some(),
        is_uncommitted: commit.hash == UNCOMMITTED,
    }
}

/// 将 `GitCommit` 上的分支/远程/Tag/Stash 转为 `git_graph_layout::RefLabel` 列表。
pub fn commit_ref_labels(commit: &GitCommit) -> Vec<RefLabel> {
    let mut labels = Vec::with_capacity(
        commit.heads.len()
            + commit.remotes.len()
            + commit.tags.len()
            + usize::from(commit.stash.is_some()),
    );
    for head in &commit.heads {
        labels.push(RefLabel {
            name: head.clone(),
            kind: RefKind::LocalBranch,
        });
    }
    for remote in &commit.remotes {
        labels.push(RefLabel {
            name: remote.name.clone(),
            kind: RefKind::RemoteBranch {
                remote: remote.remote.clone().unwrap_or_default(),
            },
        });
    }
    for tag in &commit.tags {
        labels.push(RefLabel {
            name: tag.name.clone(),
            kind: RefKind::Tag {
                annotated: tag.annotated,
            },
        });
    }
    if let Some(stash) = &commit.stash {
        labels.push(RefLabel {
            name: stash.selector.clone(),
            kind: RefKind::Stash,
        });
    }
    labels
}

/// 用系统本地时区格式化 Unix 时间戳（`YYYY-MM-DD HH:MM`）。
pub fn format_timestamp(epoch_secs: i64) -> String {
    format_timestamp_in_tz(epoch_secs, TimeZone::system(), "%Y-%m-%d %H:%M")
}

/// 用系统本地时区格式化详细时间（`YYYY-MM-DD HH:MM:SS`）。
pub fn format_timestamp_detailed(epoch_secs: i64) -> String {
    format_timestamp_in_tz(epoch_secs, TimeZone::system(), "%Y-%m-%d %H:%M:%S")
}

/// 按指定时区与格式串格式化 Unix 秒级时间戳。
pub fn format_timestamp_in_tz(epoch_secs: i64, tz: TimeZone, fmt: &str) -> String {
    match Timestamp::from_second(epoch_secs) {
        Ok(ts) => ts.to_zoned_datetime(tz).strftime(fmt).to_string(),
        Err(_) => "—".to_string(),
    }
}

/// 根据文件扩展名判断是否为 GPUI 支持预览的图片格式。
pub fn image_format_for_path(path: &str) -> Option<ImageFormat> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::Webp),
        "bmp" => Some(ImageFormat::Bmp),
        "tif" | "tiff" => Some(ImageFormat::Tiff),
        "ico" => Some(ImageFormat::Ico),
        _ => None,
    }
}

/// 截断展示短 Hash（默认 7 位）。
pub fn short_hash(hash: &str) -> &str {
    let len = hash.len().min(7);
    &hash[..len]
}

/// 提取路径显示名（最后一级目录名或完整路径）。
pub fn repo_display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_graph_core::types::{GitCommitRemote, GitCommitTag};

    fn make_commit(hash: &str, author: &str, message: &str, heads: Vec<&str>) -> GitCommit {
        GitCommit {
            hash: hash.to_string(),
            parents: vec![],
            author: author.to_string(),
            email: format!("{}@example.com", author.to_lowercase()),
            date: 1_700_000_000,
            message: message.to_string(),
            heads: heads.into_iter().map(str::to_string).collect(),
            tags: vec![],
            remotes: vec![],
            stash: None,
        }
    }

    #[test]
    fn persisted_state_roundtrip_and_recent_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested/state.json");

        let repo_a = dir.path().join("repo-a");
        let repo_b = dir.path().join("repo-b");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();

        let mut state = PersistedState {
            dark_mode: true,
            ..Default::default()
        };
        state.record_recent(&repo_a);
        state.record_recent(&repo_b);
        state.record_recent(&repo_a);
        assert_eq!(state.recent_repos.len(), 2);
        assert_eq!(
            state.recent_repos[0],
            std::fs::canonicalize(&repo_a).unwrap()
        );

        save_state_to(&file, &state).unwrap();
        let loaded = load_state_from(&file).unwrap();
        assert_eq!(loaded, state);
    }

    #[test]
    fn timestamp_formatting_is_deterministic_in_utc() {
        // 2023-11-14 22:13:20 UTC
        let formatted = format_timestamp_in_tz(1_700_000_000, TimeZone::UTC, "%Y-%m-%d %H:%M");
        assert_eq!(formatted, "2023-11-14 22:13");

        let detailed = format_timestamp_in_tz(1_700_000_000, TimeZone::UTC, "%Y-%m-%d %H:%M:%S");
        assert_eq!(detailed, "2023-11-14 22:13:20");
    }

    #[test]
    fn filter_criteria_and_commit_search_match_expected_fields() {
        let criteria = FilterCriteria {
            branch: Some("main".to_string()),
            author: Some("Alice".to_string()),
            show_remote_branches: false,
            search_query: "fix".to_string(),
        };
        let opts = criteria.to_graph_options(600);
        assert_eq!(opts.branches.as_deref(), Some(&["main".to_string()][..]));
        assert_eq!(opts.authors.as_deref(), Some(&["Alice".to_string()][..]));
        assert!(!opts.show_remote_branches);
        assert_eq!(opts.max_commits, 600);

        let mut c1 = make_commit("abcdef123456", "Alice", "Fix graph rendering", vec!["main"]);
        c1.tags.push(GitCommitTag {
            name: "v1.0.0".to_string(),
            annotated: true,
        });
        let mut c2 = make_commit("999999888888", "Bob", "Add initial README", vec!["feat/ui"]);
        c2.remotes.push(GitCommitRemote {
            name: "origin/feat/ui".to_string(),
            remote: Some("origin".to_string()),
        });
        let uncommitted = make_commit(UNCOMMITTED, "*", "Uncommitted Changes (2)", vec![]);
        let list = [uncommitted, c1, c2];

        assert_eq!(filter_commit_indices(&list, "").len(), 3);
        assert_eq!(filter_commit_indices(&list, "GRAPH"), vec![1]);
        assert_eq!(filter_commit_indices(&list, "bob"), vec![2]);
        assert_eq!(filter_commit_indices(&list, "abcdef1"), vec![1]);
        assert_eq!(filter_commit_indices(&list, "v1.0.0"), vec![1]);
        assert_eq!(filter_commit_indices(&list, "origin/feat"), vec![2]);
    }

    #[test]
    fn commit_ref_labels_maps_all_ref_kinds() {
        let mut commit = make_commit("1234567", "Ana", "Release", vec!["main"]);
        commit.remotes.push(GitCommitRemote {
            name: "origin/main".to_string(),
            remote: Some("origin".to_string()),
        });
        commit.tags.push(GitCommitTag {
            name: "v0.1.0".to_string(),
            annotated: true,
        });
        let labels = commit_ref_labels(&commit);
        assert_eq!(labels.len(), 3);
        assert!(matches!(labels[0].kind, RefKind::LocalBranch));
        assert!(matches!(labels[1].kind, RefKind::RemoteBranch { .. }));
        assert!(matches!(labels[2].kind, RefKind::Tag { annotated: true }));
    }

    #[test]
    fn image_format_detection() {
        assert_eq!(image_format_for_path("assets/logo.PNG"), Some(ImageFormat::Png));
        assert_eq!(image_format_for_path("a/b/photo.jpeg"), Some(ImageFormat::Jpeg));
        assert_eq!(image_format_for_path("src/main.rs"), None);
    }

    #[test]
    fn filtered_layout_truncates_missing_parents_and_keeps_visible_links() {
        use git_graph_layout::{NodeKind, SegmentKind};

        // 构造两条存在父子关系的可见提交（child -> parent），其中 parent 另有一个列表外父提交 missing_p，
        // 以及一条指向列表外提交 missing_other 的 merge 父提交。
        let mut child = make_commit("aaaa1111", "Alice", "Head commit", vec!["main"]);
        child.parents = vec!["bbbb2222".to_string(), "missing_other".to_string()];
        let mut parent = make_commit("bbbb2222", "Bob", "Parent commit", vec![]);
        parent.parents = vec!["missing_p1".to_string(), "missing_p2".to_string()];
        let uncommitted = make_commit(UNCOMMITTED, "*", "Uncommitted", vec![]);

        let nodes = vec![
            commit_to_node(&uncommitted),
            commit_to_node(&child),
            commit_to_node(&parent),
        ];
        let layout = compute_graph_layout(&nodes, Some("aaaa1111"), true);
        assert_eq!(layout.rows.len(), 3);
        assert_eq!(layout.rows[0].node_kind, NodeKind::Uncommitted);
        assert_eq!(layout.rows[1].node_kind, NodeKind::Head);
        assert_eq!(layout.rows[2].node_kind, NodeKind::Merge);

        // child 仅保留连向可见 parent (bbbb2222) 的向下连线，指向 missing_other 的悬空线已被截断
        let child_out: Vec<_> = layout.rows[1]
            .segments
            .iter()
            .filter(|s| s.kind == SegmentKind::NodeToBottom)
            .collect();
        assert_eq!(child_out.len(), 1);

        // parent 保留来自 child 的 TopToNode 连线，且其指向 missing_p 的悬空线已被截断
        assert!(
            layout.rows[2]
                .segments
                .iter()
                .any(|s| s.kind == SegmentKind::TopToNode)
        );
        assert!(
            !layout.rows[2]
                .segments
                .iter()
                .any(|s| s.kind == SegmentKind::NodeToBottom)
        );

        assert!(matches_author_filter(&child, None));
        assert!(matches_author_filter(&child, Some("alice")));
        assert!(!matches_author_filter(&child, Some("Bob")));
        assert!(!matches_author_filter(&uncommitted, Some("Alice")));
    }

    #[test]
    fn column_widths_legacy_json_defaults_graph_manual_to_false() {
        let legacy = r#"{"graph":150.0,"date":140.0,"author":130.0,"commit":80.0}"#;
        let parsed: ColumnWidths = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.graph, 150.0);
        assert!(!parsed.graph_manual);
    }
}
