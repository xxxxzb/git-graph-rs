//! 模态确认/输入对话框与右键上下文菜单构建。

use std::collections::BTreeMap;
use std::path::PathBuf;

use ely_core::buttons::{Button, ButtonVariant, SegmentedControl};
use ely_core::feedback::Callout;
use ely_core::forms::{Checkbox, Choice, Input, Select, TextInput};
use ely_core::menus::{Menu, MenuItem};
use ely_core::overlays::{ConfirmDialog, Dialog, PromptDialog};
use ely_core::primitives::{IconName, Severity};
use ely_core::theme::{ActiveTheme, ControlSize, TextSize};
use git_graph_core::types::GitCommit;
use git_graph_ops::{PushForceMode, PushOptions, ResetMode, SafetyWarning};
use gpui::{
    AnyElement, App, Entity, IntoElement, ParentElement, Pixels, Point, SharedString, Styled,
    Window, div,
};

use crate::app_view::GitGraphApp;
use crate::state::short_hash;

/// 待执行的 Git 写操作（用于二次确认 / 安全警告确认后派发）。
#[derive(Debug, Clone)]
pub enum PendingWriteOp {
    CheckoutBranch {
        repo_path: PathBuf,
        branch: String,
    },
    CheckoutCommit {
        repo_path: PathBuf,
        hash: String,
    },
    DeleteBranch {
        repo_path: PathBuf,
        name: String,
        force: bool,
    },
    DeleteTag {
        repo_path: PathBuf,
        name: String,
    },
    CherryPick {
        repo_path: PathBuf,
        hash: String,
    },
    RevertCommit {
        repo_path: PathBuf,
        hash: String,
    },
    ResetToCommit {
        repo_path: PathBuf,
        hash: String,
        mode: ResetMode,
    },
    StashPop {
        repo_path: PathBuf,
        index: usize,
    },
    StashDrop {
        repo_path: PathBuf,
        index: usize,
    },
    DiscardFile {
        repo_path: PathBuf,
        file_path: String,
    },
}

/// 当前打开的模态对话框状态。
pub enum ActiveModal {
    CreateBranch {
        repo_path: PathBuf,
        start_point: String,
        input: Entity<TextInput>,
        checkout: bool,
    },
    RenameBranch {
        repo_path: PathBuf,
        old_name: String,
        input: Entity<TextInput>,
    },
    CreateTag {
        repo_path: PathBuf,
        target: String,
        name_input: Entity<TextInput>,
        message_input: Entity<TextInput>,
    },
    StashPush {
        repo_path: PathBuf,
        message_input: Entity<TextInput>,
        include_untracked: bool,
    },
    PushBranch {
        repo_path: PathBuf,
        local_branches: Vec<String>,
        selected_branch: String,
        upstreams: BTreeMap<String, Option<String>>,
        remotes: Vec<String>,
        selected_remote: String,
        remote_branch_input: Entity<TextInput>,
        set_upstream: bool,
        push_tags: bool,
        force: PushForceMode,
    },
    SafetyConfirm {
        warning: SafetyWarning,
        confirm_label: SharedString,
        pending: PendingWriteOp,
    },
    SimpleConfirm {
        title: SharedString,
        message: SharedString,
        confirm_label: SharedString,
        destructive: bool,
        pending: PendingWriteOp,
    },
}

/// 推送分支弹窗的默认表单参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushDefaults {
    pub remote: String,
    pub remote_branch: String,
    pub set_upstream: bool,
}

/// 根据目标分支的 upstream 与仓库 remotes 列表计算推送弹窗默认参数：
/// - 默认远端优先取该分支 upstream 的 remote，无则优先 `origin`，再否则取首个 remote
/// - 远程分支名默认与本地分支同名
/// - 无 upstream 时自动默认勾选 `-u`（设置上游追踪）
pub fn compute_push_defaults(
    local_branch: &str,
    upstream: Option<&str>,
    remotes: &[String],
) -> PushDefaults {
    let fallback_remote = || {
        remotes
            .iter()
            .find(|r| r.as_str() == "origin")
            .cloned()
            .or_else(|| remotes.first().cloned())
            .unwrap_or_default()
    };

    let upstream_remote = upstream.and_then(|up| {
        // 优先最长前缀匹配已知 remote，兼容名称中包含 `/` 的 remote
        remotes
            .iter()
            .filter(|r| up.starts_with(r.as_str()) && up[r.len()..].starts_with('/'))
            .max_by_key(|r| r.len())
            .cloned()
            .or_else(|| {
                up.split_once('/')
                    .map(|(r, _)| r.to_string())
                    .filter(|r| remotes.is_empty() || remotes.iter().any(|known| known == r))
            })
    });

    let remote = upstream_remote.unwrap_or_else(fallback_remote);
    PushDefaults {
        remote,
        remote_branch: local_branch.to_string(),
        set_upstream: upstream.is_none(),
    }
}

/// 右键上下文菜单的目标对象（按点击对象严格隔离上下文，零重复、零串台）。
#[derive(Debug, Clone)]
pub enum ContextMenuTarget {
    LocalBranch {
        repo_path: PathBuf,
        branch: String,
        is_head: bool,
    },
    RemoteBranch {
        repo_path: PathBuf,
        remote: String,
        branch_name: String,
        full_name: String,
        is_remote_head: bool,
    },
    Tag {
        repo_path: PathBuf,
        name: String,
    },
    Stash {
        repo_path: PathBuf,
        selector: String,
        index: usize,
        hash: String,
    },
    Commit {
        repo_path: PathBuf,
        commit: GitCommit,
    },
    Uncommitted {
        repo_path: PathBuf,
    },
}

/// 右键提交或 Ref 胶囊后激活的上下文菜单请求。
#[derive(Debug, Clone)]
pub struct ContextMenuState {
    pub request_id: u64,
    pub position: Point<Pixels>,
    pub target: ContextMenuTarget,
}

/// 纯数据层的右键菜单动作（便于单元测试验证与 UI 绑定共用同一数据源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuAction {
    CheckoutBranch {
        repo_path: PathBuf,
        branch: String,
    },
    PushBranch {
        repo_path: PathBuf,
        branch: String,
    },
    RenameBranch {
        repo_path: PathBuf,
        old_name: String,
    },
    DeleteBranch {
        repo_path: PathBuf,
        branch: String,
    },
    CheckoutRemoteBranch {
        repo_path: PathBuf,
        full_name: String,
        local_name: String,
    },
    DeleteTag {
        repo_path: PathBuf,
        name: String,
    },
    StashApply {
        repo_path: PathBuf,
        index: usize,
    },
    StashPop {
        repo_path: PathBuf,
        index: usize,
    },
    StashDrop {
        repo_path: PathBuf,
        index: usize,
    },
    CheckoutCommit {
        repo_path: PathBuf,
        hash: String,
    },
    CreateBranchAt {
        repo_path: PathBuf,
        start_point: String,
    },
    CreateTagAt {
        repo_path: PathBuf,
        target: String,
    },
    CherryPick {
        repo_path: PathBuf,
        hash: String,
    },
    RevertCommit {
        repo_path: PathBuf,
        hash: String,
    },
    ResetToCommit {
        repo_path: PathBuf,
        hash: String,
        mode: ResetMode,
    },
    StashPush {
        repo_path: PathBuf,
    },
    RefreshWorkspace {
        repo_path: PathBuf,
    },
    CopyText {
        text: String,
        toast_label: String,
    },
}

/// 纯数据层的菜单条目。
#[derive(Debug, Clone, PartialEq)]
pub enum ContextMenuEntry {
    Item {
        label: String,
        icon: Option<IconName>,
        action: ContextMenuAction,
    },
    Submenu {
        label: String,
        icon: Option<IconName>,
        items: Vec<ContextMenuEntry>,
    },
    Separator,
}

/// 纯数据层的右键菜单规格（含对象小标题与菜单条目序列）。
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMenuSpec {
    pub heading: String,
    pub entries: Vec<ContextMenuEntry>,
}

/// 按字符边界截断过长的菜单头标题，避免超长分支名撑宽右键菜单。
fn truncate_heading(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

/// 根据右键目标生成纯数据结构的上下文菜单规格。
pub fn build_context_menu_spec(target: &ContextMenuTarget) -> ContextMenuSpec {
    match target {
        ContextMenuTarget::LocalBranch {
            repo_path,
            branch,
            is_head,
        } => {
            let mut entries = Vec::new();
            if !*is_head {
                entries.push(ContextMenuEntry::Item {
                    label: format!("检出分支 {branch}"),
                    icon: Some(IconName::GitBranch),
                    action: ContextMenuAction::CheckoutBranch {
                        repo_path: repo_path.clone(),
                        branch: branch.clone(),
                    },
                });
            }
            entries.push(ContextMenuEntry::Item {
                label: format!("推送分支 \"{branch}\" 到远程…"),
                icon: Some(IconName::CloudUpload),
                action: ContextMenuAction::PushBranch {
                    repo_path: repo_path.clone(),
                    branch: branch.clone(),
                },
            });
            entries.push(ContextMenuEntry::Item {
                label: "重命名分支…".to_string(),
                icon: Some(IconName::Pencil),
                action: ContextMenuAction::RenameBranch {
                    repo_path: repo_path.clone(),
                    old_name: branch.clone(),
                },
            });
            if !*is_head {
                entries.push(ContextMenuEntry::Item {
                    label: format!("删除分支 {branch}…"),
                    icon: Some(IconName::Trash2),
                    action: ContextMenuAction::DeleteBranch {
                        repo_path: repo_path.clone(),
                        branch: branch.clone(),
                    },
                });
            }
            entries.push(ContextMenuEntry::Separator);
            entries.push(ContextMenuEntry::Item {
                label: "复制分支名".to_string(),
                icon: Some(IconName::Copy),
                action: ContextMenuAction::CopyText {
                    text: branch.clone(),
                    toast_label: "已复制分支名".to_string(),
                },
            });
            ContextMenuSpec {
                heading: truncate_heading(&format!("分支 {branch}"), 36),
                entries,
            }
        }
        ContextMenuTarget::RemoteBranch {
            repo_path,
            remote,
            branch_name,
            full_name,
            is_remote_head,
        } => {
            let display_full = if full_name.is_empty() && !remote.is_empty() {
                format!("{remote}/{branch_name}")
            } else {
                full_name.clone()
            };
            let mut entries = Vec::new();
            if !*is_remote_head {
                entries.push(ContextMenuEntry::Item {
                    label: "检出为本地分支…".to_string(),
                    icon: Some(IconName::GitBranch),
                    action: ContextMenuAction::CheckoutRemoteBranch {
                        repo_path: repo_path.clone(),
                        full_name: display_full.clone(),
                        local_name: branch_name.clone(),
                    },
                });
            }
            entries.push(ContextMenuEntry::Item {
                label: "复制远程分支名".to_string(),
                icon: Some(IconName::Copy),
                action: ContextMenuAction::CopyText {
                    text: display_full.clone(),
                    toast_label: "已复制远程分支名".to_string(),
                },
            });
            ContextMenuSpec {
                heading: truncate_heading(&format!("远程分支 {display_full}"), 36),
                entries,
            }
        }
        ContextMenuTarget::Tag { repo_path, name } => ContextMenuSpec {
            heading: truncate_heading(&format!("标签 {name}"), 36),
            entries: vec![
                ContextMenuEntry::Item {
                    label: format!("删除标签 {name}…"),
                    icon: Some(IconName::Trash2),
                    action: ContextMenuAction::DeleteTag {
                        repo_path: repo_path.clone(),
                        name: name.clone(),
                    },
                },
                ContextMenuEntry::Item {
                    label: "复制标签名".to_string(),
                    icon: Some(IconName::Copy),
                    action: ContextMenuAction::CopyText {
                        text: name.clone(),
                        toast_label: "已复制标签名".to_string(),
                    },
                },
            ],
        },
        ContextMenuTarget::Stash {
            repo_path,
            selector,
            index,
            hash,
        } => ContextMenuSpec {
            heading: truncate_heading(&format!("Stash {selector}"), 36),
            entries: vec![
                ContextMenuEntry::Item {
                    label: format!("应用 Stash ({selector})"),
                    icon: Some(IconName::Check),
                    action: ContextMenuAction::StashApply {
                        repo_path: repo_path.clone(),
                        index: *index,
                    },
                },
                ContextMenuEntry::Item {
                    label: format!("弹出 Stash ({selector})"),
                    icon: Some(IconName::Upload),
                    action: ContextMenuAction::StashPop {
                        repo_path: repo_path.clone(),
                        index: *index,
                    },
                },
                ContextMenuEntry::Item {
                    label: format!("删除 Stash ({selector})…"),
                    icon: Some(IconName::Trash2),
                    action: ContextMenuAction::StashDrop {
                        repo_path: repo_path.clone(),
                        index: *index,
                    },
                },
                ContextMenuEntry::Separator,
                ContextMenuEntry::Item {
                    label: "复制 Stash 标识".to_string(),
                    icon: Some(IconName::Copy),
                    action: ContextMenuAction::CopyText {
                        text: selector.clone(),
                        toast_label: "已复制 Stash 标识".to_string(),
                    },
                },
                ContextMenuEntry::Item {
                    label: "复制完整 Hash".to_string(),
                    icon: Some(IconName::Copy),
                    action: ContextMenuAction::CopyText {
                        text: hash.clone(),
                        toast_label: "已复制完整 Hash".to_string(),
                    },
                },
            ],
        },
        ContextMenuTarget::Commit { repo_path, commit } => {
            let short = short_hash(&commit.hash).to_string();
            ContextMenuSpec {
                heading: format!("提交 {short}"),
                entries: vec![
                    ContextMenuEntry::Item {
                        label: format!("检出此提交 ({short})"),
                        icon: Some(IconName::GitCommitHorizontal),
                        action: ContextMenuAction::CheckoutCommit {
                            repo_path: repo_path.clone(),
                            hash: commit.hash.clone(),
                        },
                    },
                    ContextMenuEntry::Item {
                        label: "在此创建分支…".to_string(),
                        icon: Some(IconName::GitBranch),
                        action: ContextMenuAction::CreateBranchAt {
                            repo_path: repo_path.clone(),
                            start_point: commit.hash.clone(),
                        },
                    },
                    ContextMenuEntry::Item {
                        label: "在此创建标签…".to_string(),
                        icon: Some(IconName::Tag),
                        action: ContextMenuAction::CreateTagAt {
                            repo_path: repo_path.clone(),
                            target: commit.hash.clone(),
                        },
                    },
                    ContextMenuEntry::Separator,
                    ContextMenuEntry::Item {
                        label: "Cherry-pick 此提交".to_string(),
                        icon: Some(IconName::GitMerge),
                        action: ContextMenuAction::CherryPick {
                            repo_path: repo_path.clone(),
                            hash: commit.hash.clone(),
                        },
                    },
                    ContextMenuEntry::Item {
                        label: "Revert 此提交".to_string(),
                        icon: Some(IconName::Undo2),
                        action: ContextMenuAction::RevertCommit {
                            repo_path: repo_path.clone(),
                            hash: commit.hash.clone(),
                        },
                    },
                    ContextMenuEntry::Submenu {
                        label: "Reset 当前分支到此提交".to_string(),
                        icon: None,
                        items: vec![
                            ContextMenuEntry::Item {
                                label: "Soft（保留暂存与工作区）".to_string(),
                                icon: None,
                                action: ContextMenuAction::ResetToCommit {
                                    repo_path: repo_path.clone(),
                                    hash: commit.hash.clone(),
                                    mode: ResetMode::Soft,
                                },
                            },
                            ContextMenuEntry::Item {
                                label: "Mixed（保留工作区）".to_string(),
                                icon: None,
                                action: ContextMenuAction::ResetToCommit {
                                    repo_path: repo_path.clone(),
                                    hash: commit.hash.clone(),
                                    mode: ResetMode::Mixed,
                                },
                            },
                            ContextMenuEntry::Item {
                                label: "Hard（丢弃全部未提交修改）".to_string(),
                                icon: None,
                                action: ContextMenuAction::ResetToCommit {
                                    repo_path: repo_path.clone(),
                                    hash: commit.hash.clone(),
                                    mode: ResetMode::Hard,
                                },
                            },
                        ],
                    },
                    ContextMenuEntry::Separator,
                    ContextMenuEntry::Item {
                        label: "复制完整 Hash".to_string(),
                        icon: Some(IconName::Copy),
                        action: ContextMenuAction::CopyText {
                            text: commit.hash.clone(),
                            toast_label: "已复制完整 Hash".to_string(),
                        },
                    },
                    ContextMenuEntry::Item {
                        label: "复制短 Hash".to_string(),
                        icon: Some(IconName::Copy),
                        action: ContextMenuAction::CopyText {
                            text: short,
                            toast_label: "已复制短 Hash".to_string(),
                        },
                    },
                    ContextMenuEntry::Item {
                        label: "复制提交信息".to_string(),
                        icon: Some(IconName::Copy),
                        action: ContextMenuAction::CopyText {
                            text: commit.message.clone(),
                            toast_label: "已复制提交信息".to_string(),
                        },
                    },
                ],
            }
        }
        ContextMenuTarget::Uncommitted { repo_path } => ContextMenuSpec {
            heading: "未提交的更改".to_string(),
            entries: vec![
                ContextMenuEntry::Item {
                    label: "储藏更改 (Stash Push)…".to_string(),
                    icon: Some(IconName::Archive),
                    action: ContextMenuAction::StashPush {
                        repo_path: repo_path.clone(),
                    },
                },
                ContextMenuEntry::Item {
                    label: "刷新工作区状态".to_string(),
                    icon: Some(IconName::RefreshCw),
                    action: ContextMenuAction::RefreshWorkspace {
                        repo_path: repo_path.clone(),
                    },
                },
            ],
        },
    }
}

/// 将单条纯数据菜单条目转为 `ely_core::menus::MenuItem`。
fn entry_to_menu_item(
    entry: ContextMenuEntry,
    app_entity: &gpui::WeakEntity<GitGraphApp>,
) -> Option<MenuItem> {
    match entry {
        ContextMenuEntry::Separator => None,
        ContextMenuEntry::Item {
            label,
            icon,
            action,
        } => {
            let mut item = MenuItem::new(label);
            if let Some(icon) = icon {
                item = item.icon(icon);
            }
            let app = app_entity.clone();
            Some(item.on_click(move |window, cx| {
                dispatch_context_menu_action(&action, &app, window, cx);
            }))
        }
        ContextMenuEntry::Submenu { label, icon, items } => {
            let mut sub = Menu::new();
            for sub_entry in items {
                match sub_entry {
                    ContextMenuEntry::Separator => sub = sub.separator(),
                    other => {
                        if let Some(mi) = entry_to_menu_item(other, app_entity) {
                            sub = sub.item(mi);
                        }
                    }
                }
            }
            let mut item = MenuItem::submenu(label, sub);
            if let Some(icon) = icon {
                item = item.icon(icon);
            }
            Some(item)
        }
    }
}

/// 将菜单动作派发到 `GitGraphApp`。
fn dispatch_context_menu_action(
    action: &ContextMenuAction,
    app: &gpui::WeakEntity<GitGraphApp>,
    window: &mut Window,
    cx: &mut App,
) {
    match action {
        ContextMenuAction::CheckoutBranch { repo_path, branch } => {
            app.update(cx, |this, cx| {
                this.request_checkout_branch(repo_path.clone(), branch.clone(), cx);
            })
            .ok();
        }
        ContextMenuAction::PushBranch { repo_path, branch } => {
            app.update(cx, |this, cx| {
                this.open_push_branch_modal(repo_path.clone(), branch.clone(), window, cx);
            })
            .ok();
        }
        ContextMenuAction::RenameBranch {
            repo_path,
            old_name,
        } => {
            app.update(cx, |this, cx| {
                this.open_rename_branch_modal(repo_path.clone(), old_name.clone(), window, cx);
            })
            .ok();
        }
        ContextMenuAction::DeleteBranch { repo_path, branch } => {
            app.update(cx, |this, cx| {
                this.confirm_delete_branch(repo_path.clone(), branch.clone(), cx);
            })
            .ok();
        }
        ContextMenuAction::CheckoutRemoteBranch {
            repo_path,
            full_name,
            local_name,
        } => {
            app.update(cx, |this, cx| {
                this.open_create_branch_modal(
                    repo_path.clone(),
                    full_name.clone(),
                    Some(local_name.clone()),
                    window,
                    cx,
                );
            })
            .ok();
        }
        ContextMenuAction::DeleteTag { repo_path, name } => {
            app.update(cx, |this, cx| {
                this.confirm_delete_tag(repo_path.clone(), name.clone(), cx);
            })
            .ok();
        }
        ContextMenuAction::StashApply { repo_path, index } => {
            app.update(cx, |this, cx| {
                this.run_stash_apply(repo_path.clone(), *index, cx);
            })
            .ok();
        }
        ContextMenuAction::StashPop { repo_path, index } => {
            app.update(cx, |this, cx| {
                this.confirm_stash_pop(repo_path.clone(), *index, cx);
            })
            .ok();
        }
        ContextMenuAction::StashDrop { repo_path, index } => {
            app.update(cx, |this, cx| {
                this.confirm_stash_drop(repo_path.clone(), *index, cx);
            })
            .ok();
        }
        ContextMenuAction::CheckoutCommit { repo_path, hash } => {
            app.update(cx, |this, cx| {
                this.request_checkout_commit(repo_path.clone(), hash.clone(), cx);
            })
            .ok();
        }
        ContextMenuAction::CreateBranchAt {
            repo_path,
            start_point,
        } => {
            app.update(cx, |this, cx| {
                this.open_create_branch_modal(
                    repo_path.clone(),
                    start_point.clone(),
                    None,
                    window,
                    cx,
                );
            })
            .ok();
        }
        ContextMenuAction::CreateTagAt { repo_path, target } => {
            app.update(cx, |this, cx| {
                this.open_create_tag_modal(repo_path.clone(), target.clone(), window, cx);
            })
            .ok();
        }
        ContextMenuAction::CherryPick { repo_path, hash } => {
            app.update(cx, |this, cx| {
                this.run_cherry_pick(repo_path.clone(), hash.clone(), cx);
            })
            .ok();
        }
        ContextMenuAction::RevertCommit { repo_path, hash } => {
            app.update(cx, |this, cx| {
                this.run_revert_commit(repo_path.clone(), hash.clone(), cx);
            })
            .ok();
        }
        ContextMenuAction::ResetToCommit {
            repo_path,
            hash,
            mode,
        } => {
            app.update(cx, |this, cx| {
                this.request_reset(repo_path.clone(), hash.clone(), *mode, cx);
            })
            .ok();
        }
        ContextMenuAction::StashPush { repo_path } => {
            app.update(cx, |this, cx| {
                this.open_stash_push_modal(repo_path.clone(), window, cx);
            })
            .ok();
        }
        ContextMenuAction::RefreshWorkspace { repo_path } => {
            app.update(cx, |this, cx| {
                this.refresh_repo_workspace(repo_path.clone(), cx);
            })
            .ok();
        }
        ContextMenuAction::CopyText { text, toast_label } => {
            app.update(cx, |this, cx| {
                this.copy_text_with_toast(text.clone(), toast_label.clone(), cx);
            })
            .ok();
        }
    }
}

/// 构造右键上下文菜单。
pub fn build_context_menu(
    ctx_state: &ContextMenuState,
    app_entity: gpui::WeakEntity<GitGraphApp>,
) -> Menu {
    let spec = build_context_menu_spec(&ctx_state.target);
    let mut iter = spec.entries.into_iter();

    let mut head_items = Vec::new();
    let mut tail_entries = Vec::new();
    let mut hit_separator = false;

    for entry in iter.by_ref() {
        if !hit_separator {
            if matches!(entry, ContextMenuEntry::Separator) {
                hit_separator = true;
                tail_entries.push(entry);
            } else if let Some(item) = entry_to_menu_item(entry, &app_entity) {
                head_items.push(item);
            }
        } else {
            tail_entries.push(entry);
        }
    }

    let mut menu = Menu::new().group(spec.heading, head_items);
    for entry in tail_entries {
        match entry {
            ContextMenuEntry::Separator => menu = menu.separator(),
            other => {
                if let Some(item) = entry_to_menu_item(other, &app_entity) {
                    menu = menu.item(item);
                }
            }
        }
    }
    menu
}

/// 格式化创建分支弹窗标题中的起点标签：完整 Hash 截断为 7 位，分支/Ref 名称完整保留。
fn format_start_point_label(start_point: &str) -> &str {
    if start_point.len() >= 16 && start_point.bytes().all(|b| b.is_ascii_hexdigit()) {
        short_hash(start_point)
    } else {
        start_point
    }
}

/// 渲染当前激活的模态对话框。
pub fn render_active_modal(
    modal: &ActiveModal,
    app_entity: gpui::WeakEntity<GitGraphApp>,
    cx: &App,
) -> AnyElement {
    let close_fn = {
        let app = app_entity.clone();
        move |_window: &mut Window, cx: &mut App| {
            app.update(cx, |this, cx| {
                this.modal = None;
                cx.notify();
            })
            .ok();
        }
    };

    match modal {
        ActiveModal::CreateBranch {
            repo_path,
            start_point,
            input,
            checkout,
        } => {
            let repo = repo_path.clone();
            let sp = start_point.clone();
            let do_checkout = *checkout;
            let theme = cx.theme();
            let name_val = input.read(cx).text().trim().to_string();
            let valid = !name_val.is_empty()
                && !name_val.contains(char::is_whitespace)
                && !name_val.starts_with('-');

            Dialog::new(
                "create-branch-dialog",
                format!("在 {} 创建分支", format_start_point_label(start_point)),
                close_fn,
            )
            .focus_first(input.read(cx).focus().clone())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(theme.text_size(TextSize::Sm))
                                    .text_color(theme.colors.fg_muted)
                                    .child("分支名称"),
                            )
                            .child(Input::new(input)),
                    )
                    .child(
                        Checkbox::new("create-branch-checkout", *checkout)
                            .label("创建后立即检出该分支")
                            .on_change({
                                let app = app_entity.clone();
                                move |checked, _, cx| {
                                    app.update(cx, |this, cx| {
                                        if let Some(ActiveModal::CreateBranch { checkout, .. }) =
                                            &mut this.modal
                                        {
                                            *checkout = checked;
                                            cx.notify();
                                        }
                                    })
                                    .ok();
                                }
                            }),
                    ),
            )
            .action(|close| {
                Button::new("create-branch-cancel", "取消")
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, window, cx| close(window, cx))
            })
            .action({
                let app = app_entity.clone();
                let input = input.clone();
                move |close| {
                    Button::new("create-branch-submit", "创建分支")
                        .primary()
                        .disabled(!valid)
                        .on_click(move |_, window, cx| {
                            let branch_name = input.read(cx).text().trim().to_string();
                            if branch_name.is_empty() {
                                return;
                            }
                            close(window, cx);
                            app.update(cx, |this, cx| {
                                this.run_create_branch(
                                    repo.clone(),
                                    branch_name,
                                    sp.clone(),
                                    do_checkout,
                                    cx,
                                );
                            })
                            .ok();
                        })
                }
            })
            .into_any_element()
        }
        ActiveModal::RenameBranch {
            repo_path,
            old_name,
            input,
        } => {
            let repo = repo_path.clone();
            let old = old_name.clone();
            let theme = cx.theme();
            let new_val = input.read(cx).text().trim().to_string();
            let valid = !new_val.is_empty()
                && new_val != *old_name
                && !new_val.contains(char::is_whitespace)
                && !new_val.starts_with('-');

            Dialog::new(
                "rename-branch-dialog",
                format!("重命名分支 {old_name}"),
                close_fn,
            )
            .focus_first(input.read(cx).focus().clone())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(theme.text_size(TextSize::Sm))
                            .text_color(theme.colors.fg_muted)
                            .child("新分支名称"),
                    )
                    .child(Input::new(input)),
            )
            .action(|close| {
                Button::new("rename-branch-cancel", "取消")
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, window, cx| close(window, cx))
            })
            .action({
                let app = app_entity.clone();
                let input = input.clone();
                move |close| {
                    Button::new("rename-branch-submit", "重命名")
                        .primary()
                        .disabled(!valid)
                        .on_click(move |_, window, cx| {
                            let next_name = input.read(cx).text().trim().to_string();
                            if next_name.is_empty() || next_name == old {
                                return;
                            }
                            close(window, cx);
                            app.update(cx, |this, cx| {
                                this.run_rename_branch(repo.clone(), old.clone(), next_name, cx);
                            })
                            .ok();
                        })
                }
            })
            .into_any_element()
        }
        ActiveModal::CreateTag {
            repo_path,
            target,
            name_input,
            message_input,
        } => {
            let repo = repo_path.clone();
            let tgt = target.clone();
            let theme = cx.theme();
            let tag_name = name_input.read(cx).text().trim().to_string();
            let valid = !tag_name.is_empty() && !tag_name.contains(' ') && !tag_name.starts_with('-');

            Dialog::new(
                "create-tag-dialog",
                format!("在 {} 创建标签", short_hash(target)),
                close_fn,
            )
            .focus_first(name_input.read(cx).focus().clone())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(theme.text_size(TextSize::Sm))
                                    .text_color(theme.colors.fg_muted)
                                    .child("标签名（如 v1.2.0）"),
                            )
                            .child(Input::new(name_input)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(theme.text_size(TextSize::Sm))
                                    .text_color(theme.colors.fg_muted)
                                    .child("附注信息（可选，留空为轻量标签）"),
                            )
                            .child(Input::new(message_input)),
                    ),
            )
            .action(|close| {
                Button::new("create-tag-cancel", "取消")
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, window, cx| close(window, cx))
            })
            .action({
                let app = app_entity.clone();
                let name_input = name_input.clone();
                let msg_input = message_input.clone();
                move |close| {
                    Button::new("create-tag-submit", "创建标签")
                        .primary()
                        .disabled(!valid)
                        .on_click(move |_, window, cx| {
                            let name = name_input.read(cx).text().trim().to_string();
                            let msg = msg_input.read(cx).text().trim().to_string();
                            if name.is_empty() {
                                return;
                            }
                            let message = (!msg.is_empty()).then_some(msg);
                            close(window, cx);
                            app.update(cx, |this, cx| {
                                this.run_create_tag(repo.clone(), name, tgt.clone(), message, cx);
                            })
                            .ok();
                        })
                }
            })
            .into_any_element()
        }
        ActiveModal::StashPush {
            repo_path,
            message_input,
            include_untracked,
        } => {
            let repo = repo_path.clone();
            let untracked = *include_untracked;
            PromptDialog::new(
                "stash-push-dialog",
                "保存工作区到 Stash",
                message_input,
                close_fn,
            )
            .label("Stash 备注（可选）")
            .submit("保存 Stash")
            .on_submit({
                let app = app_entity.clone();
                move |msg, _, cx| {
                    let trimmed = msg.trim().to_string();
                    let note = (!trimmed.is_empty()).then_some(trimmed);
                    app.update(cx, |this, cx| {
                        this.run_stash_push(repo.clone(), note, untracked, cx);
                    })
                    .ok();
                }
            })
            .into_any_element()
        }
        ActiveModal::PushBranch {
            repo_path,
            local_branches,
            selected_branch,
            upstreams: _,
            remotes,
            selected_remote,
            remote_branch_input,
            set_upstream,
            push_tags,
            force,
        } => {
            let repo = repo_path.clone();
            let theme = cx.theme();
            let remote_branch_val = remote_branch_input.read(cx).text().trim().to_string();
            let branch_valid = !selected_branch.is_empty()
                && !selected_remote.is_empty()
                && !remote_branch_val.is_empty()
                && !remote_branch_val.contains(char::is_whitespace)
                && !remote_branch_val.starts_with('-');

            let branch_choices: Vec<Choice> = if local_branches.is_empty() {
                vec![Choice::new(selected_branch.clone(), selected_branch.clone())]
            } else {
                local_branches
                    .iter()
                    .map(|b| Choice::new(b.clone(), b.clone()))
                    .collect()
            };
            let branch_select = Select::new("push-local-branch-select", branch_choices)
                .size(ControlSize::Sm)
                .selected(selected_branch.clone())
                .on_change({
                    let app = app_entity.clone();
                    move |value, _, cx| {
                        let next_branch = value.to_string();
                        app.update(cx, |this, cx| {
                            if let Some(ActiveModal::PushBranch {
                                selected_branch,
                                upstreams,
                                remotes,
                                selected_remote,
                                remote_branch_input,
                                set_upstream,
                                ..
                            }) = &mut this.modal
                            {
                                *selected_branch = next_branch.clone();
                                let upstream = upstreams.get(&next_branch).and_then(|u| u.as_deref());
                                let defaults =
                                    compute_push_defaults(&next_branch, upstream, remotes);
                                *selected_remote = defaults.remote;
                                *set_upstream = defaults.set_upstream;
                                remote_branch_input.update(cx, |input, cx| {
                                    input.set_text(defaults.remote_branch, cx);
                                });
                                cx.notify();
                            }
                        })
                        .ok();
                    }
                });

            let remote_choices: Vec<Choice> = remotes
                .iter()
                .map(|r| Choice::new(r.clone(), r.clone()))
                .collect();
            let remote_select = Select::new("push-remote-select", remote_choices)
                .size(ControlSize::Sm)
                .placeholder("无可用远程仓库")
                .disabled(remotes.is_empty())
                .selected(selected_remote.clone())
                .on_change({
                    let app = app_entity.clone();
                    move |value, _, cx| {
                        let next_remote = value.to_string();
                        app.update(cx, |this, cx| {
                            if let Some(ActiveModal::PushBranch {
                                selected_remote, ..
                            }) = &mut this.modal
                            {
                                *selected_remote = next_remote;
                                cx.notify();
                            }
                        })
                        .ok();
                    }
                });

            let force_key: SharedString = match force {
                PushForceMode::None => "none",
                PushForceMode::ForceWithLease => "lease",
                PushForceMode::Force => "force",
            }
            .into();
            let force_control = SegmentedControl::new("push-force-mode", force_key)
                .size(ControlSize::Sm)
                .segment("none", "关闭", None)
                .segment("lease", "安全强推 (--force-with-lease)", None)
                .segment("force", "强制覆盖 (--force)", None)
                .on_change({
                    let app = app_entity.clone();
                    move |val, _, cx| {
                        let mode = match val.as_ref() {
                            "lease" => PushForceMode::ForceWithLease,
                            "force" => PushForceMode::Force,
                            _ => PushForceMode::None,
                        };
                        app.update(cx, |this, cx| {
                            if let Some(ActiveModal::PushBranch { force, .. }) = &mut this.modal {
                                *force = mode;
                                cx.notify();
                            }
                        })
                        .ok();
                    }
                });

            let warning_callout: Option<AnyElement> = if remotes.is_empty() {
                Some(
                    Callout::new(Severity::Warning)
                        .title("未配置远程仓库")
                        .child("请先为当前仓库添加 remote 后再执行推送。")
                        .into_any_element(),
                )
            } else {
                match force {
                    PushForceMode::None => None,
                    PushForceMode::ForceWithLease => Some(
                        Callout::new(Severity::Warning)
                            .title("已启用 --force-with-lease")
                            .child("仅在远端分支未发生他人新提交时覆盖，比 --force 更安全。")
                            .into_any_element(),
                    ),
                    PushForceMode::Force => Some(
                        Callout::new(Severity::Danger)
                            .title("高危提醒：已启用 --force 强制覆盖")
                            .child("将强制覆盖远端分支历史，可能导致他人的提交丢失且无法恢复！")
                            .into_any_element(),
                    ),
                }
            };

            let field_label = |text: &'static str| {
                div()
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(theme.colors.fg_muted)
                    .child(text)
            };

            let submit_local = selected_branch.clone();
            let submit_remote = selected_remote.clone();
            let submit_upstream = *set_upstream;
            let submit_tags = *push_tags;
            let submit_force = *force;

            Dialog::new("push-branch-dialog", "推送分支到远程", close_fn)
                .focus_first(remote_branch_input.read(cx).focus().clone())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(field_label("本地分支"))
                                .child(branch_select),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(field_label("目标远端 (Remote)"))
                                .child(remote_select),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(field_label("远程分支名"))
                                .child(Input::new(remote_branch_input)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    Checkbox::new("push-set-upstream", *set_upstream)
                                        .label("设置上游追踪 (-u)")
                                        .on_change({
                                            let app = app_entity.clone();
                                            move |checked, _, cx| {
                                                app.update(cx, |this, cx| {
                                                    if let Some(ActiveModal::PushBranch {
                                                        set_upstream,
                                                        ..
                                                    }) = &mut this.modal
                                                    {
                                                        *set_upstream = checked;
                                                        cx.notify();
                                                    }
                                                })
                                                .ok();
                                            }
                                        }),
                                )
                                .child(
                                    Checkbox::new("push-tags", *push_tags)
                                        .label("同时推送标签 (--tags)")
                                        .on_change({
                                            let app = app_entity.clone();
                                            move |checked, _, cx| {
                                                app.update(cx, |this, cx| {
                                                    if let Some(ActiveModal::PushBranch {
                                                        push_tags,
                                                        ..
                                                    }) = &mut this.modal
                                                    {
                                                        *push_tags = checked;
                                                        cx.notify();
                                                    }
                                                })
                                                .ok();
                                            }
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(field_label("强制推送模式"))
                                .child(force_control),
                        )
                        .children(warning_callout),
                )
                .action(|close| {
                    Button::new("push-branch-cancel", "取消")
                        .variant(ButtonVariant::Ghost)
                        .on_click(move |_, window, cx| close(window, cx))
                })
                .action({
                    let app = app_entity.clone();
                    let remote_branch_input = remote_branch_input.clone();
                    move |close| {
                        let btn = Button::new("push-branch-submit", "推送")
                            .variant(if submit_force == PushForceMode::Force {
                                ButtonVariant::Danger
                            } else {
                                ButtonVariant::Primary
                            })
                            .disabled(!branch_valid);
                        btn.on_click(move |_, window, cx| {
                            let remote_branch =
                                remote_branch_input.read(cx).text().trim().to_string();
                            if submit_local.is_empty()
                                || submit_remote.is_empty()
                                || remote_branch.is_empty()
                            {
                                return;
                            }
                            let opts = PushOptions {
                                local_branch: submit_local.clone(),
                                remote: submit_remote.clone(),
                                remote_branch,
                                set_upstream: submit_upstream,
                                push_tags: submit_tags,
                                force: submit_force,
                            };
                            close(window, cx);
                            app.update(cx, |this, cx| {
                                this.run_push_branch(repo.clone(), opts.clone(), cx);
                            })
                            .ok();
                        })
                    }
                })
                .into_any_element()
        }
        ActiveModal::SafetyConfirm {
            warning,
            confirm_label,
            pending,
        } => {
            let pending = pending.clone();
            let app = app_entity.clone();
            let detail = if warning.recoverable_via_reflog {
                format!("{}\n（提示：可通过 git reflog 找回）", warning.reason)
            } else {
                format!("{}\n（警告：此操作不可从 reflog 恢复！）", warning.reason)
            };
            let sev_label = if warning.recoverable_via_reflog {
                Severity::Warning
            } else {
                Severity::Danger
            };
            let _ = sev_label;
            ConfirmDialog::new("safety-confirm-dialog", warning.title.clone(), detail, close_fn)
                .confirm(confirm_label.clone())
                .destructive()
                .on_confirm(move |_, cx| {
                    app.update(cx, |this, cx| {
                        this.execute_pending_write(pending.clone(), cx);
                    })
                    .ok();
                })
                .into_any_element()
        }
        ActiveModal::SimpleConfirm {
            title,
            message,
            confirm_label,
            destructive,
            pending,
        } => {
            let pending = pending.clone();
            let app = app_entity.clone();
            let mut dlg = ConfirmDialog::new(
                "simple-confirm-dialog",
                title.clone(),
                message.clone(),
                close_fn,
            )
            .confirm(confirm_label.clone())
            .on_confirm(move |_, cx| {
                app.update(cx, |this, cx| {
                    this.execute_pending_write(pending.clone(), cx);
                })
                .ok();
            });
            if *destructive {
                dlg = dlg.destructive();
            }
            dlg.into_any_element()
        }
    }
}

pub fn parse_stash_index(selector: &str) -> Option<usize> {
    selector
        .strip_prefix("stash@{")
        .and_then(|s| s.strip_suffix('}'))
        .and_then(|n| n.parse::<usize>().ok())
}

#[cfg(test)]
mod tests {
    use git_graph_core::types::{GitCommitRemote, GitCommitTag};

    use super::*;

    fn sample_repo() -> PathBuf {
        PathBuf::from("/tmp/sample-repo")
    }

    fn sample_commit_with_refs() -> GitCommit {
        GitCommit {
            hash: "a52c6961234567890abcdef1234567890abcdef1".to_string(),
            parents: vec!["1111111111111111111111111111111111111111".to_string()],
            author: "Alice".to_string(),
            email: "alice@example.com".to_string(),
            date: 1_700_000_000,
            message: "feat: redesign context menus".to_string(),
            heads: vec!["main".to_string(), "feat/ui".to_string()],
            tags: vec![GitCommitTag {
                name: "v1.0.0".to_string(),
                annotated: true,
            }],
            remotes: vec![GitCommitRemote {
                name: "origin/main".to_string(),
                remote: Some("origin".to_string()),
            }],
            stash: None,
        }
    }

    fn item_labels(entries: &[ContextMenuEntry]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| match entry {
                ContextMenuEntry::Item { label, .. } => label.as_str(),
                ContextMenuEntry::Submenu { label, .. } => label.as_str(),
                ContextMenuEntry::Separator => "---",
            })
            .collect()
    }

    #[test]
    fn local_branch_head_excludes_checkout_delete_and_commit_items() {
        let spec = build_context_menu_spec(&ContextMenuTarget::LocalBranch {
            repo_path: sample_repo(),
            branch: "master".to_string(),
            is_head: true,
        });

        assert_eq!(spec.heading, "分支 master");
        assert_eq!(
            item_labels(&spec.entries),
            vec![
                "推送分支 \"master\" 到远程…",
                "重命名分支…",
                "---",
                "复制分支名",
            ]
        );
        assert!(!spec.entries.iter().any(|e| matches!(
            e,
            ContextMenuEntry::Item {
                action: ContextMenuAction::CheckoutBranch { .. }
                    | ContextMenuAction::DeleteBranch { .. }
                    | ContextMenuAction::CheckoutCommit { .. }
                    | ContextMenuAction::CherryPick { .. },
                ..
            }
        )));
    }

    #[test]
    fn local_branch_non_head_has_all_branch_actions_and_zero_leakage() {
        let spec = build_context_menu_spec(&ContextMenuTarget::LocalBranch {
            repo_path: sample_repo(),
            branch: "feat/menu".to_string(),
            is_head: false,
        });

        assert_eq!(spec.heading, "分支 feat/menu");
        assert_eq!(
            item_labels(&spec.entries),
            vec![
                "检出分支 feat/menu",
                "推送分支 \"feat/menu\" 到远程…",
                "重命名分支…",
                "删除分支 feat/menu…",
                "---",
                "复制分支名",
            ]
        );
        // 绝不混入任何提交操作或其他分支
        assert!(!spec.entries.iter().any(|e| matches!(
            e,
            ContextMenuEntry::Item {
                action: ContextMenuAction::CheckoutCommit { .. }
                    | ContextMenuAction::CreateBranchAt { .. }
                    | ContextMenuAction::CreateTagAt { .. }
                    | ContextMenuAction::CherryPick { .. }
                    | ContextMenuAction::RevertCommit { .. }
                    | ContextMenuAction::ResetToCommit { .. },
                ..
            } | ContextMenuEntry::Submenu { .. }
        )));
    }

    #[test]
    fn remote_branch_toggles_checkout_based_on_is_remote_head() {
        let normal = build_context_menu_spec(&ContextMenuTarget::RemoteBranch {
            repo_path: sample_repo(),
            remote: "origin".to_string(),
            branch_name: "feat".to_string(),
            full_name: "origin/feat".to_string(),
            is_remote_head: false,
        });
        assert_eq!(normal.heading, "远程分支 origin/feat");
        assert_eq!(
            item_labels(&normal.entries),
            vec!["检出为本地分支…", "复制远程分支名"]
        );
        assert!(matches!(
            &normal.entries[0],
            ContextMenuEntry::Item {
                action: ContextMenuAction::CheckoutRemoteBranch {
                    full_name,
                    local_name,
                    ..
                },
                ..
            } if full_name == "origin/feat" && local_name == "feat"
        ));

        let remote_head = build_context_menu_spec(&ContextMenuTarget::RemoteBranch {
            repo_path: sample_repo(),
            remote: "origin".to_string(),
            branch_name: "HEAD".to_string(),
            full_name: "origin/HEAD".to_string(),
            is_remote_head: true,
        });
        assert_eq!(remote_head.heading, "远程分支 origin/HEAD");
        assert_eq!(item_labels(&remote_head.entries), vec!["复制远程分支名"]);
    }

    #[test]
    fn tag_stash_and_uncommitted_only_contain_dedicated_items() {
        let tag = build_context_menu_spec(&ContextMenuTarget::Tag {
            repo_path: sample_repo(),
            name: "v2.1.0".to_string(),
        });
        assert_eq!(tag.heading, "标签 v2.1.0");
        assert_eq!(
            item_labels(&tag.entries),
            vec!["删除标签 v2.1.0…", "复制标签名"]
        );

        let stash = build_context_menu_spec(&ContextMenuTarget::Stash {
            repo_path: sample_repo(),
            selector: "stash@{2}".to_string(),
            index: 2,
            hash: "deadbeef1234567890".to_string(),
        });
        assert_eq!(stash.heading, "Stash stash@{2}");
        assert_eq!(
            item_labels(&stash.entries),
            vec![
                "应用 Stash (stash@{2})",
                "弹出 Stash (stash@{2})",
                "删除 Stash (stash@{2})…",
                "---",
                "复制 Stash 标识",
                "复制完整 Hash",
            ]
        );

        let uncommitted = build_context_menu_spec(&ContextMenuTarget::Uncommitted {
            repo_path: sample_repo(),
        });
        assert_eq!(uncommitted.heading, "未提交的更改");
        assert_eq!(
            item_labels(&uncommitted.entries),
            vec!["储藏更改 (Stash Push)…", "刷新工作区状态"]
        );
    }

    #[test]
    fn commit_menu_has_no_branch_or_tag_items_and_keeps_three_flat_copy_items() {
        let commit = sample_commit_with_refs();
        let spec = build_context_menu_spec(&ContextMenuTarget::Commit {
            repo_path: sample_repo(),
            commit,
        });

        assert_eq!(spec.heading, "提交 a52c696");
        assert_eq!(
            item_labels(&spec.entries),
            vec![
                "检出此提交 (a52c696)",
                "在此创建分支…",
                "在此创建标签…",
                "---",
                "Cherry-pick 此提交",
                "Revert 此提交",
                "Reset 当前分支到此提交",
                "---",
                "复制完整 Hash",
                "复制短 Hash",
                "复制提交信息",
            ]
        );

        // 即使该提交上附带 main/feat/ui 分支和 v1.0.0 标签，也绝不平铺任何分支/标签操作
        assert!(!spec.entries.iter().any(|e| matches!(
            e,
            ContextMenuEntry::Item {
                action: ContextMenuAction::CheckoutBranch { .. }
                    | ContextMenuAction::PushBranch { .. }
                    | ContextMenuAction::RenameBranch { .. }
                    | ContextMenuAction::DeleteBranch { .. }
                    | ContextMenuAction::DeleteTag { .. },
                ..
            }
        )));

        // Reset 子菜单包含 Soft / Mixed / Hard
        let reset_sub = spec.entries.iter().find_map(|e| match e {
            ContextMenuEntry::Submenu { label, items, .. } if label == "Reset 当前分支到此提交" => {
                Some(items)
            }
            _ => None,
        });
        let reset_items = reset_sub.expect("reset submenu should exist");
        assert_eq!(reset_items.len(), 3);
        assert_eq!(
            item_labels(reset_items),
            vec![
                "Soft（保留暂存与工作区）",
                "Mixed（保留工作区）",
                "Hard（丢弃全部未提交修改）",
            ]
        );

        // 底部 3 个复制项保持平铺（非子菜单）
        let tail = &spec.entries[spec.entries.len() - 3..];
        assert!(tail.iter().all(|e| matches!(
            e,
            ContextMenuEntry::Item {
                action: ContextMenuAction::CopyText { .. },
                ..
            }
        )));
    }

    #[test]
    fn start_point_label_truncates_hashes_and_keeps_ref_names() {
        assert_eq!(
            format_start_point_label("a52c6961234567890abcdef1234567890abcdef1"),
            "a52c696"
        );
        assert_eq!(
            format_start_point_label("origin/feature/context-menu"),
            "origin/feature/context-menu"
        );
    }

    #[test]
    fn push_defaults_auto_checks_set_upstream_when_missing() {
        let remotes = vec!["fork".to_string(), "origin".to_string()];

        // 1. 无 upstream 时自动勾选 -u，remote 优先 origin
        let no_up = compute_push_defaults("feat/new-ui", None, &remotes);
        assert_eq!(no_up.remote, "origin");
        assert_eq!(no_up.remote_branch, "feat/new-ui");
        assert!(no_up.set_upstream);

        // 2. 已有 upstream 时不自动勾选 -u，remote 优先取 upstream 的 remote
        let with_up = compute_push_defaults("feat/new-ui", Some("fork/feat/new-ui"), &remotes);
        assert_eq!(with_up.remote, "fork");
        assert_eq!(with_up.remote_branch, "feat/new-ui");
        assert!(!with_up.set_upstream);

        // 3. 无 origin 时回退到列表首个 remote
        let only_upstream = vec!["upstream".to_string()];
        let fallback = compute_push_defaults("main", None, &only_upstream);
        assert_eq!(fallback.remote, "upstream");
        assert!(fallback.set_upstream);

        // 4. 无任何 remote 时 remote 为空且仍自动勾选 -u
        let empty = compute_push_defaults("main", None, &[]);
        assert_eq!(empty.remote, "");
        assert_eq!(empty.remote_branch, "main");
        assert!(empty.set_upstream);
    }
}
