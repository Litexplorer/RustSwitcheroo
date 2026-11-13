//! 全局热键监听（US-035）。
//!
//! 参数：无
//! 核心流程：[`HotkeyListener::spawn`] 开一条专用线程，在线程内 `RegisterHotKey`
//! 注册到线程消息队列（hwnd 传空），随后 `GetMessageW` 循环等待 `WM_HOTKEY`，
//! 收到就回调 `on_press`；Drop 时向该线程投递 `WM_QUIT` 结束循环并 join，
//! 保证不留下孤儿线程。非 Windows 平台统一返回 [`HotkeyError::Unavailable`]。

use std::fmt;

/// 一个全局热键。
///
/// 参数：`key` 为虚拟键码（空格是 `0x20`）；`modifiers` 直接用 Win32 的
/// `MOD_ALT=0x1` / `MOD_CONTROL=0x2` / `MOD_SHIFT=0x4` / `MOD_WIN=0x8` 位或。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeySpec {
    pub key: u32,
    pub modifiers: u32,
}

/// 热键注册失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyError {
    /// 该组合键已被其他程序（或本进程）注册。
    AlreadyInUse,
    /// 平台或线程层不可用，附带原因。
    Unavailable(String),
}

/// 参数：`f` 为格式化输出器。
/// 核心流程：按变体给出可读的中文原因，`Unavailable` 追加底层原因。
impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInUse => write!(f, "热键已被其他程序占用"),
            Self::Unavailable(reason) => write!(f, "当前平台无法监听全局热键：{reason}"),
        }
    }
}

impl std::error::Error for HotkeyError {}

/// Windows 实现：持有消息线程的线程 ID 与 JoinHandle。
///
/// 参数：内部字段为热键线程的 Win32 线程 ID 和句柄。
/// 核心流程：spawn 时填充，Drop 时用来结束并回收线程。
#[cfg(windows)]
pub struct HotkeyListener {
    thread_id: u32,
    join: Option<std::thread::JoinHandle<()>>,
}

/// 非 Windows 平台的占位类型。
///
/// 参数：无。
/// 核心流程：不会真的创建，[`HotkeyListener::spawn`] 直接返回错误。
#[cfg(not(windows))]
pub struct HotkeyListener;

#[cfg(windows)]
impl HotkeyListener {
    /// 参数：`spec` 要注册的组合键；`on_press` 每次按下时在热键线程上执行的闭包
    /// （必须 `Send + 'static`，不要在闭包里直接操作 UI 线程的数据结构）。
    /// 核心流程：建 channel → 起热键线程 → 线程内 `GetCurrentThreadId` 取线程 ID 并
    /// `RegisterHotKey(None, id, modifiers, key)` 注册到本线程消息队列 →
    /// 成功则把线程 ID 回传给调用方并进入 `GetMessageW` 循环，失败读 `GetLastError`
    /// 回传 `AlreadyInUse`（1409，组合键已被占用）或 `Unavailable`（其它错误码）并退出
    /// → 循环里遇到 `WM_HOTKEY` 调 `on_press`，
    /// 遇到 `WM_QUIT`（Drop 投递）或取消息出错则退出并 `UnregisterHotKey`。
    pub fn spawn(
        spec: HotkeySpec,
        on_press: impl Fn() + Send + 'static,
    ) -> Result<Self, HotkeyError> {
        use std::sync::mpsc;

        use windows::Win32::Foundation::{GetLastError, ERROR_HOTKEY_ALREADY_REGISTERED};
        use windows::Win32::System::Threading::GetCurrentThreadId;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
        };
        use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

        // 注册时用固定 ID 即可：每个线程独立一套热键表，本进程只会注册一个热键。
        const HOTKEY_ID: i32 = 1;

        let (tx, rx) = mpsc::channel::<Result<u32, HotkeyError>>();

        let join = std::thread::Builder::new()
            .name("switcheroo-hotkey".into())
            .spawn(move || {
                // SAFETY: 调 kernel32 的 GetCurrentThreadId，无参数、无副作用，永远成功。
                let thread_id = unsafe { GetCurrentThreadId() };

                // SAFETY: 调 user32 的 RegisterHotKey，hwnd 传 None 表示把 WM_HOTKEY
                // 投递到本线程的消息队列；fsmodifiers 用 Win32 位掩码原样构造。
                let registered = unsafe {
                    RegisterHotKey(None, HOTKEY_ID, HOT_KEY_MODIFIERS(spec.modifiers), spec.key)
                };

                if registered.is_err() {
                    // SAFETY: 紧邻 RegisterHotKey 失败之后读取 last-error，中间没有
                    // 任何会覆盖 last-error 的 Win32 调用。
                    let code = unsafe { GetLastError() };
                    // 1409 说明该组合键已被占用；其它错误码是平台/参数层面的失败。
                    let err = if code == ERROR_HOTKEY_ALREADY_REGISTERED {
                        HotkeyError::AlreadyInUse
                    } else {
                        HotkeyError::Unavailable(format!("RegisterHotKey 失败，错误码 {}", code.0))
                    };
                    log::warn!("注册全局热键 {spec:?} 失败：{err}");
                    let _ = tx.send(Err(err));
                    return;
                }

                if tx.send(Ok(thread_id)).is_err() {
                    // 调用方已经丢弃接收端，直接注销并退出，避免热键泄漏。
                    // SAFETY: 热键 ID 与注册时一致，且此时已成功注册。
                    unsafe {
                        let _ = UnregisterHotKey(None, HOTKEY_ID);
                    }
                    return;
                }

                let mut msg = MSG::default();
                loop {
                    // SAFETY: GetMessageW 把消息写进栈上的 msg；hwnd 传 None 同时收取
                    // 投递给本线程的线程消息（WM_HOTKEY / WM_QUIT）。
                    let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                    if got.0 <= 0 {
                        // 0 表示收到 WM_QUIT，-1 表示出错，两种情况都结束循环。
                        break;
                    }
                    if msg.message == WM_HOTKEY {
                        on_press();
                    }
                }

                // SAFETY: 与注册时使用同一个热键 ID，注销本线程的热键。
                unsafe {
                    let _ = UnregisterHotKey(None, HOTKEY_ID);
                }
            })
            .map_err(|err| HotkeyError::Unavailable(format!("无法创建热键线程：{err}")))?;

        match rx.recv() {
            Ok(Ok(thread_id)) => {
                log::debug!("全局热键 {spec:?} 已注册，线程 {thread_id}");
                Ok(Self {
                    thread_id,
                    join: Some(join),
                })
            }
            Ok(Err(err)) => {
                let _ = join.join();
                Err(err)
            }
            Err(_) => {
                let _ = join.join();
                Err(HotkeyError::Unavailable("热键线程异常退出".to_string()))
            }
        }
    }
}

#[cfg(windows)]
impl Drop for HotkeyListener {
    /// 参数：无。
    /// 核心流程：`PostThreadMessageW(thread_id, WM_QUIT)` 让消息循环退出，
    /// 线程退出前会自行 `UnregisterHotKey`，最后 join 回收线程。
    fn drop(&mut self) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};

        // SAFETY: 向自己的热键线程投递 WM_QUIT；线程可能已经退出，此时调用返回错误，
        // 忽略即可，不会造成未定义行为。
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }

        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(not(windows))]
impl HotkeyListener {
    /// 参数：`spec` 期望的热键；`on_press` 按下时的回调（本平台不会调用）。
    /// 核心流程：非 Windows 平台没有 `RegisterHotKey`，直接返回
    /// [`HotkeyError::Unavailable`]，由上层决定降级行为。
    pub fn spawn(
        _spec: HotkeySpec,
        _on_press: impl Fn() + Send + 'static,
    ) -> Result<Self, HotkeyError> {
        Err(HotkeyError::Unavailable(format!(
            "{} 平台暂不支持全局热键",
            std::env::consts::OS
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个错误变体的 Display 都应带上可读信息。
    #[test]
    fn hotkey_error_display_is_readable() {
        assert!(HotkeyError::AlreadyInUse.to_string().contains("占用"));
        assert!(HotkeyError::Unavailable("boom".to_string())
            .to_string()
            .contains("boom"));
    }

    /// 非 Windows 平台 spawn 必然返回 Unavailable，保证上层可选择降级而不是崩。
    #[cfg(not(windows))]
    #[test]
    fn spawn_reports_unavailable_off_windows() {
        let spec = HotkeySpec {
            key: 0x20,
            modifiers: 0x1 | 0x2,
        };
        let err = match HotkeyListener::spawn(spec, || {}) {
            Ok(_) => panic!("非 Windows 平台不应注册成功"),
            Err(err) => err,
        };
        assert!(matches!(err, HotkeyError::Unavailable(_)));
    }
}
