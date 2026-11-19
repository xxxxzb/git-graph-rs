//! Git Graph RS — 原生 macOS GPUI 桌面应用入口。

mod app_view;
mod diff_parser;
mod graph_canvas;
mod modals;
mod repo_tab;
mod state;
mod ui_details;
mod ui_graph;

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use ely_core::theme::{Mode, Theme};
use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, point, px, size,
};

use crate::app_view::{
    CloseActiveTab, GitGraphApp, OpenRepository, QuitApp, RefreshGraph, ToggleDetails, ToggleTheme,
};
use crate::state::load_persisted_state;

fn app_menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Git Graph RS".into(),
            items: vec![MenuItem::action("退出 Git Graph RS", QuitApp)],
            disabled: false,
        },
        Menu {
            name: "文件".into(),
            items: vec![
                MenuItem::action("打开仓库…", OpenRepository),
                MenuItem::action("关闭当前标签", CloseActiveTab),
            ],
            disabled: false,
        },
        Menu {
            name: "视图".into(),
            items: vec![
                MenuItem::action("刷新图谱", RefreshGraph),
                MenuItem::action("显示 / 隐藏详情分栏", ToggleDetails),
                MenuItem::separator(),
                MenuItem::action("切换浅色 / 深色主题", ToggleTheme),
            ],
            disabled: false,
        },
    ]
}

fn parse_cli_repos() -> Vec<PathBuf> {
    std::env::args_os()
        .skip(1)
        .filter_map(|arg| {
            let s = arg.to_string_lossy();
            if s.starts_with('-') {
                None
            } else {
                Some(PathBuf::from(arg))
            }
        })
        .collect()
}

fn launch(initial_repos: Vec<PathBuf>, cx: &mut App) -> Result<()> {
    ely_core::init(cx).context("初始化 Ely UI 组件库失败")?;

    let persisted = load_persisted_state();
    if persisted.dark_mode {
        Theme::set_mode_now(Mode::Dark, cx);
    } else {
        Theme::set_mode_now(Mode::Light, cx);
    }

    cx.bind_keys([
        KeyBinding::new("cmd-q", QuitApp, None),
        KeyBinding::new("cmd-o", OpenRepository, Some("GitGraphApp")),
        KeyBinding::new("cmd-w", CloseActiveTab, Some("GitGraphApp")),
        KeyBinding::new("cmd-r", RefreshGraph, Some("GitGraphApp")),
        KeyBinding::new("cmd-j", ToggleDetails, Some("GitGraphApp")),
    ]);
    cx.on_action(|_: &QuitApp, cx| cx.quit());
    cx.set_menus(app_menus());

    let bounds = Bounds::centered(None, size(px(1360.0), px(860.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Git Graph RS".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(16.0), px(12.0))),
        }),
        window_min_size: Some(size(px(900.0), px(580.0))),
        ..Default::default()
    };

    cx.open_window(options, move |window, cx| {
        cx.new(|cx| GitGraphApp::new(initial_repos, persisted, window, cx))
    })
    .context("打开主窗口失败")?;

    cx.activate(true);
    Ok(())
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let initial_repos = parse_cli_repos();

    gpui_platform::application()
        .with_assets(ely_core::Assets)
        .run(move |cx: &mut App| {
            if let Err(err) = launch(initial_repos, cx) {
                log::error!("git-graph-app launch error: {err:#}");
                std::process::exit(1);
            }
        });
}
