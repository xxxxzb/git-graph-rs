//! 上半区视图：顶部筛选工具条、可调宽 5 列表头、虚拟滚动 Git Graph 宽表与 RefBadge 胶囊。

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ely_core::buttons::{Button, ButtonVariant, IconButton};
use ely_core::data_display::{Badge, Tone};
use ely_core::feedback::{EmptyState, ErrorView};
use ely_core::forms::{Checkbox, Choice, SearchInput, Select};
use ely_core::motion::{
    Shimmer, SkeletonTable, Spinner, SpinnerStyle, duration, ease_out_cubic,
};
use ely_core::primitives::{Icon, IconName};
use ely_core::shell::{Toolbar, ToolbarGroup, ToolbarSeparator};
use ely_core::theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize};
use git_graph_core::types::{GitCommit, UNCOMMITTED};
use git_graph_layout::{RefBadge, RowLayout, group_ref_badges};
use git_graph_ops::BranchSyncStatus;
use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, CursorStyle, DragMoveEvent, EmptyView,
    FontWeight, Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    Pixels, ScrollEvent, SharedString, StatefulInteractiveElement, Styled, TouchPhase, div,
    prelude::*, px, uniform_list,
};

use crate::app_view::GitGraphApp;
use crate::graph_canvas::{GraphCellStyle, lane_color, paint_graph_cell};
use crate::modals::{ContextMenuTarget, parse_stash_index};
use crate::repo_tab::RepoTabState;
use crate::state::{ColumnWidths, commit_ref_labels, format_timestamp, short_hash};

/// 行高：28px 经典高密度 Git Graph 列表行。
const ROW_HEIGHT: f32 = 28.0;
/// 每个泳道宽度。
const LANE_WIDTH: f32 = 16.0;
/// 距列表底部多少行时自动触发加载下一页。
const PREFETCH_TAIL: usize = 24;
/// 手动拖拽图形列的最小 / 最大宽度限制。
const GRAPH_MANUAL_MIN_WIDTH: f32 = 72.0;
const GRAPH_MANUAL_MAX_WIDTH: f32 = 360.0;

/// 统一计算表头与每一行共用的图形列宽度（保证 100% 水平对齐）。
///
/// 未手动拖拽过图形列时，随当前 `max_lanes` 在 `[76.0, 260.0]` 区间自动撑开；
/// 用户手动拖拽表头分隔线后，严格尊重用户设定宽度（并限制在 `[72.0, 360.0]`）。
pub fn effective_graph_width(tab: &RepoTabState, widths: &ColumnWidths) -> f32 {
    if widths.graph_manual {
        return widths
            .graph
            .clamp(GRAPH_MANUAL_MIN_WIDTH, GRAPH_MANUAL_MAX_WIDTH);
    }
    graph_content_width(tab.layout.max_lanes).clamp(76.0, 260.0)
}

/// 图形列完整内容所需的水平宽度（按泳道数计算）。
pub fn graph_content_width(max_lanes: usize) -> f32 {
    max_lanes.max(1) as f32 * LANE_WIDTH + 24.0
}

/// 图形列允许的最大水平滚动偏移（px）。
pub fn max_graph_scroll_x(tab: &RepoTabState, graph_col_w: f32) -> f32 {
    (graph_content_width(tab.layout.max_lanes) - graph_col_w).max(0.0)
}

/// 滚动事件的轴向分类结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollAxisDecision {
    /// 横向滑动（附带水平像素增量 `dx`），应拦截冒泡防止外层列表斜向串滚。
    Horizontal(f32),
    /// 竖向滚动，应正常放行冒泡给外层 `uniform_list`。
    Vertical,
    /// 空帧或尚未确定方向。
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockedAxis {
    Horizontal,
    Vertical,
}

/// 触控板 / 滚轮手势轴向锁：一旦某次双指手势判定为横向滑动，在整个手势
/// （含轻微手指斜偏与惯性尾帧）期间持续锁定为横向并拦截向 `uniform_list` 冒泡。
#[derive(Debug, Default)]
pub struct HorizontalScrollLatch {
    axis: Option<LockedAxis>,
    last_event: Option<Instant>,
}

impl HorizontalScrollLatch {
    /// 超过该间隔视为全新滚动手势，自动释放旧轴向锁。
    const GESTURE_TIMEOUT: Duration = Duration::from_millis(160);

    pub fn classify(&mut self, event: &ScrollEvent) -> ScrollAxisDecision {
        self.classify_at(event, Instant::now())
    }

    pub fn classify_at(&mut self, event: &ScrollEvent, now: Instant) -> ScrollAxisDecision {
        let delta = event.delta.pixel_delta(px(ROW_HEIGHT));
        let dx = f32::from(delta.x);
        let dy = f32::from(delta.y);

        // Shift + 滚轮（普通鼠标）：始终视为明确的横向滚动，不污染连续触控板锁
        if event.modifiers.shift {
            if dx.abs() > 0.01 {
                return ScrollAxisDecision::Horizontal(dx);
            }
            if dy.abs() > 0.01 {
                return ScrollAxisDecision::Horizontal(dy);
            }
            return ScrollAxisDecision::Neutral;
        }

        // 离散鼠标滚轮（非精确触控板）按单帧主导轴判定
        if !event.delta.precise() {
            self.axis = None;
            self.last_event = None;
            if dx.abs() > 0.01 && dx.abs() >= dy.abs() {
                return ScrollAxisDecision::Horizontal(dx);
            }
            if dy.abs() > 0.01 {
                return ScrollAxisDecision::Vertical;
            }
            return ScrollAxisDecision::Neutral;
        }

        // Cancelled 时立即复位；Ended（手指抬起）保留轴向锁以覆盖紧随其后的 macOS 惯性滑行帧，
        // 直到下一次 TouchPhase::Started 或超过 GESTURE_TIMEOUT 自动解锁。
        if event.touch_phase == TouchPhase::Cancelled {
            let prev = self.axis.take();
            self.last_event = None;
            return match prev {
                Some(LockedAxis::Horizontal) => ScrollAxisDecision::Horizontal(dx),
                Some(LockedAxis::Vertical) => ScrollAxisDecision::Vertical,
                None => ScrollAxisDecision::Neutral,
            };
        }

        if event.touch_phase == TouchPhase::Ended {
            self.last_event = Some(now);
            return match self.axis {
                Some(LockedAxis::Horizontal) => ScrollAxisDecision::Horizontal(dx),
                Some(LockedAxis::Vertical) => ScrollAxisDecision::Vertical,
                None if dx.abs() > 0.01 && dx.abs() >= dy.abs() => {
                    ScrollAxisDecision::Horizontal(dx)
                }
                None if dy.abs() > 0.01 => ScrollAxisDecision::Vertical,
                None => ScrollAxisDecision::Neutral,
            };
        }

        let timed_out = self
            .last_event
            .is_some_and(|prev| now.saturating_duration_since(prev) > Self::GESTURE_TIMEOUT);
        if event.touch_phase == TouchPhase::Started || timed_out {
            self.axis = None;
        }
        self.last_event = Some(now);

        if dx.abs() <= 0.01 && dy.abs() <= 0.01 {
            return match self.axis {
                Some(LockedAxis::Horizontal) => ScrollAxisDecision::Horizontal(0.0),
                Some(LockedAxis::Vertical) => ScrollAxisDecision::Vertical,
                None => ScrollAxisDecision::Neutral,
            };
        }

        let axis = match self.axis {
            Some(LockedAxis::Horizontal) => {
                // 仅当用户明显大幅转成纯竖滑时才主动解锁
                if dy.abs() > 8.0 && dy.abs() > dx.abs() * 2.5 {
                    LockedAxis::Vertical
                } else {
                    LockedAxis::Horizontal
                }
            }
            Some(LockedAxis::Vertical) => {
                if dx.abs() > 8.0 && dx.abs() > dy.abs() * 2.5 {
                    LockedAxis::Horizontal
                } else {
                    LockedAxis::Vertical
                }
            }
            None => {
                if dx.abs() >= dy.abs() {
                    LockedAxis::Horizontal
                } else {
                    LockedAxis::Vertical
                }
            }
        };
        self.axis = Some(axis);

        match axis {
            LockedAxis::Horizontal => ScrollAxisDecision::Horizontal(dx),
            LockedAxis::Vertical => ScrollAxisDecision::Vertical,
        }
    }
}

fn desc_scroll_id(tab_id: u64, row_ix: usize) -> usize {
    (tab_id as usize)
        .wrapping_mul(1_000_003)
        .wrapping_add(row_ix)
}

/// 同步按钮点击对应的动作类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncButtonAction {
    Sync,
    Publish { remote: String, branch: String },
    None,
}

/// 左下角状态栏「同步 / 发布分支」状态项的展示规格。
#[derive(Debug, Clone, PartialEq)]
pub struct SyncButtonSpec {
    pub status_bar_label: String,
    pub tooltip: String,
    pub icon: IconName,
    pub disabled: bool,
    pub action: SyncButtonAction,
}

/// 将仓库分支同步状态映射为底部状态栏展示文案与点击动作。
pub fn sync_button_spec(
    sync_status: Option<&BranchSyncStatus>,
    syncing: bool,
    action_label: Option<&str>,
) -> SyncButtonSpec {
    if syncing {
        let label = action_label.unwrap_or("同步中…").to_string();
        return SyncButtonSpec {
            status_bar_label: label,
            tooltip: "正在与远程仓库同步，请稍候…".to_string(),
            icon: IconName::RefreshCw,
            disabled: true,
            action: SyncButtonAction::None,
        };
    }

    match sync_status {
        Some(BranchSyncStatus::Synced {
            upstream,
            ahead,
            behind,
            ..
        }) => {
            if *ahead > 0 || *behind > 0 {
                SyncButtonSpec {
                    status_bar_label: format!("↓{behind} ↑{ahead}"),
                    tooltip: format!(
                        "与 {upstream} 同步（拉取 {behind} / 推送 {ahead}，遵循本机 git pull 配置）"
                    ),
                    icon: IconName::RefreshCw,
                    disabled: false,
                    action: SyncButtonAction::Sync,
                }
            } else {
                SyncButtonSpec {
                    status_bar_label: "↓0 ↑0".to_string(),
                    tooltip: format!("与 {upstream} 同步（先 pull 再 push）"),
                    icon: IconName::RefreshCw,
                    disabled: false,
                    action: SyncButtonAction::Sync,
                }
            }
        }
        Some(BranchSyncStatus::NoUpstream {
            branch,
            default_remote,
        }) => match default_remote {
            Some(remote) => SyncButtonSpec {
                status_bar_label: "发布分支".to_string(),
                tooltip: format!("发布分支 {branch} 到 {remote} 并设置上游追踪"),
                icon: IconName::CloudUpload,
                disabled: false,
                action: SyncButtonAction::Publish {
                    remote: remote.clone(),
                    branch: branch.clone(),
                },
            },
            None => SyncButtonSpec {
                status_bar_label: "发布分支".to_string(),
                tooltip: "尚未配置远程仓库".to_string(),
                icon: IconName::CloudOff,
                disabled: true,
                action: SyncButtonAction::None,
            },
        },
        Some(BranchSyncStatus::DetachedHead) => SyncButtonSpec {
            status_bar_label: "分离 HEAD".to_string(),
            tooltip: "当前处于分离 HEAD，无法同步".to_string(),
            icon: IconName::RefreshCw,
            disabled: true,
            action: SyncButtonAction::None,
        },
        None => SyncButtonSpec {
            status_bar_label: "检查同步…".to_string(),
            tooltip: "正在检查远程同步状态…".to_string(),
            icon: IconName::RefreshCw,
            disabled: true,
            action: SyncButtonAction::None,
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResizeCol {
    Graph,
    Date,
    Author,
    Commit,
}

#[derive(Clone)]
pub struct ColumnDrag {
    pub col: ResizeCol,
    pub start_width: f32,
    pub anchor_x: Rc<Cell<Option<Pixels>>>,
}

/// 渲染顶部筛选工具条。
pub fn render_filter_toolbar(
    app: &GitGraphApp,
    tab: &RepoTabState,
    cx: &mut Context<GitGraphApp>,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let weak = cx.weak_entity();

    // 分支下拉选项
    let mut branch_choices = vec![Choice::new("__all__", "全部分支")];
    if let Some(info) = &tab.info {
        for b in &info.branches {
            let label = if info.head.as_deref() == Some(b.as_str()) {
                format!("● {b}")
            } else {
                b.clone()
            };
            branch_choices.push(Choice::new(b.clone(), label));
        }
    }
    let selected_branch: SharedString = tab
        .filter
        .branch
        .clone()
        .unwrap_or_else(|| "__all__".to_string())
        .into();

    let branch_select = Select::new(("branch-filter", tab.id), branch_choices)
        .size(ControlSize::Sm)
        .selected(selected_branch)
        .on_change({
            let weak = weak.clone();
            move |value, _, cx| {
                let branch = if value.as_ref() == "__all__" {
                    None
                } else {
                    Some(value.to_string())
                };
                weak.update(cx, |this, cx| this.set_branch_filter(branch, cx))
                    .ok();
            }
        });

    // 作者下拉选项
    let mut author_choices = vec![Choice::new("__all__", "全部作者")];
    let mut seen_names = std::collections::HashSet::new();
    for a in &tab.authors {
        if seen_names.insert(a.name.clone()) {
            author_choices.push(Choice::new(a.name.clone(), a.name.clone()));
        }
    }
    let selected_author: SharedString = tab
        .filter
        .author
        .clone()
        .unwrap_or_else(|| "__all__".to_string())
        .into();

    let author_select = Select::new(("author-filter", tab.id), author_choices)
        .size(ControlSize::Sm)
        .selected(selected_author)
        .on_change({
            let weak = weak.clone();
            move |value, _, cx| {
                let author = if value.as_ref() == "__all__" {
                    None
                } else {
                    Some(value.to_string())
                };
                weak.update(cx, |this, cx| this.set_author_filter(author, cx))
                    .ok();
            }
        });

    // 远程分支开关
    let remote_check = Checkbox::new(("remote-filter", tab.id), tab.filter.show_remote_branches)
        .label("远程")
        .on_change({
            let weak = weak.clone();
            move |checked, _, cx| {
                weak.update(cx, |this, cx| this.set_show_remote_branches(checked, cx))
                    .ok();
            }
        });

    // 搜索框（支持 Esc 一键清空搜索）
    let search_box = div()
        .w(px(236.0))
        .on_key_down({
            let weak = weak.clone();
            move |event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape"
                    && !event.keystroke.modifiers.platform
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.alt
                {
                    weak.update(cx, |this, cx| {
                        if !this.search_input.read(cx).is_empty()
                            || this.active_tab().is_some_and(|t| t.is_search_active())
                        {
                            this.clear_search(cx);
                        } else {
                            window.focus(&this.focus, cx);
                        }
                    })
                    .ok();
                    cx.stop_propagation();
                }
            }
        })
        .child(SearchInput::new("graph-search", &app.search_input).size(ControlSize::Sm));

    let search_status: Option<AnyElement> = tab.is_search_active().then(|| {
        if tab.search_loading {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .px_1()
                .child(
                    Spinner::new(("search-spinner", tab.id))
                        .style(SpinnerStyle::Dots)
                        .size(IconSize::Xs)
                        .color(colors.accent),
                )
                .child(
                    div()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(colors.fg_muted)
                        .child("全历史检索中…"),
                )
                .into_any_element()
        } else if tab.search_error.is_some() {
            Badge::new("检索失败")
                .tone(Tone::Danger)
                .into_any_element()
        } else if let Some(count) = tab.search_match_count {
            let count_badge = if count == 0 {
                Badge::new("全历史 0 条").tone(Tone::Warning)
            } else {
                Badge::new(format!("全历史 {count} 条")).tone(Tone::Info)
            };
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(count_badge)
                .when(tab.search_used_literal_fallback, |el| {
                    el.child(Badge::new("字面量匹配").tone(Tone::Neutral))
                })
                .into_any_element()
        } else {
            div().into_any_element()
        }
    });

    // 右侧动作按钮
    let jump_head_btn = IconButton::new("jump-head-btn", IconName::GitCommitHorizontal)
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Sm)
        .tooltip("跳转到 HEAD")
        .disabled(tab.head_hash.is_none())
        .on_click({
            let weak = weak.clone();
            move |_, _, cx| {
                weak.update(cx, |this, cx| this.jump_to_head(cx)).ok();
            }
        });

    let toggle_details_btn = IconButton::new("toggle-details-btn", IconName::PanelBottom)
        .variant(if app.persisted.details_open {
            ButtonVariant::Secondary
        } else {
            ButtonVariant::Ghost
        })
        .size(ControlSize::Sm)
        .tooltip("显示 / 隐藏详情分栏")
        .on_click({
            let weak = weak.clone();
            move |_, _, cx| {
                weak.update(cx, |this, cx| this.toggle_details_pane(cx))
                    .ok();
            }
        });

    let refresh_btn = IconButton::new("refresh-graph-btn", IconName::RefreshCw)
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Sm)
        .tooltip("刷新仓库图谱")
        .on_click({
            let weak = weak.clone();
            move |_, _, cx| {
                weak.update(cx, |this, cx| this.refresh_active_tab(false, cx))
                    .ok();
            }
        });

    div()
        .w_full()
        .bg(colors.surface)
        .border_b_1()
        .border_color(colors.border)
        .px_2()
        .py_1()
        .child(
            Toolbar::new()
                .w_full()
                .justify_between()
                .child(
                    ToolbarGroup::new()
                        .child(div().w(px(158.0)).child(branch_select))
                        .child(div().w(px(142.0)).child(author_select))
                        .child(div().px_1().child(remote_check))
                        .child(ToolbarSeparator)
                        .child(search_box)
                        .children(search_status),
                )
                .child(
                    ToolbarGroup::new()
                        .child(jump_head_btn)
                        .child(toggle_details_btn)
                        .child(refresh_btn),
                ),
        )
        .into_any_element()
}

/// 根据提交行类型构造外层行右键对应的上下文目标。
pub fn commit_row_context_target(
    repo_path: std::path::PathBuf,
    commit: &GitCommit,
) -> ContextMenuTarget {
    if commit.hash == UNCOMMITTED {
        ContextMenuTarget::Uncommitted { repo_path }
    } else if let Some(stash) = &commit.stash {
        let index = parse_stash_index(&stash.selector).unwrap_or(0);
        ContextMenuTarget::Stash {
            repo_path,
            selector: stash.selector.clone(),
            index,
            hash: commit.hash.clone(),
        }
    } else {
        ContextMenuTarget::Commit {
            repo_path,
            commit: commit.clone(),
        }
    }
}

/// 渲染上半区 5 列 Git Graph 宽表。
pub fn render_graph_table(
    app: &GitGraphApp,
    tab: &RepoTabState,
    cx: &mut Context<GitGraphApp>,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let widths = app.persisted.column_widths.clone();
    let graph_col_w = effective_graph_width(tab, &widths);
    let max_graph_scroll = max_graph_scroll_x(tab, graph_col_w);
    let graph_scroll_x = tab.graph_scroll_x.clamp(0.0, max_graph_scroll);
    let weak = cx.weak_entity();

    let header = render_table_header(
        graph_col_w,
        graph_scroll_x,
        &widths,
        weak.clone(),
        cx,
    );

    let total_rows = tab.visible_commit_count();
    let show_load_more = tab.more_available && !tab.is_search_active();
    let show_end_row = !tab.more_available && !tab.is_search_active() && total_rows > 0;
    let item_count = total_rows + usize::from(show_load_more || show_end_row);

    let tab_id = tab.id;
    let dark = theme.is_dark();
    let head_branch = tab.info.as_ref().and_then(|i| i.head.clone());
    let head_hash = tab.head_hash.clone();
    let selected_hash = tab.selected_hash.clone();
    let overscroll_x = tab.graph_overscroll.offset();
    let vertical_bounce = tab.vertical_overscroll.offset();
    let flash_commit = tab.flash_commit.clone();

    let body: AnyElement = if let Some(err) = &tab.error {
        ErrorView::new("graph-load-error", IconName::TriangleAlert, "图谱加载失败")
            .detail(err.clone())
            .into_any_element()
    } else if tab.is_search_active() && tab.search_loading && total_rows == 0 {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                Spinner::new(("graph-search-loading", tab_id))
                    .style(SpinnerStyle::Ring)
                    .size(IconSize::Md)
                    .color(colors.accent),
            )
            .child(
                div()
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(colors.fg_muted)
                    .child("正在全历史检索提交记录…"),
            )
            .into_any_element()
    } else if tab.is_search_active() && !tab.search_loading && total_rows == 0 {
        if let Some(search_err) = &tab.search_error {
            ErrorView::new(
                "graph-search-error",
                IconName::TriangleAlert,
                "全历史搜索失败",
            )
            .detail(search_err.clone())
            .into_any_element()
        } else {
            let weak_clear = weak.clone();
            EmptyState::new("graph-search-empty", IconName::Search, "未找到匹配的提交记录")
                .body(format!(
                    "全历史中没有提交匹配「{}」。支持正则表达式、Commit Body、作者、分支/Tag 或 4+ 位 Hash。",
                    tab.filter.search_query.trim()
                ))
                .action(
                    Button::new("clear-search-empty-btn", "清空搜索 (Esc)")
                        .variant(ButtonVariant::Secondary)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            weak_clear
                                .update(cx, |this, cx| this.clear_search(cx))
                                .ok();
                        }),
                )
                .into_any_element()
        }
    } else if total_rows == 0 && !tab.loading_graph {
        EmptyState::new(
            "graph-empty",
            IconName::GitCommitHorizontal,
            "没有匹配的提交记录",
        )
        .body("可以尝试放宽分支、作者或搜索关键词筛选条件。")
        .into_any_element()
    } else if tab.loading_graph && total_rows == 0 {
        div()
            .size_full()
            .px_4()
            .py_2()
            .overflow_hidden()
            .child(
                Shimmer::new(("graph-skeleton-shimmer", tab_id))
                    .child(SkeletonTable::new(("graph-skeleton", tab_id), 12, 5)),
            )
            .into_any_element()
    } else {
        // 预先将当前列表的只读快照打包给 uniform_list 闭包使用
        let commits: Rc<Vec<GitCommit>> = Rc::new(tab.visible_commits());
        let rows_layout: Rc<Vec<RowLayout>> = Rc::new(tab.layout.rows.clone());
        let repo_path = tab.path.clone();
        let loading_more = tab.loading_more;
        let weak_for_list = weak.clone();

        let list = uniform_list(
            ("git-graph-rows", tab_id),
            item_count,
            move |range, _window, cx| {
                if show_load_more
                    && !loading_more
                    && range.end + PREFETCH_TAIL >= total_rows
                {
                    let weak_more = weak_for_list.clone();
                    cx.defer(move |cx| {
                        weak_more.update(cx, |this, cx| this.load_more_commits(cx)).ok();
                    });
                }

                let theme = cx.theme();
                let colors = theme.colors.clone();
                let row_h = px(ROW_HEIGHT);

                range
                    .map(|ix| {
                        if ix >= commits.len() {
                            if show_load_more {
                                return render_load_more_row(
                                    loading_more,
                                    weak_for_list.clone(),
                                    row_h,
                                    cx,
                                );
                            }
                            return render_end_of_list_row(total_rows, row_h, cx);
                        }
                        let commit = &commits[ix];
                        let row_layout = rows_layout.get(ix).cloned().unwrap_or(RowLayout {
                            lane: 0,
                            color_index: 0,
                            node_kind: git_graph_layout::NodeKind::Normal,
                            is_head: false,
                            is_merge: false,
                            active_lanes: 1,
                            segments: Vec::new(),
                        });
                        let is_selected = selected_hash.as_deref() == Some(commit.hash.as_str());
                        let is_head_commit = head_hash.as_deref() == Some(commit.hash.as_str());
                        let is_uncommitted = commit.hash == UNCOMMITTED;
                        let is_merge = commit.parents.len() > 1 && !is_uncommitted;
                        let row_flash_serial = flash_commit
                            .as_ref()
                            .and_then(|(h, serial)| (h == &commit.hash).then_some(*serial));

                        let row_bg = if is_selected {
                            colors.accent.opacity(if dark { 0.18 } else { 0.10 })
                        } else if ix % 2 == 1 {
                            colors.sunken.opacity(0.32)
                        } else {
                            colors.bg
                        };

                        let badges = if is_uncommitted {
                            Vec::new()
                        } else {
                            let labels = commit_ref_labels(commit);
                            group_ref_badges(&labels, head_branch.as_deref())
                        };

                        let lane_accent = lane_color(row_layout.color_index, dark);
                        let has_overflow_left = graph_scroll_x > 0.5;
                        let has_overflow_right = max_graph_scroll - graph_scroll_x > 0.5;
                        let graph_cell_canvas = paint_graph_cell(
                            row_layout,
                            GraphCellStyle {
                                col_width: px(graph_col_w),
                                row_height: row_h,
                                lane_width: px(LANE_WIDTH),
                                scroll_x: graph_scroll_x,
                                overscroll_x,
                                has_overflow_left,
                                has_overflow_right,
                                dark,
                                row_bg: if is_selected { colors.bg } else { row_bg },
                                muted_color: colors.fg_subtle,
                                stash_color: colors.success,
                                border_color: colors.border.opacity(0.6),
                                overflow_hint_color: colors.accent.opacity(0.28),
                                overscroll_glow_color: colors.accent,
                            },
                        );
                        let weak_graph_scroll = weak_for_list.clone();
                        let graph_cell = div()
                            .id(("graph-cell", ix))
                            .flex_none()
                            .w(px(graph_col_w))
                            .h(row_h)
                            .on_scroll(move |event: &ScrollEvent, _, cx| {
                                let phase = event.touch_phase;
                                let precise = event.delta.precise();
                                let decision = weak_graph_scroll
                                    .update(cx, |this, _| this.classify_scroll_axis(event))
                                    .unwrap_or(ScrollAxisDecision::Neutral);
                                if let ScrollAxisDecision::Horizontal(dx) = decision {
                                    cx.stop_propagation();
                                    if dx.abs() > 0.01
                                        || matches!(
                                            phase,
                                            TouchPhase::Started
                                                | TouchPhase::Ended
                                                | TouchPhase::Cancelled
                                        )
                                    {
                                        weak_graph_scroll
                                            .update(cx, |this, cx| {
                                                this.handle_graph_horizontal_scroll(
                                                    dx, phase, precise, cx,
                                                );
                                            })
                                            .ok();
                                    }
                                } else if phase == TouchPhase::Started {
                                    weak_graph_scroll
                                        .update(cx, |this, cx| {
                                            this.handle_graph_horizontal_scroll(
                                                0.0, phase, precise, cx,
                                            );
                                        })
                                        .ok();
                                }
                            })
                            .child(graph_cell_canvas);

                        let desc_cell = render_description_cell(
                            tab_id,
                            ix,
                            commit,
                            &badges,
                            lane_accent,
                            is_uncommitted,
                            is_merge,
                            is_head_commit,
                            &repo_path,
                            &weak_for_list,
                            cx,
                        );

                        let date_text = if is_uncommitted {
                            "工作区".to_string()
                        } else {
                            format_timestamp(commit.date)
                        };
                        let author_text = if is_uncommitted {
                            "*".to_string()
                        } else {
                            commit.author.clone()
                        };
                        let hash_text = if is_uncommitted {
                            "*".to_string()
                        } else {
                            short_hash(&commit.hash).to_string()
                        };

                        let weak_click = weak_for_list.clone();
                        let weak_ctx = weak_for_list.clone();
                        let commit_for_click = commit.clone();
                        let commit_for_ctx = commit.clone();
                        let repo_for_ctx = repo_path.clone();

                        // 闪动定位行若本身为纯不透明背景，改为透明以透出外层 Flash 高亮淡出效果
                        let inner_bg = if row_flash_serial.is_some()
                            && !is_selected
                            && ix % 2 == 0
                        {
                            gpui::transparent_black()
                        } else {
                            row_bg
                        };

                        let row_el = div()
                            .id(("graph-row", ix))
                            .w_full()
                            .h(row_h)
                            .flex()
                            .items_center()
                            .bg(inner_bg)
                            .when(!is_selected, |r| {
                                r.hover(|s| s.bg(colors.hover))
                            })
                            .border_b_1()
                            .border_color(colors.border.opacity(0.45))
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                weak_click
                                    .update(cx, |this, cx| {
                                        this.select_commit(commit_for_click.hash.clone(), cx);
                                    })
                                    .ok();
                            })
                            .on_mouse_down(MouseButton::Right, move |event, _, cx| {
                                let target = commit_row_context_target(
                                    repo_for_ctx.clone(),
                                    &commit_for_ctx,
                                );
                                weak_ctx
                                    .update(cx, |this, cx| {
                                        this.open_context_menu(target, event.position, cx);
                                    })
                                    .ok();
                            })
                            .child(graph_cell)
                            .child(desc_cell)
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(widths.date))
                                    .px_2()
                                    .text_size(theme.text_size(TextSize::Xs))
                                    .text_color(colors.fg_muted)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(date_text),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(widths.author))
                                    .px_2()
                                    .text_size(theme.text_size(TextSize::Xs))
                                    .text_color(colors.fg_muted)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(author_text),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(widths.commit))
                                    .px_2()
                                    .font_family(theme.mono_family.clone())
                                    .text_size(theme.text_size(TextSize::Xs))
                                    .text_color(if is_head_commit {
                                        colors.accent
                                    } else {
                                        colors.fg_subtle
                                    })
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(hash_text),
                            );

                        // 仅为匹配 flash_commit 的单行叠加顶层淡出高亮层（首次挂载即刻触发且不被行背景遮挡）
                        if let Some(serial) = row_flash_serial {
                            let flash_color = colors.accent;
                            let peak_alpha = if dark { 0.34 } else { 0.22 };
                            let flash_dur = duration(Duration::from_millis(900), cx);
                            row_el
                                .relative()
                                .child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .with_animation(
                                            ("commit-flash-overlay", serial as usize),
                                            Animation::new(flash_dur).with_easing(ease_out_cubic),
                                            move |overlay, t| {
                                                let alpha = peak_alpha * (1.0 - t).max(0.0);
                                                overlay.bg(flash_color.opacity(alpha))
                                            },
                                        ),
                                )
                                .into_any_element()
                        } else {
                            row_el.into_any_element()
                        }
                    })
                    .collect()
            },
        )
        .track_scroll(&tab.graph_scroll)
        .size_full();

        let top_bounce = vertical_bounce.max(0.0);
        let bottom_bounce = (-vertical_bounce).max(0.0);
        let top_ratio = (top_bounce / 20.0).clamp(0.0, 1.0);
        let bottom_ratio = (bottom_bounce / 20.0).clamp(0.0, 1.0);
        let weak_list_scroll = weak.clone();

        div()
            .id(("graph-list-scroll-wrap", tab_id))
            .relative()
            .size_full()
            .overflow_hidden()
            .on_scroll(move |event: &ScrollEvent, _, cx| {
                weak_list_scroll
                    .update(cx, |this, cx| {
                        this.handle_graph_list_vertical_scroll(event, cx);
                    })
                    .ok();
            })
            .child(
                div()
                    .size_full()
                    .when(top_bounce > 0.2, |el| el.pt(px(top_bounce * 0.7)))
                    .when(bottom_bounce > 0.2, |el| el.pb(px(bottom_bounce * 0.7)))
                    .child(list),
            )
            .when(top_ratio > 0.01, |el| {
                el.child(render_vertical_overscroll_glow(true, top_ratio, colors.accent))
            })
            .when(bottom_ratio > 0.01, |el| {
                el.child(render_vertical_overscroll_glow(false, bottom_ratio, colors.accent))
            })
            .into_any_element()
    };

    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(colors.bg)
        .child(header)
        .child(div().flex_1().min_h_0().child(body))
        .into_any_element()
}

fn render_vertical_overscroll_glow(at_top: bool, ratio: f32, accent: Hsla) -> AnyElement {
    let glow_h = px(4.0 + 16.0 * ratio);
    let strong = div()
        .w_full()
        .h(glow_h * 0.45)
        .bg(accent.opacity(0.14 * ratio));
    let soft = div()
        .w_full()
        .h(glow_h * 0.55)
        .bg(accent.opacity(0.05 * ratio));

    let base = div()
        .absolute()
        .left_0()
        .right_0()
        .h(glow_h)
        .flex()
        .flex_col();

    if at_top {
        base.top_0().child(strong).child(soft).into_any_element()
    } else {
        base.bottom_0()
            .justify_end()
            .child(soft)
            .child(strong)
            .into_any_element()
    }
}

fn render_end_of_list_row(total_rows: usize, row_h: Pixels, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .id("graph-end-of-list-row")
        .w_full()
        .h(row_h)
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .text_size(theme.text_size(TextSize::Xs))
        .text_color(theme.colors.fg_subtle)
        .child(div().w(px(28.0)).h_px().bg(theme.colors.border.opacity(0.6)))
        .child(format!("已到底部 · 共 {total_rows} 条提交"))
        .child(div().w(px(28.0)).h_px().bg(theme.colors.border.opacity(0.6)))
        .into_any_element()
}

fn render_load_more_row(
    loading: bool,
    weak: gpui::WeakEntity<GitGraphApp>,
    row_h: Pixels,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .id("graph-load-more-row")
        .w_full()
        .h(row_h)
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.colors.sunken.opacity(0.4))
        .text_size(theme.text_size(TextSize::Xs))
        .text_color(theme.colors.fg_muted)
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            weak.update(cx, |this, cx| this.load_more_commits(cx)).ok();
        })
        .child(if loading {
            "正在加载更多提交…"
        } else {
            "点击或继续向下滚动以加载更多提交"
        })
        .into_any_element()
}

fn render_table_header(
    graph_col_w: f32,
    graph_scroll_x: f32,
    widths: &ColumnWidths,
    weak: gpui::WeakEntity<GitGraphApp>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    let header_h = px(26.0);

    let col_label = |text: &'static str| {
        div()
            .text_size(theme.text_size(TextSize::Xs))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(colors.fg_muted)
            .child(text)
    };

    let drag_handle = |id: &'static str, col: ResizeCol, start_width: f32| {
        let focus = colors.focus;
        let border = colors.border;
        div()
            .id(id)
            .group("col-resize")
            .flex_none()
            .w(px(6.0))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor(CursorStyle::ResizeLeftRight)
            .on_drag(
                ColumnDrag {
                    col,
                    start_width,
                    anchor_x: Rc::new(Cell::new(None)),
                },
                |_, _, _, cx| cx.new(|_| EmptyView),
            )
            .child(
                div()
                    .w_px()
                    .h(px(14.0))
                    .bg(border)
                    .group_hover("col-resize", move |s| s.bg(focus).h_full()),
            )
    };

    let graph_drag_handle = drag_handle("resize-col-graph", ResizeCol::Graph, graph_col_w)
        .absolute()
        .top_0()
        .bottom_0()
        .right(px(-3.0));

    let weak_header_scroll = weak.clone();
    let weak_reset_scroll = weak.clone();
    let graph_header_cell = div()
        .id("graph-col-header")
        .relative()
        .flex_none()
        .w(px(graph_col_w))
        .h_full()
        .flex()
        .items_center()
        .justify_between()
        .px_2()
        .on_scroll(move |event: &ScrollEvent, _, cx| {
            let phase = event.touch_phase;
            let precise = event.delta.precise();
            let decision = weak_header_scroll
                .update(cx, |this, _| this.classify_scroll_axis(event))
                .unwrap_or(ScrollAxisDecision::Neutral);
            if let ScrollAxisDecision::Horizontal(dx) = decision {
                cx.stop_propagation();
                if dx.abs() > 0.01
                    || matches!(
                        phase,
                        TouchPhase::Started | TouchPhase::Ended | TouchPhase::Cancelled
                    )
                {
                    weak_header_scroll
                        .update(cx, |this, cx| {
                            this.handle_graph_horizontal_scroll(dx, phase, precise, cx);
                        })
                        .ok();
                }
            } else if phase == TouchPhase::Started {
                weak_header_scroll
                    .update(cx, |this, cx| {
                        this.handle_graph_horizontal_scroll(0.0, phase, precise, cx);
                    })
                    .ok();
            }
        })
        .child(col_label("图形"))
        .when(graph_scroll_x > 0.5, |el| {
            let fg = colors.accent;
            let hover_bg = colors.hover;
            el.child(
                div()
                    .id("reset-graph-scroll-btn")
                    .px_1()
                    .rounded(theme.radius(Radius::Sm))
                    .text_size(theme.text_size(TextSize::Xs))
                    .text_color(fg)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        weak_reset_scroll
                            .update(cx, |this, cx| this.reset_graph_scroll_x(cx))
                            .ok();
                    })
                    .child("↺"),
            )
        })
        .child(graph_drag_handle);

    div()
        .id("graph-table-header")
        .w_full()
        .h(header_h)
        .flex_none()
        .flex()
        .items_center()
        .bg(colors.surface)
        .border_b_1()
        .border_color(colors.border)
        .on_drag_move(move |event: &DragMoveEvent<ColumnDrag>, _, cx| {
            let drag = event.drag(cx);
            let x = event.event.position.x;
            let anchor = match drag.anchor_x.get() {
                Some(a) => a,
                None => {
                    drag.anchor_x.set(Some(x));
                    x
                }
            };
            let delta = f32::from(x - anchor);
            let next_width = match drag.col {
                ResizeCol::Graph => (drag.start_width + delta)
                    .clamp(GRAPH_MANUAL_MIN_WIDTH, GRAPH_MANUAL_MAX_WIDTH),
                ResizeCol::Date => (drag.start_width - delta).clamp(96.0, 240.0),
                ResizeCol::Author => (drag.start_width - delta).clamp(80.0, 240.0),
                ResizeCol::Commit => (drag.start_width - delta).clamp(64.0, 140.0),
            };
            let col = drag.col;
            weak.update(cx, |this, cx| {
                this.set_column_width(col, next_width, cx);
            })
            .ok();
        })
        .child(graph_header_cell)
        .child(
            div()
                .flex_1()
                .min_w(px(180.0))
                .px_2()
                .child(col_label("描述")),
        )
        .child(drag_handle("resize-col-date", ResizeCol::Date, widths.date))
        .child(
            div()
                .flex_none()
                .w(px(widths.date))
                .px_2()
                .child(col_label("日期")),
        )
        .child(drag_handle("resize-col-author", ResizeCol::Author, widths.author))
        .child(
            div()
                .flex_none()
                .w(px(widths.author))
                .px_2()
                .child(col_label("作者")),
        )
        .child(drag_handle("resize-col-commit", ResizeCol::Commit, widths.commit))
        .child(
            div()
                .flex_none()
                .w(px(widths.commit))
                .px_2()
                .child(col_label("提交")),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_description_cell(
    tab_id: u64,
    row_ix: usize,
    commit: &GitCommit,
    badges: &[RefBadge],
    lane_accent: Hsla,
    is_uncommitted: bool,
    is_merge: bool,
    is_head_commit: bool,
    repo_path: &std::path::Path,
    weak: &gpui::WeakEntity<GitGraphApp>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;

    let msg_color = if is_uncommitted {
        colors.warning
    } else if is_merge {
        colors.fg_muted
    } else {
        colors.fg
    };

    let msg_weight = if is_head_commit || is_uncommitted {
        FontWeight::SEMIBOLD
    } else {
        FontWeight::NORMAL
    };

    let weak_desc_scroll = weak.clone();
    div()
        .id(("desc-cell", desc_scroll_id(tab_id, row_ix)))
        .flex_1()
        .min_w(px(180.0))
        .h_full()
        .overflow_hidden()
        .border_r_1()
        .border_color(colors.border.opacity(0.35))
        .on_scroll(move |event: &ScrollEvent, _, cx| {
            // 内层 desc-scroll 已在冒泡前完成 X 轴滚动；
            // 若当前手势判定为横向（含轻微斜向抖动与惯性尾帧），在此截停冒泡，
            // 防止残留的微弱 dy 传递到外层 uniform_list 引起画面上下晃动。
            let decision = weak_desc_scroll
                .update(cx, |this, _| this.classify_scroll_axis(event))
                .unwrap_or(ScrollAxisDecision::Neutral);
            if matches!(decision, ScrollAxisDecision::Horizontal(_)) {
                cx.stop_propagation();
            }
        })
        .child(
            div()
                .id(("desc-scroll", desc_scroll_id(tab_id, row_ix)))
                .w_full()
                .h_full()
                .px_2()
                .flex()
                .items_center()
                .gap_1p5()
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .children(badges.iter().enumerate().map(|(b_ix, badge)| {
                    render_ref_badge(
                        row_ix,
                        b_ix,
                        badge,
                        lane_accent,
                        commit,
                        repo_path,
                        weak,
                        cx,
                    )
                }))
                .child(
                    div()
                        .flex_none()
                        .pr_3()
                        .text_size(theme.text_size(TextSize::Sm))
                        .font_weight(msg_weight)
                        .text_color(msg_color)
                        .whitespace_nowrap()
                        .child(commit.message.clone()),
                ),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_ref_badge(
    row_ix: usize,
    badge_ix: usize,
    badge: &RefBadge,
    lane_accent: Hsla,
    commit: &GitCommit,
    repo_path: &std::path::Path,
    weak: &gpui::WeakEntity<GitGraphApp>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    let dark = theme.is_dark();
    let radius = theme.radius(Radius::Sm);

    match badge {
        RefBadge::Branch {
            name,
            remotes,
            is_head,
        } => {
            let bg = if *is_head {
                lane_accent.opacity(if dark { 0.28 } else { 0.16 })
            } else {
                colors.info_subtle
            };
            let border = if *is_head {
                lane_accent
            } else {
                colors.info.opacity(0.45)
            };
            let fg = if *is_head { colors.fg } else { colors.info };
            let branch_name = name.clone();
            let branch_is_head = *is_head;
            let repo = repo_path.to_path_buf();
            let weak_ctx = weak.clone();

            div()
                .id(("badge-branch", row_ix * 64 + badge_ix))
                .flex_none()
                .flex()
                .items_center()
                .h(px(19.0))
                .rounded(radius)
                .border_1()
                .border_color(border)
                .bg(bg)
                .text_size(theme.text_size(TextSize::Xs))
                .font_weight(if *is_head {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(fg)
                .overflow_hidden()
                .on_mouse_down(MouseButton::Right, move |event, _, cx| {
                    cx.stop_propagation();
                    weak_ctx
                        .update(cx, |this, cx| {
                            this.open_context_menu(
                                ContextMenuTarget::LocalBranch {
                                    repo_path: repo.clone(),
                                    branch: branch_name.clone(),
                                    is_head: branch_is_head,
                                },
                                event.position,
                                cx,
                            );
                        })
                        .ok();
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_1p5()
                        .when(*is_head, |el| {
                            el.child(div().size(px(5.0)).rounded_full().bg(lane_accent))
                        })
                        .child(
                            Icon::new(IconName::GitBranch)
                                .size(IconSize::Xs)
                                .color(if *is_head { lane_accent } else { fg }),
                        )
                        .child(name.clone()),
                )
                .when(!remotes.is_empty(), |pill| {
                    pill.child(div().w_px().h_full().bg(border.opacity(0.6))).child(
                        div()
                            .px_1p5()
                            .text_color(colors.fg_muted)
                            .child(remotes.join(", ")),
                    )
                })
                .into_any_element()
        }
        RefBadge::RemoteBranch {
            remote,
            name,
            is_remote_head,
        } => {
            let label = if remote.is_empty() {
                name.clone()
            } else {
                format!("{remote}/{name}")
            };
            let repo = repo_path.to_path_buf();
            let remote_name = remote.clone();
            let branch_name = name.clone();
            let full_name = label.clone();
            let remote_head = *is_remote_head;
            let weak_ctx = weak.clone();
            div()
                .id(("badge-remote", row_ix * 64 + badge_ix))
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .h(px(19.0))
                .px_1p5()
                .rounded(radius)
                .border_1()
                .border_color(colors.border_strong)
                .bg(colors.sunken)
                .text_size(theme.text_size(TextSize::Xs))
                .font_weight(if *is_remote_head {
                    FontWeight::MEDIUM
                } else {
                    FontWeight::NORMAL
                })
                .text_color(colors.fg_muted)
                .on_mouse_down(MouseButton::Right, move |event, _, cx| {
                    cx.stop_propagation();
                    weak_ctx
                        .update(cx, |this, cx| {
                            this.open_context_menu(
                                ContextMenuTarget::RemoteBranch {
                                    repo_path: repo.clone(),
                                    remote: remote_name.clone(),
                                    branch_name: branch_name.clone(),
                                    full_name: full_name.clone(),
                                    is_remote_head: remote_head,
                                },
                                event.position,
                                cx,
                            );
                        })
                        .ok();
                })
                .child(label)
                .into_any_element()
        }
        RefBadge::Tag { name, annotated } => {
            let tag_name = name.clone();
            let repo = repo_path.to_path_buf();
            let weak_ctx = weak.clone();
            div()
                .id(("badge-tag", row_ix * 64 + badge_ix))
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .h(px(19.0))
                .px_1p5()
                .rounded(radius)
                .border_1()
                .border_color(colors.warning.opacity(0.55))
                .bg(colors.warning_subtle)
                .text_size(theme.text_size(TextSize::Xs))
                .font_weight(if *annotated {
                    FontWeight::MEDIUM
                } else {
                    FontWeight::NORMAL
                })
                .text_color(colors.warning)
                .on_mouse_down(MouseButton::Right, move |event, _, cx| {
                    cx.stop_propagation();
                    weak_ctx
                        .update(cx, |this, cx| {
                            this.open_context_menu(
                                ContextMenuTarget::Tag {
                                    repo_path: repo.clone(),
                                    name: tag_name.clone(),
                                },
                                event.position,
                                cx,
                            );
                        })
                        .ok();
                })
                .child(Icon::new(IconName::Tag).size(IconSize::Xs).color(colors.warning))
                .child(name.clone())
                .into_any_element()
        }
        RefBadge::Stash { selector } => {
            let repo = repo_path.to_path_buf();
            let stash_selector = selector.clone();
            let stash_index = parse_stash_index(selector).unwrap_or(0);
            let stash_hash = commit.hash.clone();
            let weak_ctx = weak.clone();
            div()
                .id(("badge-stash", row_ix * 64 + badge_ix))
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .h(px(19.0))
                .px_1p5()
                .rounded(radius)
                .border_1()
                .border_color(colors.success.opacity(0.5))
                .bg(colors.success_subtle)
                .text_size(theme.text_size(TextSize::Xs))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors.success)
                .on_mouse_down(MouseButton::Right, move |event, _, cx| {
                    cx.stop_propagation();
                    weak_ctx
                        .update(cx, |this, cx| {
                            this.open_context_menu(
                                ContextMenuTarget::Stash {
                                    repo_path: repo.clone(),
                                    selector: stash_selector.clone(),
                                    index: stash_index,
                                    hash: stash_hash.clone(),
                                },
                                event.position,
                                cx,
                            );
                        })
                        .ok();
                })
                .child(
                    Icon::new(IconName::Archive)
                        .size(IconSize::Xs)
                        .color(colors.success),
                )
                .child(selector.clone())
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use git_graph_core::api::Engine;

    use super::*;

    fn init_empty_repo() -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = Command::new("git")
            .current_dir(dir.path())
            .args(["init", "-b", "main"])
            .output()
            .expect("git init");
        assert!(out.status.success());
        let engine = Engine::open(dir.path()).expect("open engine");
        (dir, engine)
    }

    #[test]
    fn effective_graph_width_adapts_and_respects_manual_override() {
        let (_dir, engine) = init_empty_repo();
        let mut tab = RepoTabState::new(1, engine);
        let mut widths = ColumnWidths::default();
        assert!(!widths.graph_manual);

        // 0 或 1 泳道：clamp 到下限 76.0
        tab.layout.max_lanes = 0;
        assert_eq!(effective_graph_width(&tab, &widths), 76.0);
        tab.layout.max_lanes = 1;
        assert_eq!(effective_graph_width(&tab, &widths), 76.0);

        // 5 泳道：5 * 16 + 24 = 104.0
        tab.layout.max_lanes = 5;
        assert_eq!(effective_graph_width(&tab, &widths), 104.0);

        // 超多泳道（30 泳道）：自适应上限 clamp 在 260.0
        tab.layout.max_lanes = 30;
        assert_eq!(effective_graph_width(&tab, &widths), 260.0);

        // 用户手动拖窄至 88.0：尊重手动设置
        widths.graph_manual = true;
        widths.graph = 88.0;
        assert_eq!(effective_graph_width(&tab, &widths), 88.0);

        // 手动宽度的极窄/极宽边界安全 clamp 在 [72.0, 360.0]
        widths.graph = 20.0;
        assert_eq!(effective_graph_width(&tab, &widths), 72.0);
        widths.graph = 600.0;
        assert_eq!(effective_graph_width(&tab, &widths), 360.0);
    }

    #[test]
    fn graph_horizontal_scroll_bounds() {
        let (_dir, engine) = init_empty_repo();
        let mut tab = RepoTabState::new(1, engine);

        // 5 条泳道内容宽 104px，列宽 120px 时不可滚动
        tab.layout.max_lanes = 5;
        assert_eq!(graph_content_width(5), 104.0);
        assert_eq!(max_graph_scroll_x(&tab, 120.0), 0.0);

        // 25 条泳道内容宽 424px，列宽 260px 时最大可滚 164px
        tab.layout.max_lanes = 25;
        assert_eq!(graph_content_width(25), 424.0);
        assert_eq!(max_graph_scroll_x(&tab, 260.0), 164.0);
    }

    #[test]
    fn horizontal_scroll_latch_locks_axis_across_jitter_and_resets_on_new_gesture() {
        use gpui::{Modifiers, ScrollDelta, point};

        let mut latch = HorizontalScrollLatch::default();
        let t0 = Instant::now();

        // 1. 触控板开始横向右滑（附带少量垂直分量 dy=2.5）-> 锁定横向
        let start = ScrollEvent {
            delta: ScrollDelta::Pixels(point(px(-14.0), px(2.5))),
            touch_phase: TouchPhase::Started,
            ..Default::default()
        };
        assert_eq!(
            latch.classify_at(&start, t0),
            ScrollAxisDecision::Horizontal(-14.0)
        );

        // 2. 滑动中段手指轻微抖斜（dy 略大于 dx 但未达到大幅转竖滑阈值）-> 仍锁死横向，不让外层列表上下动
        let jitter = ScrollEvent {
            delta: ScrollDelta::Pixels(point(px(-3.0), px(4.5))),
            touch_phase: TouchPhase::Moved,
            ..Default::default()
        };
        assert_eq!(
            latch.classify_at(&jitter, t0 + Duration::from_millis(16)),
            ScrollAxisDecision::Horizontal(-3.0)
        );

        // 3. 手指抬起 (Ended) 及紧随其后的惯性滑行帧 -> 全程保持横向锁定不串滚
        let ended = ScrollEvent {
            delta: ScrollDelta::Pixels(point(px(-1.0), px(2.0))),
            touch_phase: TouchPhase::Ended,
            ..Default::default()
        };
        assert_eq!(
            latch.classify_at(&ended, t0 + Duration::from_millis(32)),
            ScrollAxisDecision::Horizontal(-1.0)
        );
        let momentum = ScrollEvent {
            delta: ScrollDelta::Pixels(point(px(-0.8), px(1.5))),
            touch_phase: TouchPhase::Moved,
            ..Default::default()
        };
        assert_eq!(
            latch.classify_at(&momentum, t0 + Duration::from_millis(48)),
            ScrollAxisDecision::Horizontal(-0.8)
        );

        // 4. 新手势开始竖向滚动 -> 立即释放旧锁，判定为 Vertical 放行给 uniform_list
        let next_vert = ScrollEvent {
            delta: ScrollDelta::Pixels(point(px(1.0), px(-20.0))),
            touch_phase: TouchPhase::Started,
            ..Default::default()
        };
        assert_eq!(
            latch.classify_at(&next_vert, t0 + Duration::from_millis(64)),
            ScrollAxisDecision::Vertical
        );

        // 5. Shift + 鼠标滚轮 -> 始终判定为横向
        let shift_mouse = ScrollEvent {
            delta: ScrollDelta::Lines(point(0.0, -2.0)),
            modifiers: Modifiers {
                shift: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            latch.classify_at(&shift_mouse, t0 + Duration::from_millis(80)),
            ScrollAxisDecision::Horizontal(_)
        ));
    }

    #[test]
    fn sync_button_spec_maps_all_branch_sync_states() {
        // 1. Synced(0, 0) -> 状态栏显示 ↓0 ↑0
        let clean = BranchSyncStatus::Synced {
            branch: "main".to_string(),
            upstream: "origin/main".to_string(),
            ahead: 0,
            behind: 0,
        };
        let clean_spec = sync_button_spec(Some(&clean), false, None);
        assert_eq!(clean_spec.status_bar_label, "↓0 ↑0");
        assert!(!clean_spec.disabled);
        assert_eq!(clean_spec.action, SyncButtonAction::Sync);

        // 2. Synced(ahead=2, behind=1) -> 状态栏清晰显示 ↓1 ↑2（图标由 IconName::RefreshCw 提供，不重复）
        let diverged = BranchSyncStatus::Synced {
            branch: "main".to_string(),
            upstream: "origin/main".to_string(),
            ahead: 2,
            behind: 1,
        };
        let div_spec = sync_button_spec(Some(&diverged), false, None);
        assert_eq!(div_spec.status_bar_label, "↓1 ↑2");
        assert!(!div_spec.disabled);
        assert!(div_spec.tooltip.contains("origin/main"));
        assert_eq!(div_spec.action, SyncButtonAction::Sync);

        // 3. NoUpstream 有默认 remote -> 显示发布分支
        let no_up = BranchSyncStatus::NoUpstream {
            branch: "feat/sync".to_string(),
            default_remote: Some("origin".to_string()),
        };
        let pub_spec = sync_button_spec(Some(&no_up), false, None);
        assert_eq!(pub_spec.status_bar_label, "发布分支");
        assert!(!pub_spec.disabled);
        assert_eq!(
            pub_spec.action,
            SyncButtonAction::Publish {
                remote: "origin".to_string(),
                branch: "feat/sync".to_string(),
            }
        );

        // 4. NoUpstream 无 remote -> 置灰
        let no_remote = BranchSyncStatus::NoUpstream {
            branch: "feat/local".to_string(),
            default_remote: None,
        };
        let no_rem_spec = sync_button_spec(Some(&no_remote), false, None);
        assert!(no_rem_spec.disabled);
        assert_eq!(no_rem_spec.tooltip, "尚未配置远程仓库");
        assert_eq!(no_rem_spec.action, SyncButtonAction::None);

        // 5. DetachedHead -> 置灰
        let detached = sync_button_spec(Some(&BranchSyncStatus::DetachedHead), false, None);
        assert!(detached.disabled);
        assert!(detached.tooltip.contains("分离 HEAD"));
        assert_eq!(detached.action, SyncButtonAction::None);

        // 6. syncing = true -> 禁用并显示进度文案
        let busy = sync_button_spec(Some(&diverged), true, Some("发布中…"));
        assert!(busy.disabled);
        assert_eq!(busy.status_bar_label, "发布中…");
        assert_eq!(busy.action, SyncButtonAction::None);
    }

    #[test]
    fn commit_row_context_target_dispatches_uncommitted_stash_and_commit() {
        use git_graph_core::types::GitCommitStash;

        let repo = std::path::PathBuf::from("/tmp/repo");
        let base_commit = GitCommit {
            hash: "a52c6961234567890abcdef".to_string(),
            parents: vec![],
            author: "Alice".to_string(),
            email: "alice@example.com".to_string(),
            date: 1_700_000_000,
            message: "init".to_string(),
            heads: vec!["main".to_string()],
            tags: vec![],
            remotes: vec![],
            stash: None,
        };

        let uncommitted = GitCommit {
            hash: UNCOMMITTED.to_string(),
            ..base_commit.clone()
        };
        assert!(matches!(
            commit_row_context_target(repo.clone(), &uncommitted),
            ContextMenuTarget::Uncommitted { .. }
        ));

        let stash_commit = GitCommit {
            stash: Some(GitCommitStash {
                selector: "stash@{1}".to_string(),
                base_hash: "1111111".to_string(),
                untracked_files_hash: None,
            }),
            ..base_commit.clone()
        };
        assert!(matches!(
            commit_row_context_target(repo.clone(), &stash_commit),
            ContextMenuTarget::Stash {
                selector,
                index: 1,
                ..
            } if selector == "stash@{1}"
        ));

        assert!(matches!(
            commit_row_context_target(repo, &base_commit),
            ContextMenuTarget::Commit { commit, .. } if commit.hash == base_commit.hash
        ));
    }
}
