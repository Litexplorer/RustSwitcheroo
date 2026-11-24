//! 请求关闭指定窗口 + 判断窗口是否已关闭（US-043 依赖）。
//!
//! 参数：只接收上层的 [`WindowId`]，不接收任何 Win32 句柄
//! 核心流程：`request_close` 用 `IsWindow` 校验目标后 `PostMessageW(WM_CLOSE)` 投递
//! 关闭请求（异步，不等窗口真的销毁，也不会误杀未保存的文档——是否退出由目标进程
//! 自己决定），并把结果分成 [`CloseOutcome`] 的三种情形，让上层能把「UIPI 拒绝」
//! 与其它失败分开提示；`is_window_alive` 用 `IsWindow && IsWindowVisible` 回答
//! "还能不能看到"，供上层每 125ms 轮询判断浮层条目是否该撤掉。判定口径统一收敛在
//! [`should_drop_entry`] 这个纯函数里，不碰 Win32，因此在 Linux 上也能直接单测。

use switcheroo_core::WindowId;

/// Win32 的 `ERROR_ACCESS_DENIED`：UIPI 拒绝低完整性级别进程向高完整性级别窗口投递消息。
/// 写成本地常量而不是 `windows::Win32::Foundation::ERROR_ACCESS_DENIED`，是为了让
/// [`outcome_from_error`] 在非 Windows 上也能编译并单测（归类规则不该只有 Windows 能验）。
const ERROR_ACCESS_DENIED_CODE: u32 = 5;

/// 参数：无
/// 核心流程：把「请求关闭」的结果分成三种，供上层决定提示文案、是否继续轮询。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    /// 关闭请求已投递进目标窗口的消息队列；窗口是否真的关掉要由 [`is_window_alive`] 轮询确认。
    Requested,
    /// 目标窗口属于更高完整性级别的进程，UIPI 拒绝了 WM_CLOSE 投递（需以同等或更高权限运行才能关掉它）。
    Denied,
    /// 其它失败：目标窗口已不存在、句柄无效，或投递时出现了别的 Win32 错误。
    Failed,
}

/// 参数：`code` Win32 错误码（`GetLastError()` 取到的原值）
/// 核心流程：只有 `ERROR_ACCESS_DENIED` 归为 [`CloseOutcome::Denied`]（UIPI 拦截）；
/// 0（没取到错误码）与其它任何错误码一律 [`CloseOutcome::Failed`]。抽成纯函数是为了
/// 让这条归类规则脱离 Win32 单测。
pub fn outcome_from_error(code: u32) -> CloseOutcome {
    if code == ERROR_ACCESS_DENIED_CODE {
        CloseOutcome::Denied
    } else {
        CloseOutcome::Failed
    }
}

/// 参数：`alive` 平台层探测到的"窗口仍存在且可见"
/// 核心流程：轮询时的唯一判据——**不**存活就视为已关闭，浮层应把该条目移除。
/// 抽成纯函数是为了让"什么时候算关掉了"这条规则可以脱离 Win32 单测，
/// 也避免上层各处自己写 `!alive` 而出现口径分歧。
pub(crate) fn should_drop_entry(alive: bool) -> bool {
    !alive
}

/// 参数：`id` 要关闭的窗口标识（通常来自 [`crate::snapshot`] 的当前选中项）
/// 核心流程：`IsWindow` 校验句柄仍指向有效窗口（窗口可能已被用户关掉）→
/// `PostMessageW(hwnd, WM_CLOSE, 0, 0)` 把关闭请求投递进目标窗口的消息队列 →
/// 成功给 [`CloseOutcome::Requested`]，失败按 `GetLastError()` 分成
/// [`CloseOutcome::Denied`]（UIPI 拦截）与 [`CloseOutcome::Failed`]（其它），
/// 三条路径的日志各不相同，便于事后区分"目标已消失"和"投递被拒"。
/// "已投递"只表示请求已入队，窗口是否真的关掉要由 [`is_window_alive`] 轮询确认。
///
/// 用 `PostMessageW` 而不是 `SendMessageW`：上层是 UI 线程，同步发送会阻塞到目标
/// 窗口处理完 WM_CLOSE（用户可能正停在"是否保存"的对话框上），把浮层卡死。
/// 也不走 `TerminateProcess` / `WM_DESTROY`：那等于强杀，会丢用户数据。
#[cfg(windows)]
pub fn request_close(id: WindowId) -> CloseOutcome {
    let hwnd = crate::window_enum::hwnd_from_id(id);

    // SAFETY: 调用 user32 的 IsWindow 校验句柄是否仍指向有效窗口，只读、不持有也
    // 不释放句柄；目标已被关闭时返回 FALSE。
    if !unsafe { is_window(hwnd) } {
        log::warn!("关闭目标已不存在，未投递 WM_CLOSE：id={:#X}", id.raw());
        return CloseOutcome::Failed;
    }

    // SAFETY: PostMessageW 只把一个 WM_CLOSE 消息放进目标窗口线程的消息队列，
    // 不读写本进程内存、不转移任何所有权；wparam/lparam 按 WM_CLOSE 的约定都为 0。
    match unsafe { post_close(hwnd) } {
        Ok(()) => {
            log::debug!("已请求关闭窗口 id={:#X}", id.raw());
            CloseOutcome::Requested
        }
        Err(code) => {
            let outcome = outcome_from_error(code);
            match outcome {
                CloseOutcome::Denied => log::warn!(
                    "PostMessageW(WM_CLOSE) 被 UIPI 拒绝（目标属于更高完整性级别进程）：\
                     id={:#X} err={code}",
                    id.raw()
                ),
                _ => log::warn!(
                    "PostMessageW(WM_CLOSE) 投递失败：id={:#X} err={code}",
                    id.raw()
                ),
            }
            outcome
        }
    }
}

/// 参数：`id` 要关闭的窗口标识
/// 核心流程：非 Windows 平台没有可关闭的桌面窗口，恒返回 [`CloseOutcome::Failed`]，
/// 让上层按"关闭请求失败"处理，保证 workspace 在开发机上可编译。
#[cfg(not(windows))]
pub fn request_close(id: WindowId) -> CloseOutcome {
    log::debug!("platform-win close stub on this target：{:#X}", id.raw());
    CloseOutcome::Failed
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
/// 核心流程：`PostMessageW(hwnd, WM_CLOSE, 0, 0)` 异步投递关闭请求；失败时当场用
/// `GetLastError()` 取出**原始 Win32 错误码**（不是 `windows` crate 折成的 HRESULT）交回
/// [`request_close`]，由 [`outcome_from_error`] 归类。
///
/// 为什么不用 `windows::core::Result` 里的 `Error`：`BOOL::ok()` 走的是
/// `Error::from_thread()`，错误码被编码成 `HRESULT_FROM_WIN32(5)` = `0x80070005`，
/// 想还原出 `5` 得先确认 facility 再掩码，属于依赖编码细节的做法。而 `GetLastError`
/// 取值是线程局部的，`BOOL::ok()` 内部已经读过一次、并不会清零或改写它，紧接着再读
/// 得到的仍是这次失败的原值，因此这里直接取原码最稳。
#[cfg(windows)]
unsafe fn post_close(hwnd: windows::Win32::Foundation::HWND) -> Result<(), u32> {
    use windows::Win32::Foundation::{GetLastError, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

    // SAFETY: 调用方保证句柄有效；PostMessageW 只入队一条消息，不转移所有权。
    match unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) } {
        Ok(()) => Ok(()),
        // SAFETY: GetLastError 只读本线程的 last-error 值，无参数、不分配资源；
        // 与失败的调用之间没有任何别的 Win32 调用会覆盖它。
        Err(_) => Err(unsafe { GetLastError() }.0),
    }
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

    /// 不存在的窗口不该收到关闭请求：非 Windows 恒为 `Failed`，Windows 上被 `IsWindow` 挡掉。
    #[test]
    fn bogus_handle_is_not_closable() {
        assert_eq!(request_close(WindowId::new(0)), CloseOutcome::Failed);
        assert_eq!(
            request_close(WindowId::new(0xDEAD_BEEF)),
            CloseOutcome::Failed
        );
    }

    /// 只有 `ERROR_ACCESS_DENIED(5)` 算 UIPI 拒绝；0（没取到码）与其它码都只是普通失败。
    #[test]
    fn access_denied_maps_to_denied() {
        assert_eq!(outcome_from_error(5), CloseOutcome::Denied);
        assert_eq!(outcome_from_error(0), CloseOutcome::Failed);
        assert_eq!(outcome_from_error(87), CloseOutcome::Failed);
        assert_eq!(outcome_from_error(1400), CloseOutcome::Failed);
    }
}
