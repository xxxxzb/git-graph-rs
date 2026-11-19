<p align="center">
  <img src="assets/app-icon.png" width="128" height="128" alt="Git Graph RS" />
</p>

<h1 align="center">Git Graph RS</h1>

<p align="center">
  <strong>一款基于 Rust + GPUI 的 macOS 原生 Git 提交图谱桌面客户端</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-nightly%20%7C%20edition%202024-orange?logo=rust" alt="Rust nightly" />
  <img src="https://img.shields.io/badge/UI-GPUI%20%2B%20Ely%20Components-2563EB" alt="GPUI" />
  <img src="https://img.shields.io/badge/Engine-gix%200.90%20(in--process)-0D9488" alt="gix" />
  <img src="https://img.shields.io/badge/Platform-macOS%20(Metal)-111827?logo=apple" alt="macOS" />
  <img src="https://img.shields.io/badge/License-MIT-16A34A" alt="MIT License" />
</p>

---

**Git Graph RS** 是一个轻量、极速、零 Electron/WebView 的原生桌面 Git 客户端。

它采用 **「`gix` 内存直读 + `git` CLI 安全写入 + GPUI Metal 原生渲染」** 架构：所有仓库读取（分支、Tag、提交历史、提交详情、文件 Diff、全历史搜索、工作区状态）均通过常驻内存的纯 Rust `gix` 引擎在进程内完成；所有写入操作均安全路由至系统 `git` CLI，并内置防数据丢失守卫。整个 Release `.app` 应用包仅约 **28 MB**。

## 核心特性

- **极速只读引擎**：复用并升级 `git-graph-core` 至 `gix 0.90`，常驻热仓库句柄池，无需反复拉起 `git` 子进程即可毫秒级加载提交图谱、提交详情与文件 Diff。
- **经典 5 列 Git Graph 宽表**：
  - `图形 | 描述 | 日期 | 作者 | 提交`，支持列宽自适应撑开与拖拽微调，图形列采用 GPUI `ContentMask` 硬裁剪与分割线隔离，杜绝多分支溢出压字。
  - GPUI `canvas` + `PathBuilder` 绘制 10 色平滑三次贝塞尔分支曲线、`HEAD` 双环节点、`Merge` 空心环节点、`Stash` 节点与置顶「未提交的更改」虚线连线。
  - 描述列内嵌高辨识度合并胶囊（如 `[master | origin]`、独立远程分支、Tag、`stash@{0}`）；搜索或筛选时自动截断指向列表外的悬空连线，保持图谱清爽。
- **全历史正则搜索（`⌘F`）**：后台防抖调用 `Engine::search_history` 遍历全仓提交标题与正文（commit body），非法正则自动字面量转义兜底，同时支持 4~40 位短 Hash 跨页直达。
- **内置可拖拽分栏详情与 Diff 视图**：
  - 上下/左右可拖拽 `SplitPane`：左侧展示提交完整元数据（可划选复制、点击父提交跳转）与变更文件列表（异步回填 `+N/-M` 统计），右侧集成 `ely-git::DiffViewer`，支持 **Unified / Split** 双视图切换、行内词级差异高亮、图片 `BeforeAfter` 对比及二进制文件大小对比。
  - 选中置顶「未提交的更改」行可直接按文件执行 Stage / Unstage / Discard。
- **多仓库顶部标签页 & 类 VS Code 状态栏同步**：
  - macOS 透明融合标题栏 + 顶部多仓库 `EditorTabs` + 浅色/深色主题一键切换 + 最近仓库与布局持久化。
  - 左下角状态栏实时显示当前分支与上游的 `↓behind ↑ahead` 计数（无上游时自动切换为「发布分支」），点一下直接遵循本机 `git pull` 配置拉取并在成功后推送；拉取冲突时立刻中止推送。
- **对象隔离的精准右键菜单 & 防误操作守卫**：
  - 严格区分右键点击目标（本地分支胶囊 / 远程分支胶囊 / Tag / Stash / 提交行 / 未提交行），零重复、零串台。
  - 支持 Checkout、创建/重命名/删除分支、推送分支到远程弹窗（`-u` / `--tags` / `--force-with-lease` / `--force` 警示）、创建/删除标签、Cherry-pick、Revert、Reset（Soft / Mixed / Hard，触发脏工作区与分离 HEAD 安全确认弹窗）、Stash 全流程。
- **自动监听刷新**：基于 `notify 8` 监听 `.git/` 与工作区变更，300ms 防抖软刷新，不闪烁、不丢选中状态。

## 项目架构

```text
git-graph-rs/
├── assets/
│   ├── app-icon.svg          # 矢量应用图标源稿
│   ├── app-icon.png          # 1024×1024 高清 PNG
│   └── AppIcon.icns          # macOS 全尺寸 Retina 图标
├── scripts/
│   └── bundle-macos.sh       # macOS .app 一键打包脚本
└── crates/
    ├── git-core/             # 纯 Rust 只读 Git 引擎（gix 0.90，热句柄池）
    ├── git-ops/              # Git CLI 写操作封装、同步状态感知、安全守卫与 notify 8 监听
    ├── graph-layout/         # 零依赖纯 Rust DAG 泳道拓扑布局（含悬空线截断）与 Ref 胶囊归组
    └── app/                  # GPUI + Ely-GPUI-Components macOS 桌面端主程序
```

## 快速开始

### 环境要求

- **macOS 13+**（Apple Silicon / Intel，Metal GPU 加速）
- **Rust nightly**（根目录 `rust-toolchain.toml` 已自动指定 `nightly` 工具链，支持 `edition = "2024"`）
- 系统已安装 `git`（用于写操作与远程同步）

### 本地编译与运行

```bash
# 克隆仓库
git clone git@github.com:xxxxzb/git-graph-rs.git
cd git-graph-rs

# 启动并直接打开一个或多个本地 Git 仓库
cargo run -p git-graph-app -- /path/to/your/repo

# Release 模式运行（120fps 满血流畅度）
cargo run --release -p git-graph-app -- /path/to/your/repo
```

### 打包并安装到 macOS 应用程序（`/Applications`）

```bash
# 构建 Release 版 .app 应用包（产出在 target/bundle/Git Graph RS.app）
./scripts/bundle-macos.sh release

# 安装到 /Applications 并做本地 ad-hoc 签名
rm -rf "/Applications/Git Graph RS.app"
cp -R "target/bundle/Git Graph RS.app" "/Applications/Git Graph RS.app"
codesign --force --deep --sign - "/Applications/Git Graph RS.app"

# 启动应用
open -a "Git Graph RS" --args /path/to/your/repo
```

### 检查与测试

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 常用快捷键

| 快捷键 | 功能 |
| :--- | :--- |
| `⌘ O` | 打开本地 Git 仓库 |
| `⌘ W` | 关闭当前仓库标签页 |
| `⌘ F` | 聚焦全历史搜索框 |
| `Esc` | 清空当前搜索关键词 |
| `⌘ R` | 刷新当前仓库图谱 |
| `⌘ J` | 显示 / 隐藏下方详情与 Diff 分栏 |
| `⌘ Q` | 退出应用 |

## 开源协议与致谢

本项目基于 **[MIT License](LICENSE)** 开源。

特别致谢以下优秀开源项目：
- **[neophack/vscode-git-graph-rs](https://github.com/neophack/vscode-git-graph-rs)**：`crates/git-core` 源自其 MIT 开源的 `native/core` Rust 引擎，并在本项目升级适配至 `gix 0.90` 与 Rust 2024。
- **[Gitoxide (`gix`)](https://github.com/GitoxideLabs/gitoxide)**：高性能纯 Rust Git 实现。
- **[Zed GPUI](https://www.gpui.rs/)** & **[Ely-GPUI-Components](https://github.com/ELY-Open-Source/Ely-GPUI-Components)**（含 **[ElyOffice / `ely-tree`](https://github.com/ELY-Open-Source/ElyOffice)** 的优秀实践参考）：提供高性能 GPU 渲染框架与温润克制的桌面组件体系。
