//! 模态确认/输入对话框与右键上下文菜单构建。

use std::path::PathBuf;

use ely_core::buttons::{Button, ButtonVariant};
use ely_core::forms::{Checkbox, Input, TextInput};
use ely_core::menus::{Menu, MenuItem};
use ely_core::overlays::{ConfirmDialog, Dialog, PromptDialog};
use ely_core::primitives::{IconName, Severity};
use ely_core::theme::{ActiveTheme, TextSize};
use git_graph_core::types::{GitCommit, UNCOMMITTED};
use git_graph_ops::{ResetMode, SafetyWarning};
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

/// 右键提交或 Ref 胶囊后激活的上下文菜单请求。
#[derive(Clone)]
pub struct ContextMenuState {
    pub request_id: u64,
    pub position: Point<Pixels>,
    pub repo_path: PathBuf,
    pub commit: GitCommit,
    pub focused_branch: Option<String>,
    pub focused_tag: Option<String>,
}

/// 构造右键上下文菜单。
pub fn build_commit_context_menu(
    ctx_state: &ContextMenuState,
    app_entity: gpui::WeakEntity<GitGraphApp>,
) -> Menu {
    let commit = &ctx_state.commit;
    let repo_path = &ctx_state.repo_path;
    let hash = commit.hash.clone();
    let short = short_hash(&hash).to_string();

    if hash == UNCOMMITTED {
        return Menu::new().item(
            MenuItem::new("暂存区保存为 Stash...")
                .icon(IconName::Archive)
                .on_click({
                    let app = app_entity.clone();
                    let repo = repo_path.clone();
                    move |window, cx| {
                        app.update(cx, |this, cx| this.open_stash_push_modal(repo.clone(), window, cx))
                            .ok();
                    }
                }),
        );
    }

    let mut menu = Menu::new();

    // 若右键点在特定本地分支胶囊上，置顶分支操作
    if let Some(branch) = &ctx_state.focused_branch {
        menu = menu
            .item(
                MenuItem::new(format!("检出分支 {branch}"))
                    .icon(IconName::GitBranch)
                    .on_click({
                        let app = app_entity.clone();
                        let repo = repo_path.clone();
                        let branch = branch.clone();
                        move |_, cx| {
                            app.update(cx, |this, cx| {
                                this.request_checkout_branch(repo.clone(), branch.clone(), cx);
                            })
                            .ok();
                        }
                    }),
            )
            .item(
                MenuItem::new(format!("删除分支 {branch}"))
                    .icon(IconName::Trash2)
                    .on_click({
                        let app = app_entity.clone();
                        let repo = repo_path.clone();
                        let branch = branch.clone();
                        move |_, cx| {
                            app.update(cx, |this, cx| {
                                this.confirm_delete_branch(repo.clone(), branch.clone(), cx);
                            })
                            .ok();
                        }
                    }),
            )
            .separator();
    } else if !commit.heads.is_empty() {
        for branch in &commit.heads {
            menu = menu.item(
                MenuItem::new(format!("检出分支 {branch}"))
                    .icon(IconName::GitBranch)
                    .on_click({
                        let app = app_entity.clone();
                        let repo = repo_path.clone();
                        let branch = branch.clone();
                        move |_, cx| {
                            app.update(cx, |this, cx| {
                                this.request_checkout_branch(repo.clone(), branch.clone(), cx);
                            })
                            .ok();
                        }
                    }),
            );
        }
        menu = menu.separator();
    }

    if let Some(tag) = &ctx_state.focused_tag {
        menu = menu
            .item(
                MenuItem::new(format!("删除标签 {tag}"))
                    .icon(IconName::Trash2)
                    .on_click({
                        let app = app_entity.clone();
                        let repo = repo_path.clone();
                        let tag = tag.clone();
                        move |_, cx| {
                            app.update(cx, |this, cx| {
                                this.confirm_delete_tag(repo.clone(), tag.clone(), cx);
                            })
                            .ok();
                        }
                    }),
            )
            .separator();
    }

    // 若为 Stash 节点，提供 Stash 快捷操作
    if let Some(stash) = &commit.stash
        && let Some(idx) = parse_stash_index(&stash.selector)
    {
        menu = menu
            .item(
                MenuItem::new(format!("应用 {}", stash.selector))
                    .icon(IconName::Check)
                    .on_click({
                        let app = app_entity.clone();
                        let repo = repo_path.clone();
                        move |_, cx| {
                            app.update(cx, |this, cx| {
                                this.run_stash_apply(repo.clone(), idx, cx);
                            })
                            .ok();
                        }
                    }),
            )
            .item(
                MenuItem::new(format!("弹出 (Pop) {}", stash.selector))
                    .icon(IconName::Upload)
                    .on_click({
                        let app = app_entity.clone();
                        let repo = repo_path.clone();
                        move |_, cx| {
                            app.update(cx, |this, cx| {
                                this.confirm_stash_pop(repo.clone(), idx, cx);
                            })
                            .ok();
                        }
                    }),
            )
            .item(
                MenuItem::new(format!("丢弃 (Drop) {}", stash.selector))
                    .icon(IconName::Trash2)
                    .on_click({
                        let app = app_entity.clone();
                        let repo = repo_path.clone();
                        move |_, cx| {
                            app.update(cx, |this, cx| {
                                this.confirm_stash_drop(repo.clone(), idx, cx);
                            })
                            .ok();
                        }
                    }),
            )
            .separator();
    }

    // 核心提交操作
    menu = menu
        .item(
            MenuItem::new(format!("检出此提交 ({short})"))
                .icon(IconName::GitCommitHorizontal)
                .on_click({
                    let app = app_entity.clone();
                    let repo = repo_path.clone();
                    let h = hash.clone();
                    move |_, cx| {
                        app.update(cx, |this, cx| {
                            this.request_checkout_commit(repo.clone(), h.clone(), cx);
                        })
                        .ok();
                    }
                }),
        )
        .item(
            MenuItem::new("在此创建分支...")
                .icon(IconName::GitBranch)
                .on_click({
                    let app = app_entity.clone();
                    let repo = repo_path.clone();
                    let h = hash.clone();
                    move |window, cx| {
                        app.update(cx, |this, cx| {
                            this.open_create_branch_modal(repo.clone(), h.clone(), window, cx);
                        })
                        .ok();
                    }
                }),
        )
        .item(
            MenuItem::new("在此创建标签...")
                .icon(IconName::Tag)
                .on_click({
                    let app = app_entity.clone();
                    let repo = repo_path.clone();
                    let h = hash.clone();
                    move |window, cx| {
                        app.update(cx, |this, cx| {
                            this.open_create_tag_modal(repo.clone(), h.clone(), window, cx);
                        })
                        .ok();
                    }
                }),
        )
        .separator()
        .item(
            MenuItem::new("Cherry-pick 此提交")
                .icon(IconName::GitMerge)
                .on_click({
                    let app = app_entity.clone();
                    let repo = repo_path.clone();
                    let h = hash.clone();
                    move |_, cx| {
                        app.update(cx, |this, cx| {
                            this.run_cherry_pick(repo.clone(), h.clone(), cx);
                        })
                        .ok();
                    }
                }),
        )
        .item(
            MenuItem::new("Revert 此提交")
                .icon(IconName::Undo2)
                .on_click({
                    let app = app_entity.clone();
                    let repo = repo_path.clone();
                    let h = hash.clone();
                    move |_, cx| {
                        app.update(cx, |this, cx| {
                            this.run_revert_commit(repo.clone(), h.clone(), cx);
                        })
                        .ok();
                    }
                }),
        )
        .item(MenuItem::submenu(
            "Reset 当前分支到此提交",
            Menu::new()
                .item(MenuItem::new("Soft（保留暂存与工作区）").on_click({
                    let app = app_entity.clone();
                    let repo = repo_path.clone();
                    let h = hash.clone();
                    move |_, cx| {
                        app.update(cx, |this, cx| {
                            this.request_reset(repo.clone(), h.clone(), ResetMode::Soft, cx);
                        })
                        .ok();
                    }
                }))
                .item(MenuItem::new("Mixed（保留工作区）").on_click({
                    let app = app_entity.clone();
                    let repo = repo_path.clone();
                    let h = hash.clone();
                    move |_, cx| {
                        app.update(cx, |this, cx| {
                            this.request_reset(repo.clone(), h.clone(), ResetMode::Mixed, cx);
                        })
                        .ok();
                    }
                }))
                .item(MenuItem::new("Hard（丢弃全部未提交修改）").on_click({
                    let app = app_entity.clone();
                    let repo = repo_path.clone();
                    let h = hash.clone();
                    move |_, cx| {
                        app.update(cx, |this, cx| {
                            this.request_reset(repo.clone(), h.clone(), ResetMode::Hard, cx);
                        })
                        .ok();
                    }
                })),
        ))
        .separator()
        .item(
            MenuItem::new("复制完整 Hash")
                .icon(IconName::Copy)
                .on_click({
                    let h = hash.clone();
                    move |_, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(h.clone()));
                    }
                }),
        )
        .item(
            MenuItem::new("复制短 Hash")
                .icon(IconName::Copy)
                .on_click({
                    let s = short.clone();
                    move |_, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(s.clone()));
                    }
                }),
        )
        .item(
            MenuItem::new("复制提交信息")
                .icon(IconName::Copy)
                .on_click({
                    let msg = commit.message.clone();
                    move |_, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(msg.clone()));
                    }
                }),
        );

    menu
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
            let valid = !name_val.is_empty() && !name_val.contains(' ') && !name_val.starts_with('-');

            Dialog::new(
                "create-branch-dialog",
                format!("在 {} 创建分支", short_hash(start_point)),
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

fn parse_stash_index(selector: &str) -> Option<usize> {
    selector
        .strip_prefix("stash@{")
        .and_then(|s| s.strip_suffix('}'))
        .and_then(|n| n.parse::<usize>().ok())
}
