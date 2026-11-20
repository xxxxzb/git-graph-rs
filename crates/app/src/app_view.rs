//! 主应用视图 `GitGraphApp`：macOS 融合标题栏、多仓库 Tab、异步调度、写操作、文件监听与全局壳层。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ely_core::buttons::{Button, ButtonVariant, IconButton};
use ely_core::feedback::{EmptyState, Toast, ToastViewport, Toaster};
use ely_core::forms::{InputEvent, TextInput};
use ely_core::layout::SplitPane;
use ely_core::lists::{List, ListItem};
use ely_core::menus::{ContextMenu, Menu, MenuItem, OverflowMenu};
use ely_core::motion::{
    Entrance, FAST, Flash, Spinner, SpinnerStyle, Stagger, duration, ease_out_cubic, since_change,
};
use ely_core::primitives::{Icon, IconName, Severity};
use ely_core::shell::{StatusBar, StatusBarItem, drag_region};
use ely_core::theme::{ActiveTheme, ControlSize, IconSize, Mode, Radius, TextSize, Theme};
use ely_git::DiffLayout;
use git_graph_core::api::Engine;
use git_graph_core::types::UNCOMMITTED;
use git_graph_ops::{
    BranchSyncStatus, GitCli, PushForceMode, PushOptions, RepoWatcher, ResetMode, WatchEvent,
    check_checkout_safety, check_reset_safety,
};
use gpui::{
    AnyElement, Axis, Context, ElementId, Entity, FocusHandle, InteractiveElement, IntoElement,
    MouseButton, ParentElement, PathPromptOptions, Pixels, Point, Render, ScrollEvent,
    ScrollStrategy, SharedString, Styled, Subscription, TouchPhase, Window, div, prelude::*, px,
};

use crate::modals::{
    ActiveModal, ContextMenuState, ContextMenuTarget, PendingWriteOp, build_context_menu,
    compute_push_defaults, render_active_modal,
};
use crate::motion_fx::RubberBandSpring;
use crate::repo_tab::{
    FileSection, PendingFlashTarget, RepoTabState, SelectedFile, execute_history_search,
    load_commit_details, load_file_diff, load_file_line_counts, load_repo_snapshot,
};
use crate::state::{
    DEFAULT_UI_SCALE, MAX_UI_SCALE, MIN_UI_SCALE, PersistedState, ThemePreset, repo_display_name,
    save_persisted_state, short_hash,
};
use crate::theme_presets::apply_theme_preset;
use crate::ui_details::render_details_pane;
use crate::ui_graph::{
    ResizeCol, SyncButtonAction, render_filter_toolbar, render_graph_table, row_height,
    sync_button_spec,
};

gpui::actions!(
    git_graph_app,
    [
        OpenRepository,
        CloseActiveTab,
        RefreshGraph,
        ToggleTheme,
        ToggleThemePreset,
        ToggleDetails,
        FocusSearch,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        QuitApp
    ]
);

pub struct GitGraphApp {
    pub focus: FocusHandle,
    pub tabs: Vec<RepoTabState>,
    pub active_tab_id: Option<u64>,
    pub next_tab_id: u64,
    pub persisted: PersistedState,

    pub search_input: Entity<TextInput>,
    pub _search_sub: Subscription,
    pub toaster: Entity<Toaster>,

    pub diff_layout: DiffLayout,
    pub opened_diff_stretches: Vec<usize>,

    pub modal: Option<ActiveModal>,
    pub modal_serial: u64,
    pub details_open_serial: u64,
    pub context_menu: Option<ContextMenuState>,
    pub next_ctx_request_id: u64,
    pub cli: Option<GitCli>,
    pub scroll_latch: crate::ui_graph::HorizontalScrollLatch,
}

impl GitGraphApp {
    pub fn new(
        initial_repos: Vec<PathBuf>,
        persisted: PersistedState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let search_input = cx.new(|cx| {
            TextInput::new(window, cx).placeholder("搜索提交信息、作者、Hash 或分支…")
        });
        let _search_sub = cx.subscribe(&search_input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed | InputEvent::Submit) {
                let query = input.read(cx).text().to_string();
                this.set_search_query(query, cx);
            }
        });
        let toaster = cx.new(|_| Toaster::default());
        let cli = match GitCli::new() {
            Ok(cli) => Some(cli),
            Err(err) => {
                log::error!("failed to locate git executable: {err}");
                None
            }
        };

        window.set_rem_size(px(16.0 * persisted.ui_scale));

        let mut app = Self {
            focus,
            tabs: Vec::new(),
            active_tab_id: None,
            next_tab_id: 1,
            persisted,
            search_input,
            _search_sub,
            toaster,
            diff_layout: DiffLayout::Unified,
            opened_diff_stretches: Vec::new(),
            modal: None,
            modal_serial: 0,
            details_open_serial: 0,
            context_menu: None,
            next_ctx_request_id: 1,
            cli,
            scroll_latch: crate::ui_graph::HorizontalScrollLatch::default(),
        };

        for path in initial_repos {
            app.open_repo_path(path, cx);
        }

        app
    }

    /// 当前界面全局缩放倍率。
    pub fn ui_scale(&self) -> f32 {
        self.persisted.ui_scale
    }

    fn apply_ui_scale(&mut self, scale: f32, window: &mut Window, cx: &mut Context<Self>) {
        let clamped = (scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE) * 20.0).round() / 20.0;
        if (self.persisted.ui_scale - clamped).abs() < 0.001 {
            return;
        }
        self.persisted.ui_scale = clamped;
        window.set_rem_size(px(16.0 * clamped));
        self.save_state();
        cx.notify();
    }

    pub fn active_tab(&self) -> Option<&RepoTabState> {
        let id = self.active_tab_id?;
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut RepoTabState> {
        let id = self.active_tab_id?;
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    fn tab_by_id_mut(&mut self, id: u64) -> Option<&mut RepoTabState> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    fn tab_by_path_mut(&mut self, path: &std::path::Path) -> Option<&mut RepoTabState> {
        self.tabs.iter_mut().find(|t| t.path == path)
    }

    fn push_toast(&self, title: impl Into<SharedString>, severity: Severity, body: Option<String>, cx: &mut Context<Self>) {
        let mut toast = Toast::new(title).severity(severity);
        if let Some(body) = body {
            toast = toast.body(body);
        }
        self.toaster.update(cx, |toaster, cx| {
            toaster.push(toast, cx);
        });
    }

    fn save_state(&self) {
        save_persisted_state(&self.persisted);
    }

    /* ---------- 仓库打开 / 切换 / 关闭 ---------- */

    pub fn pick_and_open_repo(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择本地 Git 仓库".into()),
        });
        cx.spawn(async move |weak, cx| {
            let picked = match rx.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(_)) | Err(_) => rfd::FileDialog::new()
                    .set_title("选择本地 Git 仓库")
                    .pick_folder(),
            };
            if let Some(path) = picked {
                weak.update(cx, |this, cx| {
                    this.open_repo_path(path, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn open_repo_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let engine = match Engine::open(&path) {
            Ok(engine) => engine,
            Err(err) => {
                self.push_toast(
                    "无法打开仓库",
                    Severity::Danger,
                    Some(format!("{}: {err}", path.display())),
                    cx,
                );
                return;
            }
        };

        let root = engine.root().to_path_buf();
        if let Some(existing) = self.tabs.iter().find(|t| t.path == root) {
            let id = existing.id;
            self.activate_tab(id, cx);
            return;
        }

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;

        let mut tab = RepoTabState::new(tab_id, engine);
        Self::attach_watcher(&mut tab, cx);

        self.tabs.push(tab);
        self.active_tab_id = Some(tab_id);
        self.persisted.record_recent(&root);
        self.save_state();
        self.sync_search_input_to_active(cx);

        self.load_tab_graph(tab_id, false, cx);
        cx.notify();
    }

    fn attach_watcher(tab: &mut RepoTabState, cx: &mut Context<Self>) {
        let tab_id = tab.id;
        let (tx, rx) = mpsc::channel::<WatchEvent>();
        match RepoWatcher::new(&tab.path, tx) {
            Ok(watcher) => {
                tab._watcher = Some(watcher);
                tab._watch_task = Some(cx.spawn(async move |weak, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;

                        let mut got_event = false;
                        while rx.try_recv().is_ok() {
                            got_event = true;
                        }
                        if !got_event {
                            continue;
                        }

                        // ~300ms 防抖窗口：等待同批文件写入完成后再合并为一次软刷新
                        cx.background_executor()
                            .timer(Duration::from_millis(300))
                            .await;
                        while rx.try_recv().is_ok() {}

                        let still_alive = weak
                            .update(cx, |this, cx| {
                                this.soft_refresh_tab(tab_id, cx);
                            })
                            .is_ok();
                        if !still_alive {
                            break;
                        }
                    }
                }));
            }
            Err(err) => {
                log::warn!("failed to watch repo {}: {err}", tab.path.display());
            }
        }
    }

    pub fn activate_tab(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        if self.active_tab_id == Some(tab_id) {
            return;
        }
        if self.tabs.iter().any(|t| t.id == tab_id) {
            self.active_tab_id = Some(tab_id);
            self.context_menu = None;
            self.opened_diff_stretches.clear();
            self.sync_search_input_to_active(cx);
            cx.notify();
        }
    }

    pub fn close_tab(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        let Some(pos) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        self.tabs.remove(pos);
        if self.active_tab_id == Some(tab_id) {
            let next = if self.tabs.is_empty() {
                None
            } else {
                let next_ix = pos.min(self.tabs.len() - 1);
                Some(self.tabs[next_ix].id)
            };
            self.active_tab_id = next;
            self.sync_search_input_to_active(cx);
        }
        self.context_menu = None;
        cx.notify();
    }

    pub fn clear_recent_repos(&mut self, cx: &mut Context<Self>) {
        if self.persisted.recent_repos.is_empty() {
            return;
        }
        self.persisted.recent_repos.clear();
        self.save_state();
        self.push_toast("已清空最近打开的仓库记录", Severity::Info, None, cx);
        cx.notify();
    }

    fn sync_search_input_to_active(&self, cx: &mut Context<Self>) {
        let text = self
            .active_tab()
            .map(|t| t.filter.search_query.clone())
            .unwrap_or_default();
        self.search_input.update(cx, |input, cx| {
            if input.text() != text {
                input.set_text(text, cx);
            }
        });
    }

    /* ---------- 图谱与详情异步加载（serial 防过期） ---------- */

    pub fn refresh_active_tab(&mut self, soft: bool, cx: &mut Context<Self>) {
        if let Some(id) = self.active_tab_id {
            self.load_tab_graph(id, soft, cx);
        }
    }

    pub fn soft_refresh_tab(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        self.load_tab_graph(tab_id, true, cx);
    }

    pub fn load_tab_graph(&mut self, tab_id: u64, soft: bool, cx: &mut Context<Self>) {
        let cli = self.cli.clone();
        let Some(tab) = self.tab_by_id_mut(tab_id) else {
            return;
        };
        tab.load_serial += 1;
        let serial = tab.load_serial;
        let engine = tab.engine.clone();
        let filter = tab.filter.clone();
        let max_commits = tab.max_commits;
        if !soft {
            tab.loading_graph = true;
            tab.error = None;
            cx.notify();
        }

        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move {
                    load_repo_snapshot(&engine, cli.as_ref(), &filter, max_commits)
                })
                .await;

            weak.update(cx, |this, cx| {
                let Some(tab) = this.tab_by_id_mut(tab_id) else {
                    return;
                };
                if tab.load_serial != serial {
                    return;
                }
                tab.loading_graph = false;
                tab.loading_more = false;
                let mut post_refresh_flash: Option<String> = None;
                match res {
                    Ok(snap) => {
                        tab.error = None;
                        tab.info = Some(snap.info);
                        tab.authors = snap.authors;
                        tab.all_commits = snap.commits;
                        tab.head_hash = snap.head_hash;
                        tab.more_available = snap.more_available;
                        tab.scm_changes = snap.scm_changes;
                        tab.uncommitted_count = snap.uncommitted_count;
                        tab.sync_status = snap.sync_status;
                        tab.recompute_visible_rows();
                        let should_research = !soft && tab.is_search_active();

                        if let Some(target) = tab.pending_flash_target.take() {
                            let resolved = match target {
                                PendingFlashTarget::Hash(h) => {
                                    if tab.visible_index_of(&h).is_some() {
                                        Some(h)
                                    } else {
                                        tab.head_hash.clone()
                                    }
                                }
                                PendingFlashTarget::Head => tab.head_hash.clone(),
                            };
                            if let Some(hash) = resolved
                                && let Some(ix) = tab.visible_index_of(&hash)
                            {
                                tab.graph_scroll.scroll_to_item(ix, ScrollStrategy::Center);
                                post_refresh_flash = Some(hash);
                            }
                        }

                        // 软刷新时若之前选中了某提交或未提交行，同步刷新详情与 diff
                        if let Some(sel) = tab.selected_hash.clone() {
                            let still_present =
                                sel == UNCOMMITTED || tab.find_commit(&sel).is_some();
                            if still_present {
                                this.load_tab_details(tab_id, sel, true, cx);
                            } else {
                                tab.selected_hash = None;
                                tab.details = None;
                                tab.selected_file = None;
                                tab.file_view = None;
                            }
                        }

                        if should_research {
                            this.trigger_history_search(tab_id, Duration::ZERO, cx);
                        }
                    }
                    Err(err) => {
                        tab.error = Some(err);
                        tab.pending_flash_target = None;
                    }
                }
                if let Some(hash) = post_refresh_flash {
                    this.trigger_commit_flash(tab_id, hash, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn load_more_commits(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if !tab.more_available || tab.loading_more || tab.loading_graph {
            return;
        }
        tab.loading_more = true;
        tab.max_commits = tab.max_commits.saturating_add(300);
        let id = tab.id;
        self.load_tab_graph(id, true, cx);
    }

    pub fn select_commit(&mut self, hash: String, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_tab_id else {
            return;
        };
        let mut should_bump_details = false;
        if !self.persisted.details_open {
            self.persisted.details_open = true;
            self.save_state();
            should_bump_details = true;
        }
        let Some(tab) = self.tab_by_id_mut(tab_id) else {
            return;
        };
        if tab.selected_hash.as_deref() == Some(hash.as_str()) {
            if should_bump_details {
                self.details_open_serial = self.details_open_serial.saturating_add(1);
                cx.notify();
            }
            return;
        }
        if tab.selected_hash.is_none() {
            should_bump_details = true;
        }
        tab.selected_hash = Some(hash.clone());
        tab.details = None;
        tab.line_counts.clear();
        tab.selected_file = None;
        tab.file_view = None;
        if should_bump_details {
            self.details_open_serial = self.details_open_serial.saturating_add(1);
        }
        self.opened_diff_stretches.clear();
        self.load_tab_details(tab_id, hash, false, cx);
        cx.notify();
    }

    fn load_tab_details(
        &mut self,
        tab_id: u64,
        hash: String,
        preserve_selected_file: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_by_id_mut(tab_id) else {
            return;
        };
        tab.detail_serial += 1;
        let serial = tab.detail_serial;
        let engine = tab.engine.clone();
        let stash = tab.find_commit(&hash).and_then(|c| c.stash.clone());
        tab.loading_details = true;

        let hash_clone = hash.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move {
                    load_commit_details(&engine, &hash_clone, stash.as_ref())
                })
                .await;

            weak.update(cx, |this, cx| {
                let Some(tab) = this.tab_by_id_mut(tab_id) else {
                    return;
                };
                if tab.detail_serial != serial {
                    return;
                }
                tab.loading_details = false;
                tab.details = Some(res);

                // 如果是已提交记录，则异步补算文件增删行数并自动选中第一个文件
                if hash == UNCOMMITTED {
                    let next_file = if preserve_selected_file {
                        tab.selected_file.clone().filter(|sel| {
                            tab.scm_changes.iter().any(|c| c.path == sel.path)
                        })
                    } else {
                        None
                    }
                    .or_else(|| {
                        tab.scm_changes.first().map(|c| {
                            let section = if c.staged.is_some() {
                                FileSection::Staged
                            } else {
                                FileSection::Unstaged
                            };
                            SelectedFile {
                                path: c.path.clone(),
                                old_path: c.old_path.clone().unwrap_or_else(|| c.path.clone()),
                                section,
                            }
                        })
                    });
                    tab.selected_file = next_file.clone();
                    if let Some(file) = next_file {
                        this.load_tab_file_diff(tab_id, file, cx);
                    } else {
                        tab.file_view = None;
                    }
                } else if let Some(Ok(details)) = &tab.details {
                    let paths: Vec<String> = details
                        .file_changes
                        .iter()
                        .map(|f| f.new_file_path.clone())
                        .collect();
                    let next_file = if preserve_selected_file {
                        tab.selected_file.clone().filter(|sel| {
                            details
                                .file_changes
                                .iter()
                                .any(|f| f.new_file_path == sel.path)
                        })
                    } else {
                        None
                    }
                    .or_else(|| {
                        details.file_changes.first().map(|f| SelectedFile {
                            path: f.new_file_path.clone(),
                            old_path: f.old_file_path.clone(),
                            section: FileSection::Committed,
                        })
                    });
                    tab.selected_file = next_file.clone();
                    if let Some(file) = next_file {
                        this.load_tab_file_diff(tab_id, file, cx);
                    } else {
                        tab.file_view = None;
                    }
                    this.load_tab_line_counts(tab_id, hash, paths, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_tab_line_counts(
        &mut self,
        tab_id: u64,
        hash: String,
        paths: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() || hash == UNCOMMITTED {
            return;
        }
        let Some(tab) = self.tab_by_id_mut(tab_id) else {
            return;
        };
        tab.line_count_serial += 1;
        let serial = tab.line_count_serial;
        let engine = tab.engine.clone();
        let stash_base = tab
            .find_commit(&hash)
            .and_then(|c| c.stash.as_ref().map(|s| s.base_hash.clone()));

        cx.spawn(async move |weak, cx| {
            let counts = cx
                .background_spawn(async move {
                    load_file_line_counts(&engine, stash_base.as_deref(), &hash, &paths)
                })
                .await;

            weak.update(cx, |this, cx| {
                let Some(tab) = this.tab_by_id_mut(tab_id) else {
                    return;
                };
                if tab.line_count_serial != serial {
                    return;
                }
                if let Ok(map) = counts {
                    tab.line_counts = map;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn select_file(&mut self, file: SelectedFile, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_tab_id else {
            return;
        };
        self.opened_diff_stretches.clear();
        if let Some(tab) = self.tab_by_id_mut(tab_id) {
            tab.selected_file = Some(file.clone());
            tab.file_view = None;
        }
        self.load_tab_file_diff(tab_id, file, cx);
        cx.notify();
    }

    fn load_tab_file_diff(&mut self, tab_id: u64, file: SelectedFile, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_by_id_mut(tab_id) else {
            return;
        };
        let Some(hash) = tab.selected_hash.clone() else {
            return;
        };
        tab.diff_serial += 1;
        let serial = tab.diff_serial;
        let engine = tab.engine.clone();
        let stash = tab.selected_stash();
        let first_parent = tab
            .details
            .as_ref()
            .and_then(|res| res.as_ref().ok())
            .and_then(|d| d.parents.first().cloned())
            .or_else(|| {
                tab.find_commit(&hash)
                    .and_then(|c| c.parents.first().cloned())
            });
        let file_change = tab
            .details
            .as_ref()
            .and_then(|res| res.as_ref().ok())
            .and_then(|d| {
                d.file_changes
                    .iter()
                    .find(|fc| fc.new_file_path == file.path)
                    .cloned()
            });
        tab.loading_diff = true;

        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move {
                    load_file_diff(
                        &engine,
                        &hash,
                        stash.as_ref(),
                        first_parent.as_deref(),
                        &file,
                        file_change.as_ref(),
                    )
                })
                .await;

            weak.update(cx, |this, cx| {
                let Some(tab) = this.tab_by_id_mut(tab_id) else {
                    return;
                };
                if tab.diff_serial != serial {
                    return;
                }
                tab.loading_diff = false;
                tab.file_view = Some(res);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /* ---------- 筛选 / 跳转 / 主题 / 布局控制 ---------- */

    pub fn set_branch_filter(&mut self, branch: Option<String>, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.filter.branch == branch {
            return;
        }
        tab.filter.branch = branch;
        let id = tab.id;
        self.load_tab_graph(id, false, cx);
    }

    pub fn set_author_filter(&mut self, author: Option<String>, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.filter.author == author {
            return;
        }
        tab.filter.author = author;
        if tab.is_search_active() {
            tab.search_serial += 1;
            tab.search_loading = true;
            tab.search_results = None;
            tab.search_match_count = None;
            tab.search_error = None;
        }
        let id = tab.id;
        self.load_tab_graph(id, false, cx);
    }

    pub fn set_show_remote_branches(&mut self, show: bool, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.filter.show_remote_branches == show {
            return;
        }
        tab.filter.show_remote_branches = show;
        let id = tab.id;
        self.load_tab_graph(id, false, cx);
    }

    pub fn set_search_query(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.filter.search_query == query {
            return;
        }
        tab.filter.search_query = query;
        let tab_id = tab.id;

        if !tab.is_search_active() {
            tab.search_serial += 1;
            tab.clear_search_state();
            cx.notify();
            return;
        }

        tab.search_results = None;
        tab.search_error = None;
        tab.search_match_count = None;
        tab.search_used_literal_fallback = false;
        tab.recompute_visible_rows();
        self.trigger_history_search(tab_id, Duration::from_millis(180), cx);
        cx.notify();
    }

    fn trigger_history_search(
        &mut self,
        tab_id: u64,
        debounce: Duration,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_by_id_mut(tab_id) else {
            return;
        };
        tab.search_serial += 1;
        let serial = tab.search_serial;
        tab.search_loading = true;
        tab.search_error = None;

        let engine = tab.engine.clone();
        let query = tab.filter.search_query.clone();
        let loaded_commits = tab.all_commits.clone();
        let author_filter = tab.filter.author.clone();

        cx.spawn(async move |weak, cx| {
            if !debounce.is_zero() {
                cx.background_executor().timer(debounce).await;
                let still_current = weak
                    .update(cx, |this, _| {
                        this.tab_by_id_mut(tab_id)
                            .is_some_and(|t| t.search_serial == serial && t.is_search_active())
                    })
                    .unwrap_or(false);
                if !still_current {
                    return;
                }
            }

            let res = cx
                .background_spawn(async move {
                    execute_history_search(
                        &engine,
                        &query,
                        &loaded_commits,
                        author_filter.as_deref(),
                    )
                })
                .await;

            weak.update(cx, |this, cx| {
                let Some(tab) = this.tab_by_id_mut(tab_id) else {
                    return;
                };
                if tab.search_serial != serial {
                    return;
                }
                tab.search_loading = false;
                match res {
                    Ok(output) => {
                        tab.search_error = None;
                        tab.search_match_count = Some(output.commits.len());
                        tab.search_used_literal_fallback = output.used_literal_fallback;
                        tab.search_results = Some(output.commits);
                    }
                    Err(err) => {
                        tab.search_error = Some(err);
                        tab.search_match_count = None;
                        tab.search_used_literal_fallback = false;
                        tab.search_results = None;
                    }
                }
                tab.recompute_visible_rows();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab_id.is_none() {
            return;
        }
        let focus = self.search_input.read(cx).focus().clone();
        window.focus(&focus, cx);
        self.search_input.update(cx, |input, cx| {
            let len = input.text().len();
            if len > 0 {
                input.select(0..len, cx);
            }
        });
    }

    pub fn clear_search(&mut self, cx: &mut Context<Self>) {
        let had_text = !self.search_input.read(cx).is_empty();
        if had_text {
            self.search_input.update(cx, |input, cx| {
                input.set_text("", cx);
            });
        } else {
            self.set_search_query(String::new(), cx);
        }
    }

    pub fn jump_to_head(&mut self, cx: &mut Context<Self>) {
        let Some(head) = self.active_tab().and_then(|t| t.head_hash.clone()) else {
            return;
        };
        self.jump_to_commit(head, cx);
    }

    pub fn jump_to_commit(&mut self, hash: String, cx: &mut Context<Self>) {
        let info = self
            .active_tab()
            .and_then(|t| t.visible_index_of(&hash).map(|ix| (t.id, ix)));
        if let Some((tab_id, ix)) = info {
            if let Some(tab) = self.active_tab() {
                tab.graph_scroll.scroll_to_item(ix, ScrollStrategy::Center);
            }
            self.select_commit(hash.clone(), cx);
            self.trigger_commit_flash(tab_id, hash, cx);
        } else {
            self.push_toast(
                "未在当前视图中找到该提交",
                Severity::Info,
                Some(format!("提交 {} 可能被当前筛选条件隐藏或尚未加载。", short_hash(&hash))),
                cx,
            );
        }
    }

    /// 触发指定提交行的 900ms 定位高亮闪动（仅作用于目标单行）。
    pub fn trigger_commit_flash(&mut self, tab_id: u64, hash: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_by_id_mut(tab_id) else {
            return;
        };
        let initial_serial = tab.trigger_commit_flash(hash.clone());
        let hash_defer = hash.clone();
        let weak = cx.weak_entity();
        cx.defer(move |cx| {
            weak.update(cx, |this, cx| {
                if let Some(tab) = this.tab_by_id_mut(tab_id)
                    && tab.bump_commit_flash_key(&hash_defer, initial_serial)
                {
                    cx.notify();
                }
            })
            .ok();
        });

        let cleanup_delay = if cx.theme().reduced_motion {
            Duration::from_millis(40)
        } else {
            Duration::from_millis(1_000)
        };
        cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(cleanup_delay).await;
            weak.update(cx, |this, cx| {
                if let Some(tab) = this.tab_by_id_mut(tab_id)
                    && let Some((h, s)) = &tab.flash_commit
                    && h == &hash
                    && *s <= initial_serial.saturating_add(1)
                {
                    tab.flash_commit = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn toggle_details_pane(&mut self, cx: &mut Context<Self>) {
        self.persisted.details_open = !self.persisted.details_open;
        if self.persisted.details_open {
            self.details_open_serial = self.details_open_serial.saturating_add(1);
        }
        self.save_state();
        cx.notify();
    }

    pub fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let next_dark = !cx.theme().is_dark();
        let mode = if next_dark { Mode::Dark } else { Mode::Light };
        Theme::set_mode(mode, cx);
        self.persisted.dark_mode = next_dark;
        self.save_state();
        cx.notify();
    }

    pub fn set_theme_preset(&mut self, preset: ThemePreset, cx: &mut Context<Self>) {
        if self.persisted.theme_preset == preset {
            return;
        }
        self.persisted.theme_preset = preset;
        apply_theme_preset(preset, cx);
        self.save_state();
        self.push_toast(
            format!("已切换至「{}」配色", preset.display_name()),
            Severity::Info,
            None,
            cx,
        );
        cx.notify();
    }

    pub fn toggle_theme_preset(&mut self, cx: &mut Context<Self>) {
        let next = self.persisted.theme_preset.toggled();
        self.set_theme_preset(next, cx);
    }

    pub fn set_column_width(&mut self, col: ResizeCol, width: f32, cx: &mut Context<Self>) {
        match col {
            ResizeCol::Graph => {
                self.persisted.column_widths.graph = width;
                self.persisted.column_widths.graph_manual = true;
                let widths = self.persisted.column_widths.clone();
                if let Some(tab) = self.active_tab_mut() {
                    let effective = crate::ui_graph::effective_graph_width(tab, &widths);
                    let max_scroll = crate::ui_graph::max_graph_scroll_x(tab, effective);
                    tab.graph_scroll_x = tab.graph_scroll_x.clamp(0.0, max_scroll);
                }
            }
            ResizeCol::Date => self.persisted.column_widths.date = width,
            ResizeCol::Author => self.persisted.column_widths.author = width,
            ResizeCol::Commit => self.persisted.column_widths.commit = width,
        }
        self.save_state();
        cx.notify();
    }

    /// 分类当前滚动事件的轴向意图（用于横向手势期间锁死外层列表，避免斜向串滚）。
    pub fn classify_scroll_axis(
        &mut self,
        event: &gpui::ScrollEvent,
    ) -> crate::ui_graph::ScrollAxisDecision {
        self.scroll_latch.classify(event)
    }

    /// 水平滚动当前活动仓库的图形列（整列同步平移，并在触边时施加阻尼橡皮筋回弹）。
    pub fn handle_graph_horizontal_scroll(
        &mut self,
        delta_x: f32,
        touch_phase: TouchPhase,
        precise: bool,
        cx: &mut Context<Self>,
    ) {
        let reduced_motion = cx.theme().reduced_motion;
        let widths = self.persisted.column_widths.clone();
        let Some(tab_id) = self.active_tab_id else {
            return;
        };
        let Some(tab) = self.tab_by_id_mut(tab_id) else {
            return;
        };

        if touch_phase == TouchPhase::Started || !precise {
            tab.graph_overscroll.begin_touch();
        }

        if matches!(touch_phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            tab._graph_overscroll_task = None;
            tab.graph_overscroll.end_touch(reduced_motion);
            cx.notify();
            return;
        }

        if delta_x.abs() > 0.01 {
            let now = Instant::now();
            let remaining = tab
                .graph_overscroll
                .consume_opposite(delta_x, now, reduced_motion);

            if remaining.abs() > 0.01 {
                let graph_col_w = crate::ui_graph::effective_graph_width(tab, &widths);
                let max_scroll = crate::ui_graph::max_graph_scroll_x(tab, graph_col_w);
                let target = tab.graph_scroll_x - remaining;
                let clamped = target.clamp(0.0, max_scroll);
                tab.graph_scroll_x = clamped;

                // remaining > 0 且到达最左 (target < 0) => 左触边正向过冲；
                // remaining < 0 且到达最右 (target > max_scroll) => 右触边负向过冲。
                let overflow = clamped - target;
                if overflow.abs() > 0.01 {
                    tab.graph_overscroll.pull_at(overflow, now, reduced_motion);
                }
            }
        }

        if tab.graph_overscroll.is_active()
            && !tab.graph_overscroll.is_animating()
            && !reduced_motion
        {
            tab._graph_overscroll_task = Some(cx.spawn(async move |weak, cx| {
                cx.background_executor()
                    .timer(RubberBandSpring::IDLE_RELEASE_TIMEOUT + Duration::from_millis(6))
                    .await;
                weak.update(cx, |this, cx| {
                    let reduced = cx.theme().reduced_motion;
                    if let Some(tab) = this.tab_by_id_mut(tab_id)
                        && tab.graph_overscroll.needs_idle_release(Instant::now())
                    {
                        tab.graph_overscroll.release(reduced);
                        cx.notify();
                    }
                })
                .ok();
            }));
        }
        cx.notify();
    }

    /// 处理主提交列表顶部与底部的纯物理橡皮筋触边阻尼回弹反馈。
    pub fn handle_graph_list_vertical_scroll(
        &mut self,
        event: &ScrollEvent,
        cx: &mut Context<Self>,
    ) {
        if event.modifiers.shift {
            return;
        }
        let delta = event.delta.pixel_delta(px(row_height(self.ui_scale())));
        let dx = f32::from(delta.x);
        let dy = f32::from(delta.y);
        if dx.abs() > dy.abs() && dx.abs() > 0.5 {
            return;
        }

        let reduced_motion = cx.theme().reduced_motion;
        let precise = event.delta.precise();
        let Some(tab_id) = self.active_tab_id else {
            return;
        };
        let Some(tab) = self.tab_by_id_mut(tab_id) else {
            return;
        };

        if event.touch_phase == TouchPhase::Started || !precise {
            tab.vertical_overscroll.begin_touch();
        }

        if matches!(event.touch_phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            tab._vertical_overscroll_task = None;
            let was_active = tab.vertical_overscroll.is_active();
            tab.vertical_overscroll.end_touch(reduced_motion);
            if was_active {
                cx.notify();
            }
            return;
        }

        if tab.vertical_overscroll.is_ignoring_momentum() {
            return;
        }

        if tab.visible_commit_count() == 0 || tab.loading_graph {
            return;
        }

        let (at_top, at_bottom) = {
            let state = tab.graph_scroll.0.borrow();
            let offset_y = state.base_handle.offset().y;
            let max_y = state.base_handle.max_offset().y;
            let top = offset_y >= px(-1.0);
            let bottom = max_y <= px(0.0) || -offset_y >= max_y - px(1.0);
            (top, bottom)
        };

        let now = Instant::now();
        let mut changed = false;

        if dy > 0.01 {
            let rem = if tab.vertical_overscroll.offset() < -0.01 {
                changed = true;
                tab.vertical_overscroll
                    .consume_opposite(dy, now, reduced_motion)
            } else {
                dy
            };
            if rem > 0.01 && at_top {
                tab.vertical_overscroll.pull_at(rem, now, reduced_motion);
                changed = true;
            }
        } else if dy < -0.01 {
            let rem = if tab.vertical_overscroll.offset() > 0.01 {
                changed = true;
                tab.vertical_overscroll
                    .consume_opposite(dy, now, reduced_motion)
            } else {
                dy
            };
            if rem < -0.01 && at_bottom && !tab.more_available {
                tab.vertical_overscroll.pull_at(rem, now, reduced_motion);
                changed = true;
            }
        }

        let active = tab.vertical_overscroll.is_active();
        if active && !tab.vertical_overscroll.is_animating() && !reduced_motion {
            tab._vertical_overscroll_task = Some(cx.spawn(async move |weak, cx| {
                cx.background_executor()
                    .timer(RubberBandSpring::IDLE_RELEASE_TIMEOUT + Duration::from_millis(6))
                    .await;
                weak.update(cx, |this, cx| {
                    let reduced = cx.theme().reduced_motion;
                    if let Some(tab) = this.tab_by_id_mut(tab_id)
                        && tab.vertical_overscroll.needs_idle_release(Instant::now())
                    {
                        tab.vertical_overscroll.release(reduced);
                        cx.notify();
                    }
                })
                .ok();
            }));
        }

        if changed || active {
            cx.notify();
        }
    }

    /// 将当前活动仓库图形列的水平滚动位置一键复位到最左侧。
    pub fn reset_graph_scroll_x(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let had_overscroll = tab.graph_overscroll.is_active();
        tab.graph_overscroll.reset();
        tab._graph_overscroll_task = None;
        if tab.graph_scroll_x > 0.0 || had_overscroll {
            tab.graph_scroll_x = 0.0;
            cx.notify();
        }
    }

    pub fn set_vertical_split(&mut self, sizes: [f32; 2]) {
        self.persisted.vertical_split = sizes;
        self.save_state();
    }

    pub fn set_horizontal_split(&mut self, sizes: [f32; 2]) {
        self.persisted.horizontal_split = sizes;
        self.save_state();
    }

    /* ---------- 右键上下文菜单 & 模态框打开 ---------- */

    pub fn open_context_menu(
        &mut self,
        target: ContextMenuTarget,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.next_ctx_request_id += 1;
        self.context_menu = Some(ContextMenuState {
            request_id: self.next_ctx_request_id,
            position,
            target,
        });
        cx.notify();
    }

    pub fn copy_text_with_toast(
        &self,
        text: String,
        toast_title: String,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
        let first_line = text.lines().next().unwrap_or(&text).trim();
        let preview = if first_line.chars().count() > 64 {
            let truncated: String = first_line.chars().take(64).collect();
            format!("{truncated}…")
        } else {
            first_line.to_string()
        };
        let body = (!preview.is_empty()).then_some(preview);
        self.push_toast(toast_title, Severity::Success, body, cx);
    }

    pub fn refresh_repo_workspace(&mut self, repo_path: PathBuf, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_by_path_mut(&repo_path) {
            let id = tab.id;
            self.load_tab_graph(id, false, cx);
        } else {
            self.refresh_active_tab(false, cx);
        }
    }

    fn set_modal(&mut self, modal: ActiveModal, cx: &mut Context<Self>) {
        self.modal_serial = self.modal_serial.saturating_add(1);
        self.modal = Some(modal);
        cx.notify();
    }

    pub fn open_create_branch_modal(
        &mut self,
        repo_path: PathBuf,
        start_point: String,
        initial_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| TextInput::new(window, cx).placeholder("例如 feature/my-branch"));
        if let Some(default_name) = initial_name {
            input.update(cx, |input, cx| {
                input.set_text(default_name, cx);
                let len = input.text().len();
                if len > 0 {
                    input.select(0..len, cx);
                }
            });
        }
        self.set_modal(
            ActiveModal::CreateBranch {
                repo_path,
                start_point,
                input,
                checkout: true,
                shake_serial: 0,
                validation_error: None,
            },
            cx,
        );
    }

    pub fn open_rename_branch_modal(
        &mut self,
        repo_path: PathBuf,
        old_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| TextInput::new(window, cx).placeholder("输入新分支名称"));
        input.update(cx, |input, cx| {
            input.set_text(old_name.clone(), cx);
            let len = input.text().len();
            if len > 0 {
                input.select(0..len, cx);
            }
        });
        self.set_modal(
            ActiveModal::RenameBranch {
                repo_path,
                old_name,
                input,
                shake_serial: 0,
                validation_error: None,
            },
            cx,
        );
    }

    pub fn open_create_tag_modal(
        &mut self,
        repo_path: PathBuf,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name_input = cx.new(|cx| TextInput::new(window, cx).placeholder("v1.0.0"));
        let message_input = cx.new(|cx| TextInput::new(window, cx).placeholder("可选附注说明"));
        self.set_modal(
            ActiveModal::CreateTag {
                repo_path,
                target,
                name_input,
                message_input,
                shake_serial: 0,
                validation_error: None,
            },
            cx,
        );
    }

    pub fn open_stash_push_modal(
        &mut self,
        repo_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message_input = cx.new(|cx| TextInput::new(window, cx).placeholder("输入 Stash 描述（可留空）"));
        self.set_modal(
            ActiveModal::StashPush {
                repo_path,
                message_input,
                include_untracked: true,
            },
            cx,
        );
    }

    pub fn open_push_branch_modal(
        &mut self,
        repo_path: PathBuf,
        target_branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let mut local_branches = self
            .tabs
            .iter()
            .find(|t| t.path == repo_path)
            .or_else(|| self.active_tab())
            .and_then(|t| t.info.as_ref().map(|i| i.branches.clone()))
            .unwrap_or_default();
        if !local_branches.iter().any(|b| b == &target_branch) {
            local_branches.insert(0, target_branch.clone());
        }

        let remote_branch_input = cx.new(|cx| TextInput::new(window, cx).placeholder("远程分支名"));
        remote_branch_input.update(cx, |input, cx| {
            input.set_text(target_branch.clone(), cx);
        });

        let repo_bg = repo_path.clone();
        let branches_bg = local_branches.clone();
        let target_bg = target_branch.clone();
        cx.spawn(async move |weak, cx| {
            let (remotes, upstreams) = cx
                .background_spawn(async move {
                    let remotes = cli.list_remotes(&repo_bg).unwrap_or_default();
                    let mut upstreams = BTreeMap::new();
                    if branches_bg.len() <= 64 {
                        for b in &branches_bg {
                            let up = cli.branch_upstream(&repo_bg, b).ok().flatten();
                            upstreams.insert(b.clone(), up);
                        }
                    } else {
                        let up = cli.branch_upstream(&repo_bg, &target_bg).ok().flatten();
                        upstreams.insert(target_bg, up);
                    }
                    (remotes, upstreams)
                })
                .await;

            weak.update(cx, |this, cx| {
                let upstream = upstreams.get(&target_branch).and_then(|u| u.as_deref());
                let defaults = compute_push_defaults(&target_branch, upstream, &remotes);
                remote_branch_input.update(cx, |input, cx| {
                    input.set_text(defaults.remote_branch.clone(), cx);
                });
                this.set_modal(
                    ActiveModal::PushBranch {
                        repo_path,
                        local_branches,
                        selected_branch: target_branch,
                        upstreams,
                        remotes,
                        selected_remote: defaults.remote,
                        remote_branch_input,
                        set_upstream: defaults.set_upstream,
                        push_tags: false,
                        force: PushForceMode::None,
                        shake_serial: 0,
                        validation_error: None,
                    },
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    /* ---------- 后台写操作与防数据丢失安全检查 ---------- */

    fn ensure_cli(&self, cx: &mut Context<Self>) -> Option<GitCli> {
        if let Some(cli) = &self.cli {
            Some(cli.clone())
        } else {
            self.push_toast(
                "未找到 Git 可执行文件",
                Severity::Danger,
                Some("请确认系统 PATH 中已安装 git。".to_string()),
                cx,
            );
            None
        }
    }

    pub fn run_sync_or_publish_active(&mut self, cx: &mut Context<Self>) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.syncing {
            return;
        }
        let spec = sync_button_spec(
            tab.sync_status.as_ref(),
            tab.syncing,
            tab.sync_action_label.as_deref(),
        );
        let tab_id = tab.id;
        let repo_path = tab.path.clone();

        match spec.action {
            SyncButtonAction::Sync => {
                tab.syncing = true;
                tab.sync_action_label = Some("同步中…".into());
                cx.notify();

                cx.spawn(async move |weak, cx| {
                    let res = cx
                        .background_spawn(async move { cli.sync_branch(&repo_path) })
                        .await;
                    weak.update(cx, |this, cx| {
                        if let Some(tab) = this.tab_by_id_mut(tab_id) {
                            tab.syncing = false;
                            tab.sync_action_label = None;
                        }
                        match res {
                            Ok(outcome) => {
                                if let Some(tab) = this.tab_by_id_mut(tab_id) {
                                    tab.sync_flash_serial = tab.sync_flash_serial.saturating_add(1);
                                }
                                let detail = match (outcome.pulled, outcome.pushed) {
                                    (true, true) => Some("已拉取远端更新并推送本地提交。".to_string()),
                                    (true, false) => Some("已拉取远端更新。".to_string()),
                                    (false, true) => Some("已推送本地提交到远端。".to_string()),
                                    (false, false) => Some("本地分支与远端已是最新。".to_string()),
                                };
                                this.push_toast(
                                    format!("已与 {} 同步", outcome.upstream),
                                    Severity::Success,
                                    detail,
                                    cx,
                                );
                                this.soft_refresh_tab(tab_id, cx);
                            }
                            Err(err) => {
                                this.push_toast(
                                    "同步失败（已中止推送）",
                                    Severity::Danger,
                                    Some(err.to_string()),
                                    cx,
                                );
                                // pull 失败或出现冲突时立即刷新工作区状态让用户看到冲突
                                this.soft_refresh_tab(tab_id, cx);
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            SyncButtonAction::Publish { remote, branch } => {
                tab.syncing = true;
                tab.sync_action_label = Some("发布中…".into());
                cx.notify();

                let remote_label = remote.clone();
                let branch_label = branch.clone();
                cx.spawn(async move |weak, cx| {
                    let res = cx
                        .background_spawn(async move {
                            cli.publish_branch(&repo_path, &remote, &branch)
                        })
                        .await;
                    weak.update(cx, |this, cx| {
                        if let Some(tab) = this.tab_by_id_mut(tab_id) {
                            tab.syncing = false;
                            tab.sync_action_label = None;
                        }
                        match res {
                            Ok(_) => {
                                if let Some(tab) = this.tab_by_id_mut(tab_id) {
                                    tab.sync_flash_serial = tab.sync_flash_serial.saturating_add(1);
                                }
                                this.push_toast(
                                    format!("已发布分支 {branch_label} 到 {remote_label}"),
                                    Severity::Success,
                                    None,
                                    cx,
                                );
                                this.soft_refresh_tab(tab_id, cx);
                            }
                            Err(err) => {
                                this.push_toast(
                                    "发布分支失败",
                                    Severity::Danger,
                                    Some(err.to_string()),
                                    cx,
                                );
                                this.soft_refresh_tab(tab_id, cx);
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            SyncButtonAction::None => {}
        }
    }

    pub fn run_push_branch(
        &mut self,
        repo_path: PathBuf,
        options: PushOptions,
        cx: &mut Context<Self>,
    ) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let repo_for_refresh = repo_path.clone();
        let target_label = format!(
            "{} -> {}/{}",
            options.local_branch, options.remote, options.remote_branch
        );
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move { cli.push_branch(&repo_path, &options).map(|_| ()) })
                .await;
            weak.update(cx, |this, cx| {
                this.finish_write_op(
                    &repo_for_refresh,
                    format!("已推送 {target_label}"),
                    "推送分支失败",
                    None,
                    res,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    pub fn request_checkout_branch(
        &mut self,
        repo_path: PathBuf,
        branch: String,
        cx: &mut Context<Self>,
    ) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let pending = PendingWriteOp::CheckoutBranch {
            repo_path: repo_path.clone(),
            branch: branch.clone(),
        };
        cx.spawn(async move |weak, cx| {
            let safety = cx
                .background_spawn(async move { check_checkout_safety(&cli, &repo_path) })
                .await;
            weak.update(cx, |this, cx| match safety {
                Ok(Some(warning)) => {
                    this.set_modal(
                        ActiveModal::SafetyConfirm {
                            warning,
                            confirm_label: format!("仍然检出 {branch}").into(),
                            pending,
                        },
                        cx,
                    );
                }
                Ok(None) | Err(_) => {
                    this.execute_pending_write(pending, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn request_checkout_commit(
        &mut self,
        repo_path: PathBuf,
        hash: String,
        cx: &mut Context<Self>,
    ) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let short = short_hash(&hash).to_string();
        let pending = PendingWriteOp::CheckoutCommit {
            repo_path: repo_path.clone(),
            hash,
        };
        cx.spawn(async move |weak, cx| {
            let safety = cx
                .background_spawn(async move { check_checkout_safety(&cli, &repo_path) })
                .await;
            weak.update(cx, |this, cx| match safety {
                Ok(Some(warning)) => {
                    this.set_modal(
                        ActiveModal::SafetyConfirm {
                            warning,
                            confirm_label: format!("仍然检出 {short}").into(),
                            pending,
                        },
                        cx,
                    );
                }
                Ok(None) | Err(_) => {
                    this.execute_pending_write(pending, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn request_reset(
        &mut self,
        repo_path: PathBuf,
        hash: String,
        mode: ResetMode,
        cx: &mut Context<Self>,
    ) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let short = short_hash(&hash).to_string();
        let pending = PendingWriteOp::ResetToCommit {
            repo_path: repo_path.clone(),
            hash,
            mode,
        };
        cx.spawn(async move |weak, cx| {
            let safety = cx
                .background_spawn(async move { check_reset_safety(&cli, &repo_path, mode) })
                .await;
            weak.update(cx, |this, cx| match safety {
                Ok(Some(warning)) => {
                    this.set_modal(
                        ActiveModal::SafetyConfirm {
                            warning,
                            confirm_label: "确认 Hard Reset".into(),
                            pending,
                        },
                        cx,
                    );
                }
                Ok(None) | Err(_) => {
                    let mode_label = match mode {
                        ResetMode::Soft => "Soft",
                        ResetMode::Mixed => "Mixed",
                        ResetMode::Hard => "Hard",
                    };
                    this.set_modal(
                        ActiveModal::SimpleConfirm {
                            title: format!("Reset ({mode_label}) 到 {short}？").into(),
                            message: format!("将把当前分支指针重置到提交 {short}。").into(),
                            confirm_label: format!("执行 Reset ({mode_label})").into(),
                            destructive: mode == ResetMode::Hard,
                            pending,
                        },
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn confirm_delete_branch(
        &mut self,
        repo_path: PathBuf,
        branch: String,
        cx: &mut Context<Self>,
    ) {
        self.set_modal(
            ActiveModal::SimpleConfirm {
                title: format!("删除本地分支 {branch}？").into(),
                message: "将执行 git branch -d 删除该分支（若有未合并提交则会阻止删除）。".into(),
                confirm_label: "删除分支".into(),
                destructive: true,
                pending: PendingWriteOp::DeleteBranch {
                    repo_path,
                    name: branch,
                    force: false,
                },
            },
            cx,
        );
    }

    pub fn confirm_delete_tag(&mut self, repo_path: PathBuf, tag: String, cx: &mut Context<Self>) {
        self.set_modal(
            ActiveModal::SimpleConfirm {
                title: format!("删除本地标签 {tag}？").into(),
                message: "将从本地仓库移除该标签引用。".into(),
                confirm_label: "删除标签".into(),
                destructive: true,
                pending: PendingWriteOp::DeleteTag {
                    repo_path,
                    name: tag,
                },
            },
            cx,
        );
    }

    pub fn confirm_stash_pop(&mut self, repo_path: PathBuf, index: usize, cx: &mut Context<Self>) {
        self.set_modal(
            ActiveModal::SimpleConfirm {
                title: format!("弹出 stash@{{{index}}}？").into(),
                message: "将应用该 Stash 到工作区并从 Stash 栈中移除。".into(),
                confirm_label: "弹出 Stash".into(),
                destructive: false,
                pending: PendingWriteOp::StashPop { repo_path, index },
            },
            cx,
        );
    }

    pub fn confirm_stash_drop(&mut self, repo_path: PathBuf, index: usize, cx: &mut Context<Self>) {
        self.set_modal(
            ActiveModal::SimpleConfirm {
                title: format!("丢弃 stash@{{{index}}}？").into(),
                message: "丢弃后该 Stash 中的修改记录将很难恢复。".into(),
                confirm_label: "丢弃 Stash".into(),
                destructive: true,
                pending: PendingWriteOp::StashDrop { repo_path, index },
            },
            cx,
        );
    }

    pub fn confirm_discard_file(
        &mut self,
        repo_path: PathBuf,
        file_path: String,
        cx: &mut Context<Self>,
    ) {
        self.set_modal(
            ActiveModal::SimpleConfirm {
                title: "丢弃工作区修改？".into(),
                message: format!("确定要丢弃文件 {file_path} 中未暂存的修改吗？此操作不可撤销。").into(),
                confirm_label: "丢弃修改".into(),
                destructive: true,
                pending: PendingWriteOp::DiscardFile {
                    repo_path,
                    file_path,
                },
            },
            cx,
        );
    }

    pub fn run_create_branch(
        &mut self,
        repo_path: PathBuf,
        name: String,
        start_point: String,
        checkout: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let repo_for_refresh = repo_path.clone();
        let label = name.clone();
        let flash_target = Some(PendingFlashTarget::Hash(start_point.clone()));
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move {
                    cli.create_branch(&repo_path, &name, Some(&start_point), checkout)
                })
                .await;
            weak.update(cx, |this, cx| {
                this.finish_write_op(
                    &repo_for_refresh,
                    format!("已创建分支 {label}"),
                    "创建分支失败",
                    flash_target,
                    res,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    pub fn run_rename_branch(
        &mut self,
        repo_path: PathBuf,
        old_name: String,
        new_name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let repo_for_refresh = repo_path.clone();
        let old_label = old_name.clone();
        let new_label = new_name.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move { cli.rename_branch(&repo_path, &old_name, &new_name) })
                .await;
            weak.update(cx, |this, cx| {
                if res.is_ok()
                    && let Some(tab) = this.tab_by_path_mut(&repo_for_refresh)
                    && tab.filter.branch.as_deref() == Some(old_label.as_str())
                {
                    tab.filter.branch = Some(new_label.clone());
                }
                this.finish_write_op(
                    &repo_for_refresh,
                    format!("已将分支 {old_label} 重命名为 {new_label}"),
                    "重命名分支失败",
                    None,
                    res,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    pub fn run_create_tag(
        &mut self,
        repo_path: PathBuf,
        name: String,
        target: String,
        message: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let repo_for_refresh = repo_path.clone();
        let label = name.clone();
        let flash_target = Some(PendingFlashTarget::Hash(target.clone()));
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move {
                    cli.create_tag(&repo_path, &name, &target, message.as_deref())
                })
                .await;
            weak.update(cx, |this, cx| {
                this.finish_write_op(
                    &repo_for_refresh,
                    format!("已创建标签 {label}"),
                    "创建标签失败",
                    flash_target,
                    res,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    pub fn run_cherry_pick(&mut self, repo_path: PathBuf, hash: String, cx: &mut Context<Self>) {
        self.execute_pending_write(PendingWriteOp::CherryPick { repo_path, hash }, cx);
    }

    pub fn run_revert_commit(&mut self, repo_path: PathBuf, hash: String, cx: &mut Context<Self>) {
        self.execute_pending_write(PendingWriteOp::RevertCommit { repo_path, hash }, cx);
    }

    pub fn run_stash_push(
        &mut self,
        repo_path: PathBuf,
        message: Option<String>,
        include_untracked: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let repo_for_refresh = repo_path.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move {
                    cli.stash_push(&repo_path, message.as_deref(), include_untracked)
                })
                .await;
            weak.update(cx, |this, cx| {
                this.finish_write_op(
                    &repo_for_refresh,
                    "已保存工作区到 Stash",
                    "保存 Stash 失败",
                    Some(PendingFlashTarget::Head),
                    res,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    pub fn run_stash_apply(&mut self, repo_path: PathBuf, index: usize, cx: &mut Context<Self>) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let repo_for_refresh = repo_path.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move { cli.stash_apply(&repo_path, index) })
                .await;
            weak.update(cx, |this, cx| {
                this.finish_write_op(
                    &repo_for_refresh,
                    format!("已应用 stash@{{{index}}}"),
                    "应用 Stash 失败",
                    Some(PendingFlashTarget::Head),
                    res,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    pub fn run_stage_file(
        &mut self,
        repo_path: PathBuf,
        file_path: String,
        cx: &mut Context<Self>,
    ) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let repo_for_refresh = repo_path.clone();
        let label = file_path.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move { cli.stage_file(&repo_path, &file_path) })
                .await;
            weak.update(cx, |this, cx| {
                this.finish_write_op(
                    &repo_for_refresh,
                    format!("已暂存 {label}"),
                    "暂存文件失败",
                    None,
                    res,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    pub fn run_unstage_file(
        &mut self,
        repo_path: PathBuf,
        file_path: String,
        cx: &mut Context<Self>,
    ) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let repo_for_refresh = repo_path.clone();
        let label = file_path.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move { cli.unstage_file(&repo_path, &file_path) })
                .await;
            weak.update(cx, |this, cx| {
                this.finish_write_op(
                    &repo_for_refresh,
                    format!("已取消暂存 {label}"),
                    "取消暂存失败",
                    None,
                    res,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    pub fn execute_pending_write(&mut self, op: PendingWriteOp, cx: &mut Context<Self>) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let (repo_for_refresh, ok_msg, err_title, flash_target) = match &op {
            PendingWriteOp::CheckoutBranch { repo_path, branch } => (
                repo_path.clone(),
                format!("已切换到分支 {branch}"),
                "检出分支失败",
                Some(PendingFlashTarget::Head),
            ),
            PendingWriteOp::CheckoutCommit { repo_path, hash } => (
                repo_path.clone(),
                format!("已检出提交 {}", short_hash(hash)),
                "检出提交失败",
                Some(PendingFlashTarget::Hash(hash.clone())),
            ),
            PendingWriteOp::DeleteBranch {
                repo_path, name, ..
            } => (
                repo_path.clone(),
                format!("已删除分支 {name}"),
                "删除分支失败",
                None,
            ),
            PendingWriteOp::DeleteTag { repo_path, name } => (
                repo_path.clone(),
                format!("已删除标签 {name}"),
                "删除标签失败",
                None,
            ),
            PendingWriteOp::CherryPick { repo_path, hash } => (
                repo_path.clone(),
                format!("已 Cherry-pick {}", short_hash(hash)),
                "Cherry-pick 失败",
                Some(PendingFlashTarget::Head),
            ),
            PendingWriteOp::RevertCommit { repo_path, hash } => (
                repo_path.clone(),
                format!("已 Revert {}", short_hash(hash)),
                "Revert 失败",
                Some(PendingFlashTarget::Head),
            ),
            PendingWriteOp::ResetToCommit {
                repo_path, hash, ..
            } => (
                repo_path.clone(),
                format!("已 Reset 到 {}", short_hash(hash)),
                "Reset 失败",
                Some(PendingFlashTarget::Hash(hash.clone())),
            ),
            PendingWriteOp::StashPop { repo_path, index } => (
                repo_path.clone(),
                format!("已弹出 stash@{{{index}}}"),
                "弹出 Stash 失败",
                Some(PendingFlashTarget::Head),
            ),
            PendingWriteOp::StashDrop { repo_path, index } => (
                repo_path.clone(),
                format!("已丢弃 stash@{{{index}}}"),
                "丢弃 Stash 失败",
                None,
            ),
            PendingWriteOp::DiscardFile {
                repo_path,
                file_path,
            } => (
                repo_path.clone(),
                format!("已丢弃更改 {file_path}"),
                "丢弃文件更改失败",
                None,
            ),
        };

        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move {
                    match op {
                        PendingWriteOp::CheckoutBranch { repo_path, branch } => {
                            cli.checkout_branch(&repo_path, &branch)
                        }
                        PendingWriteOp::CheckoutCommit { repo_path, hash } => {
                            cli.checkout_commit(&repo_path, &hash)
                        }
                        PendingWriteOp::DeleteBranch {
                            repo_path,
                            name,
                            force,
                        } => cli.delete_branch(&repo_path, &name, force),
                        PendingWriteOp::DeleteTag { repo_path, name } => {
                            cli.delete_tag(&repo_path, &name)
                        }
                        PendingWriteOp::CherryPick { repo_path, hash } => {
                            cli.cherry_pick(&repo_path, &hash)
                        }
                        PendingWriteOp::RevertCommit { repo_path, hash } => {
                            cli.revert_commit(&repo_path, &hash)
                        }
                        PendingWriteOp::ResetToCommit {
                            repo_path,
                            hash,
                            mode,
                        } => cli.reset_to_commit(&repo_path, &hash, mode),
                        PendingWriteOp::StashPop { repo_path, index } => {
                            cli.stash_pop(&repo_path, index)
                        }
                        PendingWriteOp::StashDrop { repo_path, index } => {
                            cli.stash_drop(&repo_path, index)
                        }
                        PendingWriteOp::DiscardFile {
                            repo_path,
                            file_path,
                        } => cli.discard_file(&repo_path, &file_path),
                    }
                })
                .await;

            weak.update(cx, |this, cx| {
                this.finish_write_op(
                    &repo_for_refresh,
                    ok_msg,
                    err_title,
                    flash_target,
                    res,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    fn finish_write_op(
        &mut self,
        repo_path: &std::path::Path,
        ok_title: impl Into<SharedString>,
        err_title: &'static str,
        flash_target: Option<PendingFlashTarget>,
        res: git_graph_ops::Result<()>,
        cx: &mut Context<Self>,
    ) {
        match res {
            Ok(()) => {
                self.push_toast(ok_title, Severity::Success, None, cx);
                if let Some(tab) = self.tab_by_path_mut(repo_path) {
                    let id = tab.id;
                    tab.pending_flash_target = flash_target;
                    self.soft_refresh_tab(id, cx);
                }
            }
            Err(err) => {
                self.push_toast(err_title, Severity::Danger, Some(err.to_string()), cx);
            }
        }
    }

    /* ---------- 壳层 UI 渲染 ---------- */

    fn render_titlebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let scale = self.ui_scale();
        let (colors, dark, title_text_size, tab_text_size, tab_radius) = {
            let theme = cx.theme();
            (
                theme.colors.clone(),
                theme.is_dark(),
                theme.text_size(TextSize::Base),
                theme.text_size(TextSize::Sm),
                theme.radius(Radius::Sm),
            )
        };
        let weak = cx.weak_entity();
        let fullscreen = window.is_fullscreen();

        let tabs_strip: AnyElement = if self.tabs.is_empty() {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Icon::new(IconName::GitBranch)
                        .size(IconSize::Sm)
                        .color(colors.accent),
                )
                .child(
                    div()
                        .text_size(title_text_size)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(colors.fg)
                        .child("Git Graph RS"),
                )
                .into_any_element()
        } else {
            let tab_items = self.tabs.iter().map(|tab| {
                let is_active = self.active_tab_id == Some(tab.id);
                let tab_id = tab.id;
                let dirty = tab.uncommitted_count > 0;
                let weak_select = weak.clone();
                let weak_close = weak.clone();
                let tab_bg = if is_active { colors.bg } else { colors.sunken };
                let tab_border = if is_active {
                    colors.border
                } else {
                    gpui::transparent_black()
                };
                let fg = if is_active {
                    colors.fg
                } else {
                    colors.fg_muted
                };
                let hover_bg = colors.hover;

                div()
                    .id(("repo-tab", tab_id as usize))
                    .flex_none()
                    .h(px(28.0 * scale))
                    .max_w(px(210.0 * scale))
                    .px_2p5()
                    .rounded(tab_radius)
                    .bg(tab_bg)
                    .border_1()
                    .border_color(tab_border)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .cursor_pointer()
                    .occlude()
                    .when(!is_active, |el| el.hover(move |s| s.bg(hover_bg)))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        weak_select
                            .update(cx, |this, cx| this.activate_tab(tab_id, cx))
                            .ok();
                    })
                    .child(
                        Icon::new(IconName::GitBranch)
                            .size(IconSize::Xs)
                            .color(if is_active { colors.accent } else { fg }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_size(tab_text_size)
                            .font_weight(if is_active {
                                gpui::FontWeight::MEDIUM
                            } else {
                                gpui::FontWeight::NORMAL
                            })
                            .text_color(fg)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(tab.name.clone()),
                    )
                    .when(dirty, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .size(px(5.0 * scale))
                                .rounded_full()
                                .bg(colors.warning),
                        )
                    })
                    .child(
                        div()
                            .id(("repo-tab-close", tab_id as usize))
                            .flex_none()
                            .size(px(16.0 * scale))
                            .rounded(tab_radius)
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover_bg))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                weak_close
                                    .update(cx, |this, cx| this.close_tab(tab_id, cx))
                                    .ok();
                            })
                            .child(
                                Icon::new(IconName::X)
                                    .size(IconSize::Xs)
                                    .color(colors.fg_muted),
                            ),
                    )
                    .into_any_element()
            });

            div()
                .id("repo-tabs-strip")
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .gap_1()
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .children(tab_items)
                .into_any_element()
        };

        let active_repo_path = self.active_tab().map(|t| t.path.clone());
        let recent_menu = build_recent_repos_menu(
            &self.persisted.recent_repos,
            active_repo_path.as_deref(),
            &weak,
        );
        let recent_btn = OverflowMenu::new("titlebar-recent-repos", recent_menu)
            .icon(IconName::ChevronDown)
            .tooltip("最近打开的仓库");

        let open_btn = IconButton::new("titlebar-open-repo", IconName::Plus)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Md)
            .tooltip("打开本地仓库 (⌘O)")
            .on_click({
                let weak = weak.clone();
                move |_, _, cx| {
                    weak.update(cx, |this, cx| this.pick_and_open_repo(cx)).ok();
                }
            });

        let palette_menu = build_theme_preset_menu(self.persisted.theme_preset, dark, &weak);
        let palette_btn = OverflowMenu::new("titlebar-theme-preset", palette_menu)
            .icon(IconName::Palette)
            .tooltip(format!(
                "配色主题：{}（点击切换默认 / Studio）",
                self.persisted.theme_preset.display_name()
            ));

        let theme_btn = IconButton::new(
            "titlebar-theme-toggle",
            if dark { IconName::Sun } else { IconName::Moon },
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Md)
        .tooltip(if dark { "切换至浅色模式" } else { "切换至深色模式" })
        .on_click({
            let weak = weak.clone();
            move |_, _, cx| {
                weak.update(cx, |this, cx| this.toggle_theme(cx)).ok();
            }
        });

        drag_region("app-titlebar-drag", window, cx)
            .w_full()
            .h(px(40.0 * scale))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .when(!fullscreen, |bar| bar.pl(px(76.0)))
            .when(fullscreen, |bar| bar.pl_3())
            .pr_3()
            .bg(colors.sunken)
            .border_b_1()
            .border_color(colors.border)
            .child(tabs_strip)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .occlude()
                    .child(recent_btn)
                    .child(open_btn)
                    .child(palette_btn)
                    .child(theme_btn),
            )
            .into_any_element()
    }

    fn render_start_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = &theme.colors;
        let weak = cx.weak_entity();

        let empty = EmptyState::new(
            "start-empty-state",
            IconName::GitBranch,
            "Git Graph RS",
        )
        .body("打开一个本地 Git 仓库，查看高速原生提交图谱、Ref 胶囊与行内 Diff。")
        .action(
            Button::new("start-open-repo-btn", "打开本地仓库")
                .icon(IconName::FolderOpen)
                .variant(ButtonVariant::Primary)
                .on_click({
                    let weak = weak.clone();
                    move |_, _, cx| {
                        weak.update(cx, |this, cx| this.pick_and_open_repo(cx)).ok();
                    }
                }),
        );

        let recent_section = (!self.persisted.recent_repos.is_empty()).then(|| {
            let items = self
                .persisted
                .recent_repos
                .iter()
                .enumerate()
                .map(|(ix, path)| {
                    let weak_open = weak.clone();
                    let weak_del = weak.clone();
                    let open_path = path.clone();
                    let del_path = path.clone();

                    ListItem::new(("recent-repo", ix), repo_display_name(path))
                        .leading(
                            Icon::new(IconName::Folder)
                                .size(IconSize::Sm)
                                .color(colors.fg_muted),
                        )
                        .description(path.display().to_string())
                        .trailing(
                            IconButton::new(("remove-recent", ix), IconName::Trash2)
                                .variant(ButtonVariant::Ghost)
                                .size(ControlSize::Md)
                                .tooltip("从最近列表移除")
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    weak_del
                                        .update(cx, |this, cx| {
                                            this.persisted.remove_recent(&del_path);
                                            this.save_state();
                                            cx.notify();
                                        })
                                        .ok();
                                }),
                        )
                        .on_click(move |_, _, cx| {
                            weak_open
                                .update(cx, |this, cx| this.open_repo_path(open_path.clone(), cx))
                                .ok();
                        })
                });

            let scale = self.ui_scale();
            div()
                .w_full()
                .max_w(px(520.0 * scale))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .px_2()
                        .text_size(theme.text_size(TextSize::Sm))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(colors.fg_muted)
                        .child("最近打开的仓库"),
                )
                .child(
                    div()
                        .rounded(theme.radius(ely_core::theme::Radius::Md))
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.surface)
                        .p_1()
                        .child(List::new().children(items)),
                )
        });

        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p_6()
            .bg(colors.bg)
            .child(
                div().w_full().max_w(px(560.0 * self.ui_scale())).child(
                    Stagger::new("start-page-stagger")
                        .entrance(Entrance::Rise)
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .justify_center()
                                .mb_4()
                                .child(empty),
                        )
                        .when_some(recent_section, |stagger, section| {
                            stagger.child(
                                div()
                                    .w_full()
                                    .flex()
                                    .justify_center()
                                    .child(section),
                            )
                        }),
                ),
            )
            .into_any_element()
    }

    fn render_workspace_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let active_tab_key = self.active_tab_id.unwrap_or(0);
        let tab_fade = since_change(
            &ElementId::from("repo-tab-transition"),
            active_tab_key,
            duration(FAST, cx),
            window,
            cx,
        );

        let content: AnyElement = match self.active_tab() {
            None => self.render_start_page(cx),
            Some(tab) => {
                let toolbar = render_filter_toolbar(self, tab, cx);
                let graph_table = render_graph_table(self, tab, cx);

                let main_area: AnyElement = if self.persisted.details_open {
                    let details_pane = render_details_pane(self, tab, cx);
                    let weak = cx.weak_entity();
                    let v_sizes = self.persisted.vertical_split;
                    SplitPane::new("main-v-split", Axis::Vertical, px(160.0 * self.ui_scale()))
                        .pane(graph_table)
                        .pane(details_pane)
                        .sizes(&v_sizes)
                        .on_resize(move |sizes, _, cx| {
                            if sizes.len() == 2 {
                                let pair = [sizes[0], sizes[1]];
                                weak.update(cx, |this, _| this.set_vertical_split(pair))
                                    .ok();
                            }
                        })
                        .into_any_element()
                } else {
                    graph_table
                };

                let menu = match &self.context_menu {
                    Some(ctx_menu) => build_context_menu(ctx_menu, cx.weak_entity()),
                    None => Menu::new(),
                };
                let request = self
                    .context_menu
                    .as_ref()
                    .map(|c| (c.request_id, c.position));

                ContextMenu::new("workspace-context-menu", menu)
                    .manual(request)
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(toolbar)
                    .child(div().flex_1().min_h_0().child(main_area))
                    .into_any_element()
            }
        };

        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .when_some(tab_fade, |el, t| el.opacity(ease_out_cubic(t)))
            .child(content)
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &Context<Self>) -> StatusBar {
        let mut bar = StatusBar::new();
        let weak = cx.weak_entity();
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let sync_flash_radius = theme.radius(Radius::Sm);
        if let Some(tab) = self.active_tab() {
            let branch_label: SharedString = match tab.info.as_ref().and_then(|i| i.head.as_ref())
            {
                Some(b) => b.clone().into(),
                None => match &tab.head_hash {
                    Some(h) => format!("HEAD ({})", short_hash(h)).into(),
                    None => "空仓库".into(),
                },
            };
            bar = bar.left(
                StatusBarItem::new("status-branch")
                    .icon(IconName::GitBranch)
                    .label(branch_label),
            );

            // 紧挨当前分支右侧：复刻 VS Code 左下角同步 / 发布状态项
            let show_sync_item = tab.syncing
                || matches!(
                    tab.sync_status,
                    Some(BranchSyncStatus::Synced { .. } | BranchSyncStatus::NoUpstream { .. })
                );
            if show_sync_item {
                let spec = sync_button_spec(
                    tab.sync_status.as_ref(),
                    tab.syncing,
                    tab.sync_action_label.as_deref(),
                );
                let mut item = StatusBarItem::new("status-sync")
                    .label(spec.status_bar_label)
                    .tooltip(spec.tooltip);
                if tab.syncing {
                    item = item.leading(
                        Spinner::new(("sync-spinner", tab.id))
                            .style(SpinnerStyle::Ring)
                            .size(IconSize::Xs),
                    );
                } else {
                    item = item.icon(spec.icon);
                }
                if !spec.disabled {
                    let weak_sync = weak.clone();
                    item = item.on_click(move |_, _, cx| {
                        weak_sync
                            .update(cx, |this, cx| this.run_sync_or_publish_active(cx))
                            .ok();
                    });
                }
                let flashed = Flash::new(("sync-status-flash", tab.id), tab.sync_flash_serial)
                    .tint(colors.success.opacity(0.28))
                    .rounded(sync_flash_radius)
                    .child(item);
                bar = bar.left(flashed);
            }

            bar = bar
                .left(StatusBarItem::new("status-uncommitted").label(
                    if tab.uncommitted_count > 0 {
                        format!("未提交: {}", tab.uncommitted_count)
                    } else {
                        "工作区干净".to_string()
                    },
                ))
                .left(StatusBarItem::new("status-path").label(tab.path.display().to_string()))
                .right(
                    StatusBarItem::new("status-count").label(if tab.is_search_active() {
                        if tab.search_loading {
                            "全历史检索中…".to_string()
                        } else {
                            format!("命中 {} 条提交", tab.visible_commit_count())
                        }
                    } else {
                        format!("{} 条提交", tab.visible_commit_count())
                    }),
                );
        }
        bar.right(StatusBarItem::new("status-engine").label("Engine: gix (in-process)"))
    }
}

impl Render for GitGraphApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reduced_motion = cx.theme().reduced_motion;
        if let Some(tab) = self.active_tab_mut() {
            let mut need_frame = false;
            if tab.graph_overscroll.is_active() && tab.graph_overscroll.tick(reduced_motion) {
                need_frame = true;
            }
            if tab.vertical_overscroll.is_active() && tab.vertical_overscroll.tick(reduced_motion) {
                need_frame = true;
            }
            if need_frame {
                window.request_animation_frame();
            }
        }

        let colors = cx.theme().colors.clone();
        let weak = cx.weak_entity();

        let modal_el = self
            .modal
            .as_ref()
            .map(|m| render_active_modal(m, self.modal_serial, weak.clone(), cx));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.bg)
            .text_color(colors.fg)
            .key_context("GitGraphApp")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &OpenRepository, _, cx| {
                this.pick_and_open_repo(cx);
            }))
            .on_action(cx.listener(|this, _: &CloseActiveTab, _, cx| {
                if let Some(id) = this.active_tab_id {
                    this.close_tab(id, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RefreshGraph, _, cx| {
                this.refresh_active_tab(false, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleTheme, _, cx| {
                this.toggle_theme(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleThemePreset, _, cx| {
                this.toggle_theme_preset(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleDetails, _, cx| {
                this.toggle_details_pane(cx);
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.focus_search(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, window, cx| {
                this.apply_ui_scale(this.ui_scale() + 0.10, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, window, cx| {
                this.apply_ui_scale(this.ui_scale() - 0.10, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomReset, window, cx| {
                this.apply_ui_scale(DEFAULT_UI_SCALE, window, cx);
            }))
            .child(self.render_titlebar(window, cx))
            .child(self.render_workspace_body(window, cx))
            .child(self.render_status_bar(cx))
            .children(modal_el)
            .child(ToastViewport::new("app-toasts", &self.toaster))
    }
}

/// 生成「最近打开的仓库」下拉菜单中单条记录的展示文本（遇到同名仓库时追加父目录名以便区分）。
fn recent_repo_menu_label(path: &std::path::Path, all_paths: &[PathBuf]) -> String {
    let name = repo_display_name(path);
    let duplicated = all_paths
        .iter()
        .filter(|other| repo_display_name(other) == name)
        .count()
        > 1;
    if duplicated
        && let Some(parent) = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy())
    {
        format!("{name}  ({parent})")
    } else {
        name
    }
}

/// 构建标题栏「最近打开的仓库」下拉菜单。
fn build_recent_repos_menu(
    recent_repos: &[PathBuf],
    active_repo: Option<&std::path::Path>,
    weak: &gpui::WeakEntity<GitGraphApp>,
) -> Menu {
    let recent_items: Vec<MenuItem> = if recent_repos.is_empty() {
        vec![MenuItem::new("暂无最近打开的仓库").disabled(true)]
    } else {
        recent_repos
            .iter()
            .map(|path| {
                let label = recent_repo_menu_label(path, recent_repos);
                let is_active = active_repo.is_some_and(|active| active == path.as_path());
                let target = path.clone();
                let weak_open = weak.clone();
                MenuItem::check(label, is_active).on_click(move |_, cx| {
                    weak_open
                        .update(cx, |this, cx| this.open_repo_path(target.clone(), cx))
                        .ok();
                })
            })
            .collect()
    };

    let weak_pick = weak.clone();
    let mut menu = Menu::new()
        .group("最近打开的仓库", recent_items)
        .separator()
        .item(
            MenuItem::new("打开其他本地仓库…")
                .icon(IconName::FolderOpen)
                .keys("cmd-o")
                .on_click(move |_, cx| {
                    weak_pick
                        .update(cx, |this, cx| this.pick_and_open_repo(cx))
                        .ok();
                }),
        );

    if !recent_repos.is_empty() {
        let weak_clear = weak.clone();
        menu = menu.item(
            MenuItem::new("清空最近记录")
                .icon(IconName::Trash2)
                .on_click(move |_, cx| {
                    weak_clear
                        .update(cx, |this, cx| this.clear_recent_repos(cx))
                        .ok();
                }),
        );
    }

    menu
}

/// 构建标题栏「配色主题」下拉菜单（默认配色 / Studio 高对比 + 浅色/深色）。
fn build_theme_preset_menu(
    current: ThemePreset,
    dark: bool,
    weak: &gpui::WeakEntity<GitGraphApp>,
) -> Menu {
    let weak_default = weak.clone();
    let weak_studio = weak.clone();
    let weak_mode = weak.clone();
    Menu::new()
        .group(
            "配色主题",
            vec![
                MenuItem::check("默认配色（暖灰极简）", current == ThemePreset::Default).on_click(
                    move |_, cx| {
                        weak_default
                            .update(cx, |this, cx| {
                                this.set_theme_preset(ThemePreset::Default, cx)
                            })
                            .ok();
                    },
                ),
                MenuItem::check("Studio 高对比（Zinc + Sky）", current == ThemePreset::Studio)
                    .on_click(move |_, cx| {
                        weak_studio
                            .update(cx, |this, cx| {
                                this.set_theme_preset(ThemePreset::Studio, cx)
                            })
                            .ok();
                    }),
            ],
        )
        .separator()
        .item(
            MenuItem::check("深色模式", dark).on_click(move |_, cx| {
                weak_mode
                    .update(cx, |this, cx| this.toggle_theme(cx))
                    .ok();
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_repo_menu_label_disambiguates_duplicate_names() {
        let paths = vec![
            PathBuf::from("/Users/dev/work/app"),
            PathBuf::from("/Users/dev/personal/app"),
            PathBuf::from("/Users/dev/work/git-graph-rs"),
        ];
        assert_eq!(recent_repo_menu_label(&paths[0], &paths), "app  (work)");
        assert_eq!(recent_repo_menu_label(&paths[1], &paths), "app  (personal)");
        assert_eq!(recent_repo_menu_label(&paths[2], &paths), "git-graph-rs");
    }
}
