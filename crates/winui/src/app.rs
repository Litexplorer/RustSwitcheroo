//! 窗口、消息循环与线程模型。
//!
//! 参数：无（模块级说明）
//! 核心流程：注册窗口类 → 创建无边框 / 工具窗口 / 置顶 / 分层的隐藏窗口 → 把 [`App`] 指针
//! 挂到 `GWLP_USERDATA` → 在窗口里建原生输入框（[`SearchEdit`]）→ 注册全局热键（回调只
//! `PostMessageW(WM_APP + 1)`）→ 消息循环里处理切换、输入框转发过来的导航键、`EN_CHANGE`
//! 与销毁；所有 UI、D2D 与 GDI 调用都只发生在窗口线程。
//!
//! 图标（S5）：每次唤起重取快照之后，把快照里「还没请求过」的窗口 id 交给
//! [`IconLoader`] 的后台线程去解码；线程每解出一张就往本窗口投递 `WM_APP + 3`，
//! 窗口线程收到后把结果交给渲染器缓存成 D2D 位图——解码从不占用窗口线程。
//!
//! 键盘与鼠标（S7）：焦点在输入框里时按键由 `edit` 子类化后转发回来（`WM_APP + 2` 按下、
//! `WM_APP + 4` 抬起、`WM_APP + 5` 滚轮），父窗口据此做导航 / 切换 / 关闭 / 滚动；鼠标
//! 点击、双击、悬停与滚轮在父窗口的 `WM_MOUSE*` 里按行矩形命中，与渲染共用
//! `layout::ROW_HEIGHT` 与 `layout::list_top()`，画出来的行与点得到的行永远对得上。
//!
//! 关闭窗口（S8）：`Ctrl+W` / `Alt+X` 对选中窗口调 `request_close`，结果按 [`CloseOutcome`]
//! 分三路：`Requested` 把 id 记进 [`ClosingWindows`] 并用 `SetTimer(hwnd, id, 125ms, None)`
//! 起一个窗口定时器，每个 `WM_TIMER` 用 `is_window_alive` 复查一遍，确认消失的条目从快照与
//! 排名里一起移除；`Denied` 是 UIPI 拒绝——目标窗口以管理员（更高完整性级别）运行时，
//! `WM_CLOSE` 会被系统挡在目标进程之外，此时只提示「需要管理员权限才能关闭」，不登记关闭中
//! 也不起轮询；要关掉这类窗口，本程序自身必须先提权（发布用的清单已是 `requireAdministrator`）。
//! 全程在窗口线程上跑，没有额外线程。
//!
//! 失焦收起：`WM_ACTIVATE` 的低字为 `WA_INACTIVE`（光标点到别的窗口 / 程序被切到后台）时，
//! 浮层自动收起——与原版的 Deactivated 隐藏语义一致。这套逻辑成立的前提是浮层真的拿到过前台
//! 与激活，所以 `show()` 不再裸调 `SetForegroundWindow`（后台进程的调用会被前台锁静默拒绝），
//! 而是走 `take_foreground`：`AttachThreadInput` 借用前台线程的输入队列后再抢前台，焦点直接给
//! 到 EDIT 子窗口，结束时用 `GetForegroundWindow` 复核并落一条日志。`show()` 之后有 300ms
//! 保护期（[`DEACTIVATE_GRACE`]），把 `ShowWindow` / `SetForegroundWindow` / 挂接解绑 / 焦点
//! 交接带来的瞬时激活变化过滤掉，否则会出现「热键一按、浮层刚亮就自己消失」。收起只隐藏窗口，
//! 进程与热键继续常驻。
//!
//! 托盘（S11）：[`App::new`] 里 `Tray::spawn` 起托盘线程，回调只把 [`TrayCommand`] 塞进
//! `std::sync::mpsc` 通道并 `PostMessageW(WM_APP + 5)` 唤醒窗口线程；窗口线程在 `WM_TRAY`
//! 分支里 `try_recv` 排空队列后逐条分派（显示 / 选项 / 自启 / 主题 / 退出）。托盘线程不碰任何
//! UI 状态，所有 `App` 字段都只在窗口线程上读写。`Tray` 句柄由 `App` 持有，`WM_DESTROY` 里
//! `take()` 让它的 `Drop` 摘掉图标并 join 托盘线程。
//!

//! 透明方式的选择：这里走 `WS_EX_LAYERED` + `UpdateLayeredWindow`（逐像素 alpha），
//! 而不是 DWM 透明（`DwmExtendFrameIntoClientArea`）。原因是 DWM 那条路只能给出
//! 「整窗一档透明度」，圆角处会留下不透明的直角；分层窗口把一张 32 位预乘 alpha 位图
//! 直接交给系统，卡片圆角之外是真正的 alpha=0。

use std::collections::HashSet;
use std::mem::size_of;
use std::ops::Range;
use std::ptr;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use switcheroo_core::{
    load_settings, save_settings, HotkeySetting, MatchResult, Settings, WindowId, APP_NAME,
    MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};
use switcheroo_platform_win::{
    autostart, close::CloseOutcome, is_system_dark, is_window_alive, request_close, switch_to,
    HotkeyError, HotkeyListener, HotkeySpec, SingleInstance, SingleInstanceError, Tray,
    TrayCommand,
};

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, GetMonitorInfoW, MonitorFromPoint, SetBkColor, SetTextColor,
    HBRUSH, HDC, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::AttachThreadInput;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetActiveWindow, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_LWIN, VK_MENU, VK_RETURN, VK_RIGHT, VK_RWIN,
    VK_SHIFT, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowThreadProcessId, KillTimer,
    LoadImageW, PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW, SetForegroundWindow,
    SetTimer, SetWindowLongPtrW, ShowWindow, TranslateMessage, CS_DBLCLKS, EN_CHANGE,
    GWLP_USERDATA, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, MSG, SW_HIDE, SW_SHOW,
    WA_INACTIVE, WM_ACTIVATE, WM_APP, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT, WM_DESTROY,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_SETFOCUS, WM_SETICON, WM_SYSCHAR, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::close::{ClosingWindows, POLL_INTERVAL_MS, POLL_TIMER_ID};
use crate::edit::{
    SearchEdit, SEARCH_HINT, WM_EDIT_IME, WM_EDIT_KEY, WM_EDIT_KEYUP, WM_EDIT_MOUSE, WM_EDIT_WHEEL,
};
use crate::icons::IconLoader;
use crate::layout;
use crate::list::ListState;
use crate::options::{self, OptionsDraft};
use crate::render::{palette, EditSurface, Frame, OptionsView, Renderer, RowText};
use crate::theme;

/// 窗口逻辑尺寸（DIP）：与 egui 版 `main.rs` 的 600×640 对齐，列表一屏 18 行。
const WINDOW_WIDTH: f32 = 600.0;
/// 见 [`WINDOW_WIDTH`]。
const WINDOW_HEIGHT: f32 = 640.0;
/// 窗口类名（进程内唯一即可）。
const CLASS_NAME: &str = "SwitcherooWinuiWindow";
/// 热键线程 → 窗口线程的自定义消息：切换浮层显示。
const WM_TOGGLE: u32 = WM_APP + 1;
/// 图标线程 → 窗口线程的自定义消息：有图标结果可以取了（`WM_APP + 2` 已被输入框占用）。
const WM_ICONS_READY: u32 = WM_APP + 3;
/// 托盘线程 → 窗口线程的自定义消息：命令队列里有新命令（`WM_APP + 5`）。
/// 托盘回调跑在托盘线程上，只负责往通道里塞 [`TrayCommand`] 再投递这条消息；
/// 输入框转发滚轮的消息为了让位已改到 `WM_APP + 8`（见 `edit::WM_EDIT_WHEEL`）。
const WM_TRAY: u32 = WM_APP + 5;
/// `MDT_EFFECTIVE_DPI`：取监视器当前生效的缩放值。
const MDT_EFFECTIVE_DPI: i32 = 0;
/// 取不到监视器信息时的兜底屏幕尺寸（物理像素）。
const FALLBACK_SCREEN: RECT = RECT {
    left: 0,
    top: 0,
    right: 1920,
    bottom: 1080,
};
/// Ctrl+Q 里的 `Q`：Win32 里字母键的虚拟键码等于该字母的 ASCII 码。
const VK_Q: u16 = 0x51;
/// Ctrl+W 里的 `W`（见 [`VK_Q`] 的说明）。
const VK_W: u16 = 0x57;
/// Alt+X 里的 `X`（见 [`VK_Q`] 的说明）。
const VK_X: u16 = 0x58;
/// `WM_MOUSELEAVE`（0x02A3）：鼠标移出窗口。windows-sys 0.61 把它放在未开启的
/// `Win32_UI_Controls` feature 下，这里按 WinUser.h 的取值自定义；本 crate 只拿它当消息号比较。
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Ctrl+, 里的逗号（`VK_OEM_COMMA`）：打开 / 关闭选项面板。
const VK_COMMA: u16 = 0xBC;
/// Ctrl+T 里的 `T`（见 [`VK_Q`] 的说明）。
const VK_T: u16 = 0x54;
/// `WM_HELP`（0x0053）：系统在按下 `F1` 时发给焦点窗口。windows-sys 0.61 把它放在未开启的
/// `Win32_UI_Shell` feature 下，这里按 WinUser.h 的取值自定义；本 crate 只拿它当消息号比较。
const WM_HELP: u32 = 0x0053;
/// 「刚显示就失去激活」的保护期：`show()` 里的 `ShowWindow` / `SetForegroundWindow` / 焦点交接
/// 会连带引发若干次瞬时激活切换（系统还会给上一个前台窗口补发 `WM_ACTIVATE`），不设保护期
/// 会出现「热键一按、浮层刚亮就自己收起来」。300ms 足够覆盖这些瞬时消息，又短到不影响
/// 用户「唤出后马上点别的窗口」的直觉。
///
/// `AttachThreadInput` 取前台带来的激活变化也在这段保护期内：挂接 / 解绑期间 `show()` 还没把
/// `visible` 置真（那段 `WM_ACTIVATE` 直接被 [`App::on_activate`] 的首个判断挡掉），而
/// `shown_at` 又是在取前台序列之后才记的，所以「挂接 → 解绑 → 焦点交接」之后才投递过来的消息
/// 也落在保护期里；300ms 对这条序列绰绰有余（整条序列只有几个同步调用，耗时远低于 1ms）。
const DEACTIVATE_GRACE: Duration = Duration::from_millis(300);

// GetDpiForMonitor 在 windows-sys 里挂在 `Win32_UI_HiDpi` feature 下，而那里链接的是
// `api-ms-win-shcore-scaling-l1-1-1.dll`——mingw 的 gnu 目标没有对应的导入库，
// 交叉编译到 x86_64-pc-windows-gnu 会链接失败。因此这里自行声明并直接链接 shcore.dll。
#[link(name = "shcore")]
extern "system" {
    /// `GetDpiForMonitor`：读监视器的 DPI（只读查询，失败返回负 HRESULT）。
    fn GetDpiForMonitor(monitor: HMONITOR, dpi_type: i32, dpi_x: *mut u32, dpi_y: *mut u32) -> i32;
}

/// 浮层的运行状态，生命周期与窗口一致。
///
/// 参数：`hwnd` 窗口句柄；`renderer` D2D / DWrite 对象（初始化失败时为 `None`）；
/// `error_text` 需要以红字显示的提示（热键或渲染初始化失败）；`hotkey` 当前生效的热键监听器，
/// Drop 时结束热键线程；`visible` 当前是否处于显示状态；`edit` 原生输入框；`list` 列表状态；
/// `geometry` 上一次显示时算出的窗口几何（重绘时复用，避免每次都查监视器）；
/// `edit_background` 输入框背景刷（`WM_CTLCOLOREDIT` 返回它，换主题时重建）；
/// `suppress_change` 为真时忽略 `EN_CHANGE`（清空输入框时用，避免重入重排）；`icons` 后台图标
/// 加载器（`WM_DESTROY` 时停掉线程）；`icon_attempted` 已经提交过请求的窗口 id（成功的、失败
/// 的一起记，避免重复解码）；`hover` 鼠标悬停行（排名下标，渲染时换算成可见行下标）；
/// `tracking_mouse` 是否已向系统登记 `WM_MOUSELEAVE`；`closing` 已请求关闭、等窗口消失的条目；
/// `timer_running` 关闭轮询定时器是否在跑；`notice` 一次性提示（如「关闭失败：…」），显示在
/// 卡片底部的红字行；`settings` 当前生效（已落盘）的设置；`dark` 当前是否深色配色；
/// `panel` 选项面板的草稿（`Some` 表示面板开着）；`panel_error` 面板内部的红字（热键冲突 /
/// 自启写注册表失败 / 配置保存失败）；`composing` 输入法是否正在组字（组字期间不画自绘占位文案，
/// 否则会和组字串叠在一起）；`tray` 托盘图标句柄（`None` 表示托盘没起来，只剩热键可用）；
/// `tray_rx` 托盘线程 → 窗口线程的命令队列（`WM_TRAY` 分支里 `try_recv` 排空）；
/// `shown_at` 最近一次 [`App::show`] 完成的时刻（失去激活的防误触保护期靠它判断）。
/// 核心流程：由 [`run`] 创建并挂在窗口的 `GWLP_USERDATA` 上，窗口过程通过它读写状态；
/// 所有句柄都在 `WM_DESTROY` 里由 [`App::release_resources`] 统一释放。
struct App {
    hwnd: HWND,
    renderer: Option<Renderer>,
    error_text: Option<String>,
    hotkey: Option<HotkeyListener>,
    visible: bool,
    edit: Option<SearchEdit>,
    list: ListState,
    geometry: Geometry,
    edit_background: HBRUSH,
    suppress_change: bool,
    icons: Option<IconLoader>,
    icon_attempted: HashSet<WindowId>,
    hover: Option<usize>,
    tracking_mouse: bool,
    closing: ClosingWindows,
    timer_running: bool,
    notice: Option<String>,
    settings: Settings,
    dark: bool,
    panel: Option<OptionsDraft>,
    panel_error: Option<String>,
    composing: bool,
    tray: Option<Tray>,
    tray_rx: Receiver<TrayCommand>,
    shown_at: Option<Instant>,
}

impl App {
    /// 参数：`hwnd` 已创建但尚未显示的窗口句柄。
    /// 核心流程：创建 Direct2D / DirectWrite 渲染器 → 读配置 → 把旧版写在 HKCU Run 下的自启值
    /// 迁移到计划任务（失败只 `warn`，不拦启动）→ 自启状态以计划任务为准，不一致时立刻回写一次
    /// → 按配置里的主题与系统明暗定下 `dark` → 用该套配色的搜索框底色建输入框背景刷 → 建命令
    /// 通道并起托盘线程（勾选按对账后的自启真值初始化；起不来只 `warn`）。
    /// 任一步失败都只把原因记进 `error_text` / 日志，窗口仍可显示。
    fn new(hwnd: HWND) -> Self {
        let (renderer, error_text) = match Renderer::new() {
            Ok(renderer) => {
                log::info!("Direct2D / DirectWrite 初始化完成");
                (Some(renderer), None)
            }
            Err(err) => {
                log::error!("Direct2D / DirectWrite 初始化失败：{err}");
                (None, Some(format!("渲染初始化失败：{err}")))
            }
        };

        // S10：开机自启以计划任务为准。用户在任务管理器里关过自启、或换了 exe 路径，都以
        // 计划任务的真实状态修正配置并回写，面板打开时看到的勾选才是真的。
        let mut settings = load_settings();
        // 旧 egui 版把自启写在 HKCU Run 下，新版改用计划任务：先迁移再读真值，否则旧值会被当成
        // 「已启用」而计划任务其实并不存在。迁移幂等、没有旧值时零副作用；失败只记 warn——
        // 旧值仍在（自启不会凭空消失），不值得为它拦住启动。
        if let Err(err) = autostart::migrate_legacy_run_value() {
            log::warn!("开机自启迁移（HKCU Run → 计划任务）失败，保留旧值：{err}");
        }
        // 任务里可能还存着旧的 8.3 短路径（短名会重排、卷关掉 8.3 名称生成后直接失效）：比对
        // 任务目标，不一致就用当前长路径覆盖一次；一致时零副作用，所以每次启动都能调。
        if let Err(err) = autostart::reconcile_task_target() {
            log::warn!("自启任务目标校正失败，保留原任务：{err}");
        }
        let registered = autostart::is_enabled();
        if settings.autostart != registered {
            settings.autostart = registered;
            log::info!(
                "开机自启以计划任务为准修正为 {registered}，回写配置{}",
                match save_settings(&settings) {
                    Ok(()) => String::new(),
                    Err(err) => format!("失败：{err}"),
                }
            );
        }

        // S9：主题来源是配置（System 时问系统），启动时就定下来，首帧颜色直接正确。
        let dark = theme::dark_for(settings.theme, is_system_dark());
        log::info!(
            "主题：{}（系统当前{}）",
            theme::theme_label(settings.theme),
            if is_system_dark() { "深色" } else { "浅色" }
        );

        // 输入框背景刷：EDIT 自己不画背景时会向父窗口要一个刷子（WM_CTLCOLOREDIT），
        // 这里预先建好，换主题时重建、WM_DESTROY 里删除，避免每条消息新建/删除刷子。
        // SAFETY: CreateSolidBrush 只读颜色值，不接触其它指针；返回的刷子归本结构体所有。
        let edit_background = unsafe { CreateSolidBrush(palette(dark).search_fill.to_colorref()) };

        // 图标后台线程：它只有两种产出（解好的图标 / 解不出来），每产出一个就投递一条
        // WM_ICONS_READY 唤醒窗口线程。hwnd 以整数形式带进闭包，窗口在消息循环结束前有效。
        let hwnd_bits = hwnd as isize;
        let icons = IconLoader::spawn(move || {
            // SAFETY: PostMessageW 是线程安全的异步投递；hwnd 属于本进程，窗口销毁后投递
            // 只会返回 0，不会造成未定义行为。
            unsafe {
                PostMessageW(hwnd_bits as HWND, WM_ICONS_READY, 0, 0);
            }
        });

        // S11：托盘。回调跑在托盘线程上，只做两件事——把命令塞进 mpsc、投递 WM_TRAY 唤醒
        // 窗口线程，绝不碰 App 的任何字段（UI 状态只在窗口线程上读写）。勾选状态用上面
        // 对账过的（`registered` 就是 `autostart::is_enabled()` 的结果，与注册表一致）。
        let (tray_tx, tray_rx) = mpsc::channel::<TrayCommand>();
        let tray = Tray::spawn("Switcheroo", registered, move |command| {
            if tray_tx.send(command).is_err() {
                // 窗口线程已经退出、Receiver 被丢掉：命令无人处理，记一条日志就够了。
                log::warn!("托盘命令无人接收（窗口线程已退出）：{command:?}");
                return;
            }
            // SAFETY: PostMessageW 是线程安全的异步投递；hwnd 属于本进程，窗口销毁后投递
            // 只会返回 0，不会造成未定义行为。
            unsafe {
                PostMessageW(hwnd_bits as HWND, WM_TRAY, 0, 0);
            }
        });
        if tray.is_none() {
            log::warn!("托盘图标创建失败：本次只有热键可用，没有托盘菜单");
        }

        Self {
            hwnd,
            renderer,
            error_text,
            hotkey: None,
            visible: false,
            edit: None,
            list: ListState::new(),
            geometry: Geometry::default(),
            edit_background,
            suppress_change: false,
            icons: Some(icons),
            icon_attempted: HashSet::new(),
            hover: None,
            tracking_mouse: false,
            closing: ClosingWindows::new(),
            timer_running: false,
            notice: None,
            settings,
            dark,
            panel: None,
            panel_error: None,
            composing: false,
            tray,
            tray_rx,
            // 还没显示过：没有保护期需要判断（`visible` 为假时也不会去查它）。
            shown_at: None,
        }
    }

    /// 参数：`scale` DPI 缩放（1.0 = 96 DPI）。
    /// 核心流程：调 [`SearchEdit::create`] 建子控件；成功就记进 `self.edit`，失败只记一条
    /// error 日志（浮层仍然能用方向键与热键，只是没有输入框）。
    fn create_search_edit(&mut self, scale: f32) {
        match SearchEdit::create(self.hwnd, scale) {
            Ok(edit) => self.edit = Some(edit),
            Err(err) => log::error!("创建输入框失败（浮层仍可显示）：{err}"),
        }
    }

    /// 参数：无。
    /// 核心流程：按鼠标所在监视器算居中矩形与 DPI 缩放 → 取前台窗口与快照并重排列表 →
    /// 清空输入框、选中第 1 项、偏移归零 → 先把卡片画进分层窗口（`UpdateLayeredWindow` 同时
    /// 完成定位与尺寸设置）→ 再把输入框摆到卡片顶部 → [`take_foreground`] 抢前台并在挂接期间
    /// 设好焦点（`AttachThreadInput` 绕前台锁，详见该函数）→ 焦点交给输入框并全选 → 用
    /// `GetForegroundWindow` 复核这一次到底有没有真的拿到前台：成功记 `info`、失败记 `warn`
    /// （附 `GetLastError`）。面板开着时（S10）输入框保持隐藏、焦点留在父窗口，面板的按键才
    /// 不会被打字吞掉。
    fn show(&mut self) {
        let geometry = self.center_geometry();
        self.geometry = geometry;
        // 缩放按整数百分比打日志：`{:.2}` 这种浮点格式化会把 core::fmt 的 grisu 算法
        // （约 15 KB 代码 + 查找表）拉进 dist，而日志里 125% 足够表达精度。
        log::info!(
            "浮层几何：位置 ({}, {})，尺寸 {}×{}，DPI 缩放 {}%",
            geometry.position.x,
            geometry.position.y,
            geometry.width,
            geometry.height,
            layout::scale_percent(geometry.scale)
        );

        // S4：每次唤起都重新取前台窗口 + 快照，并把上次的前台窗口排到末尾。
        self.list.reload();
        // S5：快照变了才提交图标请求（唤起 / 窗口集合变动），绝不逐帧提交。
        self.sync_icons();
        // S7 / S8：上一轮的悬停行、一次性提示与组字状态都不该跨唤起保留。
        self.hover = None;
        self.notice = None;
        self.composing = false;

        // 换到不同 DPI 的监视器时，输入框字体要按新的缩放重建（内部会判断是否需要重建）。
        if let Some(edit) = self.edit.as_mut() {
            edit.ensure_scale(geometry.scale);
        }

        // S4.6：清空查询。SetWindowTextW 会同步触发 EN_CHANGE，这里用标志忽略它，
        // 避免在 &mut self 的方法里被窗口过程重入调用。
        self.suppress_change = true;
        if let Some(edit) = self.edit.as_ref() {
            edit.set_text("");
        }
        self.suppress_change = false;

        // 先把输入框摆到卡片顶部，再画：卡片内容里有一步是让 EDIT 用 WM_PRINTCLIENT 把自己
        // 画进位图，如果它还没被 MoveWindow 到目标尺寸，首帧会按旧的 100×28 客户区绘制。
        self.place_edit();
        // 面板开着时输入框保持隐藏：面板的按键都由父窗口处理，有个隐藏的 EDIT 抢到焦点反而
        // 会让方向键 / Tab 失去作用（浮动层重新唤起时面板状态会被保留）。
        let panel_open = self.panel.is_some();
        if let Some(edit) = self.edit.as_ref() {
            edit.set_visible(!panel_open);
        }
        self.render();

        // 取前台的焦点目标：面板没开且有输入框时必须落在 EDIT 子窗口上——输入法与按键转发都
        // 挂在 EDIT 上，`SetFocus` 要是给了顶层窗口，输入法根本不会工作。面板开着 / 没有输入框
        // 时才退化成顶层窗口（面板按键全在 `wndproc` 里处理）。
        let focus_target = match (panel_open, self.edit.as_ref()) {
            (false, Some(edit)) => edit.hwnd(),
            _ => self.hwnd,
        };

        // 抢前台：后台进程直接 `SetForegroundWindow` 会被前台锁静默拒绝（真机上「窗口加载完成」
        // 那一刻 `GetForegroundWindow` 仍是别的程序，键盘就全落到别人身上），所以走
        // [`take_foreground`] 里的 `AttachThreadInput` 序列。
        let foreground_error = take_foreground(self.hwnd, focus_target);

        match (panel_open, self.edit.as_ref()) {
            // 面板开着 / 没有输入框：退化成父窗口自己接收键盘消息（面板按键、Esc 仍可用）。
            (true, _) | (false, None) => {
                // SAFETY: SetFocus 只改本线程的焦点窗口，句柄是本进程的窗口。
                unsafe {
                    SetFocus(self.hwnd);
                }
            }
            // 有输入框就把键盘焦点与输入法交给它；方向键 / Tab / 回车由它转发回父窗口。
            (false, Some(edit)) => edit.focus_and_select_all(),
        }

        self.visible = true;
        // 记下显示完成的时刻：`ShowWindow` / `SetForegroundWindow` / 焦点交接会引发几条瞬时
        // `WM_ACTIVATE`，[`App::on_activate`] 靠这段保护期把它们过滤掉（详见 `DEACTIVATE_GRACE`）。
        // 这一行刻意放在取前台序列**之后**记：保护期要覆盖的是「挂接 / 解绑 / 焦点交接」之后
        // 才投递过来的那些消息，而不是挂接之前的时间。
        self.shown_at = Some(Instant::now());

        // 复核这次到底有没有拿到前台：`SetForegroundWindow` 的返回值只表示「请求没被当场拒」，
        // 真值只能问 `GetForegroundWindow`。这一行是排障锚点——前台不是本窗口，就说明前台锁
        // 没绕过去：键盘事件会全落进别的程序，`WM_ACTIVATE` 的失焦隐藏也不会触发。
        // SAFETY: GetForegroundWindow 是只读查询，无参数、不接触任何指针。
        let foreground = unsafe { GetForegroundWindow() };
        if foreground == self.hwnd {
            log::info!(
                "取前台结果：hwnd={}，前台={}（期望是本窗口）",
                self.hwnd as usize,
                foreground as usize
            );
        } else {
            log::warn!(
                "取前台失败：hwnd={}，前台={}（期望是本窗口），GetLastError={foreground_error}",
                self.hwnd as usize,
                foreground as usize
            );
        }
    }

    /// 参数：`reason` 本次隐藏的触发原因（如 `"失去激活"` / `"Esc"` / `"切换窗口后"`），只用于日志：
    /// 真机上看这一行就能分清「是点别处收起的，还是按 Esc / 切窗口收起的」。
    /// 核心流程：先记一条带原因的 info → 停掉关闭轮询定时器、丢掉「关闭中」标记（浮层都看不见了，
    /// 没必要再每 125ms 醒一次）→ 面板开着就一并丢掉草稿（不走 `close_panel`：那会把输入框重新
    /// 显示出来并抢回焦点，而窗口马上要隐藏）→ 清掉悬停行、鼠标离开追踪与组字状态 → 隐藏输入框
    /// （它是子窗口，父窗口隐藏后它本来也看不见，但显式隐藏可以保证下次显示时不会被系统自动
    /// "恢复"）→ `ShowWindow(SW_HIDE)` 隐藏浮层；分层位图留在窗口上，下次 show 会重画。
    /// 进程与热键都不受影响：隐藏后仍在消息循环里等 `WM_TOGGLE` / `WM_TRAY`。
    fn hide(&mut self, reason: &str) {
        log::info!("隐藏浮层：{reason}");
        self.stop_close_timer();
        if !self.closing.is_empty() {
            log::debug!("隐藏浮层：丢弃 {} 个关闭中条目", self.closing.len());
            self.closing.clear();
        }
        // 面板（Ctrl+,）开着时也一起收掉：草稿直接丢，下一次唤起回到列表视图。
        if self.panel.take().is_some() {
            self.panel_error = None;
            log::info!("隐藏浮层时一并丢弃选项面板草稿（未保存的修改不保留）");
        }
        self.hover = None;
        self.tracking_mouse = false;
        self.composing = false;
        // 先清 `visible` 再隐藏输入框：隐藏掉「正拿着键盘焦点」的子控件时，系统会把焦点退回父
        // 窗口（并送来 `WM_SETFOCUS`），那时 [`App::on_set_focus`] 必须看到「浮层不可见」才不会
        // 把焦点又抢回一个马上要隐藏的输入框。
        self.visible = false;
        if let Some(edit) = self.edit.as_ref() {
            edit.set_visible(false);
        }
        // SAFETY: 只对本进程窗口调用 ShowWindow(SW_HIDE)，不改任何句柄所有权。
        unsafe {
            ShowWindow(self.hwnd, SW_HIDE);
        }
        log::debug!("浮层已隐藏");
    }

    /// 参数：无。
    /// 核心流程：按当前可见状态在 show / hide 之间切换；录制热键时忽略这次触发——用户很可能
    /// 正在录的就是现在这个组合键，把浮层藏起来会让录制当场断掉。
    fn toggle(&mut self) {
        if self.panel.as_ref().is_some_and(|draft| draft.recording) {
            log::debug!("录制热键期间忽略全局热键，避免把浮层藏起来");
            return;
        }
        if self.visible {
            self.hide("热键再次触发（切换到隐藏）");
        } else {
            self.show();
        }
    }

    /// 参数：无。
    /// 核心流程：把输入框按 `layout::edit_box` 的 DIP 矩形乘当前 DPI 缩放后 `MoveWindow` 到
    /// 卡片顶部（缩放在 `show` 里刚算好，这里只做换算）。
    fn place_edit(&self) {
        let Some(edit) = self.edit.as_ref() else {
            return;
        };
        let scale = self.geometry.scale;
        let logical_width = self.geometry.width as f32 / scale;
        let (left, top, width, height) = layout::edit_box(logical_width);
        edit.place(
            (left * scale).round() as i32,
            (top * scale).round() as i32,
            (width * scale).round() as i32,
            (height * scale).round() as i32,
        );
    }

    /// 参数：无。
    /// 核心流程：按当前几何算出「一屏几行」→ 保证选中项在可视区内 → 把选中行与悬停行换算成
    /// 可见行下标 → 只把可见行交给渲染器（虚拟化：行数再多也只画一屏，关闭中的行带上
    /// 「变灰」标记）→ 面板打开时改交面板内容（列表的提示 / 红字都让位给面板）→
    /// `present` 重画并贴到分层窗口。
    /// 只在显示、查询变化、选中 / 悬停变化、图标到货、关闭轮询移除条目、面板开关与草稿变化时
    /// 调用，绝不放进每帧循环——本程序没有帧循环。
    fn render(&mut self) {
        let geometry = self.geometry;
        let visible_rows = self.visible_rows();
        self.list.ensure_selected_visible(visible_rows);

        let (start, end) = self.list.visible_range(visible_rows);
        let selected = self
            .list
            .selected()
            .filter(|row| *row >= start && *row < end)
            .map(|row| row - start);
        let hover = self
            .hover
            .filter(|row| *row >= start && *row < end)
            .map(|row| row - start);
        let rows: Vec<RowText<'_>> = (start..end)
            .filter_map(|row| {
                let window = self.list.row(row)?;
                let ranked = self.list.ranked(row)?;
                Some(RowText {
                    id: window.id,
                    title: window.title.as_str(),
                    process: window.process_name.as_str(),
                    title_ranges: hit_ranges(&ranked.title),
                    process_ranges: hit_ranges(&ranked.process),
                    closing: self.closing.contains(window.id),
                })
            })
            .collect();
        let panel_open = self.panel.is_some();
        // 面板铺满卡片内容区，列表的提示与红字在它下面，不再画。
        let message = if panel_open {
            None
        } else {
            self.list.message()
        };
        // 一次性提示（关闭失败等）优先于常驻错误：两者共用卡片底部的那一行红字。
        let error = if panel_open {
            None
        } else {
            self.notice.as_deref().or(self.error_text.as_deref())
        };
        let logical_width = geometry.width as f32 / geometry.scale;
        // 输入框位置与 place_edit 用同一个 layout::edit_box，保证「画出来的位置」与
        // 「真正的子窗口位置」一致（分层窗口的子控件不会被系统合成，得由渲染器代画）。
        // 面板打开时输入框被隐藏，也就不画。
        let edit = if panel_open {
            None
        } else {
            self.edit.as_ref().map(|edit| {
                let (left, top, width, height) = layout::edit_box(logical_width);
                EditSurface {
                    hwnd: edit.hwnd(),
                    left,
                    top,
                    width,
                    height,
                }
            })
        };
        // 占位文案（S9）：输入框为空时由我们自己画，颜色走 palette.text_muted。输入法正在组字
        // 时先不画——组字串就落在同一块位置上，两者叠起来两边都看不清。
        let edit_hint = match self.edit.as_ref() {
            Some(edit) if !panel_open && !self.composing && edit.is_empty() => Some(SEARCH_HINT),
            _ => None,
        };
        // 面板视图：只把草稿抄成一份只读快照，渲染器不回写任何东西。
        let panel = self.panel.as_ref().map(|draft| OptionsView {
            enable_hotkey: draft.enable_hotkey,
            recording: draft.recording,
            hotkey: draft.hotkey,
            autostart: draft.autostart,
            theme: draft.theme,
            error: self.panel_error.as_deref(),
        });

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let frame = Frame {
            rows: &rows,
            selected,
            hover,
            message,
            error,
            edit,
            edit_hint,
            dark: self.dark,
            panel: panel.as_ref(),
        };
        if let Err(err) = renderer.present(
            self.hwnd,
            geometry.position,
            geometry.width,
            geometry.height,
            geometry.scale,
            &frame,
        ) {
            log::error!("绘制分层窗口失败：{err}");
        }
    }

    /// 参数：无。
    /// 核心流程：按刚 reload 出来的快照做三件事——① 把渲染器的图标缓存裁剪到「快照里还存在」
    /// 的窗口（窗口消失就丢，含窗口销毁时的整表释放）；② 把「已请求过」的集合同样收缩
    /// （关掉的窗口下次再出现时会重新请求）；③ 给还没请求过的窗口各提交一次后台解码。
    /// 只在快照变化时调用（唤起 / 窗口集合变动），绝不逐帧提交。
    fn sync_icons(&mut self) {
        let ids: Vec<WindowId> = self.list.windows().iter().map(|window| window.id).collect();
        let keep: HashSet<WindowId> = ids.iter().copied().collect();

        if let Some(renderer) = self.renderer.as_mut() {
            renderer.retain_icons(&keep);
        }
        self.icon_attempted.retain(|id| keep.contains(id));

        let Some(icons) = self.icons.as_ref() else {
            return;
        };
        let mut submitted = 0;
        for id in ids {
            if self.icon_attempted.insert(id) {
                icons.request(id);
                submitted += 1;
            }
        }
        log::debug!(
            "图标请求提交 {submitted} 个（快照 {} 个窗口，已尝试 {} 个）",
            keep.len(),
            self.icon_attempted.len()
        );
    }

    /// 参数：无。
    /// 核心流程：后台线程每解出一张图标就投递一条 `WM_ICONS_READY`，窗口过程转到这里：把结果
    /// 通道里的条目全部取回，成功的交给渲染器（转预乘 BGRA + 建 D2D 位图缓存），拿到 `None`
    /// 的窗口只记 debug——「已尝试」标记在提交时就记下了，不会重复请求。浮层可见且有新图标时
    /// 立刻重画一次。
    fn on_icons_ready(&mut self) {
        let Some(loader) = self.icons.as_ref() else {
            return;
        };
        let finished = loader.drain();
        if finished.is_empty() {
            return;
        }

        let mut added = 0;
        for (id, image) in finished {
            let Some(image) = image else {
                log::debug!("窗口 {:#X} 没有可用图标，继续画占位块", id.raw());
                continue;
            };
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.insert_icon(id, &image);
                added += 1;
            }
        }
        log::debug!(
            "图标到货 {added} 张，缓存共 {} 张",
            self.renderer.as_ref().map_or(0, Renderer::icon_count)
        );

        if added > 0 && self.visible {
            self.render();
        }
    }

    /// 参数：`source` 触发 `EN_CHANGE` 的控件句柄
    /// 核心流程：不是自己的输入框、或正处于「清空输入框」的抑制期就直接返回；否则读当前文本
    /// 交给列表重排（查询变化是唯一会重排排名的情况之一），然后重画。
    fn on_edit_change(&mut self, source: HWND) {
        if self.suppress_change {
            return;
        }
        let Some(edit) = self.edit.as_ref() else {
            return;
        };
        if edit.hwnd() != source {
            return;
        }
        let query = edit.text();
        self.list.set_query(&query);
        // 重排之后悬停行的下标指向的已经是另一个窗口了，直接清掉，等下一次鼠标移动重新算。
        self.hover = None;
        self.render();
    }

    /// 参数：`composing` 输入法是否开始组字
    /// 核心流程：记下组字状态，值真的变了才重绘一次——占位文案的可见性只取决于这个标志与
    /// 输入框是否为空，重绘一次就够了。
    fn on_ime_composition(&mut self, composing: bool) {
        if self.composing == composing {
            return;
        }
        self.composing = composing;
        self.render();
    }

    /// 参数：`virtual_key` 从输入框转发过来（或父窗口直接收到）的虚拟键码
    /// 核心流程：面板打开时整个键盘交给 [`App::on_panel_key_down`]（列表的导航 / 切换一律不响应）
    /// → 否则先把组合键交给 [`App::handle_hotkey`]（`Ctrl+Q` / `Ctrl+W` / `Alt+X` / `Ctrl+T` /
    /// `Ctrl+,`，与输入框转发路径共用同一份实现）→ 再处理导航键：方向键 / Tab 移动选中项
    /// （越界由 core 的 [`switcheroo_core::Selection`] 回绕，Tab 按 Shift 决定方向）→ 每次移动后
    /// 重画，让选中行与滚动偏移立刻生效 → Esc 隐藏浮层 → 回车只记日志（真正的切换放在
    /// [`App::on_key_up`]，避免自动重复连发时切好几次）。
    /// 返回 `true` 表示这个键已被消费，窗口过程据此决定要不要再交回 `DefWindowProcW`。
    fn on_key_down(&mut self, virtual_key: u32) -> bool {
        if self.panel.is_some() {
            return self.on_panel_key_down(virtual_key);
        }
        // 组合键只有这一份实现：焦点在父窗口（`WM_KEYDOWN` / `WM_SYSKEYDOWN`）与焦点在输入框
        // （`WM_EDIT_KEY`）都先走到这里，再落到 `handle_hotkey`。
        if self.handle_hotkey(virtual_key as u16) {
            return true;
        }
        match virtual_key as u16 {
            VK_DOWN => {
                self.list.next();
                self.render();
                true
            }
            VK_UP => {
                self.list.previous();
                self.render();
                true
            }
            VK_TAB => {
                // SAFETY: GetKeyState 是只读查询，不接触任何指针；最高位为 1 表示键当前按下。
                let shift_down = unsafe { GetKeyState(VK_SHIFT as i32) < 0 };
                if shift_down {
                    self.list.previous();
                } else {
                    self.list.next();
                }
                self.render();
                true
            }
            VK_ESCAPE => {
                self.hide("Esc");
                true
            }
            VK_RETURN => {
                log::debug!(
                    "回车按下：当前选中 {:?}（切换在 KeyUp 上执行）",
                    self.list.selected()
                );
                true
            }
            _ => {
                log::debug!("未处理的按键 0x{virtual_key:02X}");
                false
            }
        }
    }

    /// 参数：`virtual_key` 候选虚拟键码
    /// 核心流程：`Ctrl+Q` 退出 → `Ctrl+W` / `Alt+X` 关闭选中窗口 → `Ctrl+T` 浅深翻面并落盘 →
    /// `Ctrl+,` 打开选项面板；每次都用 `GetKeyState` 复核修饰键是否真按着（避免单独敲字母就触发），
    /// 没有修饰键就返回 `false`，让这个按键继续走导航 / 输入。每次命中都先打一条日志：真机上
    /// 只要看有没有这一行，就能判断按键到底有没有进到浮层。
    ///
    /// 这是组合键的**唯一**实现：父窗口焦点下的 `WM_KEYDOWN` / `WM_SYSKEYDOWN` 与输入框转发来的
    /// `WM_EDIT_KEY` 都经 [`App::on_key_down`] 落到这里，因此焦点在顶层窗口还是输入框都能按到
    /// 同一套组合键。返回 `true` 表示这个键已被消费。
    fn handle_hotkey(&mut self, virtual_key: u16) -> bool {
        // SAFETY: GetKeyState 是只读查询，不接触任何指针；最高位为 1 表示该键当前按下。
        let (ctrl_down, alt_down) = unsafe {
            (
                GetKeyState(VK_CONTROL as i32) < 0,
                GetKeyState(VK_MENU as i32) < 0,
            )
        };
        match virtual_key {
            VK_Q if ctrl_down => {
                log::info!("快捷键 Ctrl+Q：退出浮层");
                self.request_quit();
                true
            }
            VK_W if ctrl_down => {
                log::info!("快捷键 Ctrl+W：关闭选中窗口");
                self.close_selected();
                true
            }
            VK_X if alt_down => {
                log::info!("快捷键 Alt+X：关闭选中窗口");
                self.close_selected();
                true
            }
            VK_T if ctrl_down => {
                log::info!(
                    "快捷键 Ctrl+T：切换明暗（当前主题 {}，热键 {}）",
                    theme::theme_label(self.settings.theme),
                    options::hotkey_text(&self.settings.hotkey)
                );
                self.toggle_theme();
                true
            }
            VK_COMMA if ctrl_down => {
                log::info!(
                    "快捷键 Ctrl+,：打开选项面板（当前主题 {}，热键 {}）",
                    theme::theme_label(self.settings.theme),
                    options::hotkey_text(&self.settings.hotkey)
                );
                self.open_panel();
                true
            }
            _ => false,
        }
    }

    /// 参数：`virtual_key` 抬起的虚拟键码
    /// 核心流程：面板打开时抬起一律不处理（否则回车抬起会把列表里的窗口切走）→ 回车：
    /// 切换动作放在抬起时执行，自动重复不会把同一个窗口切好几次 → `Esc`：`WM_KEYDOWN`
    /// 那一步已经收起浮层，这里只负责把这条消息消费掉；其它键抬起一律忽略。
    /// 返回 `true` 表示已消费。
    fn on_key_up(&mut self, virtual_key: u32) -> bool {
        if self.panel.is_some() {
            return false;
        }
        match virtual_key as u16 {
            VK_RETURN => {
                self.commit_selected();
                true
            }
            VK_ESCAPE => true,
            _ => false,
        }
    }

    /// 参数：`virtual_key` 面板打开时按下的虚拟键码（父窗口焦点下的 `WM_KEYDOWN` /
    /// `WM_SYSKEYDOWN`，以及输入框转发过来的按键）
    /// 核心流程：录制态 → `Esc` 取消录制、修饰键与单独 Tab 忽略、其余按键按当前修饰键现状折算成
    /// [`HotkeySetting`] 写进草稿并退出录制（S10.1）→ 非录制态 → `Esc` 关面板（不隐藏浮层）、
    /// 回车等同「确定」、`Ctrl+,` 等同「取消」、`Ctrl+T` 只记一条 debug 且不切主题、Tab / 方向键
    /// 只消费不动作（面板打开时这些键不能挪动列表选中项，S10.4）→ 其余键交回默认过程。
    /// 返回 `true` 表示已消费。
    fn on_panel_key_down(&mut self, virtual_key: u32) -> bool {
        let vk = virtual_key as u16;
        let Some(recording) = self.panel.as_ref().map(|draft| draft.recording) else {
            return false;
        };

        if vk == VK_ESCAPE {
            if recording {
                if let Some(draft) = self.panel.as_mut() {
                    draft.recording = false;
                }
                log::info!("热键录制已取消（保留原来的组合键）");
                self.render();
            } else {
                self.cancel_panel();
            }
            return true;
        }

        if recording {
            // 修饰键本身只用来凑修饰位；单独按 Tab 是用户在换焦点，不录制也不结束录制。
            if options::is_modifier_key(u32::from(vk)) || vk == VK_TAB {
                return true;
            }
            match options::hotkey_from_key(u32::from(vk), modifiers_now()) {
                Some(setting) => {
                    if let Some(draft) = self.panel.as_mut() {
                        draft.hotkey = setting;
                        draft.recording = false;
                    }
                    log::info!("热键录制完成：{}", options::hotkey_text(&setting));
                    self.render();
                }
                None => log::debug!("录制中忽略按键 0x{vk:02X}（不足以构成全局热键）"),
            }
            return true;
        }

        // SAFETY: GetKeyState 是只读查询，不接触任何指针；最高位为 1 表示键当前按下。
        let ctrl_down = unsafe { GetKeyState(VK_CONTROL as i32) < 0 };
        match vk {
            VK_RETURN => {
                self.save_options();
                true
            }
            VK_COMMA if ctrl_down => {
                self.cancel_panel();
                true
            }
            // 面板打开时 Ctrl+T 不切主题（也不能让按键漏给默认过程）：只记一条 debug，
            // 真机上看到这行就知道「按键进来了，是被面板有意拦下的」，不是没收到。
            VK_T if ctrl_down => {
                log::debug!("选项面板打开时 Ctrl+T 被面板拦下（不切换主题）");
                true
            }
            // 面板打开时这些键不移动列表选中项，也不做别的事，只吃掉它们。
            VK_TAB | VK_UP | VK_DOWN | VK_LEFT | VK_RIGHT => true,
            _ => false,
        }
    }

    /// 参数：无
    /// 核心流程：面板已经开着就直接返回（`Ctrl+,` 的「打开」不会覆盖正在编辑的草稿）→ 否则把
    /// 当前设置抄成草稿、清掉上次留下的面板红字、收起悬停行 → 隐藏输入框并把焦点交还父窗口
    /// （面板的按键全部在 `wndproc` 里处理，输入框抢到焦点反而会让 Tab / 方向键失效）→ 重绘。
    fn open_panel(&mut self) {
        if self.panel.is_some() {
            return;
        }
        self.panel = Some(OptionsDraft::new(&self.settings));
        self.panel_error = None;
        self.hover = None;
        if let Some(edit) = self.edit.as_ref() {
            edit.set_visible(false);
        }
        // SAFETY: SetFocus 只改本线程的焦点窗口，句柄是本进程的窗口。
        unsafe {
            SetFocus(self.hwnd);
        }
        log::info!(
            "打开选项面板（当前主题 {}，热键 {}）",
            theme::theme_label(self.settings.theme),
            options::hotkey_text(&self.settings.hotkey)
        );
        self.render();
    }

    /// 参数：无
    /// 核心流程：丢掉草稿、清掉面板红字、把输入框重新显示出来并把焦点还给它（S10.4「关闭面板后
    /// 重新聚焦输入框」）→ 重绘一次回到列表视图。浮层不可见时只清状态，不动焦点。
    fn close_panel(&mut self) {
        self.panel = None;
        self.panel_error = None;
        if self.visible {
            if let Some(edit) = self.edit.as_ref() {
                edit.set_visible(true);
            }
            self.focus_edit();
        }
        log::info!(
            "关闭选项面板（当前主题 {}，热键 {}）",
            theme::theme_label(self.settings.theme),
            options::hotkey_text(&self.settings.hotkey)
        );
        self.render();
    }

    /// 参数：无
    /// 核心流程（S10.3）：「取消」= 丢弃草稿 + 恢复主题 → 关面板。热键监听全程没被动过，
    /// 所以只要把配色按「已保存的设置」重算一次就回到打开面板之前的样子。
    fn cancel_panel(&mut self) {
        log::info!("选项面板已取消，运行时状态保持为已保存的设置");
        self.set_dark(theme::dark_for(self.settings.theme, is_system_dark()));
        self.close_panel();
    }

    /// 参数：无
    /// 核心流程（S10.2）：先试注册、成功才落盘——按 [`options::plan_hotkey_rebind`] 决定要不要用
    /// 草稿里的组合键试注册（组合键没变且旧监听还在就跳过，否则会撞上自己占着的那一个）→
    /// 试注册失败：旧监听全程没被动过，草稿热键回滚到已保存值、面板内报红字后直接返回
    /// （不写配置文件、不关面板）→ 通过后写注册表自启（失败同样不落盘）→ `save_settings` 落盘
    /// （失败不关面板）→ 新监听接管（旧监听随之 Drop，注销旧组合键）→ 应用主题 → 关面板。
    ///
    /// 早退路径上新建的监听随作用域结束一起 Drop，它注册的组合键会被注销，不会留下「注册了但
    /// 没人用」的热键。
    fn save_options(&mut self) {
        let Some(next) = self.panel.as_ref().map(OptionsDraft::to_settings) else {
            return;
        };

        // 1) 热键：先试注册，此时还没落盘。旧监听在成功之前一直开着，所以不会出现
        //    「旧热键已注销、新热键也没注册上」的空窗。
        let plan = options::plan_hotkey_rebind(&self.settings, &next, self.hotkey.is_some());
        let mut pending = None;
        if plan == options::HotkeyPlan::Register {
            match self.spawn_hotkey(next.hotkey) {
                Ok(listener) => pending = Some(listener),
                Err(err) => {
                    if let Some(draft) = self.panel.as_mut() {
                        draft.hotkey = self.settings.hotkey;
                        draft.recording = false;
                    }
                    self.panel_error = Some(hotkey_failure_text(&err));
                    log::warn!("选项面板：热键未保存（注册失败）：{err}");
                    self.render();
                    return;
                }
            }
        }

        // 2) 开机自启：先落到注册表；失败时不落盘、不关面板，允许用户重试。
        let autostart_result = if next.autostart {
            autostart::enable()
        } else {
            autostart::disable()
        };
        if let Err(err) = autostart_result {
            self.panel_error = Some(format!("写入开机自启失败（{err}），未保存"));
            log::warn!("选项面板：开机自启未保存：{err}");
            self.render();
            return;
        }

        // 3) 落盘：热键与自启都已经真生效，这里才写配置文件。
        if let Err(err) = save_settings(&next) {
            self.panel_error = Some(format!("保存配置失败（{err}），未保存"));
            log::warn!("选项面板：配置未保存：{err}");
            self.render();
            return;
        }

        // 4) 全部成功：运行时设置与监听此刻才切换。
        self.settings = next;
        // 托盘菜单的「开机自启」勾选跟着面板里的选择一起走（菜单只在下次右键时读这个值）。
        if let Some(tray) = self.tray.as_ref() {
            tray.set_autostart_checked(self.settings.autostart);
        }
        match plan {
            // 组合键没变：旧监听继续用着，`pending` 本来就是 `None`。
            options::HotkeyPlan::Keep => {}
            // 新监听接管与旧监听 Drop 在同一次赋值里完成：旧监听的 Drop 会向热键线程投递退出
            // 消息、注销旧组合键并 join。
            options::HotkeyPlan::Register => {
                if let Some(listener) = pending {
                    self.hotkey = Some(listener);
                }
            }
            // 关掉热键：配置已经落盘，旧监听到这里才释放。
            options::HotkeyPlan::Release => self.hotkey = None,
        }
        self.error_text = None;
        self.set_dark(theme::dark_for(self.settings.theme, is_system_dark()));
        self.close_panel();
        log::info!(
            "选项已保存：热键 {}（{}），自启 {}，主题 {}",
            options::hotkey_text(&self.settings.hotkey),
            if self.settings.enable_hotkey {
                "启用"
            } else {
                "关闭"
            },
            self.settings.autostart,
            theme::theme_label(self.settings.theme)
        );
    }

    /// 参数：`lparam` `WM_LBUTTONDOWN` / `WM_LBUTTONDBLCLK` 里的客户区坐标（物理像素）
    /// 核心流程：客户区坐标除以 DPI 缩放换成 DIP → 用 [`layout::panel_layout`] 做命中测试
    /// （与绘制共用同一份布局）→ 勾选 / 录制 / 主题只改草稿并重绘，确定 / 取消走保存与回滚。
    fn on_panel_click(&mut self, lparam: LPARAM) {
        if self.geometry.scale <= 0.0 {
            return;
        }
        let x = (lparam as u16) as i16 as f32 / self.geometry.scale;
        let y = ((lparam >> 16) as u16) as i16 as f32 / self.geometry.scale;
        let panel = layout::panel_layout(
            self.geometry.width as f32 / self.geometry.scale,
            self.geometry.height as f32 / self.geometry.scale,
        );
        match panel.hit(x, y) {
            layout::PanelHit::EnableHotkey => {
                if let Some(draft) = self.panel.as_mut() {
                    draft.enable_hotkey = !draft.enable_hotkey;
                    // 热键被关掉时不该还停在录制态。
                    if !draft.enable_hotkey {
                        draft.recording = false;
                    }
                }
                self.render();
            }
            layout::PanelHit::Autostart => {
                if let Some(draft) = self.panel.as_mut() {
                    draft.autostart = !draft.autostart;
                }
                self.render();
            }
            layout::PanelHit::Record | layout::PanelHit::HotkeyValue => {
                if let Some(draft) = self.panel.as_mut() {
                    // 录制中再点一次 = 取消录制；热键被关掉时不允许开始录制。
                    if draft.recording {
                        draft.recording = false;
                        log::info!("热键录制已取消（保留原来的组合键）");
                    } else if draft.enable_hotkey {
                        draft.recording = true;
                        log::info!("开始录制热键（Esc 取消）");
                    }
                }
                self.render();
            }
            layout::PanelHit::Theme(index) => {
                if let Some(draft) = self.panel.as_mut() {
                    let current = draft.theme;
                    // 点已选中的那一项 = 三态循环到下一项；点别的项 = 直接选它。
                    draft.theme = match theme::THEME_CHOICES.get(index) {
                        Some(choice) if *choice != current => *choice,
                        _ => theme::cycle_theme(current),
                    };
                }
                self.render();
            }
            layout::PanelHit::Save => self.save_options(),
            layout::PanelHit::Cancel => self.cancel_panel(),
            layout::PanelHit::Nothing => {}
        }
    }

    /// 参数：无
    /// 核心流程（S9.3）：按当前设置在浅 / 深之间翻面 → 写回 `settings.theme` 并落盘 → 应用新
    /// 配色并重绘。落盘失败只记 warn：本次切换照样生效，下次启动会退回原值。
    fn toggle_theme(&mut self) {
        let next = theme::toggled_theme(self.settings.theme, is_system_dark());
        self.settings.theme = next;
        match save_settings(&self.settings) {
            Ok(()) => log::info!(
                "主题已切换为{}（热键 {}）并写回配置",
                theme::theme_label(next),
                options::hotkey_text(&self.settings.hotkey)
            ),
            Err(err) => log::warn!("主题写回配置失败（本次仍然生效）：{err}"),
        }
        self.set_dark(theme::dark_for(next, is_system_dark()));
        self.render();
    }

    /// 参数：无
    /// 核心流程（S11）：`WM_TRAY` 分支调用——把命令队列 `try_recv` 排空，逐条交给
    /// [`App::on_tray_command`]；某条命令返回 `false`（只可能是 `Exit`）就立刻停止排空，
    /// 因为此时窗口已经在退出流程里，剩下的命令只会作用在已经拆掉的状态上。
    fn drain_tray_commands(&mut self) {
        while let Ok(command) = self.tray_rx.try_recv() {
            log::debug!("收到托盘命令：{command:?}");
            if !self.on_tray_command(command) {
                log::debug!("托盘命令触发了退出，停止处理剩余命令");
                break;
            }
        }
    }

    /// 参数：`command` 托盘上用户选中的操作；返回 `false` 表示这次操作触发了退出
    /// 核心流程：五条命令各走一条已经存在的路径——`Show` 走显示漏斗（重载列表、居中、
    /// 聚焦输入框）；`Options` 走显示漏斗 + 打开选项面板（面板已开时只把窗口置前）；
    /// `ToggleAutostart` 先写注册表、成功才落盘并同步托盘勾选，失败回退勾选并报红字；
    /// `ToggleTheme` 复用 [`App::toggle_theme`]（只在浅 / 深之间翻面并落盘，托盘菜单没有
    /// 主题勾选，不需要刷新托盘状态）；`Exit` 走 [`App::exit`] 优雅退出。
    fn on_tray_command(&mut self, command: TrayCommand) -> bool {
        match command {
            TrayCommand::Show => {
                log::info!("托盘：显示浮层");
                self.show();
            }
            TrayCommand::Options => self.show_options_from_tray(),
            TrayCommand::ToggleAutostart => self.toggle_autostart_from_tray(),
            TrayCommand::ToggleTheme => {
                log::info!("托盘：切换明暗主题");
                self.toggle_theme();
            }
            TrayCommand::Exit => {
                self.exit();
                return false;
            }
        }
        true
    }

    /// 参数：无
    /// 核心流程：面板已经开着且浮层可见时只 `SetForegroundWindow` 把它置前（草稿、列表、
    /// 查询都原样保留，不重建草稿）；否则走显示漏斗 [`App::show`] 把浮层亮出来，再
    /// [`App::open_panel`] 打开面板（`open_panel` 在面板已开时是空操作）。
    fn show_options_from_tray(&mut self) {
        if self.panel.is_some() && self.visible {
            log::info!("托盘：面板已打开，只把窗口置前");
            // SAFETY: SetForegroundWindow 只是请求前台，句柄是本进程仍在存活的窗口，
            // 不转移任何所有权。
            unsafe {
                SetForegroundWindow(self.hwnd);
            }
            return;
        }
        log::info!("托盘：显示浮层并打开选项面板");
        self.show();
        self.open_panel();
    }

    /// 参数：无
    /// 核心流程（S11）：先按当前设置算出目标值 → `autostart::enable()` / `disable()` 写注册表 →
    /// 失败：托盘勾选按「已保存的设置」回退（`set_autostart_checked`，注册表没变所以还是原值）、
    /// 面板开着就写面板红字、否则显示浮层并把红字放进卡片底部的一行提示 → 成功：
    /// `settings.autostart` 跟进、`save_settings` 落盘、`tray.set_autostart_checked(next)` 让
    /// 菜单下次弹出时勾选正确。落盘失败只记 warn：注册表已经真的改了，勾选照旧同步。
    fn toggle_autostart_from_tray(&mut self) {
        let next = !self.settings.autostart;
        let outcome = if next {
            autostart::enable()
        } else {
            autostart::disable()
        };
        match outcome {
            Ok(()) => {
                self.settings.autostart = next;
                match save_settings(&self.settings) {
                    Ok(()) => log::info!(
                        "托盘：开机自启已{}并写回配置",
                        if next { "启用" } else { "关闭" }
                    ),
                    Err(err) => log::warn!(
                        "托盘：开机自启已{}（注册表生效），但写回配置失败：{err}",
                        if next { "启用" } else { "关闭" }
                    ),
                }
                // 同步托盘勾选：菜单不重建，只在下次 TrackPopupMenu 时按这个值打勾。
                if let Some(tray) = self.tray.as_ref() {
                    tray.set_autostart_checked(next);
                }
            }
            Err(err) => {
                log::warn!(
                    "托盘：开机自启写注册表失败（{}），勾选回退为 {}：{err}",
                    if next { "启用" } else { "关闭" },
                    self.settings.autostart
                );
                // 回退复选框状态：注册表没被改动，勾选保持「已保存的设置」里的值。
                if let Some(tray) = self.tray.as_ref() {
                    tray.set_autostart_checked(self.settings.autostart);
                }
                let text = format!(
                    "开机自启{}失败（{err}）",
                    if next { "启用" } else { "关闭" }
                );
                if self.panel.is_some() {
                    // 面板开着：红字画在面板内部，和保存选项时的报错一个位置。
                    self.panel_error = Some(text);
                } else {
                    // 浮层藏着的话先把窗口亮出来，否则这行红字用户根本看不到。
                    if !self.visible {
                        self.show();
                    }
                    self.notice = Some(text);
                }
                self.render();
            }
        }
    }

    /// 参数：无
    /// 核心流程（S11）：按「停定时器 → 摘托盘 → 注销热键 → 释放渲染器等资源 → 投递
    /// `WM_CLOSE`」的顺序优雅退出。`WM_CLOSE` 由默认窗口过程转成 `DestroyWindow`，随后
    /// `WM_DESTROY` 里再跑一遍幂等的 [`App::release_resources`] 并由那里 `PostQuitMessage`——
    /// 结束消息循环的仍然是 `WM_DESTROY`，这样销毁子控件 / 分层位图时窗口句柄一定还活着。
    fn exit(&mut self) {
        log::info!("托盘：退出 Switcheroo");
        self.stop_close_timer();
        if self.tray.take().is_some() {
            log::info!("托盘图标已移除（托盘线程已结束）");
        }
        if self.hotkey.take().is_some() {
            log::info!("全局热键已注销");
        }
        self.release_resources();
        self.request_quit();
    }

    /// 参数：`dark` 目标配色
    /// 核心流程：已经是这一档就什么都不做；否则改 `dark` → 用新配色的搜索框底色重建输入框
    /// 背景刷（`WM_CTLCOLOREDIT` 下次就用新刷子 + 新文字色）。重绘由调用方负责，避免一次操作
    /// 画两帧。
    fn set_dark(&mut self, dark: bool) {
        if self.dark == dark {
            return;
        }
        self.dark = dark;
        self.rebuild_edit_brush();
        log::info!("配色已切换为{}", if dark { "深色" } else { "浅色" });
    }

    /// 参数：无
    /// 核心流程：按当前配色建一把新的 `HBRUSH`（`CreateSolidBrush`）→ 成功才替换并删除旧刷子；
    /// 失败只记 warn，继续用旧刷子（输入框底色会落后一档，但不会崩）。
    fn rebuild_edit_brush(&mut self) {
        // SAFETY: CreateSolidBrush 只读颜色值，不接触其它指针；返回的刷子由本结构体持有。
        let brush = unsafe { CreateSolidBrush(palette(self.dark).search_fill.to_colorref()) };
        if brush.is_null() {
            log::warn!("重建输入框背景刷失败，沿用旧刷子");
            return;
        }
        let previous = std::mem::replace(&mut self.edit_background, brush);
        if !previous.is_null() {
            // SAFETY: previous 是上一把由本结构体持有、已经换下场的刷子，只删除一次。
            unsafe { DeleteObject(previous) };
        }
    }

    /// 参数：`setting` 要注册的组合键
    /// 核心流程：`HotkeyListener::spawn` 起热键线程，回调里只把 `WM_TOGGLE` 投给窗口线程
    /// （热键线程绝不碰 UI 状态）。失败时错误原样返回，由调用方决定回滚文案。
    fn spawn_hotkey(&self, setting: HotkeySetting) -> Result<HotkeyListener, HotkeyError> {
        let hwnd_bits = self.hwnd as isize;
        HotkeyListener::spawn(
            HotkeySpec {
                key: setting.key,
                modifiers: setting.modifiers,
            },
            move || {
                // SAFETY: PostMessageW 是线程安全的异步投递；hwnd 属于本进程，窗口在消息循环
                // 结束前一直有效（窗口销毁后投递只会失败返回 0，不会造成未定义行为）。
                unsafe {
                    PostMessageW(hwnd_bits as HWND, WM_TOGGLE, 0, 0);
                }
            },
        )
    }

    /// 参数：无
    /// 核心流程：取当前选中行的窗口 id → `switch_to` → 成功就隐藏浮层并清空查询 / 选中 / 滚动
    /// 偏移；失败只记警告并保持浮层（用户可以换一行再试）。列表为空时什么都不做。
    fn commit_selected(&mut self) {
        let Some(id) = self.selected_window() else {
            log::debug!("切换被忽略：列表为空，没有可切换的窗口");
            return;
        };
        if switch_to(id) {
            log::info!("已切换到窗口 {:#X}，隐藏浮层", id.raw());
            self.hide("切换窗口后");
            self.reset_after_commit();
        } else {
            log::warn!("切换窗口失败：{:#X}（窗口可能已被关闭）", id.raw());
        }
    }

    /// 参数：无
    /// 核心流程：切换成功后收拾状态——抑制 `EN_CHANGE` 把输入框清空 → 清空列表状态（查询、快照、
    /// 排名、选中、滚动偏移）→ 悬停行归零，下次唤起从干净状态开始。
    fn reset_after_commit(&mut self) {
        self.suppress_change = true;
        if let Some(edit) = self.edit.as_ref() {
            edit.set_text("");
        }
        self.suppress_change = false;
        self.list.clear();
        self.hover = None;
    }

    /// 参数：无
    /// 核心流程：取当前选中行的窗口 id，`None` 表示列表为空或下标失效。
    fn selected_window(&self) -> Option<WindowId> {
        let row = self.list.selected()?;
        self.list.row(row).map(|window| window.id)
    }

    /// 参数：无
    /// 核心流程：选中窗口已经在关闭中就直接返回（不重复投递 `WM_CLOSE`）→ `request_close` 的结果
    /// 按 [`CloseOutcome`] 分三路：`Requested` 记进关闭中集合并起 125ms 轮询；`Denied`（UIPI 拒绝）
    /// 只提示「需要管理员权限才能关闭：<标题>」，不登记、不轮询、也不移除该行——重试同样会被拒；
    /// `Failed` 时如果窗口还活着就把「关闭失败：<标题>」显示在卡片底部（浮层保持），窗口已经不存在
    /// 则当场把它从列表里移除。
    fn close_selected(&mut self) {
        let Some(row) = self.list.selected() else {
            log::debug!("关闭被忽略：列表为空，没有选中的窗口");
            return;
        };
        let Some(window) = self.list.row(row) else {
            log::debug!("关闭被忽略：选中行 {row} 已经不在快照里");
            return;
        };
        let id = window.id;
        let title = window.title.clone();
        if self.closing.contains(id) {
            log::debug!("窗口 {:#X} 已在关闭中，忽略重复请求", id.raw());
            return;
        }

        match request_close(id) {
            CloseOutcome::Requested => {
                self.closing.insert(id);
                self.start_close_timer();
                self.notice = None;
                log::info!(
                    "已请求关闭窗口 {:#X}（{}），开启 {POLL_INTERVAL_MS}ms 轮询",
                    id.raw(),
                    title
                );
                self.render();
            }
            CloseOutcome::Denied => {
                // UIPI 拒绝：目标属于更高完整性级别的进程，再投递多少次都会被拦，所以既不影响
                // 关闭中集合（上面已经确认它不在里面），也不移除该行——行留着，用户还能切过去。
                log::warn!(
                    "关闭窗口 {:#X}（{}）被 UIPI 拒绝：目标进程完整性级别更高，重试无意义",
                    id.raw(),
                    title
                );
                self.notice = Some(format!("需要管理员权限才能关闭：{title}"));
                self.render();
            }
            CloseOutcome::Failed => {
                // 投递失败：集合里本来就没有它，这里显式移除一次，保证不会残留「关闭中」标记。
                self.closing.remove(id);
                if is_window_alive(id) {
                    log::warn!("关闭窗口失败：{:#X}（{}），窗口仍存活", id.raw(), title);
                    self.notice = Some(format!("关闭失败：{title}"));
                    self.render();
                } else {
                    log::info!(
                        "关闭窗口 {:#X} 的请求失败，但窗口已消失，直接移除",
                        id.raw()
                    );
                    self.list.remove_window(id);
                    self.sync_icons();
                    self.hover = None;
                    if self.list.is_empty() {
                        log::info!("列表已关空，隐藏浮层");
                        self.hide("列表已关空");
                    } else {
                        self.render();
                    }
                }
            }
        }
    }

    /// 参数：无
    /// 核心流程：每个 `WM_TIMER` 复查一遍关闭中的窗口 → 已经消失的从「关闭中」集合、窗口快照与
    /// 排名里一起移除（选中项由 [`ListState::remove_window`] 修正）→ 有变化就重新同步图标缓存
    /// 并重绘；列表被关空则隐藏浮层；关闭中集合空了顺手停掉定时器，避免空转唤醒窗口线程。
    fn poll_closing(&mut self) {
        if self.closing.is_empty() {
            self.stop_close_timer();
            return;
        }

        let mut removed = 0;
        for id in self.closing.ids() {
            if is_window_alive(id) {
                continue;
            }
            let waited = self
                .closing
                .remove(id)
                .map(|start| start.elapsed().as_millis());
            self.list.remove_window(id);
            removed += 1;
            log::info!(
                "窗口 {:#X} 已关闭（等待 {}ms），从列表移除",
                id.raw(),
                waited.unwrap_or_default()
            );
        }

        if removed > 0 {
            // 行数变了：图标缓存与「已请求」集合都要跟着收缩；鼠标没动，但光标下的行已经换了，
            // 悬停高亮直接清掉，等下一次 WM_MOUSEMOVE 重新算。
            self.sync_icons();
            self.hover = None;
            if self.list.is_empty() {
                log::info!("列表已关空，隐藏浮层");
                self.hide("列表已关空");
                return;
            }
            self.render();
        }

        if self.closing.is_empty() {
            self.stop_close_timer();
        }
    }

    /// 参数：无
    /// 核心流程：定时器已经在跑就直接返回；否则 `SetTimer(hwnd, POLL_TIMER_ID, 125ms, None)`
    /// 让系统每 125ms 往本窗口投一条 `WM_TIMER`（不新建线程）。失败只记警告——关闭请求仍然
    /// 生效，只是列表行要等下次唤起才会消失。
    fn start_close_timer(&mut self) {
        if self.timer_running {
            return;
        }
        // SAFETY: SetTimer 只给本进程窗口登记一个 125ms 定时器，回调传空表示「发 WM_TIMER」；
        // 返回的 id 与传入一致（非 0 成功），退出路径上由 stop_close_timer 用同一个 id 撤销。
        let timer = unsafe { SetTimer(self.hwnd, POLL_TIMER_ID, POLL_INTERVAL_MS, None) };
        if timer == 0 {
            log::warn!("SetTimer({POLL_INTERVAL_MS}ms) 失败，关闭轮询不可用");
            return;
        }
        self.timer_running = true;
        log::debug!("关闭轮询定时器已启动：{POLL_INTERVAL_MS}ms");
    }

    /// 参数：无
    /// 核心流程：`KillTimer` 撤销本窗口的关闭轮询定时器（浮层隐藏、列表被关空、退出时调用），
    /// 没在跑就什么都不做。
    fn stop_close_timer(&mut self) {
        if !self.timer_running {
            return;
        }
        // SAFETY: KillTimer 只撤销本窗口由 SetTimer 登记的同 id 定时器，不接触其它句柄；
        // 定时器未在跑时上面的判断已经拦下，不会重复撤销。
        unsafe { KillTimer(self.hwnd, POLL_TIMER_ID) };
        self.timer_running = false;
        log::debug!("关闭轮询定时器已停止");
    }

    /// 参数：无
    /// 核心流程：窗口逻辑高度 → 卡片底部有没有红字行（常驻错误或一次性提示）决定要不要预留
    /// [`layout::ERROR_RESERVE`] → 交给 [`layout::visible_rows`] 得到一屏行数。渲染、鼠标命中
    /// 与滚轮夹取都用这一个数，画出来的行与点得到的行才不会错位。
    fn visible_rows(&self) -> usize {
        let logical_height = self.geometry.height as f32 / self.geometry.scale;
        let reserve = if self.notice.is_some() || self.error_text.is_some() {
            layout::ERROR_RESERVE
        } else {
            0.0
        };
        layout::visible_rows(logical_height, reserve)
    }

    /// 参数：`lparam` `WM_MOUSE*` / `WM_LBUTTON*` 里的客户区坐标（低字 x、高字 y，物理像素）
    /// 核心流程：客户区坐标除以 DPI 缩放换成 DIP → 横坐标必须落在卡片内 → 纵坐标减去列表顶边
    /// 后除以 [`layout::ROW_HEIGHT`] 得到「可视区第几行」→ 加上滚动偏移得到排名下标 →
    /// 只接受落在当前可见区间 `[start, end)` 里的行，空白区域（列表下方、可视区之外）返回 `None`。
    fn row_at(&self, lparam: LPARAM) -> Option<usize> {
        if self.geometry.scale <= 0.0 {
            return None;
        }
        let x = (lparam as u16) as i16 as f32 / self.geometry.scale;
        let y = ((lparam >> 16) as u16) as i16 as f32 / self.geometry.scale;

        let card_left = layout::CARD_MARGIN;
        let card_right = self.geometry.width as f32 / self.geometry.scale - layout::CARD_MARGIN;
        if x < card_left || x > card_right {
            return None;
        }

        let list_top = layout::list_top();
        if y < list_top {
            return None;
        }
        let visible_row = ((y - list_top) / layout::ROW_HEIGHT).floor() as usize;
        let (start, end) = self.list.visible_range(self.visible_rows());
        let row = start + visible_row;
        if row >= end {
            return None;
        }
        Some(row)
    }

    /// 参数：`lparam` 鼠标位置
    /// 核心流程：按需向系统登记 `WM_MOUSELEAVE`（否则鼠标移出窗口后收不到通知，悬停底色会永远
    /// 留在最后一行）→ 命中测试出悬停行 → 只有悬停行变了才重绘（移动消息量很大，不能每条都画）。
    fn on_mouse_move(&mut self, lparam: LPARAM) {
        // 面板没有悬停态：列表行被面板盖住了，光标下面的行不再重要。
        if !self.visible || self.panel.is_some() {
            return;
        }
        self.track_mouse_leave();
        let hover = self.row_at(lparam);
        if hover != self.hover {
            self.hover = hover;
            self.render();
        }
    }

    /// 参数：无
    /// 核心流程：`TRACKMOUSEEVENT` + `TME_LEAVE` 登记「鼠标离开窗口」通知，登记成功后置位
    /// `tracking_mouse`，收到 `WM_MOUSELEAVE` 时复位（一次登记只对应一次通知）。
    fn track_mouse_leave(&mut self) {
        if self.tracking_mouse {
            return;
        }
        let mut track = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: TrackMouseEvent 只读栈上的 track 并给本进程窗口登记一次 WM_MOUSELEAVE；
        // cbSize 已按结构体实际大小填好，hwndTrack 是仍在存活的窗口。
        let ok = unsafe { TrackMouseEvent(&mut track) };
        if ok != 0 {
            self.tracking_mouse = true;
        }
    }

    /// 参数：无
    /// 核心流程：清掉悬停行并重绘（只有真的画过悬停底色时才重绘），同时复位追踪标志——
    /// 下次进入窗口时重新登记。
    fn on_mouse_leave(&mut self) {
        self.tracking_mouse = false;
        if self.hover.take().is_some() {
            self.render();
        }
    }

    /// 参数：`lparam` 点击位置
    /// 核心流程：命中某行就把它设为选中（重绘让浅蓝底色立刻跟上）；点空白或行之外只把键盘焦点
    /// 还给输入框，保证点完还能继续打字。
    fn on_left_click(&mut self, lparam: LPARAM) {
        if self.panel.is_some() {
            self.on_panel_click(lparam);
            return;
        }
        if let Some(row) = self.row_at(lparam) {
            self.list.set_selected(row);
            self.render();
        }
        self.focus_edit();
    }

    /// 参数：`lparam` 双击位置
    /// 核心流程：命中某行就选中它并直接切换（等价于回车，走同一条 [`App::commit_selected`]
    /// 路径：切换成功隐藏浮层并清空查询 / 选中 / 滚动）；没命中就只把焦点还给输入框。
    fn on_left_double_click(&mut self, lparam: LPARAM) {
        if self.panel.is_some() {
            // 面板里双击不切换窗口：等同一次单击（否则「确定」连点两下会顺手关掉一个窗口）。
            self.on_panel_click(lparam);
            return;
        }
        let Some(row) = self.row_at(lparam) else {
            self.focus_edit();
            return;
        };
        self.list.set_selected(row);
        self.commit_selected();
    }

    /// 参数：`delta` 滚轮增量（正数向上滚、负数向下滚，一格是 120）
    /// 核心流程：向上滚偏移减 1、向下滚加 1，夹取与「一屏放得下就不滚」交给
    /// [`ListState::scroll`]；它返回真才继续 → 选中行如果被滚出了可视区，就把它拉到视窗第一行
    /// （否则 [`App::render`] 里的 `ensure_selected_visible` 会把偏移又弹回去，滚轮等于没用）→
    /// 悬停行清掉（光标下面的行已经换了）→ 重绘。
    fn on_wheel(&mut self, delta: i32) {
        // 面板打开时滚轮不滚动列表（列表被面板盖着）。
        if !self.visible || delta == 0 || self.panel.is_some() {
            return;
        }
        let visible_rows = self.visible_rows();
        let step = if delta > 0 { -1 } else { 1 };
        if !self.list.scroll(step, visible_rows) {
            return;
        }
        self.hover = None;
        let (start, end) = self.list.visible_range(visible_rows);
        match self.list.selected() {
            Some(row) if row < start || row >= end => self.list.set_selected(start),
            _ => {}
        }
        self.render();
    }

    /// 参数：无
    /// 核心流程：窗口自己拿到键盘焦点时（外部 `SetForegroundWindow` 把浮层置前、点了卡片空白处、
    /// 曾经持焦点的子控件被隐藏后系统把焦点退回父窗口）——只要浮层可见且面板没开，就把焦点交还
    /// 输入框：输入法与按键转发都挂在 EDIT 上，焦点留在顶层窗口就只有 `wndproc` 的
    /// `WM_KEYDOWN` 一条路。面板打开时焦点本来就该留在顶层窗口（面板按键全在 `wndproc` 里
    /// 处理），这里不动它。
    fn on_set_focus(&mut self) {
        if !self.visible || self.panel.is_some() {
            return;
        }
        self.focus_edit();
    }

    /// 参数：`wparam` `WM_ACTIVATE` 的 wparam（低 16 位是激活状态：`WA_INACTIVE` = 0 表示失去激活，
    /// 1 / 2 表示被点击 / 其它方式激活）。
    /// 核心流程：只处理「失去激活 + 浮层可见」这一种情况（对齐原版 Deactivated 隐藏语义，用户
    /// 光标点到别处就收起）→ 三重防误触：① 显示后 [`DEACTIVATE_GRACE`] 内的激活变化一律忽略
    /// （`show()` 的 `ShowWindow` / `SetForegroundWindow` / 焦点交接会连带产生瞬时消息，不挡住会
    /// 「刚显示就隐藏」）；② 用 `GetForegroundWindow` 复核前台确实已经不是本窗口；③ 前台为空
    /// 视为「系统正在切前台的中间态」（托盘菜单弹出 / 关闭、Alt+Tab 过渡）同样忽略 →
    /// 三条都过了才走 [`App::hide`]（面板、关闭轮询定时器、关闭中集合都由它统一收拾，进程与
    /// 热键继续常驻）。
    fn on_activate(&mut self, wparam: WPARAM) {
        // 低 16 位是激活状态；高位（最小化标志）这里不关心。
        if (wparam & 0xFFFF) as u32 != WA_INACTIVE || !self.visible {
            return;
        }
        if let Some(shown_at) = self.shown_at {
            let since_show = shown_at.elapsed();
            if since_show < DEACTIVATE_GRACE {
                log::debug!(
                    "显示后 {}ms 内收到失去激活，落在 {}ms 保护期内，忽略",
                    since_show.as_millis(),
                    DEACTIVATE_GRACE.as_millis()
                );
                return;
            }
        }
        // SAFETY: GetForegroundWindow 是只读查询，不接触任何指针，返回当前前台窗口句柄（可能为空）。
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.is_null() {
            log::debug!("失去激活但前台窗口为空（系统正在切换前台），这次不收起");
            return;
        }
        // 前台仍是本窗口（典型是焦点在输入框与父窗口之间来回）就不算「点到别处」。
        // 输入框是子窗口，它的前台窗口本来就是本顶层窗口，因此这里只需跟 self.hwnd 比。
        if foreground == self.hwnd {
            log::debug!("失去激活但前台仍是浮层自身，忽略");
            return;
        }
        self.hide("失去激活（光标点到别的窗口）");
    }

    /// 参数：无
    /// 核心流程：把键盘焦点交还输入框（用户点了卡片空白处之后，焦点会落到父窗口上，
    /// 这时再敲字就没人接了）。没有输入框时什么都不做。
    fn focus_edit(&self) {
        if let Some(edit) = self.edit.as_ref() {
            // SAFETY: SetFocus 只改本线程的焦点窗口，句柄是本进程的子控件。
            unsafe { SetFocus(edit.hwnd()) };
        }
    }

    /// 参数：无
    /// 核心流程：用 `PostMessageW(WM_CLOSE)` 请求退出，而不是直接 `DestroyWindow`——Ctrl+Q 也
    /// 可能是在输入框的消息处理里（`SendMessage` 嵌套装上来）转发过来的，如果在那一层直接
    /// 销毁父窗口，子控件的过程会在窗口已经销毁后继续执行。投递 `WM_CLOSE` 让销毁发生在
    /// 消息循环顶层，`DefWindowProcW` 收到后调用 `DestroyWindow`。
    fn request_quit(&self) {
        // SAFETY: PostMessageW 是线程安全的异步投递；hwnd 是本进程窗口，在本函数调用时存活。
        unsafe {
            PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
        }
    }

    /// 参数：`hdc` 输入框要画的设备上下文
    /// 核心流程（S9.2）：从当前配色（`render::palette`）取背景色 `search_fill` 与文字色
    /// `text_primary`，`SetBkColor` / `SetTextColor` 之后返回同色系的 `HBRUSH`；刷子为空时返回
    /// 0，由窗口过程交回 `DefWindowProcW`。深色下这里是「#E8E8E8 字 + #1A1A1A 底」。
    fn paint_edit_background(&self, hdc: HDC) -> LRESULT {
        if self.edit_background.is_null() {
            return 0;
        }
        let colors = palette(self.dark);
        // SAFETY: SetBkColor / SetTextColor 只改本 HDC 的文本与背景色属性，hdc 来自系统消息，
        // 在本消息处理期间有效。
        unsafe {
            SetBkColor(hdc, colors.search_fill.to_colorref());
            SetTextColor(hdc, colors.text_primary.to_colorref());
        }
        self.edit_background as isize
    }

    /// 参数：无。
    /// 核心流程：在 `WM_DESTROY` 里按「先停定时器、再摘托盘、再子控件、再资源、最后工厂」的
    /// 顺序统一释放：关闭轮询定时器（`KillTimer`，连同关闭中集合与一次性提示一起丢掉）→ 托盘
    /// （`take()` 后 Drop：`NIM_DELETE` 摘图标、销毁托盘窗口、join 托盘线程）→ 输入框
    /// （Drop → `DestroyWindow` + `DeleteObject(font)`）→ 输入框背景刷
    /// （`DeleteObject`）→ 图标后台线程（关请求通道 + `join`）→ 渲染器（图标位图缓存与
    /// `Com` 字段逐个 `IUnknown::Release`：图标位图 → 渲染目标 → 画布 → 文本格式 →
    /// DWrite 工厂 → D2D 工厂）。全部发生在窗口线程、窗口句柄还没被系统回收的时刻；
    /// 每个字段都用 `take()` / 判空保护，重复调用（退出时 [`App::exit`] 先调一次）是幂等的。
    fn release_resources(&mut self) {
        self.stop_close_timer();
        if !self.closing.is_empty() {
            log::debug!("WM_DESTROY：丢弃 {} 个关闭中条目", self.closing.len());
            self.closing.clear();
        }
        self.notice = None;
        // 托盘在渲染器之前摘：图标一消失，用户就不会再点到菜单，也就不会再有命令进来。
        if self.tray.take().is_some() {
            log::debug!("已在 WM_DESTROY 中移除托盘图标并结束托盘线程");
        }
        // 面板草稿与红字也一起丢掉（窗口都没了，草稿留着没有意义）。
        self.panel = None;
        self.panel_error = None;
        if self.edit.take().is_some() {
            log::debug!("已在 WM_DESTROY 中销毁输入框与字体");
        }
        if !self.edit_background.is_null() {
            // SAFETY: 刷子由 App::new / rebuild_edit_brush 创建且只在这里删除一次（换主题时旧的
            // 已在 rebuild_edit_brush 里删掉），删除后立刻置空。
            unsafe { DeleteObject(self.edit_background) };
            self.edit_background = ptr::null_mut();
        }
        // 先停图标线程，再释放渲染器：线程退出前不会再插入新的图标，缓存随渲染器一起
        // Drop（位图 Release、像素表释放）。
        if self.icons.take().is_some() {
            log::debug!("已在 WM_DESTROY 中停止图标加载线程");
        }
        if let Some(renderer) = self.renderer.as_ref() {
            log::debug!("释放图标位图缓存：{} 张", renderer.icon_count());
        }
        if self.renderer.take().is_some() {
            log::debug!("已在 WM_DESTROY 中释放 Direct2D / DirectWrite 对象");
        }
    }

    /// 参数：无。
    /// 核心流程：`GetCursorPos` 取鼠标位置 → `MonitorFromPoint` 找所在监视器（取不到就用主
    /// 显示器）→ `GetMonitorInfoW` 拿监视器矩形 → `GetDpiForMonitor` 拿缩放 →
    /// 按「600×640 逻辑像素 × 缩放」算出窗口尺寸并在监视器矩形里居中。
    fn center_geometry(&self) -> Geometry {
        let mut cursor = POINT { x: 0, y: 0 };
        // SAFETY: GetCursorPos 只把光标位置写进栈上的 cursor，失败时保持 (0, 0)。
        let ok = unsafe { GetCursorPos(&mut cursor) };
        if ok == 0 {
            log::warn!("GetCursorPos 失败，按主显示器居中");
        }

        // SAFETY: MonitorFromPoint 只读 cursor 并按 MONITOR_DEFAULTTONEAREST 返回最近的
        // 监视器句柄（即使坐标不在任何屏幕上也有返回值，绝不为空）。
        let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };

        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            rcMonitor: FALLBACK_SCREEN,
            rcWork: FALLBACK_SCREEN,
            dwFlags: 0,
        };
        // SAFETY: GetMonitorInfoW 把结果写进栈上的 info；cbSize 已按结构体大小填好，
        // monitor 是上一行返回的有效句柄。失败时保留 FALLBACK_SCREEN 作为兜底矩形。
        let ok = unsafe { GetMonitorInfoW(monitor, &mut info) };
        if ok == 0 {
            // SAFETY: 紧邻 GetMonitorInfoW 失败之后读 last-error，中间无其它 Win32 调用。
            let code = unsafe { GetLastError() };
            log::warn!("GetMonitorInfoW 失败（错误码 {code}），按 1920×1080 兜底");
        }
        let screen = info.rcMonitor;

        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        // SAFETY: GetDpiForMonitor 是只读查询：monitor 有效，两个输出指针指向栈上变量。
        let hr = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        let scale = if hr >= 0 && dpi_x >= 96 {
            dpi_x as f32 / 96.0
        } else {
            log::warn!("GetDpiForMonitor 失败（HRESULT 0x{hr:08X}），按 96 DPI 处理");
            1.0
        };

        let width = (WINDOW_WIDTH * scale).round() as i32;
        let height = (WINDOW_HEIGHT * scale).round() as i32;
        let position = POINT {
            x: screen.left + ((screen.right - screen.left) - width) / 2,
            y: screen.top + ((screen.bottom - screen.top) - height) / 2,
        };

        Geometry {
            position,
            width,
            height,
            scale,
        }
    }
}

/// 「临时挂接两个线程的输入队列」的资源守卫。
///
/// 参数：`our` 本 UI 线程 id；`foreground` 取前台那一刻的前台线程 id（0 = 没有前台窗口）；
/// `attached` 是否真的挂上了。
/// 核心流程：[`AttachGuard::attach`] 只在两个 id 都非零且不同的时候调
/// `AttachThreadInput(..., true)`；[`Drop`] 在挂着的时候无条件 `AttachThreadInput(..., false)`
/// 解绑并复位标志。取前台序列无论从哪条路径离开（正常返回、提前 return、panic 展开）都会经过
/// `Drop`，所以成对调用不会漏——这正是选资源守卫而不是「函数末尾手动解绑」的原因：半挂状态会
/// 让两个线程永久共享输入队列，按键与焦点行为会变得莫名其妙。
struct AttachGuard {
    our: u32,
    foreground: u32,
    attached: bool,
}

impl AttachGuard {
    /// 参数：`our` 本 UI 线程 id；`foreground` 前台线程 id（0 表示当前没有前台窗口）。
    /// 核心流程：只填字段，一个 Win32 调用都不发；真正的挂接交给 [`AttachGuard::attach`]，
    /// 这样「要不要挂」与「挂失败的错误码」能放在同一处判断。
    fn new(our: u32, foreground: u32) -> Self {
        Self {
            our,
            foreground,
            attached: false,
        }
    }

    /// 参数：无。
    /// 核心流程：两个 id 非零且不同才挂接（前台就是自己、或根本没有前台窗口时挂了也没意义）；
    /// `AttachThreadInput` 返回 0 表示失败，把 `GetLastError` 原样交回给调用方记日志——挂不上
    /// 不致命，后面照样会试 `SetForegroundWindow`，只是大概率还会被前台锁拒绝。
    /// 返回：失败时的 `GetLastError`（0 = 挂上了或不需要挂）。
    fn attach(&mut self) -> u32 {
        if self.our == 0 || self.foreground == 0 || self.our == self.foreground {
            return 0;
        }
        // SAFETY: AttachThreadInput 只把当前线程的输入队列临时挂到前台线程上，两个 id 都来自
        // 刚查出来的活线程（前台线程属于别的进程、ours 是属于本进程的当前线程），不涉及内存访问；
        // 第三个参数 true 表示挂接（成对的解绑在 Drop 里）。
        let ok = unsafe { AttachThreadInput(self.our, self.foreground, 1) };
        if ok == 0 {
            // SAFETY: GetLastError 只读当前线程的错误码，紧跟失败的调用取值才有意义。
            return unsafe { GetLastError() };
        }
        self.attached = true;
        0
    }
}

impl Drop for AttachGuard {
    /// 参数：无。
    /// 核心流程：挂上过就 `AttachThreadInput(..., false)` 解绑一次并复位 `attached`；没挂上 /
    /// 已经解绑过就什么都不做（重复 Drop 或 attach 失败都不会多调一次）。
    fn drop(&mut self) {
        if !self.attached {
            return;
        }
        // SAFETY: 与 `attach` 里严格成对的解绑，id 还是那两个有效线程 id，第三个参数 0 表示解绑。
        unsafe {
            AttachThreadInput(self.our, self.foreground, 0);
        }
        self.attached = false;
    }
}

/// 参数：`hwnd` 浮层顶层窗口；`focus_target` 拿到前台之后要持有键盘焦点的窗口句柄
/// （面板没开时是 EDIT 子窗口，面板开着时是顶层窗口）。
/// 核心流程（方案 A：`AttachThreadInput` 借用前台资格）：
/// ① `GetForegroundWindow` 取当前前台窗口，`GetWindowThreadProcessId` 把它和本窗口分别换成
///    所属线程 id（本窗口是当前线程创建的，所以这条就是「我们自己的线程」）；
/// ② 两个线程不同且都非零 → `AttachThreadInput(我们, 前台, true)` 临时挂接输入队列。Windows
///    的前台锁只对「当前前台线程 / 刚收到过用户输入」的进程放行 `SetForegroundWindow`，把我们的
///    输入队列挂到前台线程上就等于借用它的前台资格——这是社区里最成熟的做法（方案 B 的
///    `keybd_event` 要真的往系统里注入按键，副作用大，这里不用）。代价是挂接期间两边共享输入
///    队列（对方线程卡死会连带卡住我们），所以挂接只包住下面几个同步调用、做完立刻解绑；
/// ③ 挂接期间依次 `ShowWindow(SW_SHOW)` → `BringWindowToTop` → `SetForegroundWindow` →
///    `SetActiveWindow` → `SetFocus(focus_target)`：焦点属于 **输入队列** 的属性，必须在挂接
///    期间设，解绑之后再设会被算回原前台线程；
/// ④ 解绑交给 [`AttachGuard`] 的 `Drop`（见上）；解绑之后由调用方（[`App::show`]）再设一次
///    焦点兜底——解绑时系统可能把焦点重算回顶层窗口，再设一次保证键盘落在 EDIT 上（我们就是
///    创建它的线程，这个 `SetFocus` 一定成功）。
/// 返回：第一处失败步骤的 `GetLastError`（0 = 三处调用都没报失败），由 [`App::show`] 统一记日志。
fn take_foreground(hwnd: HWND, focus_target: HWND) -> u32 {
    // SAFETY: 三条都是只读查询，不接触任何指针：GetForegroundWindow 取当前前台窗口（可能为空），
    // GetWindowThreadProcessId 取窗口所属线程，第二个参数传空指针表示「只要线程 id」。
    let (foreground, our_thread, foreground_thread) = unsafe {
        let foreground = GetForegroundWindow();
        let our_thread = GetWindowThreadProcessId(hwnd, ptr::null_mut());
        let foreground_thread = if foreground.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, ptr::null_mut())
        };
        (foreground, our_thread, foreground_thread)
    };
    log::debug!(
        "取前台：当前前台={}，前台线程={foreground_thread}，本窗口线程={our_thread}",
        foreground as usize
    );

    // 资源守卫：`guard` 在这个函数返回 / panic 时一定解绑，见 [`AttachGuard`] 的说明。
    let mut guard = AttachGuard::new(our_thread, foreground_thread);
    let mut error = guard.attach();

    // SAFETY: hwnd 是本进程创建的存活窗口，ShowWindow 只改可见性、BringWindowToTop /
    // SetForegroundWindow / SetActiveWindow 都只是前台与 Z 序请求；SetFocus 的目标是本进程的
    // 顶层窗口或它的 EDIT 子窗口。四个调用都不转移句柄所有权。
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        BringWindowToTop(hwnd);
        if SetForegroundWindow(hwnd) == 0 && error == 0 {
            // SAFETY: GetLastError 只读当前线程的错误码，紧跟失败的调用取值才有意义。
            error = GetLastError();
        }
        SetActiveWindow(hwnd);
        SetFocus(focus_target);
    }

    // 显式解绑（而不是靠函数尾部的隐式 Drop）：让「前台已经拿到、挂接已经撤掉」成为返回前
    // 的确定状态，日志里的错误码也不会被 Drop 里的调用覆盖。即使这里没有这行，Drop 也会解。
    drop(guard);
    error
}

/// 参数：无
/// 核心流程：用 `GetKeyState` 读出此刻按着的修饰键，折成 `core` 的 `MOD_*` 位或（与 Win32 的
/// `MOD_ALT` / `MOD_CONTROL` / `MOD_SHIFT` / `MOD_WIN` 取值一致）。录制热键时用它把「按了哪个主键」
/// 补成完整组合键；Win 键不在 `GetKeyState(VK_MENU)` 这类通用键里，所以左右 Win 各查一次。
fn modifiers_now() -> u32 {
    // SAFETY: GetKeyState 是只读查询，不接触任何指针；返回值最高位为 1 表示该键当前按下。
    unsafe {
        let mut bits = 0;
        if GetKeyState(VK_MENU as i32) < 0 {
            bits |= MOD_ALT;
        }
        if GetKeyState(VK_CONTROL as i32) < 0 {
            bits |= MOD_CONTROL;
        }
        if GetKeyState(VK_SHIFT as i32) < 0 {
            bits |= MOD_SHIFT;
        }
        if GetKeyState(VK_LWIN as i32) < 0 || GetKeyState(VK_RWIN as i32) < 0 {
            bits |= MOD_WIN;
        }
        bits
    }
}

/// 参数：`err` 平台层给出的热键注册错误
/// 核心流程：把错误翻成面板里的那句红字。组合键被别的程序占着是最常见的一种，按需求用固定
/// 文案「热键已被其他程序占用，未保存」；其它失败（平台不可用等）附上底层原因，同样标明未保存。
fn hotkey_failure_text(err: &HotkeyError) -> String {
    match err {
        HotkeyError::AlreadyInUse => "热键已被其他程序占用，未保存".to_string(),
        HotkeyError::Unavailable(reason) => format!("热键注册失败（{reason}），未保存"),
    }
}

/// 参数：`result` core 给出的匹配结果。
/// 核心流程：只有真的命中（`matched` 为真）时才把区间交给渲染器加粗；空查询 / 未命中一律
/// 返回空切片，行文本按正常字重绘制。
fn hit_ranges(result: &MatchResult) -> &[Range<usize>] {
    if result.matched {
        &result.ranges
    } else {
        &[]
    }
}

/// 一帧的窗口几何。
///
/// 参数：`position` 左上角（屏幕物理像素）；`width` / `height` 物理像素尺寸；
/// `scale` DPI 缩放（1.0 = 96 DPI）。
/// 核心流程：由 [`App::center_geometry`] 产出，同时用于日志、分层窗口的定位与子控件换算；
/// `Default` 是首次显示之前的占位值（96 DPI 下的 600×640，左上角在原点）。
#[derive(Clone, Copy)]
struct Geometry {
    position: POINT,
    width: i32,
    height: i32,
    scale: f32,
}

/// 参数：无
/// 核心流程：给 [`App::new`] 一个「还没算过几何」的合法初值，数值与 96 DPI 的窗口尺寸一致。
impl Default for Geometry {
    fn default() -> Self {
        Self {
            position: POINT { x: 0, y: 0 },
            width: WINDOW_WIDTH.round() as i32,
            height: WINDOW_HEIGHT.round() as i32,
            scale: 1.0,
        }
    }
}

/// 参数：无（初始化日志、单实例、COM、窗口、输入框、热键并跑消息循环）。
/// 核心流程：单实例抢锁（`AlreadyRunning` 直接返回 0；`Failed` 只记日志并 fail-open 继续启动）→
/// `CoInitializeEx` 进 STA → [`create_window`] → 把 [`App`] 指针写进 `GWLP_USERDATA` → 建输入框 →
/// 注册热键（失败时立即显示窗口并带上红字原因）→ 消息循环 → 循环结束后按字段顺序 Drop
/// （输入框、渲染器、热键线程、托盘）→ `CoUninitialize`。
/// 托盘图标在 [`App::new`] 里创建，所以这里不需要额外步骤；它起不来只会少一个入口，不影响启动。
pub fn run() -> i32 {
    crate::logger::init();
    log::info!("Switcheroo 浮层启动（RUST_LOG 控制级别）");

    // 单实例：互斥量句柄存在 _single 里，进程退出时随 Drop 关闭。
    let _single = match SingleInstance::acquire() {
        Ok(instance) => {
            log::info!("单实例互斥量获取成功");
            Some(instance)
        }
        Err(SingleInstanceError::AlreadyRunning) => {
            // 语义不变：已有实例在跑就静默退出，退出码 0（对外表现为「什么都没发生」）。
            log::info!("已有 Switcheroo 实例在运行（互斥量已被占用），本次静默退出，退出码 0");
            return 0;
        }
        Err(err) => {
            // 互斥量本身坏掉时 fail-open：继续启动，否则用户会完全打不开程序。
            // 代价是两个实例可能同时存在（两个托盘图标、两个热键注册），所以日志写清楚原因。
            log::warn!(
                "单实例互斥量获取失败（fail-open 继续启动，可能出现多个实例 / 多个托盘图标）：{err}"
            );
            None
        }
    };

    // SAFETY: CoInitializeEx 在当前线程初始化 COM（此处要求 STA，D2D / DWrite 都用它）；
    // 重复初始化只会返回 S_FALSE，同样是可用的。
    let hr = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    if hr < 0 {
        log::error!("CoInitializeEx 失败，HRESULT 0x{hr:08X}");
        return 1;
    }

    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(err) => {
            log::error!("创建窗口失败：{err}");
            // SAFETY: 与上面的 CoInitializeEx 配对，只在本线程调用一次。
            unsafe { CoUninitialize() };
            return 1;
        }
    };

    let mut app = Box::new(App::new(hwnd));

    // SAFETY: 把 Box 里的 App 指针存进窗口的 GWLP_USERDATA。Box 的堆地址在 run 返回前
    // 不会变（即使 Box 本身被移动），窗口过程只在处理本窗口消息时解引用它；
    // 消息循环结束、窗口早已销毁后才会 Drop 这个 Box。
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut *app as *mut App as isize);
    }

    // 输入框先按 96 DPI 建：此刻还不知道用户会在哪块屏幕上唤出浮层，
    // 首次 show 时 `ensure_scale` 会按真实的 DPI 缩放重建字体并重新排布。
    app.create_search_edit(1.0);

    // 热键与主题都来自配置：注册失败或配置里关掉热键时，窗口直接显示出来——否则用户既没有
    // 托盘也没有热键，完全没有入口可以再打开它（连 Ctrl+, 都按不到）。
    if app.settings.enable_hotkey {
        match app.spawn_hotkey(app.settings.hotkey) {
            Ok(listener) => {
                log::info!(
                    "全局热键 {} 注册成功",
                    options::hotkey_text(&app.settings.hotkey)
                );
                app.hotkey = Some(listener);
            }
            Err(err) => {
                log::warn!("全局热键注册失败：{err}");
                app.error_text = Some(format!("热键注册失败：{err}"));
                app.show();
            }
        }
    } else {
        log::warn!("配置已关闭全局热键：本次不注册，浮层直接显示以便重新启用");
        app.error_text = Some("全局热键已关闭（Ctrl+, 打开选项重新启用）".to_string());
        app.show();
    }

    let mut msg = MSG::default();
    loop {
        // SAFETY: GetMessageW 把下一条消息写进栈上的 msg；hwnd 传空表示取本线程所有窗口
        // 的消息。返回值 0 是 WM_QUIT、-1 是出错，两种情况都退出循环。
        let got = unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) };
        if got <= 0 {
            break;
        }
        // SAFETY: TranslateMessage / DispatchMessageW 只处理上一步取到的 msg，
        // DispatchMessageW 会同步回调本窗口的 wndproc。
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // 正常路径下输入框与渲染器已经在 WM_DESTROY 里释放过，这里的 Drop 是兜底；
    // 热键监听器在这里 Drop：向热键线程投递 WM_QUIT 并 join，保证不留孤儿线程。
    drop(app);

    // SAFETY: 与开头的 CoInitializeEx 严格配对，只执行一次。
    unsafe { CoUninitialize() };
    log::info!("Switcheroo 浮层已退出");
    0
}

/// 参数：无。
/// 核心流程：`GetModuleHandleW` 取本进程模块句柄 → 填 `WNDCLASSW`（窗口过程、类名，
/// 无背景刷、无光标、无图标，背景交给分层窗口）→ `RegisterClassW` →
/// `CreateWindowExW` 建一个隐藏的弹出窗口，扩展样式是「分层 + 工具窗口（不进任务栏）+
/// 置顶」，初始尺寸按 96 DPI 的 600×640 给，真正尺寸在首次 show 时由 `UpdateLayeredWindow` 设置。
fn create_window() -> Result<HWND, String> {
    let class: Vec<u16> = CLASS_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let title: Vec<u16> = APP_NAME.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: GetModuleHandleW(None) 返回本进程的模块句柄，不转移所有权，进程存活期间始终有效；
    // 这里只用于填 WNDCLASSW.hInstance。
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    if instance.is_null() {
        // SAFETY: 紧邻 GetModuleHandleW 失败之后读 last-error。
        let code = unsafe { GetLastError() };
        return Err(format!("GetModuleHandleW 失败，错误码 {code}"));
    }

    let window_class = WNDCLASSW {
        // CS_DBLCLKS：没有这个类样式，系统就不会把第二次按下合成为 WM_LBUTTONDBLCLK，
        // 双击切换也就永远收不到消息。
        style: CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: ptr::null_mut(),
        hCursor: ptr::null_mut(),
        // 背景刷留空：窗口内容全部由分层位图提供，DefWindowProc 不会擦除它。
        hbrBackground: ptr::null_mut(),
        lpszMenuName: ptr::null(),
        lpszClassName: class.as_ptr(),
    };

    // SAFETY: RegisterClassW 只读取 window_class 与它指向的类名字符串（class 在调用期间存活），
    // 返回 0 表示失败（例如同名类已注册）。
    let atom = unsafe { RegisterClassW(&window_class) };
    if atom == 0 {
        // SAFETY: 紧邻 RegisterClassW 失败之后读 last-error。
        let code = unsafe { GetLastError() };
        return Err(format!("RegisterClassW 失败，错误码 {code}"));
    }
    log::debug!("窗口类 {CLASS_NAME} 已注册");

    let width = WINDOW_WIDTH.round() as i32;
    let height = WINDOW_HEIGHT.round() as i32;
    // SAFETY: CreateWindowExW 的所有指针参数都指向调用期间有效的 NUL 结尾 UTF-16 缓冲；
    // 父窗口、菜单、创建参数都传空，窗口过程在 WM_CREATE 阶段走 DefWindowProc。
    // 样式：WS_POPUP 无标题栏 / 无边框；扩展样式见下面的注释。
    let hwnd = unsafe {
        CreateWindowExW(
            // WS_EX_TOOLWINDOW：不进任务栏、不参与 Alt+Tab；
            // WS_EX_TOPMOST：置顶；
            // WS_EX_LAYERED：配合 UpdateLayeredWindow 做逐像素透明。
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            width,
            height,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        )
    };
    if hwnd.is_null() {
        // SAFETY: 紧邻 CreateWindowExW 失败之后读 last-error。
        let code = unsafe { GetLastError() };
        return Err(format!("CreateWindowExW 失败，错误码 {code}"));
    }

    log::info!("窗口已创建（隐藏）：初始尺寸 {width}×{height}（96 DPI）");

    // 窗口图标：资源 ID 1 的 HICON 只在本进程内加载一次，窗口拿到句柄后系统会按需缩放，
    // 所以 32×32 的那份同时用于 ICON_BIG / ICON_SMALL。
    apply_window_icon(hwnd);
    Ok(hwnd)
}

/// 参数：`hwnd` 刚创建好的浮层窗口。
/// 核心流程：`GetModuleHandleW(None)` 取本模块句柄 → `LoadImageW(模块, MAKEINTRESOURCE(1),
/// IMAGE_ICON, 32, 32, LR_DEFAULTCOLOR)` 从 exe 内嵌资源里取一份图标副本（`build.rs` 用
/// `assets/app.rc` 的 `1 ICON "app.ico"` 把图标嵌在资源 ID 1）→ `SendMessageW(WM_SETICON,
/// ICON_BIG / ICON_SMALL, icon)` 装上 → 取不到只 warn，不影响窗口本身。这份 HICON 刻意不
/// `DestroyIcon`：`WM_SETICON` 只保存句柄不复制，窗口活到进程退出，提前销毁会让窗口持有
/// 悬空图标；一次性的进程级副本随进程回收即可。
fn apply_window_icon(hwnd: HWND) {
    // SAFETY: GetModuleHandleW(None) 只取本进程主模块句柄，不增加引用计数、不转移所有权，
    // 进程存活期间始终有效。
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    if instance.is_null() {
        log::warn!("GetModuleHandleW 失败，跳过窗口图标设置");
        return;
    }

    // MAKEINTRESOURCE(1)：资源 ID 直接当指针传（高位为 0 即「整数 ID」），LoadImageW 只按
    // 这个整数查资源表、不会解引用它。这里必须裸写整数转指针：`std::ptr::dangling()`
    // 给的是对齐后地址（u16 对齐 2），资源 ID 会变成 2。
    #[allow(clippy::manual_dangling_ptr)]
    let name = 1 as *const u16;
    // SAFETY: LoadImageW 从本模块资源里新建一份图标副本，失败返回空指针；name 是伪指针，
    // 只当资源 ID 用；尺寸参数与标志都是纯数值。
    let icon = unsafe { LoadImageW(instance, name, IMAGE_ICON, 32, 32, LR_DEFAULTCOLOR) };
    if icon.is_null() {
        // SAFETY: 紧邻 LoadImageW 失败之后读 last-error，中间无其它 Win32 调用。
        let code = unsafe { GetLastError() };
        log::warn!("取内嵌图标资源（ID 1）失败，窗口图标保持系统默认，错误码 {code}");
        return;
    }

    // SAFETY: SendMessageW 同线程同步投递给刚创建的窗口，WM_SETICON 的 lparam 是图标句柄、
    // wparam 是 ICON_BIG / ICON_SMALL；窗口过程对这条消息走 DefWindowProcW，只做保存。
    unsafe {
        SendMessageW(hwnd, WM_SETICON, ICON_BIG as WPARAM, icon as LPARAM);
        SendMessageW(hwnd, WM_SETICON, ICON_SMALL as WPARAM, icon as LPARAM);
    }
    log::debug!("窗口图标已设置为内嵌资源 ID 1");
}

/// 窗口过程。
///
/// 参数：`hwnd` 接收消息的窗口；`msg` 消息号；`wparam` / `lparam` 消息参数（语义随消息变化）。
/// 核心流程：先从 `GWLP_USERDATA` 取回 [`App`] 指针（`WM_CREATE` 阶段还是空指针，统一先判空）→
/// `WM_APP + 1` 切 show / hide → `WM_APP + 5` 排空托盘命令队列（S11）→ `WM_APP + 2` / `+4` / `+8`
/// 处理输入框转发来的按键按下 / 抬起 / 滚轮 → `WM_COMMAND` 的 `EN_CHANGE` 更新查询并重排 → `WM_CTLCOLOREDIT` 给输入框刷白底 →
/// 鼠标消息（移动 / 离开 / 单击 / 双击 / 滚轮）做命中测试、悬停、选中与滚动 →
/// `WM_TIMER`（关闭轮询定时器）复查关闭中的窗口 → `WM_ACTIVATE` 的 `WA_INACTIVE`（点到别的窗口）
/// 自动收起浮层 → `WM_SETFOCUS`（顶层窗口重新拿到焦点时把焦点
/// 交回输入框）→ `WM_KEYDOWN` / `WM_KEYUP`（焦点在父窗口时）处理导航与快捷键（快捷键与输入框
/// 转发路径共用 [`App::handle_hotkey`]）→ `WM_CHAR` / `WM_SYSCHAR` 一律吞掉（本窗口没有任何文本
/// 输入，交回默认过程会响系统默认提示音）→ `WM_DESTROY` 释放全部资源并投递 `WM_QUIT`；其余交给
/// `DefWindowProcW`。
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: 只读本窗口的 GWLP_USERDATA 槽；窗口创建完成后由 run 写入 App 指针，
    // 在那之前读到 0，下面每个分支都先判空再解引用。
    let app = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;

    match msg {
        WM_TOGGLE => {
            if !app.is_null() {
                // SAFETY: app 指向 run 里那个仍然存活的 Box<App>；热键消息只在窗口线程处理，
                // 不存在并发访问。
                unsafe { (*app).toggle() };
            }
            0
        }
        WM_ICONS_READY => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE，单线程访问同一个 App；图标结果只在窗口线程里落地。
                unsafe { (*app).on_icons_ready() };
            }
            0
        }
        // 光标点到别的窗口 / 程序被切到后台时的激活变化：低字是 WA_INACTIVE(0) 就收起浮层
        // （对齐原版的 Deactivated 语义，防误触机制在 [`App::on_activate`] 里）。
        WM_ACTIVATE => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE，单线程访问同一个 App；wparam 的低 16 位是激活状态。
                unsafe { (*app).on_activate(wparam) };
            }
            // SAFETY: 激活消息仍要默认处理（DefWindowProcW 维护系统的激活 / 焦点内部状态），
            // 参数均来自系统。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_TRAY => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE——托盘线程只往 mpsc 里塞命令再投递本消息，队列由窗口
                // 线程独占取值，App 的所有字段仍然只在窗口线程上读写。
                unsafe { (*app).drain_tray_commands() };
            }
            0
        }
        WM_EDIT_KEY => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE，单线程访问同一个 App；wparam 是输入框转发来的虚拟键码。
                unsafe { (*app).on_key_down(wparam as u32) };
            }
            0
        }
        WM_EDIT_KEYUP => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE；按键抬起只在窗口线程处理，这里触发真正的切换。
                unsafe { (*app).on_key_up(wparam as u32) };
            }
            0
        }
        WM_EDIT_WHEEL => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE；wparam 的高字是 WM_MOUSEWHEEL 原始的带符号滚动增量。
                unsafe { (*app).on_wheel(((wparam >> 16) & 0xFFFF) as u16 as i16 as i32) };
            }
            0
        }
        WM_EDIT_IME => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE，单线程访问同一个 App；wparam 非 0 表示开始组字。
                unsafe { (*app).on_ime_composition(wparam != 0) };
            }
            0
        }
        WM_EDIT_MOUSE => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE；鼠标进了输入框，清掉列表的悬停行并重绘一次。
                unsafe { (*app).on_mouse_leave() };
            }
            0
        }
        WM_COMMAND => {
            // HIWORD(wparam) 是通知码，LOWORD(wparam) 是控件 id（这里输入框 id 为 0）。
            let notification = ((wparam >> 16) & 0xFFFF) as u32;
            if !app.is_null() && notification == EN_CHANGE {
                // SAFETY: lparam 是发通知的控件句柄，on_edit_change 会核对它是不是我们的输入框。
                unsafe { (*app).on_edit_change(lparam as HWND) };
                return 0;
            }
            // SAFETY: 默认窗口过程处理其余通知，参数均来自系统。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CTLCOLOREDIT => {
            if !app.is_null() {
                // SAFETY: wparam 是输入框要用的 HDC，只在本消息处理期间有效；
                // paint_edit_background 只对它设颜色并返回本进程的刷子句柄。
                let brush = unsafe { (*app).paint_edit_background(wparam as HDC) };
                if brush != 0 {
                    return brush;
                }
            }
            // SAFETY: 没有刷子时交回默认过程。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_MOUSEMOVE => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE，单线程访问同一个 App；lparam 是客户区鼠标坐标。
                unsafe { (*app).on_mouse_move(lparam) };
            }
            0
        }
        // 顶层窗口自己拿到键盘焦点（外部 SetForegroundWindow、面板关闭后系统把焦点退回父窗口）：
        // 面板没开就把焦点交回输入框，输入与按键转发都靠它；面板开着则留给顶层窗口。
        WM_SETFOCUS => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE，单线程访问同一个 App；这里只改本线程的焦点窗口。
                unsafe { (*app).on_set_focus() };
            }
            0
        }
        WM_MOUSELEAVE => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE，单线程访问同一个 App；这里只清悬停行并重绘一次。
                unsafe { (*app).on_mouse_leave() };
            }
            0
        }
        WM_LBUTTONDOWN => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE；命中测试只读几何与列表状态，随后把焦点还给输入框。
                unsafe { (*app).on_left_click(lparam) };
            }
            0
        }
        WM_LBUTTONDBLCLK => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE；双击命中行就直接切换窗口（与回车同一条路径）。
                unsafe { (*app).on_left_double_click(lparam) };
            }
            0
        }
        WM_MOUSEWHEEL => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE；wparam 的高字是带符号的滚动增量（一格 120）。
                unsafe { (*app).on_wheel(((wparam >> 16) & 0xFFFF) as u16 as i16 as i32) };
            }
            0
        }
        // TranslateMessage 会把按键翻成 WM_CHAR / WM_SYSCHAR 投给本窗口——面板打开时焦点在父
        // 窗口上，就是这一类；输入框隐藏 / 焦点交接的短暂间隙里也可能落在这里。本窗口自己没有任何
        // 文本输入（文字全部进 EDIT 子窗口），这些字符消息没有消费者，交回默认过程只会响一声
        // 系统默认提示音（Alt+字母查不到菜单助记符时的「咚」），所以一律吞掉。
        WM_CHAR | WM_SYSCHAR => 0,
        WM_TIMER if wparam == POLL_TIMER_ID => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE；定时器消息只在窗口线程派发，轮询里会读写列表与关闭中集合。
                unsafe { (*app).poll_closing() };
            }
            0
        }
        // Alt 组合键（Alt+X）走 WM_SYSKEYDOWN，与 WM_KEYDOWN 走同一套处理；
        // 未消费的键（如 Alt+F4）照常交回默认过程。
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE，单线程访问同一个 App；on_key_down 返回是否消费了这个键。
                if unsafe { (*app).on_key_down(wparam as u32) } {
                    return 0;
                }
            }
            // SAFETY: 未处理的消息原样交回默认过程，参数均来自系统。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_KEYUP => {
            if !app.is_null() {
                // SAFETY: 同 WM_TOGGLE；焦点不在输入框时，回车抬起也要能触发切换。
                if unsafe { (*app).on_key_up(wparam as u32) } {
                    return 0;
                }
            }
            // SAFETY: 未处理的消息原样交回默认过程，参数均来自系统。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DESTROY => {
            if !app.is_null() {
                // SAFETY: app 仍指向有效的 Box<App>；在窗口线程、窗口句柄失效之前释放
                // 输入框、字体、背景刷与 D2D / DWrite 对象，避免资源跨生命周期残留。
                unsafe { (*app).release_resources() };
            }
            // SAFETY: 结束本线程的消息循环；App 的 Box 在 run 里消息循环结束后回收。
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => {
            // F1：系统在按键**按下**时就会为它发 `WM_HELP`，浮层没有任何帮助内容，
            // `DefWindowProcW` 查不到就响一声系统默认提示音；这里直接吞掉。
            if msg == WM_HELP {
                return 0;
            }
            // SAFETY: 默认窗口过程处理其余消息，参数均来自系统。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}
