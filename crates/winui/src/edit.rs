//! 原生单行输入框（`EDIT`）：创建、字体、占位文案、子类化与按键转发。
//!
//! 参数：无（模块级说明）
//! 核心流程：[`SearchEdit::create`] 在浮层窗口里建一个 `WS_CHILD` 的 EDIT → 用
//! `SPI_GETNONCLIENTMETRICS` 的 `lfMessageFont` 按 DPI 缩放后 `CreateFontIndirectW` 建字体并
//! `WM_SETFONT` → 用 `GWLP_WNDPROC` + `CallWindowProcW` 子类化，把导航键转发给父窗口
//! （`WM_APP + 2`），其余消息一律交回 EDIT 自己的过程，中文 IME 的 `WM_IME_*` / `WM_CHAR`
//! 因此完全不受影响。
//!
//! 占位文案（S9）为什么不再用 `EM_SETCUEBANNER`：那条消息的 cue 文字颜色跟随系统视觉样式，
//! `WM_CTLCOLOREDIT` 改不动它，深色卡片上会变成「深灰字压深色底」看不清。所以这里只负责
//! 输入本身，空输入框时的占位文案由 `render` 用同一套配色的次要文字自己画（见
//! [`SEARCH_HINT`] 与 `render::paint_edit_hint`），深浅两套主题下颜色都可控。
//!
//! 为什么选 `GWLP_WNDPROC` 而不是 `SetWindowSubclass`：后者要链接 comctl32 并维护
//! `dwRefData` 与 `RemoveWindowSubclass` 的配对，好处只是能挂多个子类过程。这里全局只有一个
//! 输入框，只需要"换掉原过程、再调回原过程"这一件事，`CallWindowProcW` 足够，也省掉一份
//! 依赖与一处容易漏写的注销路径。

use std::ffi::c_void;
use std::mem::{size_of, transmute, zeroed};
use std::ptr;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateFontIndirectW, DeleteObject, HFONT, LOGFONTW};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_MENU, VK_RETURN, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetParent,
    GetWindowTextLengthW, GetWindowTextW, MoveWindow, SendMessageW, SetWindowLongPtrW,
    SetWindowTextW, ShowWindow, SystemParametersInfoW, ES_AUTOHSCROLL, GWLP_WNDPROC,
    NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SW_HIDE, SW_SHOW, WM_APP, WM_KEYDOWN, WM_KEYUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_SETFONT, WM_SYSKEYDOWN, WNDPROC, WS_CHILD, WS_VISIBLE,
};

use crate::layout;

/// 输入框把「导航键」转给父窗口用的自定义消息：`wparam` 是虚拟键码，`lparam` 是原始 lparam。
/// 只在按键**按下**时投递，抬起走 [`WM_EDIT_KEYUP`]。
pub const WM_EDIT_KEY: u32 = WM_APP + 2;

/// 输入框把「按键抬起」转给父窗口用的自定义消息：回车抬起才触发切换，所以要能和按下分开。
pub const WM_EDIT_KEYUP: u32 = WM_APP + 4;

/// 输入框把滚轮转给父窗口用的自定义消息：`wparam` / `lparam` 原样保留 `WM_MOUSEWHEEL` 的值
/// （高字是带符号的滚动增量）。
///
/// 这里用 `WM_APP + 8` 而不是早期的 `WM_APP + 5`：`WM_APP + 5` 已经留给托盘（S11，见
/// `app::WM_TRAY`），同一条消息号不能既表示「滚轮」又表示「托盘命令到了」。
pub const WM_EDIT_WHEEL: u32 = WM_APP + 8;

/// 输入框把「鼠标进到自己身上」通知父窗口用的自定义消息：父窗口据此清掉列表的悬停行
/// （子控件的鼠标消息不会冒泡给父窗口，不清的话光标移到输入框后悬停底色会留在原行）。
pub const WM_EDIT_MOUSE: u32 = WM_APP + 6;

/// 输入框把 IME 组字开始 / 结束通知父窗口用的自定义消息：`wparam` 非 0 表示开始组字。
/// 父窗口据此暂时不画自绘占位文案——组字串就画在空输入框的同一块位置上，两者会叠在一起。
pub const WM_EDIT_IME: u32 = WM_APP + 7;

/// `WM_IME_STARTCOMPOSITION`：IME 开始组字（windows-sys 把它放在未开启的 `Win32_UI_Input_Ime`
/// feature 下，这里按 WinUser.h 的取值自定义；只拿它当消息号比较）。
const WM_IME_STARTCOMPOSITION: u32 = 0x010D;
/// `WM_IME_ENDCOMPOSITION`：IME 结束组字（见 [`WM_IME_STARTCOMPOSITION`]）。
const WM_IME_ENDCOMPOSITION: u32 = 0x010E;

/// Ctrl+, 里的逗号：`VK_OEM_COMMA`（主键盘逗号，不是小键盘的 `VK_SEPARATOR`）。
const VK_COMMA: u32 = 0xBC;
/// Ctrl+T 里的 `T`：Win32 里字母键的虚拟键码等于该字母的 ASCII 码。
const VK_T: u32 = b'T' as u32;
/// `EM_SETSEL`：选中字符区间，`wparam=0, lparam=-1` 表示全选。
const EM_SETSEL: u32 = 0xB1;
/// `ES_LEFT`：左对齐。SDK 里它的值就是 0，写出来是为了让样式一眼可读。
const ES_LEFT: u32 = 0x0000;
/// 占位文案：说明可用「进程.标题」复合查询与拼音首字母。输入框为空时由渲染器自己画
/// （见模块文档里对 `EM_SETCUEBANNER` 的说明）。
pub const SEARCH_HINT: &str = "搜索窗口，可用「进程.标题」或拼音首字母";

/// EDIT 原本的窗口过程：子类化时由 `SetWindowLongPtrW` 返回，存在这里供
/// [`edit_proc`] 调回。全局只有一个输入框，所以一个静态槽位就够；值为 0 表示还没子类化。
static PREVIOUS_EDIT_PROC: AtomicIsize = AtomicIsize::new(0);

/// 浮层里的那个单行输入框。
///
/// 参数：`hwnd` EDIT 窗口句柄；`font` 用 `lfMessageFont` 造出来的字体（`WM_SETFONT` 之后
/// 归这个控件使用，但句柄仍由本结构体负责删除）；`scale` 造这个字体时用的 DPI 缩放。
/// 核心流程：由 [`SearchEdit::create`] 建好后随 `App` 一起活到 `WM_DESTROY`，Drop 时
/// `DestroyWindow` 销毁子窗口、`DeleteObject` 删除字体，两条释放路径都会执行且只执行一次；
/// 浮层换到不同 DPI 的监视器时由 [`SearchEdit::ensure_scale`] 重建字体。
pub struct SearchEdit {
    hwnd: HWND,
    font: HFONT,
    scale: f32,
}

impl SearchEdit {
    /// 参数：`parent` 浮层窗口；`scale` DPI 缩放（1.0 = 96 DPI），用于把 DIP 尺寸换算成物理像素
    /// 并缩放字体。
    /// 核心流程：`CreateWindowExW("EDIT")` 建子窗口（`WS_CHILD | WS_VISIBLE | ES_LEFT |
    /// ES_AUTOHSCROLL`，无边框所以背景就是卡片底色）→ 造字体并 `WM_SETFONT` →
    /// `SetWindowLongPtrW(GWLP_WNDPROC)` 换成 [`edit_proc`] 并记下原过程。
    /// 任一步失败都把已创建的句柄释放掉再返回中文错误串。
    pub fn create(parent: HWND, scale: f32) -> Result<Self, String> {
        let class = wide("EDIT");
        let empty = wide("");
        // SAFETY: GetModuleHandleW(None) 返回本进程模块句柄，不转移所有权，进程存活期间始终有效；
        // 这里只用于填 CreateWindowExW 的 hInstance。
        let instance = unsafe { GetModuleHandleW(ptr::null()) };

        let style = WS_CHILD | WS_VISIBLE | ES_LEFT | ES_AUTOHSCROLL as u32;
        // 初始尺寸随便给，显示前一定会 MoveWindow 到 layout 算出来的位置。
        let initial_height = (layout::EDIT_HEIGHT * scale).round() as i32;
        // SAFETY: 所有指针参数都指向调用期间有效的 NUL 结尾 UTF-16 缓冲（class / empty）；
        // 父窗口是本进程刚创建的浮层窗口，菜单与创建参数传空。
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                empty.as_ptr(),
                style,
                0,
                0,
                100,
                initial_height,
                parent,
                ptr::null_mut(),
                instance,
                ptr::null(),
            )
        };
        if hwnd.is_null() {
            // SAFETY: 紧邻 CreateWindowExW 失败之后读 last-error，中间无其它 Win32 调用。
            let code = unsafe { GetLastError() };
            return Err(format!("CreateWindowExW(EDIT) 失败，错误码 {code}"));
        }

        let font = match create_message_font(scale) {
            Ok(font) => font,
            Err(err) => {
                // SAFETY: hwnd 是上面刚创建、尚未交给别人的子窗口句柄，这里创建失败只销毁一次。
                unsafe { DestroyWindow(hwnd) };
                return Err(err);
            }
        };

        // 第 3 个参数是字体句柄，第 4 个参数 MAKELPARAM(1, 0) 表示立刻重绘；字体句柄始终由
        // SearchEdit 持有（Drop 里删除），控件只是使用它。
        // SAFETY: WM_SETFONT 只读取字体句柄，hwnd 有效、font 在 SearchEdit 存活期间不会被删。
        unsafe {
            SendMessageW(hwnd, WM_SETFONT, font as usize, 1);
        }

        // 子类化：把窗口过程换成 edit_proc，并记下 EDIT 自己的过程。
        let new_proc = edit_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
        // SAFETY: SetWindowLongPtrW 只改本窗口的 WNDPROC 槽并返回旧值；函数指针来自本模块的
        // unsafe extern "system" 过程，签名与 WNDPROC 完全一致。
        let previous = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, new_proc as usize as isize) };
        PREVIOUS_EDIT_PROC.store(previous, Ordering::Relaxed);

        log::info!(
            "输入框已创建（DPI 缩放 {}%，字体已按 lfMessageFont 缩放）",
            layout::scale_percent(scale)
        );
        Ok(Self { hwnd, font, scale })
    }

    /// 参数：`scale` 本次显示时实际使用的 DPI 缩放
    /// 核心流程：与建字体时的缩放一致（差小于 0.01）就直接返回；否则按新缩放重建字体 →
    /// `WM_SETFONT` 让控件按新字号重排 → 删除旧字体。重建失败只记警告，继续用旧字体，
    /// 不影响输入。
    pub fn ensure_scale(&mut self, scale: f32) {
        if (self.scale - scale).abs() < 0.01 {
            return;
        }
        match create_message_font(scale) {
            Ok(font) => {
                let old = std::mem::replace(&mut self.font, font);
                // SAFETY: WM_SETFONT 只读新字体句柄；新字体由 self.font 持有、存活到 Drop；
                // 旧字体在控件换上新字体之后才删除，不会被引用。
                unsafe {
                    SendMessageW(self.hwnd, WM_SETFONT, self.font as usize, 1);
                    DeleteObject(old);
                }
                self.scale = scale;
                log::debug!(
                    "输入框字体已按 DPI 缩放 {}% 重建",
                    layout::scale_percent(scale)
                );
            }
            Err(err) => log::warn!(
                "按 DPI 缩放 {}% 重建字体失败，沿用旧字体：{err}",
                layout::scale_percent(scale)
            ),
        }
    }

    /// 参数：无
    /// 核心流程：返回 EDIT 句柄，供父窗口比对 `WM_COMMAND` 的 `lparam`。
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// 参数：`text` 新的输入文本
    /// 核心流程：转成 NUL 结尾的 UTF-16 后 `SetWindowTextW`；这会同步触发一次 `EN_CHANGE`，
    /// 调用方需要时用 `App` 里的抑制标志忽略它。
    pub fn set_text(&self, text: &str) {
        let buffer = wide(text);
        // SAFETY: SetWindowTextW 只读取本调用期间有效的 NUL 结尾字符串。
        unsafe {
            SetWindowTextW(self.hwnd, buffer.as_ptr());
        }
    }

    /// 参数：无
    /// 核心流程：`GetWindowTextLengthW` 取长度 → 按长度 +1 分配缓冲 → `GetWindowTextW` 抄回 →
    /// 按实际写入的长度截断后转 `String`（非法 UTF-16 用有损转换，绝不 panic）。
    pub fn text(&self) -> String {
        // SAFETY: 只读查询，返回当前文本的字符数（不含结尾 NUL）。
        let len = unsafe { GetWindowTextLengthW(self.hwnd) };
        let capacity = len.max(0) as usize + 1;
        let mut buffer = vec![0u16; capacity];
        // SAFETY: GetWindowTextW 最多写 capacity 个字符进 buffer（含结尾 NUL），
        // buffer 的容量正好是这个值。
        let copied = unsafe { GetWindowTextW(self.hwnd, buffer.as_mut_ptr(), capacity as i32) };
        buffer.truncate(copied.max(0) as usize);
        String::from_utf16_lossy(&buffer)
    }

    /// 参数：无
    /// 核心流程：`GetWindowTextLengthW` 取字符数，为 0 就是空——空的时候渲染器才自己画占位文案
    /// （用户一开始打字，占位文案就随下一次重绘消失）。
    pub fn is_empty(&self) -> bool {
        // SAFETY: 只读查询，返回当前文本的字符数（不含结尾 NUL）。
        unsafe { GetWindowTextLengthW(self.hwnd) <= 0 }
    }

    /// 参数：`left` / `top` 相对父窗口客户区的左上角（物理像素）；`width` / `height` 物理像素尺寸
    /// 核心流程：`MoveWindow`（`bRepaint=TRUE`）把输入框挪到卡片顶部。
    pub fn place(&self, left: i32, top: i32, width: i32, height: i32) {
        // SAFETY: MoveWindow 只改本进程子窗口的位置与大小，句柄有效。
        unsafe {
            MoveWindow(self.hwnd, left, top, width, height, 1);
        }
    }

    /// 参数：`visible` 为真显示、为假隐藏
    /// 核心流程：`ShowWindow(SW_SHOW/SW_HIDE)`；隐藏后 EDIT 不再占位，也不会吃鼠标。
    pub fn set_visible(&self, visible: bool) {
        // SAFETY: 只改本进程子窗口的可见性，不涉及句柄所有权。
        unsafe {
            ShowWindow(self.hwnd, if visible { SW_SHOW } else { SW_HIDE });
        }
    }

    /// 参数：无
    /// 核心流程：先 `SetFocus` 把键盘焦点交给 EDIT（这样输入法才会挂到这个控件上），
    /// 再 `EM_SETSEL(0, -1)` 全选，用户直接打字就是覆盖旧查询。
    pub fn focus_and_select_all(&self) {
        // SAFETY: 两个调用都只作用于本进程窗口：SetFocus 改本线程的焦点窗口，
        // EM_SETSEL 只改 EDIT 的选区。
        unsafe {
            SetFocus(self.hwnd);
            SendMessageW(self.hwnd, EM_SETSEL, 0, -1);
        }
    }
}

/// 参数：无
/// 核心流程：先 `DestroyWindow` 销毁子窗口（之后 EDIT 不会再收到任何消息），再
/// `DeleteObject` 删除字体，并把静态槽位里的原过程清零。
impl Drop for SearchEdit {
    fn drop(&mut self) {
        // SAFETY: hwnd 与 font 都由 SearchEdit::create 成功创建且只在这里释放；
        // 先销毁窗口再删字体，避免控件在销毁过程中还引用即将被删除的字体。
        unsafe {
            DestroyWindow(self.hwnd);
            DeleteObject(self.font);
        }
        PREVIOUS_EDIT_PROC.store(0, Ordering::Relaxed);
        log::debug!("输入框与字体已释放");
    }
}

/// 参数：`scale` DPI 缩放
/// 核心流程：`SystemParametersInfoW(SPI_GETNONCLIENTMETRICS)` 取 `lfMessageFont`（资源管理器
/// 里消息框用的那套系统字体）→ 把 `lfHeight` 按 `scale` 缩放（负值表示字符高度）→
/// `CreateFontIndirectW` 建字体。取不到系统参数时返回错误串，由调用方决定是否放弃输入框。
///
/// 缩放口径说明：本项目没有声明 DPI 感知（没有清单里的 `dpiAware`），系统会按虚拟化坐标
/// 上报参数，`lfMessageFont.lfHeight` 因此是 96 DPI 下的像素高度；渲染端用 `96 × scale` 的
/// DPI 画布，所以这里的字号同样乘 `scale`，两边的字才会一样大。
fn create_message_font(scale: f32) -> Result<HFONT, String> {
    // SAFETY: NONCLIENTMETRICSW 是纯 POD，全零是合法初值；cbSize 随后按结构体大小填好。
    let mut metrics: NONCLIENTMETRICSW = unsafe { zeroed() };
    metrics.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;

    // SAFETY: SystemParametersInfoW 只把系统参数写进 metrics（容量已由 cbSize 声明），
    // 不读其它指针；第 4 个参数 0 表示只查询、不写回用户配置。
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            &mut metrics as *mut NONCLIENTMETRICSW as *mut c_void,
            0,
        )
    };
    if ok == 0 {
        // SAFETY: 紧邻 SystemParametersInfoW 失败之后读 last-error。
        let code = unsafe { GetLastError() };
        return Err(format!("SystemParametersInfoW 失败，错误码 {code}"));
    }

    let mut logfont: LOGFONTW = metrics.lfMessageFont;
    logfont.lfHeight = ((logfont.lfHeight as f32) * scale).round() as i32;

    // SAFETY: CreateFontIndirectW 只读取 logfont（栈上的 POD，字段与系统返回值一致），
    // 返回的 HFONT 由调用方负责 DeleteObject。
    let font = unsafe { CreateFontIndirectW(&logfont) };
    if font.is_null() {
        return Err("CreateFontIndirectW 失败".to_string());
    }
    Ok(font)
}

/// 输入框的窗口过程（子类化后接管所有发往 EDIT 的消息）。
///
/// 参数：`hwnd` EDIT 句柄；`msg` 消息号；`wparam` / `lparam` 消息参数。
/// 核心流程：只挑五种情况自己处理——`WM_KEYDOWN` 里的方向键 / Tab / 回车 / Esc（外加
/// `Ctrl+Q`、`Ctrl+W`、`Alt+X`、`Ctrl+T`、`Ctrl+,`）、`WM_KEYUP` 的回车（切换只在抬起时执行）、
/// `WM_MOUSEWHEEL` 的滚轮、`WM_MOUSEMOVE`（清悬停行）与 IME 组字开始 / 结束（父窗口据此暂停自绘
/// 占位文案）——统一用 `SendMessageW(GetParent(hwnd), 自定义消息, ...)` 转给父窗口（按下走
/// [`WM_EDIT_KEY`]、抬起走 [`WM_EDIT_KEYUP`]、滚轮走 [`WM_EDIT_WHEEL`]），并 `return 0` 阻止
/// EDIT 自己再处理一遍；其余消息（含 `WM_CHAR`、`WM_IME_*`、`WM_PASTE` 等）原样交回 EDIT
/// 的默认过程，中文输入法与编辑行为因此完全不受影响。
///
/// 为什么要拦滚轮：单行 EDIT 会把 `WM_MOUSEWHEEL` 当成自己的横向滚动吃掉，不转发的话
/// 焦点在输入框里时列表就永远滚不动。
unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let vk = wparam as u32;
    match msg {
        // Alt 组合键走 WM_SYSKEYDOWN（Alt+X 就是这一类），所以两个键盘消息都要看。
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if is_forwarded_key(vk) {
                forward_to_parent(hwnd, WM_EDIT_KEY, wparam, lparam);
                return 0;
            }
        }
        WM_KEYUP if vk == VK_RETURN as u32 => {
            forward_to_parent(hwnd, WM_EDIT_KEYUP, wparam, lparam);
            return 0;
        }
        WM_MOUSEWHEEL => {
            forward_to_parent(hwnd, WM_EDIT_WHEEL, wparam, lparam);
            return 0;
        }
        WM_MOUSEMOVE => {
            // 鼠标在输入框里移动：父窗口据此清掉列表的悬停底色（每次移动都通知，但父窗口
            // 只在「原本有悬停行」时才重绘一次，之后就一直是无悬停状态）。
            forward_to_parent(hwnd, WM_EDIT_MOUSE, wparam, lparam);
        }
        WM_IME_STARTCOMPOSITION => forward_to_parent(hwnd, WM_EDIT_IME, 1, 0),
        WM_IME_ENDCOMPOSITION => forward_to_parent(hwnd, WM_EDIT_IME, 0, 0),
        _ => {}
    }

    let raw = PREVIOUS_EDIT_PROC.load(Ordering::Relaxed);
    if raw == 0 {
        // 理论上不会发生（子类化与记录原过程是同一步），兜底走默认过程。
        // SAFETY: 未处理的消息原样交回默认过程，参数均来自系统。
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    // SAFETY: raw 是 SetWindowLongPtrW 返回的、EDIT 原本的 WNDPROC，非零且指向 user32 里的
    // 真实过程；把它还原成函数指针后由 CallWindowProcW 调用，调用约定一致。
    let previous: WNDPROC = Some(unsafe {
        transmute::<isize, unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>(raw)
    });
    // SAFETY: 回调 EDIT 自己的窗口过程，参数与收到的完全一致，期间 hwnd 有效。
    unsafe { CallWindowProcW(previous, hwnd, msg, wparam, lparam) }
}

/// 参数：`vk` 虚拟键码
/// 核心流程：判断这个键要不要交给父窗口——方向键 / Tab / 回车 / Esc 是列表导航；
/// `Ctrl+Q` 退出、`Ctrl+W` 关闭选中窗口、`Alt+X` 关闭选中窗口、`Ctrl+T` 切换明暗、`Ctrl+,`
/// 打开选项面板这几组组合键在焦点位于输入框时也必须能按，所以连同修饰键现状一起转发
/// （父窗口仍会复核修饰键是否真按着）。其它键一律返回 false，交给 EDIT 默认过程。
fn is_forwarded_key(vk: u32) -> bool {
    let navigation = vk == VK_DOWN as u32
        || vk == VK_UP as u32
        || vk == VK_TAB as u32
        || vk == VK_RETURN as u32
        || vk == VK_ESCAPE as u32;
    if navigation {
        return true;
    }

    // SAFETY: GetKeyState 是只读查询，不接触任何指针；最高位为 1 表示该键当前按下。
    let (ctrl_down, alt_down) = unsafe {
        (
            GetKeyState(VK_CONTROL as i32) < 0,
            GetKeyState(VK_MENU as i32) < 0,
        )
    };
    // Ctrl+, 打开 / 关闭选项面板，Ctrl+T 切换明暗：这两个组合在焦点位于输入框时也必须能按，
    // 由父窗口统一处理。
    if ctrl_down && (vk == VK_COMMA || vk == VK_T) {
        return true;
    }

    match vk {
        // Ctrl+Q 退出、Ctrl+W 关闭选中窗口：父窗口还会再复核一次 Ctrl。
        vk if vk == u32::from(b'Q') || vk == u32::from(b'W') => ctrl_down,
        // Alt+X 关闭选中窗口（与 Ctrl+W 等价）。
        vk if vk == u32::from(b'X') => alt_down,
        _ => false,
    }
}

/// 参数：`hwnd` 输入框；`msg` 转给父窗口的自定义消息号（[`WM_EDIT_KEY`] / [`WM_EDIT_KEYUP`] /
/// [`WM_EDIT_WHEEL`]）；`wparam` / `lparam` 原始消息参数
/// 核心流程：`GetParent` 拿到浮层窗口，`SendMessageW` 同步投递自定义消息，父窗口在同一线程里
/// 立即处理，处理完才回到输入框。
fn forward_to_parent(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) {
    // SAFETY: GetParent 只读父子关系；SendMessageW 是同线程同步调用，父窗口在本进程内且
    // 生命周期覆盖本次调用。
    unsafe {
        let parent = GetParent(hwnd);
        if !parent.is_null() {
            SendMessageW(parent, msg, wparam, lparam);
        }
    }
}

/// 参数：`text` 任意字符串
/// 核心流程：转成 NUL 结尾的 UTF-16 缓冲，供 Win32 的 `*W` API 使用。
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
