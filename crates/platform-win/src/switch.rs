//! 切换到指定窗口（US-039 依赖）。
//!
//! 参数：只接收上层的 [`WindowId`]，不接收 Win32 句柄
//! 核心流程：`IsWindow` 校验目标仍存在 → `GetLastActivePopup` 找该窗口最近激活的
//! 弹窗，可见就切弹窗、否则切原窗口 → `SwitchToThisWindow` 激活。任何一步失败都
//! 返回 `false`，不 panic。非 Windows 平台恒为 `false`。

use switcheroo_core::WindowId;

/// 参数：`id` 要切换到的窗口标识（来自 [`crate::snapshot`] 或
/// [`crate::foreground_window`]）
/// 核心流程：`IsWindow` 校验句柄仍然指向一个存在的窗口（窗口可能已被用户关闭）→
/// 用 `GetLastActivePopup` 取该窗口最近激活的弹窗，弹窗可见时把它作为实际切换目标，
/// 否则切换原窗口 → 调用 `SwitchToThisWindow(target, TRUE)` 走系统 Alt+Tab 的同一
/// 条激活路径。校验失败返回 `false`。
#[cfg(windows)]
pub fn switch_to(id: WindowId) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetLastActivePopup, IsWindow, IsWindowVisible, SwitchToThisWindow,
    };

    use crate::window_enum::{hwnd_from_id, raw_handle};

    let hwnd = hwnd_from_id(id);

    // SAFETY: 调用 user32 的 IsWindow 校验句柄是否仍指向有效窗口，只读、不持有也不
    // 释放句柄；目标已被关闭时返回 FALSE。
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        log::warn!("切换目标已不存在：id={:#X}", id.raw());
        return false;
    }

    // SAFETY: GetLastActivePopup 只返回目标窗口最近激活的弹窗句柄（没有弹窗时返回
    // 原句柄）；IsWindowVisible 只读该句柄的可见性。两者都不转移所有权。
    let target = unsafe {
        let popup = GetLastActivePopup(hwnd);
        if !popup.is_invalid() && IsWindowVisible(popup).as_bool() {
            popup
        } else {
            hwnd
        }
    };

    // SAFETY: SwitchToThisWindow 激活目标窗口；第二参数为 true 表示它不是调用方
    // 自己的顶层窗口，需要走系统 Alt+Tab 的同一条激活路径（含前台权限提升）。
    // 该调用只作用于目标句柄，不涉及任何内存所有权。
    unsafe { SwitchToThisWindow(target, true) };

    log::debug!(
        "已切换窗口 id={:#X} 实际目标={:#X}",
        id.raw(),
        raw_handle(target)
    );
    true
}

/// 参数：`id` 要切换到的窗口标识
/// 核心流程：非 Windows 平台没有可切换的桌面窗口，恒返回 `false`，让上层按
/// “切换失败”处理，保证 workspace 在开发机上可编译。
#[cfg(not(windows))]
pub fn switch_to(id: WindowId) -> bool {
    log::debug!("platform-win switch stub on this target：{:#X}", id.raw());
    false
}
