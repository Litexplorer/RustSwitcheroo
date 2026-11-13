//! 托盘图标与右键菜单（US-048）。
//!
//! 参数：托盘提示文本、开机自启当前勾选状态、命令回调，都由 [`Tray::spawn`] 传入
//! 核心流程：`spawn` 开一条专用线程 → 线程内 `RegisterClassW` + `CreateWindowExW` 建一个
//! 永不显示的普通窗口（只为了拿一个能收消息的 HWND）→ `Shell_NotifyIconW(NIM_ADD)` 把图标
//! 挂进任务栏通知区 → `GetMessageW` 循环分发：左键单击/双击回调 [`TrayCommand::Show`]，
//! 右键 `TrackPopupMenu` 弹出菜单，选中项同样通过回调交给上层。`Drop` 时先
//! `Shell_NotifyIconW(NIM_DELETE)` 摘图标，再销毁窗口、反注册窗口类、join 线程，
//! 不留孤儿窗口、孤儿图标或孤儿线程。非 Windows 平台 [`Tray::spawn`] 恒返回 `None`。

// 菜单布局、命令映射、双击去重这些纯逻辑在非 Windows 上没有调用方（也没有托盘），
// 但单测要在 Linux 上跑，所以整体允许未使用告警；Win32 代码本身仍由 cfg(windows) 隔离。
#![cfg_attr(not(windows), allow(dead_code))]

use std::time::Duration;

/// 托盘菜单里用户选择的操作。
///
/// 参数：无
/// 核心流程：只把「用户点了什么」编码成一个可 `Send` 的小枚举，托盘线程通过回调把它交给
/// 上层；真正的动作（唤起浮层、写注册表、切主题、退出）全部由上层执行，托盘不碰业务状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// 左键单击 / 双击托盘图标：唤起浮层。
    Show,
    /// 菜单「选项…」。
    Options,
    /// 菜单「开机自启」：上层负责真正读写注册表，托盘只负责通知。
    ToggleAutostart,
    /// 菜单「切换明暗主题」。
    ToggleTheme,
    /// 菜单「退出」。
    Exit,
}

/// 一个菜单项：要么是命令，要么是分隔线。
///
/// 参数：`command` 选中后回调的命令；`label` 菜单文字；`checkable` 是否支持勾选标记
/// 核心流程：把菜单的「样子」做成与平台无关的数据，Windows 侧只负责翻译成 `AppendMenuW`，
/// 于是菜单顺序、文案、ID 映射都能在 Linux 上单测，不用起 Windows。
pub(crate) enum MenuEntry {
    /// 可点击的命令项。
    Command {
        command: TrayCommand,
        label: &'static str,
        checkable: bool,
    },
    /// 分隔线。
    Separator,
}

/// 右键菜单的完整布局（自上而下）。
pub(crate) const MENU_LAYOUT: &[MenuEntry] = &[
    MenuEntry::Command {
        command: TrayCommand::Show,
        label: "打开 Switcheroo",
        checkable: false,
    },
    MenuEntry::Separator,
    MenuEntry::Command {
        command: TrayCommand::Options,
        label: "选项…",
        checkable: false,
    },
    MenuEntry::Command {
        command: TrayCommand::ToggleAutostart,
        label: "开机自启",
        checkable: true,
    },
    MenuEntry::Command {
        command: TrayCommand::ToggleTheme,
        label: "切换明暗主题",
        checkable: false,
    },
    MenuEntry::Separator,
    MenuEntry::Command {
        command: TrayCommand::Exit,
        label: "退出",
        checkable: false,
    },
];

/// 参数：`command` 目标命令
/// 核心流程：给每个命令一个固定且不重复的菜单 ID（从 1 开始），`TrackPopupMenu` 带
/// `TPM_RETURNCMD` 时返回的就是它；ID 必须稳定，否则右键菜单会「点 A 得 B」。
pub(crate) const fn menu_id(command: TrayCommand) -> u32 {
    match command {
        TrayCommand::Show => 1,
        TrayCommand::Options => 2,
        TrayCommand::ToggleAutostart => 3,
        TrayCommand::ToggleTheme => 4,
        TrayCommand::Exit => 5,
    }
}

/// 参数：`id` `TrackPopupMenu` 返回的菜单 ID（0 表示用户点菜单外面取消了）
/// 核心流程：按 [`MENU_LAYOUT`] 反查命令；未知 ID（含 0）返回 `None`，调用方安静忽略，
/// 这样将来往菜单里加项时漏改映射也不会误触发别的命令。
pub(crate) fn command_from_menu_id(id: u32) -> Option<TrayCommand> {
    MENU_LAYOUT.iter().find_map(|entry| match entry {
        MenuEntry::Command { command, .. } if menu_id(*command) == id => Some(*command),
        _ => None,
    })
}

/// 两次 `Show` 之间的最小间隔。
const SHOW_DEBOUNCE: Duration = Duration::from_millis(250);

/// 参数：`elapsed` 距上一次因左键发出的 `Show` 已经过去多久（`None` 表示还没发过）
/// 核心流程：一次双击会先后送来 `WM_LBUTTONUP` 与 `WM_LBUTTONDBLCLK`，两条都回调会让上层
/// 「开了又立刻关」；这里用 [`SHOW_DEBOUNCE`] 窗口去重：没有前一次就放行，否则只有间隔够久
/// 才放行。纯函数，与 Win32 无关，可直接单测。
pub(crate) fn should_emit_show(elapsed: Option<Duration>) -> bool {
    match elapsed {
        None => true,
        Some(elapsed) => elapsed >= SHOW_DEBOUNCE,
    }
}

/// 参数：`slot` 定长宽字符槽位（如 `NOTIFYICONDATAW::szTip`）；`text` 提示文本
/// 核心流程：把 `text` 转成 UTF-16 抄进槽位，最多占 `slot.len() - 1` 个宽字符，随后补一个
/// NUL 收尾——超出部分直接截断，保证提示文本再长也不会越界。纯逻辑，可跨平台单测。
pub(crate) fn fill_tip_text(slot: &mut [u16], text: &str) {
    if slot.is_empty() {
        return;
    }
    let limit = slot.len() - 1;
    let mut written = 0;
    for unit in text.encode_utf16().take(limit) {
        slot[written] = unit;
        written += 1;
    }
    slot[written] = 0;
}

/// 托盘图标的通知区 ID：同一个窗口理论上能挂多个图标，用它区分。
#[cfg(windows)]
const TRAY_ICON_ID: u32 = 1;

/// 托盘回调消息号：`WM_APP + 1`（0x8000 + 1），托盘窗口只处理这一条自定义消息。
#[cfg(windows)]
const TRAY_CALLBACK_MESSAGE: u32 = 0x8000 + 1;

/// Windows 实现：持有托盘线程的线程 ID 与 JoinHandle。
///
/// 参数：内部字段为托盘线程的 Win32 线程 ID、与托盘线程共享的勾选状态、线程句柄
/// 核心流程：spawn 时填充；`set_autostart_checked` 改共享勾选状态；Drop 时用线程 ID
/// 结束线程并 join 回收。
#[cfg(windows)]
pub struct Tray {
    /// 托盘线程的消息队列 ID：Drop 时用它投递 `WM_QUIT`。
    thread_id: u32,
    /// 「开机自启」勾选状态：上层随时可改，菜单下次弹出时读取。
    autostart_checked: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// 托盘线程句柄：Drop 时 join，保证线程真的退出后才返回。
    join: Option<std::thread::JoinHandle<()>>,
}

/// 非 Windows 平台的占位类型。
///
/// 参数：无
/// 核心流程：永远不会被构造（[`Tray::spawn`] 直接返回 `None`），只为让上层代码在
/// Linux/macOS 上照样能编译、能写 `Option<Tray>`。
#[cfg(not(windows))]
pub struct Tray(());

#[cfg(not(windows))]
impl Tray {
    /// 参数：`tooltip` 托盘提示文本；`autostart_checked` 开机自启当前是否勾选；
    /// `on_command` 用户选择命令时的回调（本平台不会被调用）
    /// 核心流程：非 Windows 平台没有任务栏通知区，直接返回 `None`，由上层退化成
    /// 「只有热键、没有托盘」。
    pub fn spawn(
        _tooltip: &str,
        _autostart_checked: bool,
        _on_command: impl Fn(TrayCommand) + Send + 'static,
    ) -> Option<Tray> {
        log::debug!(
            "托盘图标在 {} 平台上为占位实现，不创建",
            std::env::consts::OS
        );
        None
    }

    /// 参数：`checked` 新的勾选状态
    /// 核心流程：占位实现没有托盘可刷新，直接忽略。
    pub fn set_autostart_checked(&self, _checked: bool) {}
}

#[cfg(windows)]
impl Tray {
    /// 参数：`tooltip` 托盘提示文本（悬停显示，超长自动截断）；`autostart_checked`
    /// 开机自启当前是否勾选（决定「开机自启」项初始有没有勾）；`on_command` 用户在托盘上
    /// 触发命令时的回调，在托盘线程上执行，所以要求 `Send + 'static`
    /// 核心流程：建 channel → 起托盘线程 → 线程内建隐藏窗口、加图标后把线程 ID 回传 →
    /// 成功则返回 [`Tray`]；线程起不来、窗口/图标创建失败、或线程没能回传结果都返回 `None`
    /// （上层退化成「只有热键、没有托盘」，不 panic）。
    pub fn spawn(
        tooltip: &str,
        autostart_checked: bool,
        on_command: impl Fn(TrayCommand) + Send + 'static,
    ) -> Option<Tray> {
        use std::sync::atomic::AtomicBool;
        use std::sync::mpsc;
        use std::sync::Arc;

        let autostart = Arc::new(AtomicBool::new(autostart_checked));
        let thread_autostart = Arc::clone(&autostart);
        let tooltip = tooltip.to_string();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();

        let join = match std::thread::Builder::new()
            .name("switcheroo-tray".into())
            .spawn(move || tray_thread(&tooltip, thread_autostart, Box::new(on_command), ready_tx))
        {
            Ok(join) => join,
            Err(err) => {
                log::warn!("无法创建托盘线程：{err}");
                return None;
            }
        };

        match ready_rx.recv() {
            Ok(Ok(thread_id)) => {
                log::debug!("托盘图标已添加，线程 {thread_id}");
                Some(Tray {
                    thread_id,
                    autostart_checked: autostart,
                    join: Some(join),
                })
            }
            Ok(Err(reason)) => {
                log::warn!("托盘图标初始化失败：{reason}");
                let _ = join.join();
                None
            }
            Err(_) => {
                log::warn!("托盘线程未回传初始化结果，已放弃托盘");
                let _ = join.join();
                None
            }
        }
    }

    /// 参数：`checked` 新的勾选状态
    /// 核心流程：只改共享的原子布尔值，菜单在 `WM_RBUTTONUP` 时才按当时的值加
    /// `MF_CHECKED`，所以这里不需要给窗口发消息，也不必重建图标。
    pub fn set_autostart_checked(&self, checked: bool) {
        // Relaxed 足够：只要求「下一次弹菜单能看到新值」，不提供任何 happens-before 语义需求。
        self.autostart_checked
            .store(checked, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(windows)]
impl Drop for Tray {
    /// 参数：无
    /// 核心流程：`PostThreadMessageW(thread_id, WM_QUIT)` 让托盘线程的消息循环退出，线程
    /// 退出前自己完成「NIM_DELETE → 清空 USERDATA → DestroyWindow → DestroyIcon（仅自有的）
    /// → UnregisterClassW」，最后 join 回收线程。返回时图标、窗口、线程都已不存在。
    fn drop(&mut self) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};

        // SAFETY: 只是向自己的托盘线程投递一条 WM_QUIT，没有参数、没有内存所有权转移；
        // 线程可能已经退出，此时调用返回错误，忽略即可。
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }

        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// 托盘线程的状态，挂在隐藏窗口的 `GWLP_USERDATA` 上供窗口过程取用。
///
/// 参数：`icon`/`owns_icon` 托盘图标句柄与它的所有权；`on_command` 上层回调；
/// `autostart_checked` 与 [`Tray`] 共享的勾选状态；`last_show` 双击去重用的时间戳
/// 核心流程：由线程 `Box::into_raw` 后交给窗口，线程退出时再 `Box::from_raw` 收回并释放。
#[cfg(windows)]
struct TrayThreadState {
    /// 挂进通知区的图标句柄。
    icon: windows::Win32::UI::WindowsAndMessaging::HICON,
    /// `true` 表示图标句柄归我们自己（`LoadImageW` 出来的资源图标副本，或 `ExtractIconExW`
    /// 从 exe 里抽出来的），退出时必须 `DestroyIcon`；
    /// `false` 表示系统共享图标（`LoadIconW(IDI_APPLICATION)`），销毁它会打坏别的程序。
    owns_icon: bool,
    /// 上层给的命令回调。
    on_command: Box<dyn Fn(TrayCommand) + Send>,
    /// 「开机自启」勾选状态（与 [`Tray`] 共享）。
    autostart_checked: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// 上一次因左键发出的 `Show` 时刻，用于双击去重。
    last_show: Option<std::time::Instant>,
}

/// 参数：`text` 任意 Rust 字符串
/// 核心流程：转成 Win32 需要的「以 NUL 结尾的 UTF-16」缓冲区，调用期间由调用方持有。
#[cfg(windows)]
fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 参数：`tooltip` 托盘提示文本；`autostart_checked` 与 [`Tray`] 共享的勾选状态；
/// `on_command` 上层回调；`ready` 初始化结果回传通道
/// 核心流程：注册窗口类 → 建隐藏窗口 → 抽图标并 `Shell_NotifyIconW(NIM_ADD)` →
/// 把状态指针写进 `GWLP_USERDATA` → 回传线程 ID → `GetMessageW` 循环 →
/// 收到 `WM_QUIT` 后按「先摘图标、再销毁窗口、最后释放图标与窗口类」的顺序收尾。
#[cfg(windows)]
fn tray_thread(
    tooltip: &str,
    autostart_checked: std::sync::Arc<std::sync::atomic::AtomicBool>,
    on_command: Box<dyn Fn(TrayCommand) + Send>,
    ready: std::sync::mpsc::Sender<Result<u32, String>>,
) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE};
    use windows::Win32::Graphics::Gdi::HBRUSH;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DispatchMessageW, GetMessageW, RegisterClassW, TranslateMessage, HCURSOR,
        HICON, HMENU, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
    };

    const CLASS_NAME: &str = "Switcheroo.TrayWindow";
    const WINDOW_TITLE: &str = "Switcheroo";

    let class_wide = to_wide(CLASS_NAME);

    // SAFETY: GetModuleHandleW(None) 取本进程主模块句柄，只读、不增加引用计数、不转移所有权。
    let module = match unsafe { GetModuleHandleW(None) } {
        Ok(module) => module,
        Err(err) => {
            let _ = ready.send(Err(format!("GetModuleHandleW 失败：{err}")));
            return;
        }
    };
    let instance = HINSTANCE(module.0);

    let wndclass = WNDCLASSW {
        style: Default::default(),
        lpfnWndProc: Some(tray_wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: HICON::default(),
        hCursor: HCURSOR::default(),
        hbrBackground: HBRUSH::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(class_wide.as_ptr()),
    };

    // SAFETY: 注册窗口类。wndclass 是栈上结构体，它指向的 class_wide 缓冲在整个函数内有效，
    // 调用期间不会被移动或释放。返回 0 表示失败。
    let atom = unsafe { RegisterClassW(&wndclass) };
    let registered = atom != 0;
    if !registered {
        // SAFETY: 紧邻 RegisterClassW 失败之后读 last-error，中间没有任何会覆盖它的 Win32 调用。
        let code = unsafe { GetLastError() };
        // 类已存在（上一次没清理干净）不算致命：CreateWindowExW 照样能复用这个类。
        if code != ERROR_CLASS_ALREADY_EXISTS {
            let _ = ready.send(Err(format!("RegisterClassW 失败，错误码 {}", code.0)));
            return;
        }
    }

    let (icon, owns_icon) = load_tray_icon();
    let state_ptr = Box::into_raw(Box::new(TrayThreadState {
        icon,
        owns_icon,
        on_command,
        autostart_checked,
        last_show: None,
    }));

    let title_wide = to_wide(WINDOW_TITLE);
    // SAFETY: 建一个 0 尺寸、不带 WS_VISIBLE 的普通窗口——只为拿一个能收消息的 HWND，
    // 它永远不会显示；失败时返回 Err，窗口句柄为空。
    let created = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_wide.as_ptr()),
            PCWSTR(title_wide.as_ptr()),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            Option::<HMENU>::None,
            Some(instance),
            None,
        )
    };
    let hwnd = match created {
        Ok(hwnd) => hwnd,
        Err(err) => {
            let _ = ready.send(Err(format!("CreateWindowExW 失败：{err}")));
            teardown_tray_thread(state_ptr, None, registered, &class_wide, instance);
            return;
        }
    };

    // SAFETY: 把 Box 裸指针存进窗口的用户数据槽，窗口过程据此取回状态；窗口销毁前
    // teardown 会先把它清空，所以不会变成野指针。
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
            state_ptr as isize,
        );
    }

    // 组装通知区图标：回调消息号必须与窗口过程里判断的一致，否则右键菜单永远弹不出来。
    let mut nid = windows::Win32::UI::Shell::NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<windows::Win32::UI::Shell::NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: TRAY_CALLBACK_MESSAGE,
        ..Default::default()
    };
    if !icon.is_invalid() {
        nid.uFlags |= NIF_ICON;
        nid.hIcon = icon;
    }
    fill_tip_text(&mut nid.szTip, tooltip);

    // SAFETY: NIM_ADD 把 nid 描述的图标加进任务栏通知区；nid 与 hwnd 在这次调用期间都有效。
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
    if !added.as_bool() {
        log::warn!("Shell_NotifyIconW(NIM_ADD) 失败，托盘图标未添加");
        let _ = ready.send(Err("Shell_NotifyIconW(NIM_ADD) 失败".to_string()));
        teardown_tray_thread(state_ptr, Some(hwnd), registered, &class_wide, instance);
        return;
    }

    // SAFETY: GetCurrentThreadId 无参数、无副作用，取当前托盘线程的 ID。
    let thread_id = unsafe { GetCurrentThreadId() };
    if ready.send(Ok(thread_id)).is_err() {
        // 调用方已经放弃托盘（spawn 提前返回），这里必须自己把图标摘掉再走，
        // 否则通知区会留下一个点不掉的图标。
        // SAFETY: 图标上一步已成功加进通知区，这里用同一份 nid 对称删除。
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
        teardown_tray_thread(state_ptr, Some(hwnd), registered, &class_wide, instance);
        return;
    }

    let mut msg = MSG::default();
    loop {
        // SAFETY: GetMessageW 把消息写进栈上的 msg；hwnd 传 None 以便同时收到投递给本线程的
        // 线程消息（Drop 发来的 WM_QUIT）。
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            // 0 表示收到 WM_QUIT，-1 表示出错，两种情况都结束循环。
            break;
        }
        // SAFETY: 把窗口消息分发给 tray_wndproc；线程消息（hwnd 为空）会被 DispatchMessageW 忽略。
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // 收尾顺序是硬要求：先 NIM_DELETE 再把窗口销毁——通知区里的图标一旦拿不到窗口，
    // Windows 会留下一个点不掉的残影，只能注销重新登录才能清掉。
    // SAFETY: 与 NIM_ADD 用的是同一份 nid（同一个 hWnd / uID），对称删除。
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
    teardown_tray_thread(state_ptr, Some(hwnd), registered, &class_wide, instance);
    log::debug!("托盘线程已退出");
}

/// 参数：`state_ptr` 本线程 `Box::into_raw` 出来的状态；`hwnd` 已建好的隐藏窗口（可能没有）；
/// `registered` 本线程是否注册过窗口类；`class_wide` 以 NUL 结尾的类名；`instance` 模块句柄
/// 核心流程：统一的收尾路径——有窗口就先清空 `GWLP_USERDATA` 再 `DestroyWindow`（保证窗口
/// 过程不会读到已释放的状态），然后收回状态 `Box` 并 `DestroyIcon`（仅自有的图标），
/// 最后 `UnregisterClassW`。每个符号只释放一次，且顺序固定。
#[cfg(windows)]
fn teardown_tray_thread(
    state_ptr: *mut TrayThreadState,
    hwnd: Option<windows::Win32::Foundation::HWND>,
    registered: bool,
    class_wide: &[u16],
    instance: windows::Win32::Foundation::HINSTANCE,
) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyIcon, DestroyWindow, SetWindowLongPtrW, UnregisterClassW, GWLP_USERDATA,
    };

    if let Some(hwnd) = hwnd {
        // SAFETY: 先把用户数据槽清掉，DestroyWindow 期间窗口过程就不会再拿到即将释放的状态。
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
        // SAFETY: hwnd 是本线程 CreateWindowExW 建出来、尚未销毁的窗口，这里销毁并回收。
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }

    // SAFETY: state_ptr 由 Box::into_raw 产出，只在这里收回一次，之后不再使用。
    let state = unsafe { Box::from_raw(state_ptr) };
    // SAFETY: 只销毁归自己的图标（LoadImageW 的资源副本 / ExtractIconExW 抽出的）；
    // 系统共享图标（IDI_APPLICATION）不归我们，销毁它会打坏别的程序。
    unsafe {
        if state.owns_icon && !state.icon.is_invalid() {
            let _ = DestroyIcon(state.icon);
        }
    }
    drop(state);

    if registered {
        // SAFETY: 只反注册本线程注册成功过的窗口类；class_wide 以 NUL 结尾且调用期间有效。
        unsafe {
            let _ = UnregisterClassW(PCWSTR(class_wide.as_ptr()), Some(instance));
        }
    }
}

/// 参数：无
/// 核心流程：从本 exe 内嵌的 PE 资源里取托盘图标——`build.rs` 用 `assets/app.rc` 把
/// `assets/app.ico` 以 `1 ICON "app.ico"` 嵌进资源节，图标 ID 就是 1。先 `LoadImageW` 要
/// 16×16（托盘标准尺寸），拿不到再要 32×32；`LoadImageW` 每次都新建一份图标副本，句柄归
/// 调用方，退出时必须 `DestroyIcon`。返回 `None` 表示这个 exe 没有该资源（资源嵌入被跳过，
/// 或是在别处构建的二进制），由调用方降级。
#[cfg(windows)]
fn load_embedded_tray_icon() -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR};

    // SAFETY: GetModuleHandleW(None) 只取本进程主模块句柄，不增加引用计数、不转移所有权。
    let module = unsafe { GetModuleHandleW(None) }.ok()?;
    let instance = HINSTANCE(module.0);
    // MAKEINTRESOURCE(1)：把资源 ID 1 直接当指针传（高位为 0 即「整数 ID」），LoadImageW
    // 只按这个整数查资源表，不会解引用它。这里必须裸写整数转指针：`std::ptr::dangling()`
    // 给的是对齐后的地址（u16 对齐 2），资源 ID 会变成 2，所以屏蔽掉该 lint。
    #[allow(clippy::manual_dangling_ptr)]
    let name = PCWSTR(1 as *const u16);

    for size in [16_i32, 32] {
        // SAFETY: LoadImageW 从本模块资源里新建一份图标副本并返回句柄，失败时返回 Err 而
        // 不是坏句柄；name 是伪指针，只当资源 ID 用，cx/cy 是像素尺寸。
        if let Ok(handle) = unsafe {
            LoadImageW(
                Some(instance),
                name,
                IMAGE_ICON,
                size,
                size,
                LR_DEFAULTCOLOR,
            )
        } {
            let icon = HICON(handle.0);
            if !icon.is_invalid() {
                return Some(icon);
            }
        }
    }
    None
}

/// 参数：无
/// 核心流程：三级降级取托盘图标——① 本 exe 内嵌的资源图标（[`load_embedded_tray_icon`]，
/// 16×16 优先、32×32 次之）；② exe 文件图标（`ExtractIconExW` 抽第 0 个，同样先 16×16 小
/// 图标、再 32×32 大图标）；③ 系统默认应用图标 `LoadIconW(IDI_APPLICATION)`。
/// ①② 的句柄都归调用方，退出时必须 `DestroyIcon`；③ 是系统共享句柄，**不能**销毁。返回
/// `(图标句柄, 是否自有)`；连默认图标都拿不到时返回空句柄，托盘照常创建（只是没有图）。
#[cfg(windows)]
fn load_tray_icon() -> (windows::Win32::UI::WindowsAndMessaging::HICON, bool) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, LoadIconW, HICON, IDI_APPLICATION};

    // 第一级：资源图标。内容就是构建时嵌进 exe 的那个 app.ico，最贴近应用身份。
    if let Some(icon) = load_embedded_tray_icon() {
        return (icon, true);
    }
    log::debug!("托盘图标：exe 资源里没有 ID=1 的图标，退到 exe 文件图标");

    // 第二级：exe 文件图标（兜底：资源节缺失时仍能显示出图标）。
    if let Ok(exe) = std::env::current_exe() {
        let path = exe.to_string_lossy().into_owned();
        let wide: Vec<u16> = exe
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut large = HICON::default();
        let mut small = HICON::default();
        // SAFETY: ExtractIconExW 从 exe 里抽第 0 个图标，把新建的句柄写进栈上变量，返回图标
        // 个数；wide 是本次调用期间有效的 NUL 结尾宽字符串，句柄所有权归调用方。
        let count = unsafe {
            ExtractIconExW(
                PCWSTR(wide.as_ptr()),
                0,
                Some(&mut large),
                Some(&mut small),
                1,
            )
        };
        if count > 0 {
            if !small.is_invalid() {
                // 16×16 小图标是托盘的标准尺寸；大图标虽用不上，但同样归调用方，必须销毁。
                // SAFETY: large 由上面的 ExtractIconExW 新建且只有我们持有，未使用即释放。
                unsafe {
                    if !large.is_invalid() {
                        let _ = DestroyIcon(large);
                    }
                }
                return (small, true);
            }
            if !large.is_invalid() {
                return (large, true);
            }
        }
        log::debug!("托盘图标：exe 里取不到图标 path={path} count={count}");
    }

    // SAFETY: LoadIconW 传空实例句柄 + IDI_APPLICATION 取系统共享的应用图标；该句柄由系统
    // 持有并复用，任何情况下都不能 DestroyIcon，所以返回值第二个元素固定是 false。
    match unsafe { LoadIconW(None, IDI_APPLICATION) } {
        Ok(icon) if !icon.is_invalid() => (icon, false),
        _ => {
            log::warn!("托盘图标：连系统默认图标都取不到，托盘将没有图标");
            (HICON::default(), false)
        }
    }
}

/// 隐藏消息窗口的窗口过程。
///
/// 参数：Win32 标准的 `(hwnd, msg, wparam, lparam)`
/// 核心流程：只处理托盘回调消息 [`TRAY_CALLBACK_MESSAGE`]——从 `GWLP_USERDATA` 取回
/// [`TrayThreadState`]，按 `lparam` 低位的鼠标事件决定发 `Show` 还是弹右键菜单；
/// 其它消息（含初始化阶段 USERDATA 还是空时的消息）一律交给 `DefWindowProcW`。
#[cfg(windows)]
unsafe extern "system" fn tray_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetWindowLongPtrW, GWLP_USERDATA,
    };

    if msg == TRAY_CALLBACK_MESSAGE {
        // 通知区把鼠标事件放在 lparam 的低 16 位，高 16 位是图标 ID，这里只需要事件。
        let event = (lparam.0 as u32) & 0xFFFF;
        // SAFETY: GWLP_USERDATA 里只会有本模块写进去的 Box<TrayThreadState> 裸指针，且窗口
        // 销毁前一定先清空；初始化阶段读到的是 0，按空处理。
        let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut TrayThreadState;
        // SAFETY: 同上，非空指针要么为空、要么是本线程仍然持有的 TrayThreadState。
        if let Some(state) = unsafe { state.as_mut() } {
            handle_tray_event(hwnd, event, state);
        }
        return LRESULT(0);
    }

    // SAFETY: 其它消息交给系统默认窗口过程，参数原样透传，不改变默认行为。
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 参数：`hwnd` 隐藏窗口；`event` 通知区回传的鼠标事件；`state` 托盘线程状态
/// 核心流程：左键单击/双击 → 经 [`should_emit_show`] 去重后回调 [`TrayCommand::Show`]；
/// 右键 → [`show_context_menu`] 弹菜单并把选中项回调给上层；其它事件忽略。
#[cfg(windows)]
fn handle_tray_event(
    hwnd: windows::Win32::Foundation::HWND,
    event: u32,
    state: &mut TrayThreadState,
) {
    use windows::Win32::UI::WindowsAndMessaging::{WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP};

    match event {
        WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
            let now = std::time::Instant::now();
            let elapsed = state.last_show.map(|last| now.duration_since(last));
            if should_emit_show(elapsed) {
                state.last_show = Some(now);
                (state.on_command)(TrayCommand::Show);
            }
        }
        WM_RBUTTONUP => show_context_menu(hwnd, state),
        _ => {}
    }
}

/// 参数：`hwnd` 隐藏窗口（作为菜单的属主）；`state` 托盘线程状态
/// 核心流程：`CreatePopupMenu` → 按 [`MENU_LAYOUT`] 逐项 `AppendMenuW`（「开机自启」按共享
/// 状态加 `MF_CHECKED`）→ `GetCursorPos` 定位 → `SetForegroundWindow` 让菜单能因点到别处
/// 而消失 → `TrackPopupMenu`（`TPM_RETURNCMD`，返回值就是菜单 ID）→ 补投一条 `WM_NULL`
/// 修掉 MSDN 记录的老 bug → `DestroyMenu` 释放菜单 → 按 ID 反查命令并回调。
#[cfg(windows)]
fn show_context_menu(hwnd: windows::Win32::Foundation::HWND, state: &mut TrayThreadState) {
    use std::sync::atomic::Ordering;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW, SetForegroundWindow,
        TrackPopupMenu, MF_CHECKED, MF_SEPARATOR, MF_STRING, TPM_NONOTIFY, TPM_RETURNCMD,
        TPM_RIGHTBUTTON, WM_NULL,
    };

    // SAFETY: CreatePopupMenu 只新建一个空弹出菜单，句柄所有权归调用方，下面用 DestroyMenu 释放。
    let menu = match unsafe { CreatePopupMenu() } {
        Ok(menu) => menu,
        Err(err) => {
            log::warn!("CreatePopupMenu 失败，无法弹出托盘菜单：{err}");
            return;
        }
    };

    let autostart_checked = state.autostart_checked.load(Ordering::Relaxed);
    for entry in MENU_LAYOUT {
        match entry {
            MenuEntry::Separator => {
                // SAFETY: 往我们刚建的菜单里追加一条分隔线，文字指针传空。
                let _ = unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) };
            }
            MenuEntry::Command {
                command,
                label,
                checkable,
            } => {
                // 「开机自启」按当前状态决定是否打勾；其它项永远不带勾。
                let flags = if *checkable && autostart_checked {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                };
                let wide = to_wide(label);
                // SAFETY: 追加一个文本菜单项；wide 在本轮循环里一直有效，调用窗口只有这一句。
                let _ = unsafe {
                    AppendMenuW(
                        menu,
                        flags,
                        menu_id(*command) as usize,
                        PCWSTR(wide.as_ptr()),
                    )
                };
            }
        }
    }

    let mut point = POINT::default();
    // SAFETY: GetCursorPos 把光标屏幕坐标写进栈上的 POINT；失败就没法定位菜单，直接放弃。
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        log::warn!("GetCursorPos 失败，放弃弹出托盘菜单");
        // SAFETY: 菜单是上面新建的，任何退出路径都必须销毁，否则每次右键泄漏一个菜单。
        unsafe {
            let _ = DestroyMenu(menu);
        }
        return;
    }

    // 托盘菜单的固定套路：先把属主窗口临时设成前台，否则点到菜单外面时菜单不会自动消失。
    // SAFETY: SetForegroundWindow 只是请求激活一个不可见窗口，失败也无副作用，返回值忽略。
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }

    // SAFETY: TrackPopupMenu 在光标处显示菜单并阻塞到用户选择；TPM_RETURNCMD 让返回值直接是
    // 菜单 ID（0 表示用户取消），菜单句柄在本函数内有效。
    let selected = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        )
    };

    // SAFETY: 菜单关闭后给属主窗口补一条 WM_NULL——MSDN 记录过「不这么做菜单会残留」的老 bug。
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    }

    // SAFETY: 菜单是上面 CreatePopupMenu 新建的，用完必须 DestroyMenu，各条退出路径都已覆盖。
    unsafe {
        let _ = DestroyMenu(menu);
    }

    match command_from_menu_id(selected.0 as u32) {
        Some(command) => (state.on_command)(command),
        // 用户点了菜单以外的地方取消，silently 忽略。
        None => log::debug!("托盘菜单被取消（返回值 {}）", selected.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 菜单顺序与文案必须与契约一致：打开 / 分隔线 / 选项 / 开机自启 / 切主题 / 分隔线 / 退出。
    #[test]
    fn menu_layout_matches_contract() {
        assert_eq!(MENU_LAYOUT.len(), 7);

        let commands: Vec<TrayCommand> = MENU_LAYOUT
            .iter()
            .filter_map(|entry| match entry {
                MenuEntry::Command { command, .. } => Some(*command),
                MenuEntry::Separator => None,
            })
            .collect();
        assert_eq!(
            commands,
            vec![
                TrayCommand::Show,
                TrayCommand::Options,
                TrayCommand::ToggleAutostart,
                TrayCommand::ToggleTheme,
                TrayCommand::Exit,
            ]
        );

        let labels: Vec<&str> = MENU_LAYOUT
            .iter()
            .filter_map(|entry| match entry {
                MenuEntry::Command { label, .. } => Some(*label),
                MenuEntry::Separator => None,
            })
            .collect();
        assert_eq!(
            labels,
            vec![
                "打开 Switcheroo",
                "选项…",
                "开机自启",
                "切换明暗主题",
                "退出"
            ]
        );

        assert!(matches!(MENU_LAYOUT[1], MenuEntry::Separator));
        assert!(matches!(MENU_LAYOUT[5], MenuEntry::Separator));
    }

    /// 只有「开机自启」需要勾选标记，其它项都不带。
    #[test]
    fn only_autostart_item_is_checkable() {
        for entry in MENU_LAYOUT {
            if let MenuEntry::Command {
                command, checkable, ..
            } = entry
            {
                assert_eq!(*checkable, *command == TrayCommand::ToggleAutostart);
            }
        }
    }

    /// 菜单 ID 必须唯一，且 `menu_id` / `command_from_menu_id` 互为逆映射。
    #[test]
    fn menu_ids_round_trip() {
        let all = [
            TrayCommand::Show,
            TrayCommand::Options,
            TrayCommand::ToggleAutostart,
            TrayCommand::ToggleTheme,
            TrayCommand::Exit,
        ];
        let mut ids: Vec<u32> = all.iter().map(|command| menu_id(*command)).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "菜单 ID 必须互不相同");

        for command in all {
            assert_eq!(command_from_menu_id(menu_id(command)), Some(command));
        }
    }

    /// 未知 ID 与取消（0）都不能映射出命令，避免误触发。
    #[test]
    fn unknown_menu_id_maps_to_nothing() {
        assert_eq!(command_from_menu_id(0), None);
        assert_eq!(command_from_menu_id(99), None);
    }

    /// 第一次左键必须放行；随后的双击第二击要被 250ms 窗口吃掉。
    #[test]
    fn show_is_debounced() {
        assert!(should_emit_show(None));
        assert!(!should_emit_show(Some(Duration::from_millis(0))));
        assert!(!should_emit_show(Some(Duration::from_millis(249))));
        assert!(should_emit_show(Some(Duration::from_millis(250))));
        assert!(should_emit_show(Some(Duration::from_secs(5))));
    }

    /// 提示文本正常写入并以 NUL 结尾。
    #[test]
    fn tip_text_is_written_and_terminated() {
        let mut slot = [0xFFFFu16; 8];
        fill_tip_text(&mut slot, "ab");
        assert_eq!(slot[0], 'a' as u16);
        assert_eq!(slot[1], 'b' as u16);
        assert_eq!(slot[2], 0);
    }

    /// 超长提示文本按槽位截断，末尾仍是 NUL（不会越界写）。
    #[test]
    fn tip_text_is_truncated() {
        let mut slot = [0u16; 8];
        fill_tip_text(&mut slot, "0123456789abcdef");
        assert_eq!(
            &slot[..7],
            &[
                b'0' as u16,
                b'1' as u16,
                b'2' as u16,
                b'3' as u16,
                b'4' as u16,
                b'5' as u16,
                b'6' as u16
            ]
        );
        assert_eq!(slot[7], 0);
    }

    /// 空槽位不 panic。
    #[test]
    fn tip_text_handles_empty_slot() {
        let mut slot: [u16; 0] = [];
        fill_tip_text(&mut slot, "x");
    }

    /// 非 Windows 平台必须干净地返回 None，让上层退化成「只有热键、没有托盘」。
    #[cfg(not(windows))]
    #[test]
    fn spawn_returns_none_off_windows() {
        assert!(Tray::spawn("Switcheroo", false, |_command| {}).is_none());
    }
}
