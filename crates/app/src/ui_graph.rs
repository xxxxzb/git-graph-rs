//! 上半区视图：顶部筛选工具条、可调宽 5 列表头、虚拟滚动 Git Graph 宽表与 RefBadge 胶囊。

use std::cell::Cell;
use std::rc::Rc;

use ely_core::buttons::{Button, ButtonVariant, IconButton};
use ely_core::forms::{Checkbox, Choice, SearchInput, Select};
use ely_core::primitives::{Icon, IconName};
use ely_core::shell::{Toolbar, ToolbarGroup, ToolbarSeparator};
use ely_core::theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize};
use git_graph_core::types::{GitCommit, UNCOMMITTED};
use git_graph_layout::{RefBadge, RowLayout, group_ref_badges};
use gpui::{
    AnyElement, App, Context, CursorStyle, DragMoveEvent, EmptyView, FontWeight, Hsla,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px, uniform_list,
};

use crate::app_view::GitGraphApp;
use crate::graph_canvas::{GraphCellStyle, lane_color, paint_graph_cell};
use crate::repo_tab::RepoTabState;
use crate::state::{commit_ref_labels, format_timestamp, short_hash};

/// 行高：28px 经典高密度 Git Graph 列表行。
const ROW_HEIGHT: f32 = 28.0;
/// 每个泳道宽度。
const LANE_WIDTH: f32 = 16.0;
/// 距列表底部多少行时自动触发加载下一页。
const PREFETCH_TAIL: usize = 24;

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

    // 搜索框
    let search_box = div().w(px(220.0)).child(
        SearchInput::new("graph-search", &app.search_input).size(ControlSize::Sm),
    );

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

    let fetch_btn = Button::new(
        "fetch-remote-btn",
        if tab.fetching { "Fetch 中…" } else { "Fetch" },
    )
    .icon(IconName::Download)
    .variant(ButtonVariant::Secondary)
    .size(ControlSize::Sm)
    .disabled(tab.fetching)
    .on_click({
        let weak = weak.clone();
        move |_, _, cx| {
            weak.update(cx, |this, cx| this.run_fetch_active(cx)).ok();
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
                        .child(search_box),
                )
                .child(
                    ToolbarGroup::new()
                        .child(jump_head_btn)
                        .child(toggle_details_btn)
                        .child(refresh_btn)
                        .child(ToolbarSeparator)
                        .child(fetch_btn),
                ),
        )
        .into_any_element()
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
    let weak = cx.weak_entity();

    let header = render_table_header(&widths, weak.clone(), cx);

    let total_rows = tab.filtered_indices.len();
    let show_load_more = tab.more_available && tab.filter.search_query.trim().is_empty();
    let item_count = total_rows + usize::from(show_load_more);

    let tab_id = tab.id;
    let dark = theme.is_dark();
    let head_branch = tab.info.as_ref().and_then(|i| i.head.clone());
    let head_hash = tab.head_hash.clone();
    let selected_hash = tab.selected_hash.clone();

    let body: AnyElement = if let Some(err) = &tab.error {
        ely_core::feedback::ErrorView::new("graph-load-error", IconName::TriangleAlert, "图谱加载失败")
            .detail(err.clone())
            .into_any_element()
    } else if total_rows == 0 && !tab.loading_graph {
        ely_core::feedback::EmptyState::new(
            "graph-empty",
            IconName::GitCommitHorizontal,
            "没有匹配的提交记录",
        )
        .body("可以尝试放宽分支、作者或搜索关键词筛选条件。")
        .into_any_element()
    } else {
        // 预先将当前列表的只读快照打包给 uniform_list 闭包使用
        let commits: Rc<Vec<GitCommit>> = Rc::new(
            tab.filtered_indices
                .iter()
                .filter_map(|&idx| tab.all_commits.get(idx).cloned())
                .collect(),
        );
        let rows_layout: Rc<Vec<RowLayout>> = Rc::new(tab.layout.rows.clone());
        let repo_path = tab.path.clone();
        let loading_more = tab.loading_more;
        let max_lanes = tab.layout.max_lanes.max(1);
        let auto_graph_min = (max_lanes as f32 * LANE_WIDTH + 22.0).max(72.0);
        let graph_col_w = widths.graph.max(auto_graph_min.min(widths.graph.max(96.0)));

        uniform_list(
            ("git-graph-rows", tab_id),
            item_count,
            move |range, _window, cx| {
                if show_load_more
                    && !loading_more
                    && range.end + PREFETCH_TAIL >= total_rows
                {
                    let weak_more = weak.clone();
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
                            return render_load_more_row(loading_more, weak.clone(), row_h, cx);
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
                        let graph_cell = paint_graph_cell(
                            row_layout,
                            GraphCellStyle {
                                col_width: px(graph_col_w),
                                row_height: row_h,
                                lane_width: px(LANE_WIDTH),
                                dark,
                                row_bg: if is_selected { colors.bg } else { row_bg },
                                muted_color: colors.fg_subtle,
                                stash_color: colors.success,
                            },
                        );

                        let desc_cell = render_description_cell(
                            ix,
                            commit,
                            &badges,
                            lane_accent,
                            is_uncommitted,
                            is_merge,
                            is_head_commit,
                            &repo_path,
                            &weak,
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

                        let weak_click = weak.clone();
                        let weak_ctx = weak.clone();
                        let commit_for_click = commit.clone();
                        let commit_for_ctx = commit.clone();
                        let repo_for_ctx = repo_path.clone();

                        div()
                            .id(("graph-row", ix))
                            .w_full()
                            .h(row_h)
                            .flex()
                            .items_center()
                            .bg(row_bg)
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
                                weak_ctx
                                    .update(cx, |this, cx| {
                                        this.open_commit_context_menu(
                                            repo_for_ctx.clone(),
                                            commit_for_ctx.clone(),
                                            None,
                                            None,
                                            event.position,
                                            cx,
                                        );
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
                            )
                            .into_any_element()
                    })
                    .collect()
            },
        )
        .track_scroll(&tab.graph_scroll)
        .size_full()
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
    widths: &crate::state::ColumnWidths,
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
                ResizeCol::Graph => (drag.start_width + delta).clamp(72.0, 360.0),
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
        .child(
            div()
                .flex_none()
                .w(px(widths.graph))
                .px_2()
                .child(col_label("图形")),
        )
        .child(drag_handle("resize-col-graph", ResizeCol::Graph, widths.graph))
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

    div()
        .flex_1()
        .min_w(px(180.0))
        .px_2()
        .flex()
        .items_center()
        .gap_1p5()
        .overflow_hidden()
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
                .flex_1()
                .min_w_0()
                .text_size(theme.text_size(TextSize::Sm))
                .font_weight(msg_weight)
                .text_color(msg_color)
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(commit.message.clone()),
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
            let repo = repo_path.to_path_buf();
            let commit_clone = commit.clone();
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
                            this.open_commit_context_menu(
                                repo.clone(),
                                commit_clone.clone(),
                                Some(branch_name.clone()),
                                None,
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
                .child(label)
                .into_any_element()
        }
        RefBadge::Tag { name, annotated } => {
            let tag_name = name.clone();
            let repo = repo_path.to_path_buf();
            let commit_clone = commit.clone();
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
                            this.open_commit_context_menu(
                                repo.clone(),
                                commit_clone.clone(),
                                None,
                                Some(tag_name.clone()),
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
        RefBadge::Stash { selector } => div()
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
            .child(
                Icon::new(IconName::Archive)
                    .size(IconSize::Xs)
                    .color(colors.success),
            )
            .child(selector.clone())
            .into_any_element(),
    }
}
