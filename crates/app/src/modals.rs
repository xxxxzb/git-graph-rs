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
use git_graph_core::types::{GitCommit, UNCOMMITTED};
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

/// 右键提交或 Ref 胶囊后激活的上下文菜单请求。
#[derive(Clone)]
pub struct ContextMenuState {
    pub request_id: u64,
    pub position: Point<Pixels>,
    pub repo_path: PathBuf,
    pub commit: GitCommit,
    pub focused_branch: Option<String>,
    pub focused_tag: Option<String>,
    pub current_branch: Option<String>,
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
    let mut listed_push_current = false;
    if let Some(branch) = &ctx_state.focused_branch {
        if ctx_state.current_branch.as_deref() == Some(branch.as_str()) {
            listed_push_current = true;
        }
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
                MenuItem::new(format!("推送分支 \"{branch}\" 到远程…"))
                    .icon(IconName::CloudUpload)
                    .on_click({
                        let app = app_entity.clone();
                        let repo = repo_path.clone();
                        let branch = branch.clone();
                        move |window, cx| {
                            app.update(cx, |this, cx| {
                                this.open_push_branch_modal(repo.clone(), branch.clone(), window, cx);
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
            if ctx_state.current_branch.as_deref() == Some(branch.as_str()) {
                listed_push_current = true;
            }
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
                    MenuItem::new(format!("推送分支 \"{branch}\" 到远程…"))
                        .icon(IconName::CloudUpload)
                        .on_click({
                            let app = app_entity.clone();
                            let repo = repo_path.clone();
                            let branch = branch.clone();
                            move |window, cx| {
                                app.update(cx, |this, cx| {
                                    this.open_push_branch_modal(
                                        repo.clone(),
                                        branch.clone(),
                                        window,
                                        cx,
                                    );
                                })
                                .ok();
                            }
                        }),
                );
        }
        menu = menu.separator();
    }

    if !listed_push_current && let Some(cur_branch) = &ctx_state.current_branch {
        menu = menu
            .item(
                MenuItem::new(format!("推送当前分支 \"{cur_branch}\" 到远程…"))
                    .icon(IconName::CloudUpload)
                    .on_click({
                        let app = app_entity.clone();
                        let repo = repo_path.clone();
                        let branch = cur_branch.clone();
                        move |window, cx| {
                            app.update(cx, |this, cx| {
                                this.open_push_branch_modal(
                                    repo.clone(),
                                    branch.clone(),
                                    window,
                                    cx,
                                );
                            })
                            .ok();
                        }
                    }),
            )
            .separator();
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

fn parse_stash_index(selector: &str) -> Option<usize> {
    selector
        .strip_prefix("stash@{")
        .and_then(|s| s.strip_suffix('}'))
        .and_then(|n| n.parse::<usize>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

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
