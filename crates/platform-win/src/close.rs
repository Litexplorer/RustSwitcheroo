//! 请求关闭指定窗口 + 判断窗口是否已关闭（US-043 依赖）。
//!
//! 参数：只接收上层的 [`WindowId`]，不接收任何 Win32 句柄
//! 核心流程：`request_close` 用 `IsWindow` 校验目标后 `PostMessageW(WM_CLOSE)` 投递
//! 关闭请求（异步，不等窗口真的销毁，也不会误杀未保存的文档——是否退出由目标进程
//! 自己决定）；`is_window_alive` 用 `IsWindow && IsWindowVisible` 回答"还能不能看到"，
//! 供上层每 125ms 轮询判断浮层条目是否该撤掉。判定口径统一收敛在
//! [`should_drop_entry`] 这个纯函数里，不碰 Win32，因此在 Linux 上也能直接单测。

use switcheroo_core::WindowId;

/// 参数：`alive` 平台层探测到的"窗口仍存在且可见"
/// 核心流程：轮询时的唯一判据——**不**存活就视为已关闭，浮层应把该条目移除。
/// 抽成纯函数是为了让"什么时候算关掉了"这条规则可以脱离 Win32 单测，
/// 也避免上层各处自己写 `!alive` 而出现口径分歧。
pub(crate) fn should_drop_entry(alive: bool) -> bool {
    !alive
}

/// 参数：`id` 要关闭的窗口标识（通常来自 [`crate::snapshot`] 的当前选中项）
/// 核心流程：`IsWindow` 校验句柄仍指向有效窗口（窗口可能已被用户关掉）→
/// `PostMessageW(hwnd, WM_CLOSE, 0, 0)` 把关闭请求投递进目标窗口的消息队列。
/// 投递成功返回 `true`，但"成功"只表示请求已入队，窗口是否真的关掉要由
/// [`is_window_alive`] 轮询确认。
///
/// 用 `PostMessageW` 而不是 `SendMessageW`：上层是 UI 线程，同步发送会阻塞到目标
/// 窗口处理完 WM_CLOSE（用户可能正停在"是否保存"的对话框上），把浮层卡死。
/// 也不走 `TerminateProcess` / `WM_DESTROY`：那等于强杀，会丢用户数据。
#[cfg(windows)]
pub fn request_close(id: WindowId) -> bool {
    let hwnd = crate::window_enum::hwnd_from_id(id);

    // SAFETY: 调用 user32 的 IsWindow 校验句柄是否仍指向有效窗口，只读、不持有也
    // 不释放句柄；目标已被关闭时返回 FALSE。
    if !unsafe { is_window(hwnd) } {
        log::warn!("关闭目标已不存在：id={:#X}", id.raw());
        return false;
    }

    // SAFETY: PostMessageW 只把一个 WM_CLOSE 消息放进目标窗口线程的消息队列，
    // 不读写本进程内存、不转移任何所有权；wparam/lparam 按 WM_CLOSE 的约定都为 0。
    match unsafe { post_close(hwnd) } {
        Ok(()) => {
            log::debug!("已请求关闭窗口 id={:#X}", id.raw());
            true
        }
        Err(err) => {
            // 常见原因：目标以更高完整性级别运行，UIPI 拒绝低权限进程投递消息。
            log::warn!("PostMessageW(WM_CLOSE) 失败：id={:#X} err={err}", id.raw());
            false
        }
    }
}

/// 参数：`id` 要关闭的窗口标识
/// 核心流程：非 Windows 平台没有可关闭的桌面窗口，恒返回 `false`，让上层按
/// "关闭请求失败"处理，保证 workspace 在开发机上可编译。
#[cfg(not(windows))]
pub fn request_close(id: WindowId) -> bool {
    log::debug!("platform-win close stub on this target：{:#X}", id.raw());
    false
}

/// 参数：`id` 目标窗口标识
/// 核心流程：平台探测（`IsWindow && IsWindowVisible`）交给 [`window_exists_visible`]，
/// 判定统一走 [`should_drop_entry`]，因此本函数就是"没被丢弃即仍存活"。
/// 非 Windows 平台探测恒为 `false`，于是这里返回 `false`。
pub fn is_window_alive(id: WindowId) -> bool {
    !should_drop_entry(window_exists_visible(id))
}

/// 参数：`id` 目标窗口标识
/// 核心流程：`IsWindow` 确认句柄仍指向一个窗口（用户关掉后句柄即失效）→
/// `IsWindowVisible` 确认它没有隐藏到托盘/不可见。两者都成立才算"还看得见"。
#[cfg(windows)]
fn window_exists_visible(id: WindowId) -> bool {
    let hwnd = crate::window_enum::hwnd_from_id(id);

    // SAFETY: IsWindow 只校验句柄有效性，只读、不转移所有权。
    if !unsafe { is_window(hwnd) } {
        return false;
    }

    // SAFETY: IsWindowVisible 只读该句柄的 WS_VISIBLE 样式位；句柄刚被 IsWindow
    // 确认有效，即使这两个调用之间窗口被销毁，user32 也只会返回 FALSE。
    unsafe { is_window_visible(hwnd) }
}

/// 参数：`id` 目标窗口标识
/// 核心流程：非 Windows 平台没有桌面窗口，探测恒为 `false`（即"看不到"），
/// 让上层按"已经关掉"处理，保证 workspace 在开发机上也能编译通过。
#[cfg(not(windows))]
fn window_exists_visible(id: WindowId) -> bool {
    log::debug!(
        "platform-win close probe stub on this target：{:#X}",
        id.raw()
    );
    false
}

/// 参数：`hwnd` 待校验的窗口句柄
/// 核心流程：包装 user32 的 `IsWindow`，把 Win32 类型与 unsafe 都锁在这个小函数里。
#[cfg(windows)]
unsafe fn is_window(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;

    // SAFETY: 调用方保证只传句柄；IsWindow 只读句柄有效性，不分配资源。
    unsafe { IsWindow(Some(hwnd)) }.as_bool()
}

/// 参数：`hwnd` 待校验的窗口句柄
/// 核心流程：包装 user32 的 `IsWindowVisible`，只读 `WS_VISIBLE` 样式位。
#[cfg(windows)]
unsafe fn is_window_visible(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;

    // SAFETY: 调用方保证句柄有效；IsWindowVisible 只读样式位，不分配资源。
    unsafe { IsWindowVisible(hwnd) }.as_bool()
}

/// 参数：`hwnd` 目标窗口句柄
/// 核心流程：`PostMessageW(hwnd, WM_CLOSE, 0, 0)` 异步投递关闭请求，把 Win32 的
/// `Result` 原样交回 [`request_close`]，由它决定日志与返回值。
#[cfg(windows)]
unsafe fn post_close(hwnd: windows::Win32::Foundation::HWND) -> windows::core::Result<()> {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

    // SAFETY: 调用方保证句柄有效；PostMessageW 只入队一条消息，不转移所有权。
    unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 窗口还活着时不该丢条目；关掉之后必须丢。
    #[test]
    fn drop_entry_only_when_not_alive() {
        assert!(!should_drop_entry(true));
        assert!(should_drop_entry(false));
    }

    /// `is_window_alive` 与 `should_drop_entry` 必须互为反义，口径只此一份。
    #[test]
    fn alive_and_drop_are_complements() {
        for alive in [true, false] {
            assert_eq!(!should_drop_entry(alive), alive);
        }
    }

    /// 伪造的句柄在哪个平台上都不会被当成"活着的窗口"：非 Windows 恒为 false，
    /// Windows 上 `IsWindow` 也会拒绝这些无效值。
    #[test]
    fn bogus_handle_is_not_alive() {
        assert!(!is_window_alive(WindowId::new(0)));
        assert!(!is_window_alive(WindowId::new(0xDEAD_BEEF)));
    }

    /// 不存在的窗口不该收到关闭请求：非 Windows 恒为 false，Windows 上被 `IsWindow` 挡掉。
    #[test]
    fn bogus_handle_is_not_closable() {
        assert!(!request_close(WindowId::new(0)));
        assert!(!request_close(WindowId::new(0xDEAD_BEEF)));
    }
}
