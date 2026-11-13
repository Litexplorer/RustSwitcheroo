//! 唤起前的前台窗口（US-037 依赖）。
//!
//! 参数：无，全部数据都是运行期由 Win32 查询出来的
//! 核心流程：`GetForegroundWindow` 取当前前台窗口句柄 → 复用 `window_enum` 里的
//! 类名与进程名解析（含 ApplicationFrameWindow 的 UWP 分支）得到进程名 → 打包成
//! [`ForegroundWindow`]。上层在弹出切换面板之前调用一次，用来记住“用户取消切换时
//! 该回到哪个窗口”。非 Windows 平台恒为 `None`。

use switcheroo_core::WindowId;

/// 唤起前的前台窗口。
///
/// 参数：`id` 前台窗口的句柄标识，`process_name` 该窗口所属进程的短名
/// 核心流程：只承载上层判定“是否回到原窗口”所需的两项事实，不暴露任何 Win32 类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundWindow {
    /// 前台窗口标识，可直接交给 [`crate::switch_to`] 切回去。
    pub id: WindowId,
    /// 所属进程短名，例如 `explorer`、`Code`；解析失败时为 `"unknown"`。
    pub process_name: String,
}

/// 参数：无
/// 核心流程：调用 `GetForegroundWindow` 拿前台窗口句柄，空句柄说明此刻没有前台
/// 窗口，直接返回 `None`；否则用 `window_enum` 里已有的类名与进程名解析得到进程名，
/// 组装 [`ForegroundWindow`]。任何一步取不到都不 panic（进程名会降级为 `"unknown"`）。
#[cfg(windows)]
pub fn foreground_window() -> Option<ForegroundWindow> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    use crate::window_enum::{raw_handle, resolve_process_name, window_class_name};

    // SAFETY: 调用 user32 的 GetForegroundWindow，无参数、只返回当前前台窗口句柄，
    // 不转移所有权也不分配资源；没有前台窗口时返回空句柄。
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        log::debug!("当前没有前台窗口");
        return None;
    }

    // 进程名一律复用 window_enum 的实现：普通窗口按 PID 解析，ApplicationFrameWindow
    // 走子窗口找真实应用，取不到时该函数已经降级为 "unknown"。
    let process_name = resolve_process_name(hwnd, &window_class_name(hwnd));

    Some(ForegroundWindow {
        id: WindowId::new(raw_handle(hwnd)),
        process_name,
    })
}

/// 参数：无
/// 核心流程：非 Windows 平台没有前台窗口概念，返回 `None`，让上层按“没有原窗口”
/// 处理，保证 workspace 在开发机上可编译。
#[cfg(not(windows))]
pub fn foreground_window() -> Option<ForegroundWindow> {
    log::debug!("platform-win foreground stub on this target");
    None
}
