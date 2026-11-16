//! Switcheroo 的 Windows 原生浮层（迁移计划 S1 → S6）。
//!
//! 参数：无（可执行入口）
//! 核心流程：初始化日志 → 抢单实例（已有实例时退出 0）→ 在当前线程初始化 COM →
//! 注册窗口类并创建隐藏的无边框分层窗口 → 建原生输入框（EDIT，S3）→ 注册全局热键
//! （失败则直接显示窗口并画红字）→ 进入消息循环；热键线程只往窗口投递 `WM_APP + 1`，
//! 所有 UI 操作都回到窗口线程执行，退出时在 `WM_DESTROY` 里释放输入框 / 字体 / 背景刷 /
//! D2D / DWrite 对象（含图标位图缓存）并投递 `WM_QUIT`。图标的解码在独立的后台线程上
//! 完成（`icons` 模块），UI 线程只在收到「图标已就绪」的消息后把它转成 D2D 位图。
//!
//! 主题（S9）与选项面板（S10）都只在本窗口内切换：配色来自 `render::Palette`，
//! 面板是卡片内容区上的另一层视图（不新建窗口），`Ctrl+T` 切明暗、`Ctrl+,` 开关面板。
//!
//! 托盘（S11）：`platform-win` 的托盘线程只在菜单被点中时把 `TrayCommand` 塞进 mpsc 并投递
//! `WM_APP + 5`；真正显示浮层、写注册表自启、切主题、退出都由窗口线程执行，线程边界上只过
//! 一个可 `Send` 的小枚举。托盘起不来（`Tray::spawn` 返回 `None`）只记 warn，热键照常可用。
//!
//! 实现只在 Windows 上存在；其它平台保留一个可编译的占位 `main`，
//! 保证 `cargo check --workspace` / `cargo test --workspace` 在 Linux 上仍能通过。
//!
//! 子系统：Windows 上一律按 GUI 子系统链接（见下面的 `windows_subsystem` 属性）。默认的
//! console 子系统会让双击 exe 时先弹出一个控制台黑框，`logger` 写的 stderr 全刷在里面；改成
//! GUI 后进程不分配控制台，`std::io::stderr()` 拿不到有效句柄，写入直接返回错误并被
//! `logger` 忽略（不会 panic，也不影响浮层）。真机上要看日志不必改回 console：从命令行启动并
//! 重定向 stderr 即可，例如 `switcheroo-win.exe 2> %TEMP%\switcheroo.log`（PowerShell 里
//! `& .\switcheroo-win.exe 2> $env:TEMP\switcheroo.log`）——重定向给出的是真实文件句柄，
//! 日志照常落盘，同时仍然没有黑框；级别依旧由 `RUST_LOG` 控制。

// Windows 上声明为 GUI 子系统：双击 exe 不再附带控制台窗口（等价于链接参数
// `/SUBSYSTEM:WINDOWS`，即 PE 头 Subsystem = 2 IMAGE_SUBSYSTEM_WINDOWS_GUI）。
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
// workspace 的 rust lint 统一禁用了 unsafe，而 Win32 / COM 层必须直接调用 FFI：
// 这个 crate 就是全项目唯一允许写 unsafe 的地方，每个 unsafe 块都带中文 SAFETY 注释，
// 写明调用了哪些 API、指针/句柄在调用期间为什么有效。
#![allow(unsafe_code)]

#[cfg(windows)]
mod app;
// 「关闭中」窗口集合（S8）：只维护「id → 请求时刻」这一份状态，不碰 Win32，随 `app` 一起
// 只在 Windows 上编译。
#[cfg(windows)]
mod close;
#[cfg(windows)]
mod edit;
// 命中区间换算与图标加载器都只用 `std`（不碰任何 Win32 类型），因此非 Windows 平台也在
// `cargo test` 时编译，让这两组单测在开发机上就能跑。
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod highlight;
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod icons;
// 布局换算与日志器是纯 `std` 实现，非 Windows 平台只在 `cargo test` 时编译：
// 这样 `cargo test --workspace` 在开发机上也能跑到它们的单元测试，正常构建里则不会
// 出现「从未被使用」的死代码。
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod layout;
// 列表状态（含「移除已关闭窗口 + 修正选中项」的纯逻辑）只依赖 core 与平台层的可移植 API，
// 因此测试构建里也在非 Windows 平台编译，让这段最容易算错的下标同步逻辑能在开发机上直接单测。
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod list;
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod logger;
// 选项面板（S10）的纯逻辑：草稿、按键 → 组合键、保存前的热键处置。与平台无关，
// 因此非 Windows 的测试构建里也编译，让这几条规则在开发机上就能单测。
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod options;
#[cfg(windows)]
mod render;
// 明暗主题（S9）的三态折算：同样只依赖 core，Windows 与测试构建都编译。
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod theme;

/// 参数：无
/// 核心流程：Windows 上把进程退出码交给 [`app::run`]；其它平台只打印一行提示。
#[cfg(windows)]
fn main() {
    std::process::exit(app::run());
}

/// 参数：无
/// 核心流程：非 Windows 平台没有 Win32 / Direct2D，只打印提示，退出码 0。
#[cfg(not(windows))]
fn main() {
    println!("仅在 Windows 上可用");
}
