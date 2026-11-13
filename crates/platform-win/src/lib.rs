//! Switcheroo 的 Windows 平台层。
//!
//! 参数：无
//! 核心流程：把 Win32 的窗口枚举、图标、全局热键、键盘钩子、托盘图标与开机自启收敛在
//! 这一层，对上层只暴露可移植的数据结构（[`WindowInfo`] 与 [`WindowAttributes`]）与事件，
//! UI 不直接调用 Win32。非 Windows 平台保留同名 API 的空实现，保证 workspace
//! 其余 crate 可编译。

#![allow(unsafe_code)]

use switcheroo_core::{WindowAttributes, WindowInfo};

pub mod autostart;
pub mod close;
pub mod foreground;
pub mod hotkey;
pub mod icon;
pub mod single_instance;
pub mod switch;
pub mod system_theme;
pub mod tray;
pub mod win32_parse;

pub use autostart::{disable, enable, is_enabled};
pub use close::{is_window_alive, request_close};
pub use foreground::{foreground_window, ForegroundWindow};
pub use hotkey::{HotkeyError, HotkeyListener, HotkeySpec};
pub use icon::{window_icon, IconImage};
pub use single_instance::{SingleInstance, SingleInstanceError};
pub use switch::switch_to;
pub use system_theme::is_system_dark;
pub use tray::{Tray, TrayCommand};
pub use win32_parse::{cloak_type_from_prop, process_name_from_exe_path};

#[cfg(windows)]
mod window_enum;

#[cfg(windows)]
mod windows_impl;

#[cfg(windows)]
use windows_impl as imp;

#[cfg(not(windows))]
mod portable_impl;

#[cfg(not(windows))]
use portable_impl as imp;

/// 参数：无
/// 核心流程：枚举当前桌面的顶层窗口，用 `EnumWindows` 采集属性并交给
/// [`switcheroo_core::is_alt_tab_window`] 过滤后返回快照列表；
/// 非 Windows 平台返回空列表。
pub fn snapshot() -> Vec<WindowInfo> {
    imp::snapshot()
}

/// 参数：无
/// 核心流程：与 [`snapshot`] 采用同一轮枚举与过滤，额外带回每个窗口的
/// [`WindowAttributes`]，供命令行验收与调试查看可见性等原始事实。
pub fn snapshot_detailed() -> Vec<(WindowInfo, WindowAttributes)> {
    imp::snapshot_detailed()
}
