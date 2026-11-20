//! 下半区详情与 Diff 面板：左侧元数据/变更文件列表（含未提交 Stage/Unstage/Discard），右侧 DiffViewer/BeforeAfter。

use std::rc::Rc;

use ely_core::buttons::{Button, ButtonVariant, IconButton, SegmentedControl};
use ely_core::data_display::{BeforeAfter, DescriptionList};
use ely_core::feedback::{EmptyState, ErrorView};
use ely_core::layout::SplitPane;
use ely_core::lists::GitStatus;
use ely_core::motion::{
    BASE, NUDGE, Shimmer, SkeletonTable, SkeletonText, duration, ease_out_cubic,
};
use ely_core::primitives::{IconName, Image};
use ely_core::theme::{ActiveTheme, ControlSize, Radius, TextSize};
use ely_core::typography::{SelectableText, format};
use ely_git::{DiffLayout, DiffStat, DiffViewer, GitStatusBadge};
use git_graph_core::status::ScmChange;
use git_graph_core::types::{GitCommitDetails, GitFileChange, GitFileStatus, UNCOMMITTED};
use gpui::{
    Animation, AnimationExt, AnyElement, App, Axis, Context, FontWeight, InteractiveElement,
    IntoElement, ObjectFit, ParentElement, SharedString, StatefulInteractiveElement, Styled, div,
    prelude::*, px,
};

use crate::app_view::GitGraphApp;
use crate::repo_tab::{FileDiffContent, FileSection, RepoTabState, SelectedFile};
use crate::state::{format_timestamp_detailed, short_hash};

/// 渲染下半区（水平 SplitPane：左元数据与文件列表，右 Diff 预览区）。
pub fn render_details_pane(
    app: &GitGraphApp,
    tab: &RepoTabState,
    cx: &mut Context<GitGraphApp>,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let weak = cx.weak_entity();

    let anim_duration = duration(BASE, cx);
    let wrap_transition = |content: gpui::Div| -> AnyElement {
        if app.details_open_serial == 0 {
            content.into_any_element()
        } else {
            content
                .with_animation(
                    (
                        "details-transition",
                        tab.id
                            .wrapping_mul(10_007)
                            .wrapping_add(app.details_open_serial),
                    ),
                    Animation::new(anim_duration).with_easing(ease_out_cubic),
                    |el, t| el.opacity(t).pt(NUDGE * 2.0 * (1.0 - t)),
                )
                .into_any_element()
        }
    };

    let Some(selected_hash) = &tab.selected_hash else {
        return wrap_transition(
            div()
                .size_full()
                .bg(colors.surface)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    EmptyState::new(
                        "details-none-selected",
                        IconName::GitCommitHorizontal,
                        "选择一个提交以查看详情与文件变更",
                    )
                    .body("点击上方图谱中的任意提交行或「未提交的更改」行。"),
                ),
        );
    };

    let left_pane = render_left_pane(tab, selected_hash, cx);
    let right_pane = render_right_pane(app, tab, cx);

    let split_sizes = app.persisted.horizontal_split;

    let scale = app.ui_scale();
    wrap_transition(
        div().size_full().bg(colors.surface).child(
            SplitPane::new("details-h-split", Axis::Horizontal, px(240.0 * scale))
                .pane(left_pane)
                .pane(right_pane)
                .sizes(&split_sizes)
                .on_resize(move |sizes, _, cx| {
                    if sizes.len() == 2 {
                        let pair = [sizes[0], sizes[1]];
                        weak.update(cx, |this, _| {
                            this.set_horizontal_split(pair);
                        })
                        .ok();
                    }
                }),
        ),
    )
}

fn render_left_pane(
    tab: &RepoTabState,
    selected_hash: &str,
    cx: &mut Context<GitGraphApp>,
) -> AnyElement {
    let colors = cx.theme().colors.clone();

    if selected_hash == UNCOMMITTED {
        return render_uncommitted_left(tab, cx);
    }

    if tab.loading_details && tab.details.is_none() {
        return div()
            .size_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_4()
            .overflow_hidden()
            .child(
                Shimmer::new(("detail-meta-shimmer", tab.id))
                    .child(SkeletonText::new("detail-meta-skel", 4)),
            )
            .child(div().border_t_1().border_color(colors.border))
            .child(
                Shimmer::new(("detail-files-shimmer", tab.id))
                    .child(SkeletonTable::new("detail-files-skel", 4, 2)),
            )
            .into_any_element();
    }

    match &tab.details {
        None => div()
            .size_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_4()
            .overflow_hidden()
            .child(
                Shimmer::new(("detail-meta-shimmer", tab.id))
                    .child(SkeletonText::new("detail-meta-skel", 4)),
            )
            .child(div().border_t_1().border_color(colors.border))
            .child(
                Shimmer::new(("detail-files-shimmer", tab.id))
                    .child(SkeletonTable::new("detail-files-skel", 4, 2)),
            )
            .into_any_element(),
        Some(Err(err)) => ErrorView::new("commit-detail-err", IconName::TriangleAlert, "无法读取提交详情")
            .detail(err.clone())
            .into_any_element(),
        Some(Ok(details)) => render_commit_details_left(tab, details, cx),
    }
}

fn render_uncommitted_left(tab: &RepoTabState, cx: &mut Context<GitGraphApp>) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let weak = cx.weak_entity();
    let repo_path = tab.path.clone();

    let staged: Vec<&ScmChange> = tab
        .scm_changes
        .iter()
        .filter(|c| c.staged.is_some())
        .collect();
    let unstaged: Vec<&ScmChange> = tab
        .scm_changes
        .iter()
        .filter(|c| c.unstaged.is_some() || c.untracked || c.conflicted)
        .collect();

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(colors.border)
        .child(
            div()
                .text_size(theme.text_size(TextSize::Base))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.fg)
                .child(format!("未提交的更改 ({})", tab.scm_changes.len())),
        )
        .child(
            Button::new("uncommitted-stash-btn", "保存 Stash…")
                .icon(IconName::Archive)
                .variant(ButtonVariant::Secondary)
                .size(ControlSize::Md)
                .disabled(tab.scm_changes.is_empty())
                .on_click({
                    let weak = weak.clone();
                    let repo = repo_path.clone();
                    move |_, window, cx| {
                        weak.update(cx, |this, cx| {
                            this.open_stash_push_modal(repo.clone(), window, cx);
                        })
                        .ok();
                    }
                }),
        );

    let staged_rows = staged.iter().enumerate().map(|(ix, change)| {
        render_scm_row(
            ix,
            change,
            FileSection::Staged,
            tab.selected_file.as_ref(),
            &repo_path,
            &weak,
            cx,
        )
    });

    let unstaged_rows = unstaged.iter().enumerate().map(|(ix, change)| {
        render_scm_row(
            ix,
            change,
            FileSection::Unstaged,
            tab.selected_file.as_ref(),
            &repo_path,
            &weak,
            cx,
        )
    });

    let section_title = |title: String| {
        div()
            .px_3()
            .py_1p5()
            .bg(colors.sunken.opacity(0.6))
            .text_size(theme.text_size(TextSize::Sm))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(colors.fg_muted)
            .child(title)
    };

    div()
        .size_full()
        .flex()
        .flex_col()
        .child(header)
        .child(
            div()
                .id("uncommitted-file-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(section_title(format!("已暂存的更改 ({})", staged.len())))
                .when(staged.is_empty(), |el| {
                    el.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_size(theme.text_size(TextSize::Xs))
                            .text_color(colors.fg_subtle)
                            .child("暂无已暂存文件"),
                    )
                })
                .children(staged_rows)
                .child(section_title(format!("未暂存的更改 ({})", unstaged.len())))
                .when(unstaged.is_empty(), |el| {
                    el.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_size(theme.text_size(TextSize::Xs))
                            .text_color(colors.fg_subtle)
                            .child("工作区干净"),
                    )
                })
                .children(unstaged_rows),
        )
        .into_any_element()
}

fn render_scm_row(
    ix: usize,
    change: &ScmChange,
    section: FileSection,
    selected_file: Option<&SelectedFile>,
    repo_path: &std::path::Path,
    weak: &gpui::WeakEntity<GitGraphApp>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    let status = scm_to_git_status(change, section);
    let is_selected = selected_file
        .is_some_and(|s| s.path == change.path && s.section == section);

    let file_sel = SelectedFile {
        path: change.path.clone(),
        old_path: change
            .old_path
            .clone()
            .unwrap_or_else(|| change.path.clone()),
        section,
    };

    let row_id = (
        match section {
            FileSection::Staged => "scm-staged",
            FileSection::Unstaged => "scm-unstaged",
            FileSection::Committed => "scm-committed",
        },
        ix,
    );

    let actions: AnyElement = match section {
        FileSection::Staged => {
            let weak = weak.clone();
            let repo = repo_path.to_path_buf();
            let path = change.path.clone();
            IconButton::new(("unstage-btn", ix), IconName::Undo2)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .tooltip("取消暂存 (Unstage)")
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    weak.update(cx, |this, cx| {
                        this.run_unstage_file(repo.clone(), path.clone(), cx);
                    })
                    .ok();
                })
                .into_any_element()
        }
        FileSection::Unstaged => {
            let weak_stage = weak.clone();
            let weak_discard = weak.clone();
            let repo_stage = repo_path.to_path_buf();
            let repo_discard = repo_path.to_path_buf();
            let path_stage = change.path.clone();
            let path_discard = change.path.clone();
            div()
                .flex()
                .items_center()
                .gap_0p5()
                .child(
                    IconButton::new(("stage-btn", ix), IconName::Plus)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("暂存此文件 (Stage)")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            weak_stage
                                .update(cx, |this, cx| {
                                    this.run_stage_file(repo_stage.clone(), path_stage.clone(), cx);
                                })
                                .ok();
                        }),
                )
                .child(
                    IconButton::new(("discard-btn", ix), IconName::Trash2)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("丢弃更改 (Discard)")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            weak_discard
                                .update(cx, |this, cx| {
                                    this.confirm_discard_file(
                                        repo_discard.clone(),
                                        path_discard.clone(),
                                        cx,
                                    );
                                })
                                .ok();
                        }),
                )
                .into_any_element()
        }
        FileSection::Committed => div().into_any_element(),
    };

    div()
        .id(row_id)
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1p5()
        .bg(if is_selected {
            colors.active
        } else {
            gpui::transparent_black()
        })
        .when(!is_selected, |r| r.hover(|s| s.bg(colors.hover)))
        .cursor_pointer()
        .on_click({
            let weak = weak.clone();
            move |_, _, cx| {
                weak.update(cx, |this, cx| {
                    this.select_file(file_sel.clone(), cx);
                })
                .ok();
            }
        })
        .child(GitStatusBadge::new(("scm-badge", ix), status))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(theme.text_size(TextSize::Sm))
                .text_color(colors.fg)
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(change.path.clone()),
        )
        .child(actions)
        .into_any_element()
}

fn render_commit_details_left(
    tab: &RepoTabState,
    details: &GitCommitDetails,
    cx: &mut Context<GitGraphApp>,
) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    let weak = cx.weak_entity();

    let (subject, body_rest) = details
        .body
        .split_once('\n')
        .unwrap_or((&details.body, ""));

    let author_line = if details.author_email.is_empty() {
        details.author.clone()
    } else {
        format!("{} <{}>", details.author, details.author_email)
    };

    let parents_row = div().flex().flex_wrap().gap_1().children(
        details.parents.iter().enumerate().map(|(ix, parent)| {
            let weak = weak.clone();
            let hash = parent.clone();
            Button::new(
                ("parent-jump", ix),
                short_hash(parent).to_string(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Sm)
            .on_click(move |_, _, cx| {
                weak.update(cx, |this, cx| {
                    this.jump_to_commit(hash.clone(), cx);
                })
                .ok();
            })
        }),
    );

    let mut desc = DescriptionList::new()
        .stacked()
        .selectable_text("作者", author_line)
        .selectable_text("提交时间", format_timestamp_detailed(details.committer_date))
        .selectable_text("提交 Hash", details.hash.clone());

    if (details.committer.as_str(), details.committer_email.as_str())
        != (details.author.as_str(), details.author_email.as_str())
    {
        desc = desc.selectable_text(
            "提交者",
            format!("{} <{}>", details.committer, details.committer_email),
        );
    }

    if !details.parents.is_empty() {
        desc = desc.item("父提交", parents_row);
    }

    let files = details.file_changes.iter().enumerate().map(|(ix, change)| {
        render_commit_file_row(ix, change, tab, &weak, cx)
    });

    div()
        .id("commit-details-scroll")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .child(
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .border_b_1()
                .border_color(colors.border)
                .child(
                    div()
                        .text_size(theme.text_size(TextSize::Md))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.fg)
                        .child(SelectableText::new("detail-subject", subject.to_string())),
                )
                .when(!body_rest.trim().is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(theme.text_size(TextSize::Sm))
                            .text_color(colors.fg_muted)
                            .child(SelectableText::new(
                                "detail-body",
                                body_rest.trim().to_string(),
                            )),
                    )
                })
                .child(desc),
        )
        .child(
            div()
                .px_3()
                .py_1p5()
                .bg(colors.sunken.opacity(0.6))
                .text_size(theme.text_size(TextSize::Sm))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.fg_muted)
                .child(format!("变更文件 ({})", details.file_changes.len())),
        )
        .when(details.file_changes.is_empty(), |el| {
            el.child(
                div()
                    .px_3()
                    .py_3()
                    .text_size(theme.text_size(TextSize::Xs))
                    .text_color(colors.fg_subtle)
                    .child("该提交未改动任何文件"),
            )
        })
        .children(files)
        .into_any_element()
}

fn render_commit_file_row(
    ix: usize,
    change: &GitFileChange,
    tab: &RepoTabState,
    weak: &gpui::WeakEntity<GitGraphApp>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;

    let display_path = if change.kind == GitFileStatus::Renamed
        && change.old_file_path != change.new_file_path
    {
        format!("{} → {}", change.old_file_path, change.new_file_path)
    } else {
        change.new_file_path.clone()
    };

    let counts = tab
        .line_counts
        .get(&change.new_file_path)
        .or_else(|| tab.line_counts.get(&change.old_file_path));

    let additions = counts.and_then(|c| c.additions).or(change.additions);
    let deletions = counts.and_then(|c| c.deletions).or(change.deletions);

    let is_selected = tab.selected_file.as_ref().is_some_and(|s| {
        s.section == FileSection::Committed && s.path == change.new_file_path
    });

    let file_sel = SelectedFile {
        path: change.new_file_path.clone(),
        old_path: change.old_file_path.clone(),
        section: FileSection::Committed,
    };

    div()
        .id(("commit-file-row", ix))
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1p5()
        .bg(if is_selected {
            colors.active
        } else {
            gpui::transparent_black()
        })
        .when(!is_selected, |r| r.hover(|s| s.bg(colors.hover)))
        .cursor_pointer()
        .on_click({
            let weak = weak.clone();
            move |_, _, cx| {
                weak.update(cx, |this, cx| {
                    this.select_file(file_sel.clone(), cx);
                })
                .ok();
            }
        })
        .child(GitStatusBadge::new(
            ("commit-file-badge", ix),
            file_status_to_git_status(change.kind),
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(colors.fg)
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(display_path),
        )
        .when(additions.is_some() || deletions.is_some(), |el| {
            el.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .font_family(theme.mono_family.clone())
                    .text_size(theme.text_size(TextSize::Xs))
                    .when_some(additions.filter(|&a| a > 0), |d, a| {
                        d.child(div().text_color(colors.success).child(format!("+{a}")))
                    })
                    .when_some(deletions.filter(|&d| d > 0), |d, del| {
                        d.child(div().text_color(colors.danger).child(format!("-{del}")))
                    }),
            )
        })
        .into_any_element()
}

fn render_right_pane(
    app: &GitGraphApp,
    tab: &RepoTabState,
    cx: &mut Context<GitGraphApp>,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let weak = cx.weak_entity();

    let Some(selected_file) = &tab.selected_file else {
        if tab.loading_details {
            return div()
                .size_full()
                .bg(colors.bg)
                .p_4()
                .overflow_hidden()
                .child(
                    Shimmer::new(("detail-diff-shimmer", tab.id))
                        .child(SkeletonText::new("detail-diff-skel", 8)),
                )
                .into_any_element();
        }
        return EmptyState::new(
            "diff-no-file",
            IconName::FileText,
            "选择左侧文件以查看代码差异",
        )
        .into_any_element();
    };

    let stat_badge = match &tab.file_view {
        Some(Ok(FileDiffContent::Text { added, removed, .. })) => {
            Some(DiffStat::new(*added, *removed))
        }
        _ => None,
    };

    let layout_ctrl = SegmentedControl::new(
        "diff-layout-switch",
        match app.diff_layout {
            DiffLayout::Unified => "unified",
            DiffLayout::Split => "split",
        },
    )
    .size(ControlSize::Md)
    .segment("unified", "Unified", None)
    .segment("split", "Split", None)
    .on_change({
        let weak = weak.clone();
        move |key, _, cx| {
            let layout = if key.as_ref() == "split" {
                DiffLayout::Split
            } else {
                DiffLayout::Unified
            };
            weak.update(cx, |this, cx| {
                this.diff_layout = layout;
                cx.notify();
            })
            .ok();
        }
    });

    let section_tag = match selected_file.section {
        FileSection::Committed => None,
        FileSection::Staged => Some(("已暂存", colors.success, colors.success_subtle)),
        FileSection::Unstaged => Some(("未暂存", colors.warning, colors.warning_subtle)),
    };

    let header = div()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1p5()
        .border_b_1()
        .border_color(colors.border)
        .when_some(section_tag, |row, (label, fg, bg)| {
            row.child(
                div()
                    .flex_none()
                    .px_1p5()
                    .py_0p5()
                    .rounded(theme.radius(Radius::Sm))
                    .bg(bg)
                    .text_size(theme.text_size(TextSize::Xs))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(fg)
                    .child(label),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(theme.text_size(TextSize::Base))
                .text_color(colors.fg)
                .child(SelectableText::new("diff-file-path", selected_file.path.clone())),
        )
        .children(stat_badge)
        .child(layout_ctrl);

    let body: AnyElement = match &tab.file_view {
        None => div()
            .size_full()
            .p_4()
            .overflow_hidden()
            .child(
                Shimmer::new(("detail-diff-shimmer", tab.id))
                    .child(SkeletonText::new("detail-diff-skel", 8)),
            )
            .into_any_element(),
        Some(Err(err)) => ErrorView::new("diff-load-err", IconName::TriangleAlert, "无法读取文件差异")
            .detail(err.clone())
            .into_any_element(),
        Some(Ok(FileDiffContent::Text { stretches, .. })) => {
            let weak_open = weak.clone();
            DiffViewer::from_stretches(
                SharedString::from(format!(
                    "diff-viewer-{}-{}-{:?}",
                    tab.id, selected_file.path, selected_file.section
                )),
                selected_file.path.clone(),
                Rc::new(stretches.clone()),
            )
            .headless()
            .layout(app.diff_layout)
            .open(app.opened_diff_stretches.clone())
            .on_open(move |stretch_idx, _, cx| {
                weak_open
                    .update(cx, |this, cx| {
                        this.opened_diff_stretches.push(stretch_idx);
                        cx.notify();
                    })
                    .ok();
            })
            .into_any_element()
        }
        Some(Ok(FileDiffContent::Picture { old, new })) => {
            let picture = |id: &'static str, img: &std::sync::Arc<gpui::Image>| {
                Image::new(id, img.clone())
                    .fit(ObjectFit::Contain)
                    .size_full()
            };
            let content = match (old, new) {
                (Some(old), Some(new)) => BeforeAfter::new(
                    "diff-image-before-after",
                    picture("diff-img-old", old),
                    picture("diff-img-new", new),
                )
                .size_full()
                .into_any_element(),
                (Some(only), None) | (None, Some(only)) => {
                    picture("diff-img-single", only).into_any_element()
                }
                (None, None) => EmptyState::new("diff-img-none", IconName::FileImage, "图片为空")
                    .into_any_element(),
            };
            div().size_full().p_4().child(content).into_any_element()
        }
        Some(Ok(FileDiffContent::Binary { old_size, new_size })) => {
            let fmt_sz = |sz: &Option<u64>| {
                sz.map_or_else(|| "不存在".to_string(), |b| format::file_size(b, false))
            };
            EmptyState::new("diff-binary-file", IconName::File, "二进制文件差异")
                .body(format!("大小变化：{} → {}", fmt_sz(old_size), fmt_sz(new_size)))
                .into_any_element()
        }
        Some(Ok(FileDiffContent::Empty)) => EmptyState::new(
            "diff-empty-content",
            IconName::Check,
            "无文本内容变化",
        )
        .body("可能是纯重命名、权限变更或空文件。")
        .into_any_element(),
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

fn file_status_to_git_status(status: GitFileStatus) -> GitStatus {
    match status {
        GitFileStatus::Added => GitStatus::Added,
        GitFileStatus::Modified => GitStatus::Modified,
        GitFileStatus::Deleted => GitStatus::Deleted,
        GitFileStatus::Renamed => GitStatus::Renamed,
        GitFileStatus::Untracked => GitStatus::Untracked,
    }
}

fn scm_to_git_status(change: &ScmChange, section: FileSection) -> GitStatus {
    if change.conflicted {
        return GitStatus::Conflicted;
    }
    let raw = match section {
        FileSection::Staged => change.staged,
        FileSection::Unstaged => {
            if change.untracked {
                return GitStatus::Untracked;
            }
            change.unstaged
        }
        FileSection::Committed => change.staged.or(change.unstaged),
    };
    match raw {
        Some("added") => GitStatus::Added,
        Some("deleted") => GitStatus::Deleted,
        Some("renamed") => GitStatus::Renamed,
        Some("untracked") => GitStatus::Untracked,
        _ => GitStatus::Modified,
    }
}
