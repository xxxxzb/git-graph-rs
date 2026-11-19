//! 主应用视图 `GitGraphApp`：macOS 融合标题栏、多仓库 Tab、异步调度、写操作、文件监听与全局壳层。

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use ely_core::buttons::{Button, ButtonVariant, IconButton};
use ely_core::feedback::{EmptyState, Toast, ToastViewport, Toaster};
use ely_core::forms::{InputEvent, TextInput};
use ely_core::layout::SplitPane;
use ely_core::lists::{List, ListItem};
use ely_core::menus::{ContextMenu, Menu};
use ely_core::navigation::{EditorTab, EditorTabs};
use ely_core::primitives::{Icon, IconName, Severity};
use ely_core::shell::{StatusBar, StatusBarItem, drag_region};
use ely_core::theme::{ActiveTheme, ControlSize, IconSize, Mode, TextSize, Theme};
use ely_git::DiffLayout;
use git_graph_core::api::Engine;
use git_graph_core::types::{GitCommit, UNCOMMITTED};
use git_graph_ops::{
    GitCli, RepoWatcher, ResetMode, WatchEvent, check_checkout_safety, check_reset_safety,
};
use gpui::{
    AnyElement, App, Axis, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    ParentElement, PathPromptOptions, Pixels, Point, Render, ScrollStrategy, SharedString,
    Styled, Subscription, Window, div, prelude::*, px,
};

use crate::modals::{
    ActiveModal, ContextMenuState, PendingWriteOp, build_commit_context_menu, render_active_modal,
};
use crate::repo_tab::{
    FileSection, RepoTabState, SelectedFile, execute_history_search, load_commit_details,
    load_file_diff, load_file_line_counts, load_repo_snapshot,
};
use crate::state::{
    PersistedState, repo_display_name, save_persisted_state, short_hash,
};
use crate::ui_details::render_details_pane;
use crate::ui_graph::{ResizeCol, render_filter_toolbar, render_graph_table};

gpui::actions!(
    git_graph_app,
    [
        OpenRepository,
        CloseActiveTab,
        RefreshGraph,
        ToggleTheme,
        ToggleDetails,
        FocusSearch,
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
    pub context_menu: Option<ContextMenuState>,
    pub next_ctx_request_id: u64,
    pub cli: Option<GitCli>,
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
            context_menu: None,
            next_ctx_request_id: 1,
            cli,
        };

        for path in initial_repos {
            app.open_repo_path(path, cx);
        }

        app
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

    pub fn reorder_tabs(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let item = self.tabs.remove(from);
        self.tabs.insert(to, item);
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
                .background_spawn(async move { load_repo_snapshot(&engine, &filter, max_commits) })
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
                        tab.recompute_visible_rows();
                        let should_research = !soft && tab.is_search_active();

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
                    }
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
        if !self.persisted.details_open {
            self.persisted.details_open = true;
            self.save_state();
        }
        let Some(tab) = self.tab_by_id_mut(tab_id) else {
            return;
        };
        if tab.selected_hash.as_deref() == Some(hash.as_str()) {
            return;
        }
        tab.selected_hash = Some(hash.clone());
        tab.details = None;
        tab.line_counts.clear();
        tab.selected_file = None;
        tab.file_view = None;
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
        let visible_ix = self.active_tab().and_then(|t| t.visible_index_of(&hash));
        if let Some(ix) = visible_ix {
            if let Some(tab) = self.active_tab() {
                tab.graph_scroll.scroll_to_item(ix, ScrollStrategy::Center);
            }
            self.select_commit(hash, cx);
        } else {
            self.push_toast(
                "未在当前视图中找到该提交",
                Severity::Info,
                Some(format!("提交 {} 可能被当前筛选条件隐藏或尚未加载。", short_hash(&hash))),
                cx,
            );
        }
    }

    pub fn toggle_details_pane(&mut self, cx: &mut Context<Self>) {
        self.persisted.details_open = !self.persisted.details_open;
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

    pub fn set_column_width(&mut self, col: ResizeCol, width: f32, cx: &mut Context<Self>) {
        match col {
            ResizeCol::Graph => self.persisted.column_widths.graph = width,
            ResizeCol::Date => self.persisted.column_widths.date = width,
            ResizeCol::Author => self.persisted.column_widths.author = width,
            ResizeCol::Commit => self.persisted.column_widths.commit = width,
        }
        self.save_state();
        cx.notify();
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

    pub fn open_commit_context_menu(
        &mut self,
        repo_path: PathBuf,
        commit: GitCommit,
        focused_branch: Option<String>,
        focused_tag: Option<String>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.next_ctx_request_id += 1;
        self.context_menu = Some(ContextMenuState {
            request_id: self.next_ctx_request_id,
            position,
            repo_path,
            commit,
            focused_branch,
            focused_tag,
        });
        cx.notify();
    }

    pub fn open_create_branch_modal(
        &mut self,
        repo_path: PathBuf,
        start_point: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| TextInput::new(window, cx).placeholder("例如 feature/my-branch"));
        self.modal = Some(ActiveModal::CreateBranch {
            repo_path,
            start_point,
            input,
            checkout: true,
        });
        cx.notify();
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
        self.modal = Some(ActiveModal::CreateTag {
            repo_path,
            target,
            name_input,
            message_input,
        });
        cx.notify();
    }

    pub fn open_stash_push_modal(
        &mut self,
        repo_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message_input = cx.new(|cx| TextInput::new(window, cx).placeholder("输入 Stash 描述（可留空）"));
        self.modal = Some(ActiveModal::StashPush {
            repo_path,
            message_input,
            include_untracked: true,
        });
        cx.notify();
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

    pub fn run_fetch_active(&mut self, cx: &mut Context<Self>) {
        let Some(cli) = self.ensure_cli(cx) else {
            return;
        };
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.fetching {
            return;
        }
        tab.fetching = true;
        let tab_id = tab.id;
        let repo_path = tab.path.clone();
        cx.notify();

        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_spawn(async move { cli.fetch(&repo_path, None, true) })
                .await;
            weak.update(cx, |this, cx| {
                if let Some(tab) = this.tab_by_id_mut(tab_id) {
                    tab.fetching = false;
                }
                match res {
                    Ok(()) => {
                        this.push_toast("Fetch 完成", Severity::Success, None, cx);
                        this.soft_refresh_tab(tab_id, cx);
                    }
                    Err(err) => {
                        this.push_toast("Fetch 失败", Severity::Danger, Some(err.to_string()), cx);
                    }
                }
                cx.notify();
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
                    this.modal = Some(ActiveModal::SafetyConfirm {
                        warning,
                        confirm_label: format!("仍然检出 {branch}").into(),
                        pending,
                    });
                    cx.notify();
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
                    this.modal = Some(ActiveModal::SafetyConfirm {
                        warning,
                        confirm_label: format!("仍然检出 {short}").into(),
                        pending,
                    });
                    cx.notify();
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
                    this.modal = Some(ActiveModal::SafetyConfirm {
                        warning,
                        confirm_label: "确认 Hard Reset".into(),
                        pending,
                    });
                    cx.notify();
                }
                Ok(None) | Err(_) => {
                    let mode_label = match mode {
                        ResetMode::Soft => "Soft",
                        ResetMode::Mixed => "Mixed",
                        ResetMode::Hard => "Hard",
                    };
                    this.modal = Some(ActiveModal::SimpleConfirm {
                        title: format!("Reset ({mode_label}) 到 {short}？").into(),
                        message: format!("将把当前分支指针重置到提交 {short}。").into(),
                        confirm_label: format!("执行 Reset ({mode_label})").into(),
                        destructive: mode == ResetMode::Hard,
                        pending,
                    });
                    cx.notify();
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
        self.modal = Some(ActiveModal::SimpleConfirm {
            title: format!("删除本地分支 {branch}？").into(),
            message: "将执行 git branch -d 删除该分支（若有未合并提交则会阻止删除）。".into(),
            confirm_label: "删除分支".into(),
            destructive: true,
            pending: PendingWriteOp::DeleteBranch {
                repo_path,
                name: branch,
                force: false,
            },
        });
        cx.notify();
    }

    pub fn confirm_delete_tag(&mut self, repo_path: PathBuf, tag: String, cx: &mut Context<Self>) {
        self.modal = Some(ActiveModal::SimpleConfirm {
            title: format!("删除本地标签 {tag}？").into(),
            message: "将从本地仓库移除该标签引用。".into(),
            confirm_label: "删除标签".into(),
            destructive: true,
            pending: PendingWriteOp::DeleteTag {
                repo_path,
                name: tag,
            },
        });
        cx.notify();
    }

    pub fn confirm_stash_pop(&mut self, repo_path: PathBuf, index: usize, cx: &mut Context<Self>) {
        self.modal = Some(ActiveModal::SimpleConfirm {
            title: format!("弹出 stash@{{{index}}}？").into(),
            message: "将应用该 Stash 到工作区并从 Stash 栈中移除。".into(),
            confirm_label: "弹出 Stash".into(),
            destructive: false,
            pending: PendingWriteOp::StashPop { repo_path, index },
        });
        cx.notify();
    }

    pub fn confirm_stash_drop(&mut self, repo_path: PathBuf, index: usize, cx: &mut Context<Self>) {
        self.modal = Some(ActiveModal::SimpleConfirm {
            title: format!("丢弃 stash@{{{index}}}？").into(),
            message: "丢弃后该 Stash 中的修改记录将很难恢复。".into(),
            confirm_label: "丢弃 Stash".into(),
            destructive: true,
            pending: PendingWriteOp::StashDrop { repo_path, index },
        });
        cx.notify();
    }

    pub fn confirm_discard_file(
        &mut self,
        repo_path: PathBuf,
        file_path: String,
        cx: &mut Context<Self>,
    ) {
        self.modal = Some(ActiveModal::SimpleConfirm {
            title: "丢弃工作区修改？".into(),
            message: format!("确定要丢弃文件 {file_path} 中未暂存的修改吗？此操作不可撤销。").into(),
            confirm_label: "丢弃修改".into(),
            destructive: true,
            pending: PendingWriteOp::DiscardFile {
                repo_path,
                file_path,
            },
        });
        cx.notify();
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
        let (repo_for_refresh, ok_msg, err_title) = match &op {
            PendingWriteOp::CheckoutBranch { repo_path, branch } => (
                repo_path.clone(),
                format!("已切换到分支 {branch}"),
                "检出分支失败",
            ),
            PendingWriteOp::CheckoutCommit { repo_path, hash } => (
                repo_path.clone(),
                format!("已检出提交 {}", short_hash(hash)),
                "检出提交失败",
            ),
            PendingWriteOp::DeleteBranch {
                repo_path, name, ..
            } => (
                repo_path.clone(),
                format!("已删除分支 {name}"),
                "删除分支失败",
            ),
            PendingWriteOp::DeleteTag { repo_path, name } => (
                repo_path.clone(),
                format!("已删除标签 {name}"),
                "删除标签失败",
            ),
            PendingWriteOp::CherryPick { repo_path, hash } => (
                repo_path.clone(),
                format!("已 Cherry-pick {}", short_hash(hash)),
                "Cherry-pick 失败",
            ),
            PendingWriteOp::RevertCommit { repo_path, hash } => (
                repo_path.clone(),
                format!("已 Revert {}", short_hash(hash)),
                "Revert 失败",
            ),
            PendingWriteOp::ResetToCommit {
                repo_path, hash, ..
            } => (
                repo_path.clone(),
                format!("已 Reset 到 {}", short_hash(hash)),
                "Reset 失败",
            ),
            PendingWriteOp::StashPop { repo_path, index } => (
                repo_path.clone(),
                format!("已弹出 stash@{{{index}}}"),
                "弹出 Stash 失败",
            ),
            PendingWriteOp::StashDrop { repo_path, index } => (
                repo_path.clone(),
                format!("已丢弃 stash@{{{index}}}"),
                "丢弃 Stash 失败",
            ),
            PendingWriteOp::DiscardFile {
                repo_path,
                file_path,
            } => (
                repo_path.clone(),
                format!("已丢弃更改 {file_path}"),
                "丢弃文件更改失败",
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
                this.finish_write_op(&repo_for_refresh, ok_msg, err_title, res, cx);
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
        res: git_graph_ops::Result<()>,
        cx: &mut Context<Self>,
    ) {
        match res {
            Ok(()) => {
                self.push_toast(ok_title, Severity::Success, None, cx);
                if let Some(tab) = self.tab_by_path_mut(repo_path) {
                    let id = tab.id;
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
        let (colors, dark, title_text_size) = {
            let theme = cx.theme();
            (
                theme.colors.clone(),
                theme.is_dark(),
                theme.text_size(TextSize::Sm),
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
            let editor_tabs: Vec<EditorTab> = self
                .tabs
                .iter()
                .map(|tab| {
                    EditorTab::new(tab.id.to_string(), tab.name.clone())
                        .icon(IconName::GitBranch)
                        .dirty(tab.uncommitted_count > 0)
                })
                .collect();

            let mut strip = EditorTabs::new("repo-editor-tabs", editor_tabs)
                .on_select({
                    let weak = weak.clone();
                    move |id, _, cx| {
                        if let Ok(tab_id) = id.parse::<u64>() {
                            weak.update(cx, |this, cx| this.activate_tab(tab_id, cx)).ok();
                        }
                    }
                })
                .on_close({
                    let weak = weak.clone();
                    move |id, _, cx| {
                        if let Ok(tab_id) = id.parse::<u64>() {
                            weak.update(cx, |this, cx| this.close_tab(tab_id, cx)).ok();
                        }
                    }
                })
                .on_reorder({
                    let weak = weak.clone();
                    move |from, to, _, cx| {
                        weak.update(cx, |this, cx| this.reorder_tabs(from, to, cx))
                            .ok();
                    }
                });

            if let Some(active) = self.active_tab_id {
                strip = strip.selected(active.to_string());
            }

            div().flex_1().min_w_0().child(strip).into_any_element()
        };

        let open_btn = IconButton::new("titlebar-open-repo", IconName::Plus)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Sm)
            .tooltip("打开本地仓库 (⌘O)")
            .on_click({
                let weak = weak.clone();
                move |_, _, cx| {
                    weak.update(cx, |this, cx| this.pick_and_open_repo(cx)).ok();
                }
            });

        let theme_btn = IconButton::new(
            "titlebar-theme-toggle",
            if dark { IconName::Sun } else { IconName::Moon },
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Sm)
        .tooltip(if dark { "切换至浅色模式" } else { "切换至深色模式" })
        .on_click({
            let weak = weak.clone();
            move |_, _, cx| {
                weak.update(cx, |this, cx| this.toggle_theme(cx)).ok();
            }
        });

        drag_region("app-titlebar-drag", window, cx)
            .w_full()
            .h(px(40.0))
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
                    .child(open_btn)
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
                                .size(ControlSize::Sm)
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

            div()
                .w_full()
                .max_w(px(520.0))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .px_2()
                        .text_size(theme.text_size(TextSize::Xs))
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
            .gap_4()
            .p_6()
            .bg(colors.bg)
            .child(empty)
            .children(recent_section)
            .into_any_element()
    }

    fn render_workspace_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.active_tab() else {
            return self.render_start_page(cx);
        };

        let toolbar = render_filter_toolbar(self, tab, cx);
        let graph_table = render_graph_table(self, tab, cx);

        let main_area: AnyElement = if self.persisted.details_open {
            let details_pane = render_details_pane(self, tab, cx);
            let weak = cx.weak_entity();
            let v_sizes = self.persisted.vertical_split;
            SplitPane::new("main-v-split", Axis::Vertical, px(160.0))
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
            Some(ctx_menu) => build_commit_context_menu(ctx_menu, cx.weak_entity()),
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

    fn render_status_bar(&self, _cx: &App) -> StatusBar {
        let mut bar = StatusBar::new();
        if let Some(tab) = self.active_tab() {
            let branch_label: SharedString = match tab.info.as_ref().and_then(|i| i.head.as_ref())
            {
                Some(b) => b.clone().into(),
                None => match &tab.head_hash {
                    Some(h) => format!("HEAD ({})", short_hash(h)).into(),
                    None => "空仓库".into(),
                },
            };
            bar = bar
                .left(
                    StatusBarItem::new("status-branch")
                        .icon(IconName::GitBranch)
                        .label(branch_label),
                )
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
        let colors = cx.theme().colors.clone();
        let weak = cx.weak_entity();

        let modal_el = self
            .modal
            .as_ref()
            .map(|m| render_active_modal(m, weak.clone(), cx));

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
            .on_action(cx.listener(|this, _: &ToggleDetails, _, cx| {
                this.toggle_details_pane(cx);
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.focus_search(window, cx);
            }))
            .child(self.render_titlebar(window, cx))
            .child(self.render_workspace_body(cx))
            .child(self.render_status_bar(cx))
            .children(modal_el)
            .child(ToastViewport::new("app-toasts", &self.toaster))
    }
}
