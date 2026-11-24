//! 浮层界面：整窗一张卡片（细边框 + 小圆角，明 / 暗两套配色由 [`palette`] 提供）→ 顶部一行搜索框与 `?` 按钮 → 下面直接
//! 是紧凑的窗口列表，界面里不出现任何状态行、命中统计、耗时或快捷键说明。
//!
//! 参数：`cc` eframe 创建上下文；`single_instance` 由 `main` 抢占到的单实例结果
//! 核心流程：热键回调只置位共享标志并记下触发时刻 → [`eframe::App::logic`] 消费标志翻转
//! 期望可见状态，加载与显示统一由 [`SwitcherooApp::sync_window_visibility`] 在「从隐藏变
//! 可见」的那一帧完成：**先**取前台窗口、再取真实窗口快照，用 core 的 `move_foreground_to_end`
//! 把唤起前的前台窗口移到末尾并记下它的进程名 → 再居中 → 最后下发 `Visible(true)`；
//! 此外启动时先加载一次，窗口若提前可见也不会是空列表 → 查询或快照一变就用 core 的
//! `rank_windows_with_foreground` 重算一次排名（`进程.标题` 复合查询靠它拿到
//! 唤起瞬间的前台进程）→ [`eframe::App::ui`] 在渲染输入框之前消费 Tab / 方向键 / 回车 /
//! 关闭键，按排名顺序画列表（标题与进程名按命中区间加粗）并把选中项滚入可视区，
//! 回车松开或双击切换；`Ctrl+W` / `Alt+X` 只投递关闭请求，再由
//! [`SwitcherooApp::poll_closing`] 每 125ms 轮询确认。列表左侧图标由
//! [`SwitcherooApp::icons`] 在后台线程异步取回（US-044），取回前继续画占位方块。
//! 失败原因只以红字形式出现在搜索框上方，且仅在真的出错时才占一行。
//! US-046/047/048：启动时读配置决定热键 / 主题 / 自启，选项面板（`Ctrl+,`）与托盘菜单改完之后
//! 统一落盘；托盘命令经 `std::sync::mpsc` 送回 UI 线程，`logic` 非阻塞收割后立即生效。

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::text::LayoutJob;
use switcheroo_core::{
    config_path, move_foreground_to_end, rank_windows_with_foreground, save_settings,
    HotkeySetting, RankedWindow, Selection, Settings, ThemeSetting, WindowId, WindowInfo, MOD_ALT,
    MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};
use switcheroo_platform_win::close::CloseOutcome;
use switcheroo_platform_win::{
    disable, enable, foreground_window, is_enabled, is_system_dark, is_window_alive, request_close,
    snapshot, switch_to, HotkeyListener, HotkeySpec, SingleInstance, SingleInstanceError, Tray,
    TrayCommand,
};

/// 列表单行高度（逻辑像素）：图标、标题、进程名共用同一行高，行内不留额外间距。
const ROW_HEIGHT: f32 = 30.0;
/// 行内左右 padding，所有行保持一致。
const ROW_PADDING_X: f32 = 8.0;
/// 左侧图标方框边长：真图标等比缩放到这个方框内，取不到图标时画同尺寸的占位方块。
const ICON_SIZE: f32 = 16.0;
/// 图标与标题之间的固定列间距。
const COLUMN_GAP: f32 = 8.0;
/// 标题字号。
const TITLE_SIZE: f32 = 13.0;
/// 进程名字号。
const PROCESS_SIZE: f32 = 11.0;
/// 卡片与窗口边缘之间的透明留白：留出来才能看出圆角与边框。
const CARD_OUTER_MARGIN: i8 = 6;
/// 卡片内边距。
const CARD_INNER_MARGIN: i8 = 10;
/// 卡片圆角。
const CARD_RADIUS: u8 = 6;
/// 行底色圆角：比卡片再小一档，选中块看起来才贴着行。
const ROW_RADIUS: u8 = 4;
/// 搜索框与列表之间的间距。
const SEARCH_LIST_GAP: f32 = 8.0;
/// `?` 按钮的方形尺寸。
const HELP_BUTTON_SIZE: f32 = 22.0;
/// 关闭轮询间隔（毫秒）：投递 `WM_CLOSE` 之后每 125ms 问一次窗口是否还活着。
const CLOSE_POLL_MS: u64 = 125;
/// 空快照兜底重试的延迟（毫秒）：刚开机 / 刚登录时窗口枚举可能还没稳定。
const EMPTY_SNAPSHOT_RETRY_MS: u64 = 250;
/// 空快照最多重试几次：两次足够跨过登录后短暂的枚举空窗，又不会让用户干等。
const EMPTY_SNAPSHOT_RETRY_MAX: u8 = 2;
/// 居中的最大尝试帧数：首帧可能还拿不到监视器信息，试这么多帧后放弃、保持系统默认位置。
const CENTER_RETRY_MAX: u8 = 3;
/// 居中用的兜底窗口尺寸（逻辑点）：取不到真实 outer size 时按 main.rs 的 600×640 算。
const FALLBACK_WINDOW_SIZE: egui::Vec2 = egui::vec2(600.0, 640.0);

/// 关闭中真实图标的淡出 alpha：真图标不能像占位方块那样换颜色，只能整体调透明度。
const CLOSING_ICON_ALPHA: u8 = 96;
/// 托盘图标的悬停提示（同时也是托盘线程里窗口类的名字）。
const TRAY_TOOLTIP: &str = switcheroo_core::APP_NAME;
/// 录制态下热键框里的占位提示。
const RECORDING_HINT: &str = "按下新的组合键…（Esc 取消）";

/// 浮层的一套配色：明 / 暗两套只换颜色，卡片边框、圆角、行高与列宽由尺寸常量单独管。
///
/// 参数：无（纯数据，字段即颜色）
/// 核心流程：由 [`palette`] 按 `dark` 选出一套值，渲染代码只读字段、不关心当前是哪种主题；
/// 关闭中条目的两档淡色也放在这里，好让「正在关闭」的置灰在两套主题下都不留硬编码颜色。
#[derive(Clone, Copy)]
struct Palette {
    /// 卡片（浮层主体）底色。
    card_fill: egui::Color32,
    /// 卡片 1px 描边。
    card_stroke: egui::Color32,
    /// 搜索框底色。
    search_fill: egui::Color32,
    /// 搜索框 1px 描边。
    search_stroke: egui::Color32,
    /// 正文文字（窗口标题、帮助面板里的按键名）。
    text_primary: egui::Color32,
    /// 次要文字（进程名、占位文案、帮助说明）。
    text_muted: egui::Color32,
    /// 选中行填充。
    row_selected_fill: egui::Color32,
    /// 选中行 1px 描边。
    row_selected_stroke: egui::Color32,
    /// 鼠标悬停行填充。
    row_hover_fill: egui::Color32,
    /// 图标占位方块颜色（真图标还没取回来时画它）。
    icon_placeholder: egui::Color32,
    /// 失败提示文字颜色。
    error_text: egui::Color32,
    /// `?` 帮助面板底色。
    help_fill: egui::Color32,
    /// `?` 帮助面板 1px 描边。
    help_stroke: egui::Color32,
    /// 关闭中条目的标题与进程名颜色：比正常文字更淡，表示「正在关闭」。
    closing_text: egui::Color32,
    /// 关闭中条目的图标占位色：比正常图标再淡一档。
    closing_icon: egui::Color32,
}

/// 参数：`dark` 为真取深色一套，为假取浅色一套
/// 核心流程：两套值字段一一对应——卡片 / 描边 / 搜索框 / 正文 / 次要文字 / 选中 / 悬停 /
/// 图标占位 / 错误文字 / 帮助面板，外加「关闭中」的两档淡色；浅色沿用改造前的取值，
/// 因此不开深色模式时界面外观与改动前完全一致。
fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            card_fill: egui::Color32::from_rgb(0x20, 0x20, 0x20),
            card_stroke: egui::Color32::from_rgb(0x3A, 0x3A, 0x3A),
            search_fill: egui::Color32::from_rgb(0x1A, 0x1A, 0x1A),
            search_stroke: egui::Color32::from_rgb(0x3A, 0x3A, 0x3A),
            text_primary: egui::Color32::from_rgb(0xE8, 0xE8, 0xE8),
            text_muted: egui::Color32::from_rgb(0x90, 0x90, 0x90),
            row_selected_fill: egui::Color32::from_rgb(0x2C, 0x4A, 0x63),
            row_selected_stroke: egui::Color32::from_rgb(0x3E, 0x6B, 0x8F),
            row_hover_fill: egui::Color32::from_rgb(0x2A, 0x2A, 0x2A),
            icon_placeholder: egui::Color32::from_rgb(0x3C, 0x3C, 0x3C),
            error_text: egui::Color32::from_rgb(0xFF, 0x7B, 0x72),
            // 帮助面板比卡片亮一档：作为前景小卡片压在列表上时才分得清层次。
            help_fill: egui::Color32::from_rgb(0x26, 0x26, 0x26),
            help_stroke: egui::Color32::from_rgb(0x3A, 0x3A, 0x3A),
            closing_text: egui::Color32::from_rgb(0x5E, 0x5E, 0x5E),
            closing_icon: egui::Color32::from_rgb(0x2E, 0x2E, 0x2E),
        }
    } else {
        Palette {
            card_fill: egui::Color32::from_rgb(0xFF, 0xFF, 0xFF),
            card_stroke: egui::Color32::from_rgb(0xE0, 0xE0, 0xE0),
            search_fill: egui::Color32::from_rgb(0xFF, 0xFF, 0xFF),
            search_stroke: egui::Color32::from_rgb(0xE0, 0xE0, 0xE0),
            text_primary: egui::Color32::from_rgb(0x1F, 0x1F, 0x1F),
            text_muted: egui::Color32::from_rgb(0x80, 0x80, 0x80),
            row_selected_fill: egui::Color32::from_rgb(0xCC, 0xE4, 0xF7),
            row_selected_stroke: egui::Color32::from_rgb(0xA8, 0xCE, 0xEE),
            row_hover_fill: egui::Color32::from_rgb(0xF5, 0xF5, 0xF5),
            icon_placeholder: egui::Color32::from_rgb(0xDD, 0xDD, 0xDD),
            error_text: egui::Color32::from_rgb(0xB0, 0x1C, 0x1C),
            help_fill: egui::Color32::from_rgb(0xFF, 0xFF, 0xFF),
            help_stroke: egui::Color32::from_rgb(0xE0, 0xE0, 0xE0),
            closing_text: egui::Color32::from_rgb(0xA6, 0xA6, 0xA6),
            closing_icon: egui::Color32::from_rgb(0xEE, 0xEE, 0xEE),
        }
    }
}
/// 输入框的焦点锁：egui 在每帧开始时就按「焦点控件的 EventFilter」决定 Tab / 方向键要不要
/// 切换焦点，那一步早于本帧的 `consume_key`，所以这里声明 Tab 与方向键归浮层自己用。
const NAV_KEYS_LOCK: egui::EventFilter = egui::EventFilter {
    tab: true,
    horizontal_arrows: true,
    vertical_arrows: true,
    escape: false,
};

/// 参数：`theme` 主题偏好；`system_dark` 系统当前是否深色
/// 核心流程：`System` 跟随系统，`Light` / `Dark` 直接给出结果；纯函数，启动、面板与托盘共用。
fn dark_for(theme: ThemeSetting, system_dark: bool) -> bool {
    match theme {
        ThemeSetting::System => system_dark,
        ThemeSetting::Light => false,
        ThemeSetting::Dark => true,
    }
}

/// 参数：`theme` 当前主题偏好；`system_dark` 系统当前是否深色
/// 核心流程：托盘的「切换明暗」永远在浅 / 深之间翻面：`System` 先按系统实际观感落到一侧
/// 再翻转（`Light` → `Dark`、`Dark` → `Light`），于是切完就脱离了「跟随系统」。
fn toggled_theme(theme: ThemeSetting, system_dark: bool) -> ThemeSetting {
    if dark_for(theme, system_dark) {
        ThemeSetting::Light
    } else {
        ThemeSetting::Dark
    }
}

/// 参数：`theme` 主题偏好
/// 核心流程：给选项面板的三选一提供中文标签，三种取值与配置文件的 `system` / `light` /
/// `dark` 一一对应。
fn theme_name(theme: ThemeSetting) -> &'static str {
    match theme {
        ThemeSetting::System => "跟随系统",
        ThemeSetting::Light => "浅色",
        ThemeSetting::Dark => "深色",
    }
}

/// 参数：`key` egui 的逻辑按键
/// 核心流程：把 egui 按键折算成 Win32 虚拟键码（`RegisterHotKey` 要的就是它）——
/// 字母 A–Z、数字 0–9、功能键 F1–F24 三段连续区按查表下标换算成 `0x41` / `0x30` / `0x70`
/// 起始的键码，其余可录制的按键走一张定点表；修饰键本身以及 Copy / Cut / Paste 这类
/// 合成键返回 `None`（它们不能当全局热键）。
fn vk_code(key: egui::Key) -> Option<u32> {
    use egui::Key;

    const LETTERS: [egui::Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    const DIGITS: [egui::Key; 10] = [
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ];
    const FUNCTIONS: [egui::Key; 24] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
        Key::F13,
        Key::F14,
        Key::F15,
        Key::F16,
        Key::F17,
        Key::F18,
        Key::F19,
        Key::F20,
        Key::F21,
        Key::F22,
        Key::F23,
        Key::F24,
    ];

    if let Some(index) = LETTERS.iter().position(|item| *item == key) {
        return Some(0x41 + index as u32);
    }
    if let Some(index) = DIGITS.iter().position(|item| *item == key) {
        return Some(0x30 + index as u32);
    }
    if let Some(index) = FUNCTIONS.iter().position(|item| *item == key) {
        return Some(0x70 + index as u32);
    }

    match key {
        Key::Space => Some(0x20),
        Key::Tab => Some(0x09),
        Key::Enter => Some(0x0D),
        Key::Escape => Some(0x1B),
        Key::Backspace => Some(0x08),
        Key::Insert => Some(0x2D),
        Key::Delete => Some(0x2E),
        Key::Home => Some(0x24),
        Key::End => Some(0x23),
        Key::PageUp => Some(0x21),
        Key::PageDown => Some(0x22),
        Key::ArrowLeft => Some(0x25),
        Key::ArrowUp => Some(0x26),
        Key::ArrowRight => Some(0x27),
        Key::ArrowDown => Some(0x28),
        // 标点：带 Shift 的符号落到它的基准键上——VK 只描述物理键，Shift 由修饰键位体现。
        Key::Semicolon | Key::Colon => Some(0xBA),
        Key::Equals | Key::Plus => Some(0xBB),
        Key::Comma => Some(0xBC),
        Key::Minus => Some(0xBD),
        Key::Period => Some(0xBE),
        Key::Slash | Key::Questionmark => Some(0xBF),
        Key::Backtick => Some(0xC0),
        Key::OpenBracket | Key::OpenCurlyBracket => Some(0xDB),
        Key::Backslash | Key::Pipe => Some(0xDC),
        Key::CloseBracket | Key::CloseCurlyBracket => Some(0xDD),
        Key::Quote => Some(0xDE),
        Key::Exclamationmark => Some(0x31),
        _ => None,
    }
}

/// 参数：`vk` Win32 虚拟键码
/// 核心流程：给 [`vk_code`] 能产出的键码反查一个短名字（`Space` / `F5` / `A` / `0`）；
/// 查不到时返回 `None`，由 [`hotkey_text`] 退化成十六进制键码，不丢信息也不 panic。
fn key_label(vk: u32) -> Option<String> {
    // 字母 / 数字 / 功能键：直接按虚拟键码段反算，不用再写一张表。
    if (0x41..=0x5A).contains(&vk) || (0x30..=0x39).contains(&vk) {
        return Some(char::from(vk as u8).to_string());
    }
    if (0x70..=0x87).contains(&vk) {
        return Some(format!("F{}", vk - 0x70 + 1));
    }

    let label = match vk {
        0x20 => "Space",
        0x09 => "Tab",
        0x0D => "Enter",
        0x1B => "Esc",
        0x08 => "Backspace",
        0x2D => "Insert",
        0x2E => "Delete",
        0x24 => "Home",
        0x23 => "End",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0xBA => ";",
        0xBB => "=",
        0xBC => ",",
        0xBD => "-",
        0xBE => ".",
        0xBF => "/",
        0xC0 => "`",
        0xDB => "[",
        0xDC => "\\",
        0xDD => "]",
        0xDE => "'",
        _ => return None,
    };
    Some(label.to_string())
}

/// 参数：`setting` 一条组合键配置
/// 核心流程：按固定顺序 `Ctrl → Shift → Alt → Win → 主键` 拼成人类可读文本（例如
/// `Ctrl + Shift + Alt + Win + Space`），键名由 [`key_label`] 提供；未识别的键码退化成
/// `键码 0x2C` 这样的十六进制，界面上仍然看得出按了哪个键。
pub(crate) fn hotkey_text(setting: &HotkeySetting) -> String {
    const MODIFIERS: [(u32, &str); 4] = [
        (MOD_CONTROL, "Ctrl"),
        (MOD_SHIFT, "Shift"),
        (MOD_ALT, "Alt"),
        (MOD_WIN, "Win"),
    ];

    let mut text = String::new();
    for (bit, name) in MODIFIERS {
        if setting.modifiers & bit != 0 {
            text.push_str(name);
            text.push_str(" + ");
        }
    }
    match key_label(setting.key) {
        Some(label) => text.push_str(&label),
        None => text.push_str(&format!("键码 0x{:02X}", setting.key)),
    }
    text
}

/// 参数：`modifiers` egui 当时的修饰键状态；`super_down` Win（Super）键是否按下
/// 核心流程：把 egui 的布尔修饰键折成 Win32 的 `MOD_*` 位或；Win 键在 Windows 上不会进
/// `Modifiers`（`mac_cmd` 恒为 `false`），所以单独用 `super_down` 补一位。
fn modifier_bits(modifiers: egui::Modifiers, super_down: bool) -> u32 {
    let mut bits = 0;
    if modifiers.alt {
        bits |= MOD_ALT;
    }
    if modifiers.ctrl {
        bits |= MOD_CONTROL;
    }
    if modifiers.shift {
        bits |= MOD_SHIFT;
    }
    if modifiers.mac_cmd || super_down {
        bits |= MOD_WIN;
    }
    bits
}

/// 参数：`key` 按下的键；`modifiers` 当时的修饰键；`super_down` Win 键是否按下
/// 核心流程：先取虚拟键码、再取修饰键位，任一缺失（修饰键本身、或不带任何修饰键的普通键）
/// 就返回 `None`：一个光秃稚的字母做全局热键会把所有程序的打字都抢走，宁可不录。
fn hotkey_from_key(
    key: egui::Key,
    modifiers: egui::Modifiers,
    super_down: bool,
) -> Option<HotkeySetting> {
    let key = vk_code(key)?;
    let modifiers = modifier_bits(modifiers, super_down);
    if modifiers == 0 {
        return None;
    }
    Some(HotkeySetting { key, modifiers })
}

/// 参数：`key` egui 逻辑按键
/// 核心流程：判断是不是左 / 右 Win（Super）键——它们不以 `Modifiers` 形式上报，录制时
/// 靠它们自己的按下 / 松开事件维护 Win 位。
fn is_super_key(key: egui::Key) -> bool {
    matches!(key, egui::Key::SuperLeft | egui::Key::SuperRight)
}

/// 参数：`settings` 从配置文件读到的设置；`registered` 注册表里真实的自启状态
/// 核心流程：开机自启一律以注册表为准修正 `settings.autostart`，返回是否发生了修正；
/// 启动时据此决定要不要立刻把配置文件回写一次（用户手工删了 Run 键、或换了 exe 位置都会到这里）。
fn reconcile_autostart(settings: &mut Settings, registered: bool) -> bool {
    if settings.autostart == registered {
        return false;
    }
    settings.autostart = registered;
    true
}

/// 参数：`enable_hotkey` 配置里是否启用热键；`hotkey_ready` 热键是否真的注册成功；
/// `tray_ready` 托盘是否创建成功；`single_instance_error` 单实例是否出错
/// 核心流程：只在「没有任何唤起入口」时让浮层启动即显示——单实例失败（需要向用户解释）、
/// 热键本该可用却注册失败、以及热键被关掉又没有托盘，三种情况都不能把窗口藏在后台。
fn should_start_visible(
    enable_hotkey: bool,
    hotkey_ready: bool,
    tray_ready: bool,
    single_instance_error: bool,
) -> bool {
    if single_instance_error {
        return true;
    }
    if enable_hotkey && !hotkey_ready {
        return true;
    }
    !enable_hotkey && !tray_ready
}

/// 参数：`applied_visible` 上一次已经下发给窗口系统的可见状态（`None` 表示还没下发过）；
/// `visible` 本帧期望的可见状态
/// 核心流程：只有「上一次不是可见 / 从未下发过」且「本次要显示」才算一次**重新唤起**，此时
/// 应该丢弃上一次的筛选条件；窗口本来就可见（用户正在输入、选项面板切走再回来）或正在收起
/// 时都返回 false，保证同一次唤起内（含 Ctrl+W 关闭窗口后的刷新）过滤条件不被清掉。
fn should_reset_query(applied_visible: Option<bool>, visible: bool) -> bool {
    visible && applied_visible != Some(true)
}

/// 保存选项时热键这一步的处置方式。
///
/// 参数：无
/// 核心流程：把「试注册新组合键 / 继续用旧监听 / 释放旧监听」三种处置分开，保存流程据此决定
/// 要不要 spawn 新监听；判定本身是纯函数，所以「冲突时不落盘」这条路径可以脱离真机单测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotkeyPlan {
    /// 组合键没变且旧监听还在：不重注册（同一个组合键会撞上自己占着的那一个），只落盘。
    Keep,
    /// 组合键变了、或旧监听不在：先试注册新组合键。
    Register,
    /// 配置里关了热键：不注册，落盘后才释放旧监听。
    Release,
}

/// 试注册之后的结论。
///
/// 参数：无
/// 核心流程：只有「需要注册却注册失败」要回滚（不落盘 + 草稿退回旧值），其余情况一律允许落盘。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveDecision {
    /// 可以落盘。
    Persist,
    /// 不落盘，把草稿回滚到已保存的值。
    Rollback,
}

/// 参数：`current` 当前生效的设置；`next` 面板里的新设置；`listener_alive` 当前是否有监听在跑
/// 核心流程：不启用热键 → [`HotkeyPlan::Release`]；启用且组合键与当前一致、旧监听还在 →
/// [`HotkeyPlan::Keep`]（旧监听已经占着这个组合键，再注册会被判成「已被占用」）；其余
/// （组合键变了 / 旧监听不在）→ [`HotkeyPlan::Register`]。
fn plan_hotkey_rebind(current: &Settings, next: &Settings, listener_alive: bool) -> HotkeyPlan {
    if !next.enable_hotkey {
        return HotkeyPlan::Release;
    }
    if listener_alive && current.hotkey == next.hotkey {
        return HotkeyPlan::Keep;
    }
    HotkeyPlan::Register
}

/// 参数：`plan` 上面给出的处置方式；`registered` 试注册是否成功（`Keep` / `Release` 不注册，传
/// `true`）
/// 核心流程：只有「要注册却没注册上」这一种情况返回 [`SaveDecision::Rollback`]——于是配置文件
/// 不会被改成注册不上的组合键，旧监听也一直按旧键工作；其余情况都允许落盘。
fn decide_after_register(plan: HotkeyPlan, registered: bool) -> SaveDecision {
    match plan {
        HotkeyPlan::Register if !registered => SaveDecision::Rollback,
        _ => SaveDecision::Persist,
    }
}

/// 一次热键录制的判定结果。
///
/// 参数：无
/// 核心流程：把「继续等」/「用户放弃」/「录到组合键」三种结局分开，调用方据此决定是保留
/// 录制态、还是写回草稿并退出录制。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recorded {
    /// 这一帧没有能用的按键组合，继续等。
    Pending,
    /// 用户按了 Esc：放弃录制。
    Cancel,
    /// 录到了组合键。
    Captured(HotkeySetting),
}

/// 录制态的事件消费者。
///
/// 参数：`super_down` Win 键是否按下（egui 的 `Modifiers` 里没有 Windows 键）
/// 核心流程：只看 `Event::Key`——Super 键负责维护 `super_down`；按下 Esc 取消录制；单独按 Tab
/// 跳过（焦点照常移动）；其余按下事件用 [`hotkey_from_key`] 折算，折算不出来就继续等。
#[derive(Default)]
struct RecordInput {
    super_down: bool,
}

impl RecordInput {
    /// 参数：`events` 本帧的 egui 原始事件
    /// 核心流程：顺序扫描本帧事件，命中第一个可用组合就返回 `Captured`；遇到 Esc 立刻返回
    /// `Cancel`；一路没有可用按键则返回 `Pending`。只改自己的 `super_down`，不碰任何 UI 状态。
    fn feed(&mut self, events: &[egui::Event]) -> Recorded {
        for event in events {
            let egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } = event
            else {
                continue;
            };

            if is_super_key(*key) {
                self.super_down = *pressed;
                continue;
            }
            if !*pressed {
                continue;
            }
            if *key == egui::Key::Escape {
                return Recorded::Cancel;
            }
            // 单独按 Tab 应该移动焦点：既不录制，也不结束录制。
            if *key == egui::Key::Tab && modifiers.is_none() && !self.super_down {
                continue;
            }
            match hotkey_from_key(*key, *modifiers, self.super_down) {
                Some(setting) => return Recorded::Captured(setting),
                // 修饰键本身 / 不带修饰键的普通键：继续等用户把组合按全。
                None => log::debug!("录制中忽略按键 {key:?}（不足以构成全局热键）"),
            }
        }
        Recorded::Pending
    }
}

/// 选项面板里的一份草稿。
///
/// 参数：字段与 [`Settings`] 一一对应，外加只属于界面的录制状态
/// 核心流程：面板上的勾选 / 单选 / 录制都只改草稿，绝不直接动运行时；`确定` 才把草稿落到
/// [`SwitcherooApp::settings`] 并生效，`取消` 时草稿随面板一起丢掉，运行时状态保持不变。
struct OptionsDraft {
    /// 是否启用全局热键。
    enable_hotkey: bool,
    /// 草稿里的组合键（录制结果写这里）。
    hotkey: HotkeySetting,
    /// 是否开机自启。
    autostart: bool,
    /// 主题偏好。
    theme: ThemeSetting,
    /// 是否正在录制热键。
    recording: bool,
    /// 录制期间的事件消费者。
    record_input: RecordInput,
}

impl OptionsDraft {
    /// 参数：`settings` 当前生效的设置
    /// 核心流程：逐字段复制，录制状态一律从「未录制」开始，所以打开面板永远看到真实配置。
    fn new(settings: &Settings) -> Self {
        Self {
            enable_hotkey: settings.enable_hotkey,
            hotkey: settings.hotkey,
            autostart: settings.autostart,
            theme: settings.theme,
            recording: false,
            record_input: RecordInput::default(),
        }
    }

    /// 参数：无
    /// 核心流程：把草稿折回 [`Settings`]，面板里的界面状态（录制态等）不参与。
    fn to_settings(&self) -> Settings {
        Settings {
            enable_hotkey: self.enable_hotkey,
            hotkey: self.hotkey,
            autostart: self.autostart,
            theme: self.theme,
        }
    }
}

/// 选项面板底部两个按钮的点击结果。
///
/// 参数：无
/// 核心流程：先把点击记成小枚举，等草稿的可变借用于后结后再执行——保存 / 取消都要动整个
/// `self`，不能在一堆绑定到草稿的控件闭包里直接调。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionsAction {
    /// 写回设置并生效。
    Save,
    /// 丢弃草稿，回到已保存的设置。
    Cancel,
}

pub struct SwitcherooApp {
    /// 单实例句柄：所有权在这里，Drop 时关闭命名互斥量，进程退出后互斥量随之释放。
    single_instance: Option<SingleInstance>,
    /// 单实例获取失败的原因（fail-open 时非空），用于界面提示。
    single_instance_error: Option<String>,
    /// 热键监听：注册失败时为 `None`，Drop 时结束热键线程并 join。
    hotkey: Option<HotkeyListener>,
    /// 热键注册失败的原因，用于界面提示；成功时为 `None`。
    hotkey_error: Option<String>,
    /// 当前生效的设置：热键 / 自启 / 主题都从这里来，面板与托盘改完之后写回配置文件。
    settings: Settings,
    /// 选项面板是否打开：打开时 Esc 只关面板，列表按键与全局热键一律不响应。
    options_open: bool,
    /// 选项面板草稿；`None` 表示面板没开。
    draft: Option<OptionsDraft>,
    /// 托盘图标：`None` 表示托盘不可用（非 Windows 或创建失败），此时只靠热键。
    tray: Option<Tray>,
    /// 托盘命令通道的接收端：托盘线程 → UI 线程，`logic` 里非阻塞收割。
    tray_commands: Receiver<TrayCommand>,
    /// 保存配置失败的提示，和热键 / 单实例错误一样只在界面上占一行红字。
    save_error: Option<String>,
    /// 选项面板内部的失败提示（热键冲突、开机自启失败）：面板里居中显示成红字，面板没开时
    /// 与其它错误一样显示在搜索框上方——托盘切换自启失败也走这里，用户总能看见原因。
    options_error: Option<String>,
    /// 搜索框的控件 id：打开选项面板时要主动交还焦点，否则它那套「Tab 归浮层」的焦点锁
    /// 会继续生效，面板里的 Tab 就没法移动焦点了。
    search_focus: Option<egui::Id>,
    /// 热键线程与 UI 线程之间唯一的共享状态：`true` 表示「请求切换一次显示状态」。
    toggle_requested: Arc<AtomicBool>,
    /// 热键回调触发的时刻，是「首帧耗时」的计时起点：回调写入、`logic` 取走。
    hotkey_at: Arc<Mutex<Option<Instant>>>,
    /// 期望的窗口可见状态（`true` = 浮层显示）。
    visible: bool,
    /// 已经下发给窗口系统的可见状态，用来保证状态不变时不重复下发命令。
    applied_visible: Option<bool>,
    /// 本次显示还没居中成功：变可见时置位，成功下发 `OuterPosition` 后清除；
    /// 监视器信息首帧可能为空，所以之后的可见帧上还会继续重试（US-024）。
    center_pending: bool,
    /// 已尝试居中的帧数，超过 [`CENTER_RETRY_MAX`] 就放弃，避免一直空转重绘。
    center_tries: u8,
    /// 空快照兜底的剩余重试次数与下一次重试时刻（见 [`EMPTY_SNAPSHOT_RETRY_MS`]）。
    empty_retry_left: u8,
    empty_retry_due: Option<Instant>,
    /// 输入框内容：保留中文输入法检查（同时验证 Tab / 回车不会被它抢走）。
    query: String,
    /// 最近一次唤起时取到的真实窗口快照，列表直接渲染它；为空时显示占位文案。
    window_snapshot: Vec<WindowInfo>,
    /// US-041：唤起瞬间前台窗口的进程名。查询写成 `.标题`（`.` 前为空）时，
    /// core 的 `rank_windows_with_foreground` 用它补全进程片段；没有前台窗口时为 `None`。
    foreground_process: Option<String>,
    /// 当前查询串下的排名结果：列表顺序、行号到快照下标的映射都由它决定；
    /// 只在查询或快照变化时重算一次，绝不放进每帧渲染路径。
    ranked: Vec<RankedWindow>,
    /// 关闭中集合：`(窗口标识, 发起关闭的时刻)`。条目在界面上置灰，
    /// 由 `poll_closing` 每 125ms 轮询确认，窗口消失后从快照与这里一起移除。
    closing: Vec<(WindowId, Instant)>,
    /// 当前选中项，循环与越界收敛都由 core 的 `Selection` 负责。
    selection: Selection,
    /// 选中项是否需要在本次渲染中滚入可视区（键盘移动、刚唤起时置位）。
    scroll_to_selected: bool,
    /// 切换失败的提示文案；切换成功或下次唤起时清空。
    switch_error: Option<String>,
    /// 首帧计时的起点（热键回调 → 列表真正画出），只用来打日志，不上界面。
    first_frame_started: Option<Instant>,
    /// 快捷键帮助面板是否展开：`?` 按钮切换，默认关闭。
    show_help: bool,
    /// 当前是否深色配色：默认浅色，由 `Ctrl+T` 或 [`SwitcherooApp::set_dark`]（后续选项面板）改写。
    dark: bool,
    /// 本帧是否刚发起过隐藏：`hide` 会清空快照，`ui` 据此跳过本帧绘制，
    /// 避免同一帧里画出一帧「没有可切换的窗口」的空列表残影。
    skip_render_this_frame: bool,
    /// US-044：窗口图标的缓存与后台加载线程。渲染只读它，取图完全在另一个线程上。
    icons: crate::icons::IconCache,
}

/// 本帧需要处理的列表按键（已在输入队列里消费过）。
struct ListKeys {
    /// 下移一项。
    down: bool,
    /// 上移一项。
    up: bool,
    /// 回车在本帧被松开 —— 切换时机。
    enter_released: bool,
    /// `Ctrl+W` / `Alt+X`：关闭当前选中窗口，浮层不收起。
    close: bool,
}

impl SwitcherooApp {
    /// 参数：`cc` eframe 创建上下文；`single_instance` 单实例抢占结果（失败时带着原因）；
    /// `settings` `main` 从配置文件读到的设置
    /// 核心流程：注入中文字体 → **先以注册表为准修正自启状态并回写配置**（US-046）→ 按
    /// `enable_hotkey` 决定要不要按 `hotkey` 注册全局热键（回调只置位标志、记时间戳并请求重绘）
    /// → 创建托盘并把命令通道接到 UI 线程（创建失败只记 warn）→ 按 `theme`（`System` 时问
    /// `is_system_dark()`）定下 `dark` 并应用配色 → 初始化空列表与空选中项 → **启动即加载一次
    /// 窗口** → 可见性状态先留成 `None`，让首帧统一走一次 `sync_window_visibility`：该隐藏就
    /// 显式隐藏，该显示就先加载、先居中再显示。
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        single_instance: Result<SingleInstance, SingleInstanceError>,
        mut settings: Settings,
    ) -> Self {
        crate::fonts::install(&cc.egui_ctx);

        let (single_instance, single_instance_error) = match single_instance {
            Ok(instance) => (Some(instance), None),
            Err(err) => (None, Some(err.to_string())),
        };

        // US-046：自启以注册表真实状态为准。用户在任务管理器里关过自启、或换了 exe 路径，
        // 配置文件和实际状态就会打架；这里统一信注册表，并把差异回写配置文件。
        if reconcile_autostart(&mut settings, is_enabled()) {
            log::info!(
                "开机自启以注册表为准修正为 {}，回写配置 {}",
                settings.autostart,
                config_path().display()
            );
            if let Err(err) = save_settings(&settings) {
                log::warn!("开机自启修正后回写配置失败：{err}");
            }
        }

        let toggle_requested = Arc::new(AtomicBool::new(false));
        let hotkey_at = Arc::new(Mutex::new(None));
        // 热键：配置里关掉就一个都不注册（浮层仍可经托盘「打开」显示）。
        let (hotkey, hotkey_error) = if settings.enable_hotkey {
            Self::register_hotkey(&cc.egui_ctx, &toggle_requested, &hotkey_at, settings.hotkey)
        } else {
            log::info!("配置已关闭全局热键，本次启动不注册");
            (None, None)
        };

        // US-048：托盘命令经 channel 回到 UI 线程。回调里只投递 + 唤醒事件循环，
        // 不在这里碰任何 UI 或窗口命令（它在托盘线程上执行）。
        let (tray_sender, tray_commands) = std::sync::mpsc::channel::<TrayCommand>();
        let tray_ctx = cc.egui_ctx.clone();
        let tray = Tray::spawn(TRAY_TOOLTIP, settings.autostart, move |command| {
            if tray_sender.send(command).is_ok() {
                tray_ctx.request_repaint();
            }
        });
        if tray.is_none() {
            log::warn!(
                "托盘不可用（{} 平台或创建失败），本次只保留热键与选项面板",
                std::env::consts::OS
            );
        }

        // 热键本该可用却注册失败、或热键被关掉又没有托盘时，不能把窗口藏在后台：直接显示并提示。
        let visible = should_start_visible(
            settings.enable_hotkey,
            hotkey.is_some(),
            tray.is_some(),
            single_instance_error.is_some(),
        );
        let dark = dark_for(settings.theme, is_system_dark());

        let mut app = Self {
            single_instance,
            single_instance_error,
            hotkey,
            hotkey_error,
            settings,
            options_open: false,
            draft: None,
            tray,
            tray_commands,
            save_error: None,
            options_error: None,
            search_focus: None,
            toggle_requested,
            hotkey_at,
            visible,
            // 还没有向窗口系统下发过任何可见性命令：首帧据此显式下发一次（隐藏时也不会
            // 依赖 `with_visible(false)` 是否即时生效）。
            applied_visible: None,
            center_pending: false,
            center_tries: 0,
            empty_retry_left: 0,
            empty_retry_due: None,
            query: String::new(),
            window_snapshot: Vec::new(),
            foreground_process: None,
            ranked: Vec::new(),
            closing: Vec::new(),
            selection: Selection::new(0),
            scroll_to_selected: false,
            switch_error: None,
            first_frame_started: None,
            show_help: false,
            dark,
            skip_render_this_frame: false,
            icons: crate::icons::IconCache::new(),
        };

        // 主题在拿到 `dark` 字段后统一应用：明 / 暗各一套 Visuals，与系统主题无关。
        app.apply_theme(&cc.egui_ctx);

        // 启动即加载一次：窗口无论因何提前可见，列表里都已经有内容；此时若枚举为空，
        // 还会按 250ms 的节奏补两次（见 `load_windows` 里的兜底重试）。
        app.empty_retry_left = EMPTY_SNAPSHOT_RETRY_MAX;
        app.load_windows(&cc.egui_ctx);

        app
    }

    /// 参数：无（按 `dark` 字段取配色）
    /// 核心流程：渲染路径只调它一次——按当前主题挑出一套 [`Palette`]，绘制代码因此不用重复
    /// 判断明暗，也不会再散落硬编码颜色。
    fn palette(&self) -> Palette {
        palette(self.dark)
    }

    /// 参数：`ctx` egui 上下文（Visuals 直接灌进它的主题）
    /// 核心流程：按 `dark` 取 [`egui::Visuals::dark`] / [`egui::Visuals::light`] 作底 → 用
    /// [`Palette`] 覆盖卡片、窗口、输入框、选中与正文等关键色 → 先 `set_theme` 钉住当前主题，
    /// 再 `set_visuals_of` 灌进去，因此系统切明暗时浮层配色也不会被换掉。
    fn apply_theme(&self, ctx: &egui::Context) {
        let palette = palette(self.dark);
        let theme = if self.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        let mut visuals = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        visuals.panel_fill = palette.card_fill;
        visuals.window_fill = palette.card_fill;
        visuals.extreme_bg_color = palette.search_fill;
        visuals.faint_bg_color = palette.row_hover_fill;
        visuals.window_corner_radius = egui::CornerRadius::same(CARD_RADIUS);
        visuals.window_stroke = egui::Stroke::new(1.0, palette.card_stroke);
        visuals.selection.bg_fill = palette.row_selected_fill;
        visuals.selection.stroke = egui::Stroke::new(1.0, palette.row_selected_stroke);
        // 兜底文字色：没显式指定颜色的标签也跟着主题走，暗色下不会出现黑字黑底。
        visuals.override_text_color = Some(palette.text_primary);
        visuals.text_cursor.stroke.color = palette.text_primary;
        visuals.widgets.noninteractive.fg_stroke.color = palette.text_primary;
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, palette.card_stroke);
        visuals.widgets.inactive.fg_stroke.color = palette.text_primary;

        ctx.set_theme(theme);
        ctx.set_visuals_of(theme, visuals);
    }

    /// 参数：`ctx` egui 上下文（立即应用并重绘）；`dark` 目标主题（`true` = 深色）
    /// 核心流程：写回 `dark` → [`SwitcherooApp::apply_theme`] 把该主题的 Visuals 立刻灌进 egui →
    /// `request_repaint` 让本帧画出的卡片、列表与帮助面板都换成新配色 → 记一行日志方便真机核对。
    /// 后续选项面板直接调用这个入口即可，不需要再碰 Visuals。
    pub fn set_dark(&mut self, ctx: &egui::Context, dark: bool) {
        self.dark = dark;
        self.apply_theme(ctx);
        ctx.request_repaint();
        log::info!("主题切换：{}", if dark { "深色" } else { "浅色" });
    }

    /// 参数：`ctx` egui 上下文（回调里用它唤醒事件循环）；`toggle` 与 UI 线程共享的切换
    /// 标志；`hotkey_at` 与 UI 线程共享的「热键触发时刻」；`setting` 要注册的组合键
    /// 核心流程：把配置层的 [`HotkeySetting`] 原样转成平台层的 `HotkeySpec`（两边都是 Win32
    /// 虚拟键码 + `MOD_*` 位，不需要再做映射）→ 交给平台层起热键线程；回调里只做三件事：
    /// 置位 `toggle`、写下 `Instant::now()`、调用 `ctx.request_repaint()` 唤醒 eframe 事件循环
    /// （窗口隐藏时同样有效），绝不在回调里操作 UI 或窗口命令。注册失败返回原因文案，由界面提示。
    fn register_hotkey(
        ctx: &egui::Context,
        toggle: &Arc<AtomicBool>,
        hotkey_at: &Arc<Mutex<Option<Instant>>>,
        setting: HotkeySetting,
    ) -> (Option<HotkeyListener>, Option<String>) {
        let spec = HotkeySpec {
            key: setting.key,
            modifiers: setting.modifiers,
        };

        let toggle = Arc::clone(toggle);
        let stamp = Arc::clone(hotkey_at);
        // Context 是克隆成本很低的共享句柄，可以安全地在热键线程里调用。
        let repaint_ctx = ctx.clone();

        match HotkeyListener::spawn(spec, move || {
            toggle.store(true, Ordering::SeqCst);
            // 记下回调触发的时刻 -> 首帧耗时的起点；锁中毒也不 panic，只是丢一次基准。
            if let Ok(mut slot) = stamp.lock() {
                *slot = Some(Instant::now());
            }
            repaint_ctx.request_repaint();
        }) {
            Ok(listener) => {
                log::info!("全局热键 {} 注册成功", hotkey_text(&setting));
                (Some(listener), None)
            }
            Err(err) => {
                log::error!("全局热键 {} 注册失败：{err}", hotkey_text(&setting));
                (None, Some(err.to_string()))
            }
        }
    }

    /// 参数：`wanted` 目标自启状态
    /// 核心流程：先写进 `settings.autostart` → `enable()` / `disable()` 落到注册表 Run 键 →
    /// 成功时清掉面板提示；失败时记一笔 `warn`、把 `settings.autostart` 退回注册表真实状态
    /// （`is_enabled()`）、把原因写进 `options_error` 让面板显示红字并返回 false，绝不谎报成功 →
    /// 最后同步托盘勾选与面板草稿，三处显示始终一致。
    /// 返回 true 表示注册表真的改成功了。
    fn apply_autostart(&mut self, wanted: bool) -> bool {
        self.settings.autostart = wanted;
        let result = if wanted { enable() } else { disable() };
        let applied = match result {
            Ok(()) => {
                log::info!("开机自启已{}", if wanted { "开启" } else { "关闭" });
                self.options_error = None;
                true
            }
            Err(err) => {
                log::warn!("设置开机自启失败：{err}");
                self.settings.autostart = is_enabled();
                self.options_error = Some(format!("设置开机自启失败：{err}"));
                false
            }
        };

        if let Some(tray) = &self.tray {
            tray.set_autostart_checked(self.settings.autostart);
        }
        if let Some(draft) = &mut self.draft {
            draft.autostart = self.settings.autostart;
        }
        applied
    }

    /// 参数：`settings` 要落盘的设置；`reason` 写进日志的原因（面板保存 / 托盘切换）
    /// 核心流程：写到 [`config_path`]；成功记一行带完整路径的 info（用户想手改配置时能照着
    /// 路径去找）并返回 true；失败把原因存进 `save_error` 让界面显示并返回 false。
    fn persist_settings(&mut self, settings: &Settings, reason: &str) -> bool {
        match save_settings(settings) {
            Ok(()) => {
                self.save_error = None;
                log::info!("{reason}：配置已保存到 {}", config_path().display());
                true
            }
            Err(err) => {
                self.save_error = Some(format!("配置保存失败：{err}"));
                log::warn!("{reason}：配置保存失败：{err}");
                false
            }
        }
    }

    /// 参数：`ctx` egui 上下文（用来把焦点从搜索框交还出去，并下发聚焦命令）
    /// 核心流程：面板已经开着时只把浮层置前并抢焦点，**绝不重建草稿**（否则用户正在编辑的内容
    /// 会被已保存值悄悄覆盖，托盘「选项…」与 `Ctrl+,` 都走这条早退）→ 面板没开时把当前设置
    /// 复制成草稿、清掉上次留下的面板提示、收起帮助面板、让浮层可见，再把搜索框的焦点交还——
    /// 它那套「Tab / 方向键归浮层」的焦点锁不解除的话，面板里按 Tab 就没法在控件之间移动。
    /// 打开面板本身不改变任何运行时状态，所以取消时不需要回滚。
    fn open_options(&mut self, ctx: &egui::Context) {
        if self.options_open {
            self.visible = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            log::info!("选项面板已打开，仅置前聚焦（保留正在编辑的草稿）");
            return;
        }

        if let Some(id) = self.search_focus.take() {
            ctx.memory_mut(|memory| memory.surrender_focus(id));
        }
        self.draft = Some(OptionsDraft::new(&self.settings));
        self.options_open = true;
        self.options_error = None;
        self.show_help = false;
        self.visible = true;
        log::info!("打开选项面板");
    }

    /// 参数：`ctx` egui 上下文（主题可能要说回旧配色，需要重绘）
    /// 核心流程：丢掉草稿并关面板 → 无条件把运行时刷回「已保存的设置」：主题按 `settings.theme`
    /// 重算并应用、托盘勾选对齐 `settings.autostart`，热键不动（面板里从未重新绑定过）——
    /// 于是取消之后看到的就是打开面板之前的样子。上一次留在面板里的提示也一并清掉。
    fn cancel_options(&mut self, ctx: &egui::Context) {
        self.options_open = false;
        self.draft = None;
        self.options_error = None;

        let dark = dark_for(self.settings.theme, is_system_dark());
        if self.dark != dark {
            self.set_dark(ctx, dark);
        } else {
            self.apply_theme(ctx);
        }
        if let Some(tray) = &self.tray {
            tray.set_autostart_checked(self.settings.autostart);
        }
        ctx.request_repaint();
        log::info!("选项面板已取消，运行时状态保持为已保存的设置");
    }

    /// 参数：`ctx` egui 上下文（试注册热键要用它的回调唤醒事件循环）
    /// 核心流程：**先试注册、成功才落盘**——按 [`plan_hotkey_rebind`] 决定要不要用草稿里的组合键
    /// 试注册（组合键没变且旧监听还在就跳过，否则会撞上自己占着的同一个键）→ 试注册失败时旧监听
    /// 全程没被动过，草稿热键回滚到已保存值、面板内报红字后直接返回（不写配置文件、不关面板）→
    /// 通过后才写注册表自启、才落盘，任一失败都留在面板里允许重试 → 全部成功后才把 `settings`
    /// 换成新值：新监听在此刻接管（旧监听随之 drop），`enable_hotkey=false` 时旧监听被释放 →
    /// 按 `theme` 应用配色并关面板。
    fn save_options(&mut self, ctx: &egui::Context) {
        let Some(next) = self.draft.as_ref().map(OptionsDraft::to_settings) else {
            return;
        };

        // 1) 热键：先试注册，此时还没落盘。旧监听在成功之前一直开着，所以不会出现
        //    「旧热键已注销、新热键也没注册上」的空窗。
        let plan = plan_hotkey_rebind(&self.settings, &next, self.hotkey.is_some());
        let mut pending = None;
        let mut hotkey_failure = None;
        if plan == HotkeyPlan::Register {
            match Self::register_hotkey(ctx, &self.toggle_requested, &self.hotkey_at, next.hotkey) {
                (Some(listener), _) => pending = Some(listener),
                // 平台层保证失败时线程已经退出，这里的 `None` 就是「没有新监听」，旧监听照旧。
                (None, error) => {
                    hotkey_failure =
                        Some(error.unwrap_or_else(|| {
                            format!("热键 {} 注册失败", hotkey_text(&next.hotkey))
                        }));
                }
            }
        }

        // 2) 冲突：不落盘、不关面板，`settings` 与配置文件都还是旧的；只把草稿热键退回已保存值，
        //    用户换一个组合键再按一次「确定」即可。
        if decide_after_register(plan, hotkey_failure.is_none()) == SaveDecision::Rollback {
            if let Some(draft) = &mut self.draft {
                draft.hotkey = self.settings.hotkey;
                draft.recording = false;
            }
            if let Some(err) = &hotkey_failure {
                log::warn!("选项面板：热键未保存（注册失败）：{err}");
                self.options_error = Some(format!("{err}，未保存"));
            }
            ctx.request_repaint();
            return;
        }

        // 3) 自启：先落到注册表；失败时 `apply_autostart` 已把状态退回真实值并写好面板提示。
        //    新监听还没接管，随 `pending` 一起丢掉，旧监听继续按旧组合键工作。
        if !self.apply_autostart(next.autostart) {
            ctx.request_repaint();
            return;
        }

        // 4) 落盘：热键与自启都已经真生效，这里才写配置文件；写失败同样留在面板里。
        if !self.persist_settings(&next, "选项面板") {
            return;
        }

        // 5) 全部成功：运行时设置与监听此刻才切换。
        self.settings = next;
        match plan {
            // 组合键没变：旧监听继续用着，`pending` 本来就是 `None`。
            HotkeyPlan::Keep => {}
            HotkeyPlan::Register => {
                // 注册失败的那条路已经返回，所以这里 `pending` 一定在。这一行同时完成「新监听
                // 接管」与「旧监听 drop」：旧监听的 Drop 会向热键线程投递退出消息、注销旧组合键
                // 并 join。（不写 `drop(...)`：非 Windows 的占位类型没有 Drop，会被
                // `clippy::drop_non_drop` 拦下。）
                if let Some(listener) = pending {
                    let _previous = self.hotkey.replace(listener);
                }
            }
            HotkeyPlan::Release => {
                // 关掉热键：配置已经落盘，旧监听到这里才释放。
                let _released = self.hotkey.take();
            }
        }
        self.hotkey_error = None;
        self.options_error = None;

        let dark = dark_for(self.settings.theme, is_system_dark());
        self.set_dark(ctx, dark);

        self.options_open = false;
        self.draft = None;
        ctx.request_repaint();
        log::info!(
            "选项已保存：热键 {}（{}），自启 {}，主题 {}",
            hotkey_text(&self.settings.hotkey),
            if self.settings.enable_hotkey {
                "启用"
            } else {
                "关闭"
            },
            self.settings.autostart,
            theme_name(self.settings.theme)
        );
    }

    /// 参数：`ctx` egui 上下文（读本帧的按键事件）
    /// 核心流程：面板没开或不在录制态时什么都不做；录制态里把本帧事件喂给
    /// [`RecordInput::feed`]：`Captured` 写回草稿并退出录制，`Cancel` 只退出录制，`Pending`
    /// 继续等。返回 true 表示本帧按键已被录制消费，`logic` 不再把它当成快捷键处理。
    fn capture_record(&mut self, ctx: &egui::Context) -> bool {
        let Some(draft) = self.draft.as_mut() else {
            return false;
        };
        if !draft.recording {
            return false;
        }

        let events = ctx.input(|input| input.events.clone());
        match draft.record_input.feed(&events) {
            Recorded::Captured(setting) => {
                draft.hotkey = setting;
                draft.recording = false;
                log::info!("热键录制完成：{}", hotkey_text(&setting));
                ctx.request_repaint();
                true
            }
            Recorded::Cancel => {
                draft.recording = false;
                log::info!("热键录制已取消（保留原来的组合键）");
                ctx.request_repaint();
                true
            }
            Recorded::Pending => false,
        }
    }

    /// 参数：`ctx` egui 上下文（处理完命令后按需重绘）
    /// 核心流程：`try_recv` 非阻塞收割托盘线程投递的命令直到通道空，每条交给
    /// [`SwitcherooApp::handle_tray_command`]；通道空时这一步是空转，绝不阻塞事件循环。
    fn poll_tray_commands(&mut self, ctx: &egui::Context) {
        while let Ok(command) = self.tray_commands.try_recv() {
            log::debug!("收到托盘命令 {command:?}");
            self.handle_tray_command(ctx, command);
        }
    }

    /// 参数：`ctx` egui 上下文；`command` 用户在托盘图标 / 菜单上触发的命令
    /// 核心流程：`Show` 只把期望可见状态置真（加载、居中与 `Visible(true)` 由
    /// `sync_window_visibility` 统一完成）；`Options` 交给 `open_options`——面板已开着时它只
    /// 置前聚焦、不重建草稿，用户正在编辑的内容不会被静默丢弃；`ToggleAutostart` /
    /// `ToggleTheme` 立即生效并落盘；`Exit` 直接投递关闭命令。
    fn handle_tray_command(&mut self, ctx: &egui::Context, command: TrayCommand) {
        match command {
            TrayCommand::Show => {
                self.visible = true;
                ctx.request_repaint();
            }
            TrayCommand::Options => {
                // 已打开时这里只置前聚焦（保证正在编辑的草稿不被覆盖），见 `open_options`。
                self.open_options(ctx);
                ctx.request_repaint();
            }
            TrayCommand::ToggleAutostart => {
                let wanted = !self.settings.autostart;
                self.apply_autostart(wanted);
                let snapshot = self.settings.clone();
                self.persist_settings(&snapshot, "托盘：切换开机自启");
                ctx.request_repaint();
            }
            TrayCommand::ToggleTheme => {
                let theme = toggled_theme(self.settings.theme, is_system_dark());
                self.settings.theme = theme;
                if let Some(draft) = &mut self.draft {
                    draft.theme = theme;
                }
                let dark = dark_for(theme, is_system_dark());
                self.set_dark(ctx, dark);
                let snapshot = self.settings.clone();
                self.persist_settings(&snapshot, "托盘：切换明暗主题");
            }
            TrayCommand::Exit => {
                log::info!("托盘：退出");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// 参数：`ctx` egui 上下文（快照为空时用它排一次延迟重试）
    /// 核心流程：先取唤起前的前台窗口（此刻浮层还没显示，前台仍是用户原来的窗口）→
    /// 再取真实窗口快照 → 用 core 把前台窗口移到列表末尾，让首项落在「下一个窗口」→
    /// 丢掉上一轮残留的关闭中状态 → 用最新快照重算一次排名（查询空则全部展示）并选中第 1 条，
    /// 置位首帧计时起点与「滚入可视区」标记 → 快照为空、浮层可见且还有重试次数时，
    /// 记下 250ms 后的一次重试（由 `logic` 到点触发），避免用户看到空列表。
    fn load_windows(&mut self, ctx: &egui::Context) {
        let foreground = foreground_window();
        let mut windows = snapshot();

        // 前台窗口置底：没有前台窗口或没命中时列表顺序保持不变（core 返回 false）。
        match &foreground {
            Some(fg) => {
                let moved = move_foreground_to_end(&mut windows, fg.id, &fg.process_name);
                log::debug!(
                    "唤起前的前台窗口 {}({:#X}) 置底：{}",
                    fg.process_name,
                    fg.id.raw(),
                    moved
                );
            }
            None => log::debug!("唤起时没有前台窗口，列表顺序保持不变"),
        }

        // US-041：记下唤起瞬间的前台进程名，`.标题` 这类查询用它补全进程片段。
        // 这里必须在 `refresh_ranked` 之前赋值，否则本轮排名拿到的还是上一轮的进程名。
        self.foreground_process = foreground.map(|fg| fg.process_name);

        let count = windows.len();
        self.window_snapshot = windows;
        // 新一次唤起是一份全新快照：上一次残留的关闭中条目一律作废，不留下无人轮询的状态。
        self.closing.clear();
        // US-044：快照变了才提交图标请求——已缓存的窗口直接命中，不重复取图。
        self.icons.sync(&self.window_snapshot);
        // 快照变了 -> 重算一次排名，并按 US-040 自动选中第 1 条。
        self.refresh_ranked(true);
        self.switch_error = None;
        // 每次唤起都从「帮助面板收起」开始，浮层始终先露出列表。
        self.show_help = false;
        // 计时起点优先用热键回调写下的时刻；没有热键（降级启动）时退回当前时刻。
        let started = self.hotkey_at.lock().ok().and_then(|mut slot| slot.take());
        self.first_frame_started = Some(started.unwrap_or_else(Instant::now));
        log::info!("窗口加载完成：快照 {count} 个窗口，选中第 1 项");

        // 刚开机 / 刚登录时枚举可能为空：不每帧重试，只按 250ms 的节奏再取，最多两次；
        // 拿到非空快照就清掉待重试标记（本次唤起不需要再补）。
        if count == 0 && self.visible && self.empty_retry_left > 0 {
            self.empty_retry_left -= 1;
            self.empty_retry_due =
                Some(Instant::now() + Duration::from_millis(EMPTY_SNAPSHOT_RETRY_MS));
            ctx.request_repaint_after(Duration::from_millis(EMPTY_SNAPSHOT_RETRY_MS));
            log::info!(
                "快照为空，{} ms 后重试（剩余 {} 次）",
                EMPTY_SNAPSHOT_RETRY_MS,
                self.empty_retry_left
            );
        } else {
            self.empty_retry_due = None;
        }
    }

    /// 参数：`ctx` egui 上下文（立刻下发隐藏命令）
    /// 核心流程：清空本次唤起的全部状态（选中项、快照、排名结果、关闭中集合、首帧基准、
    /// 错误提示）→ 标记本帧不再绘制（同帧后续的 `show_list` 会看到空快照，不标记就会闪
    /// 一帧空列表）→ 复用现有可见性状态机立刻收起浮层，下次唤起由 `load_windows` 重新加载。
    fn hide(&mut self, ctx: &egui::Context) {
        self.skip_render_this_frame = true;
        self.visible = false;
        // 浮层收起时选项面板一并关掉：草稿随面板丢弃，下次打开重新从已保存的设置复制。
        self.options_open = false;
        self.draft = None;
        self.search_focus = None;
        self.selection = Selection::new(0);
        self.window_snapshot.clear();
        self.ranked.clear();
        // 浮层藏起来就意味着不再有任何轮询任务：清空集合，`poll_closing` 不再被驱动。
        self.closing.clear();
        // 收起后也不再需要居中与空快照重试：下次显示会重新排程。
        self.center_pending = false;
        self.center_tries = 0;
        self.empty_retry_due = None;
        // 图标缓存**不**在这里清空：二次唤起要直接命中，这正是 US-044 的验收点。
        self.scroll_to_selected = false;
        self.first_frame_started = None;
        self.switch_error = None;
        self.sync_window_visibility(ctx);
    }

    /// 参数：`reset_selection` 为真时把选中项重置为第 1 条，否则用 `Selection::clamp` 保住位置
    /// 核心流程：用当前查询串、快照与唤起瞬间的前台进程名调 core 的
    /// `rank_windows_with_foreground`（空查询走它自己的「返回全部」分支，这里不写特判）→
    /// 存进 `ranked` → 按参数重置或收敛选中项并置位「滚入可视区」。
    /// 只在查询或快照变化时调用，绝不放进每帧渲染路径。
    fn refresh_ranked(&mut self, reset_selection: bool) {
        self.ranked = rank_windows_with_foreground(
            &self.query,
            &self.window_snapshot,
            self.foreground_process.as_deref(),
        );
        let len = self.ranked.len();
        if reset_selection {
            self.selection.reset(len, false);
        } else {
            self.selection.clamp(len);
        }
        self.scroll_to_selected = len > 0;
        log::debug!(
            "排名结果 {len} / 共 {} 个窗口（查询「{}」，前台进程 {}）",
            self.window_snapshot.len(),
            self.query,
            self.foreground_process.as_deref().unwrap_or("-")
        );
    }

    /// 参数：`row` 列表行号（按排名结果计）
    /// 核心流程：排名结果给出快照里的原始下标，再回查快照取出窗口；越界一律返回 `None`，
    /// 调用方因此不用重复写边界判断。
    fn window_at(&self, row: usize) -> Option<&WindowInfo> {
        let index = self.ranked.get(row)?.index;
        self.window_snapshot.get(index)
    }

    /// 参数：`id` 窗口标识
    /// 核心流程：在关闭中集合里线性查找；集合通常只有个位数元素，不值得上哈希表。
    fn is_closing(&self, id: WindowId) -> bool {
        self.closing.iter().any(|(closing, _)| *closing == id)
    }

    /// 参数：`row` 列表行号（按排名结果计）；`ctx` egui 上下文（成功后立刻收起浮层）
    /// 核心流程：行号 → 排名结果 → 快照里的窗口 → 调平台层 `switch_to`；成功则记日志、
    /// 隐藏浮层并清空选中项状态；失败只在界面留一行提示、浮层保持显示，不崩溃也不影响后续操作。
    fn activate(&mut self, row: usize, ctx: &egui::Context) {
        let Some(window) = self.window_at(row) else {
            return;
        };
        let (id, title) = (window.id, window.title.clone());

        if switch_to(id) {
            log::info!("已切换到窗口「{title}」");
            self.hide(ctx);
        } else {
            log::warn!("切换窗口失败：「{title}」");
            self.switch_error = Some(format!("切换失败：「{title}」暂时无法激活，浮层保持显示"));
        }
    }

    /// 参数：`ctx` egui 上下文（关闭成功后用它安排第一次轮询）
    /// 核心流程：选中行 → 排名结果 → 快照拿到窗口 → 该窗口已在关闭中则直接返回（不重复投递
    /// `WM_CLOSE`）→ 否则**先登记进关闭中集合、再调平台层 `request_close`**，于是重复按键
    /// 在投递之前就被挡掉 → 投递成功只排程第一次轮询，之后由 `poll_closing` 确认 → 投递失败
    /// 用 `is_window_alive` 分辨两种情况：目标窗口已经不存在（第一次投递已生效）按关闭成功走
    /// 轮询同一条移除路径，目标仍存活才是真失败 —— 撤销登记并只留一行红色提示，浮层保持显示。
    fn close_selected(&mut self, ctx: &egui::Context) {
        let Some(row) = self.selection.index() else {
            return;
        };
        let Some(window) = self.window_at(row) else {
            return;
        };
        let (id, title) = (window.id, window.title.clone());

        if self.is_closing(id) {
            log::debug!("窗口已在关闭中，忽略重复关闭请求：「{title}」");
            return;
        }

        // 先登记、再投递：`request_close` 是异步的，目标可能在它返回之前就退出；
        // 登记放在投递之前，同一帧或连续两帧的重复关闭请求都会被上面的判断拦下。
        self.closing.push((id, Instant::now()));
        // 成功路径顺手清掉上一次的红色提示，避免历史错误残留在界面上。
        self.switch_error = None;

        let outcome = request_close(id);
        // UIPI 拒绝（目标以管理员权限运行）也是失败路径，但多说一句原因，便于用户自救。
        if outcome == CloseOutcome::Denied {
            log::warn!("关闭窗口被 UIPI 拒绝（目标以管理员权限运行）：「{title}」");
        }

        if outcome == CloseOutcome::Requested {
            log::info!("已请求关闭窗口「{title}」，每 {CLOSE_POLL_MS} ms 轮询确认");
            // 第一次轮询靠这里排程；后续每一次都由 `poll_closing` 自己续上。
            ctx.request_repaint_after(Duration::from_millis(CLOSE_POLL_MS));
            return;
        }

        // 投递失败不等于关闭失败：目标窗口在两次请求之间已经退出时，`request_close` 里的
        // `IsWindow` 校验就会返回 false。窗口确实没了就按关闭成功处理 —— 此时 id 已在关闭中
        // 集合里，直接复用 `poll_closing` 的移除路径（快照移除 + 重算排名 + 修正选中 +
        // 快照被关空则收起浮层），界面不留任何红色错误。
        if !is_window_alive(id) {
            log::info!("窗口已不存在，按关闭成功处理：「{title}」");
            self.poll_closing(ctx);
            return;
        }

        // 窗口仍存活才是真正的投递失败：撤销登记，让用户可以再按一次重试。
        self.closing.retain(|(closing, _)| *closing != id);
        log::warn!("关闭窗口失败：「{title}」");
        self.switch_error = Some(format!("关闭失败：「{title}」暂时无法关闭，浮层保持显示"));
    }

    /// 参数：`ctx` egui 上下文（用来安排下一次轮询）
    /// 核心流程：关闭中集合为空直接返回 → 先记下「选中窗口」作为锚点（关闭中的可能是别的行）
    /// → 逐个调平台层 `is_window_alive`：仍存活的留在集合里，已消失的取出来 → 从快照里
    /// 按 id 移除它们并重新排名 → 按锚点恢复选中（锚点自己也被关掉时由 `clamp` 给出
    /// 「下一条优先、否则上一条」）→ 快照被关空则收起浮层，否则还有关闭中条目就 125 ms 后再问。
    fn poll_closing(&mut self, ctx: &egui::Context) {
        if self.closing.is_empty() {
            return;
        }

        // 选中项锚点用窗口 id 而不是行号：别的行被移除会让行号漂移。
        let anchor = self
            .selection
            .index()
            .and_then(|row| self.window_at(row))
            .map(|window| window.id);

        let mut closed: Vec<(WindowId, Instant)> = Vec::new();
        self.closing.retain(|(id, started)| {
            if is_window_alive(*id) {
                true
            } else {
                closed.push((*id, *started));
                false
            }
        });

        if closed.is_empty() {
            // 还是活的：继续等（例如目标弹了「是否保存」对话框）。
            ctx.request_repaint_after(Duration::from_millis(CLOSE_POLL_MS));
            return;
        }

        for (id, started) in closed {
            log::info!(
                "窗口已关闭：id={:#X}，确认耗时 {} ms",
                id.raw(),
                started.elapsed().as_millis()
            );
            self.window_snapshot.retain(|window| window.id != id);
        }

        // 快照变了：重算排名（查询没变，命中数与行号随之更新），`clamp` 先给一个合法位置。
        self.refresh_ranked(false);
        // 关掉的窗口不再需要图标：缓存跟着快照一起收缩（句柄会被系统复用，留着可能串图）。
        self.icons.sync(&self.window_snapshot);

        // 锚点窗口还在 -> 选中回到它；它自己就是被关掉的那个 -> 保留上面 clamp 的结果：
        // 行号不变就是「下一条」，越界收敛到末项就是「上一条」。
        if let Some(id) = anchor {
            if let Some(row) = ranked_row_of(&self.window_snapshot, &self.ranked, id) {
                // `Selection` 没有单点 setter，`reset(row + 1, true)` 正好落到第 row 行。
                self.selection.reset(row + 1, true);
            }
        }

        if self.window_snapshot.is_empty() {
            log::info!("可切换窗口已全部关闭，收起浮层");
            self.hide(ctx);
            return;
        }

        if !self.closing.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(CLOSE_POLL_MS));
        }
    }

    /// 参数：`ctx` egui 上下文
    /// 核心流程：在渲染输入框之前把 Tab / 方向键 / Alt+J、Alt+K / 回车「按下」/ Ctrl+W、
    /// Alt+X 从输入队列里消费掉（否则单行 TextEdit 会拿 Tab 去切焦点、拿回车去失焦）→
    /// 按方向更新 `Selection`，越界由 core 负责循环 → 关闭键交给 `close_selected`，浮层不收起 →
    /// 回车「松开」才执行切换（保证目标窗口不会收到残留的 KeyDown/KeyUp）；无结果时回车改为
    /// 清空查询。列表为空时按键已被消费，但没有任何副作用、也不会 panic。
    fn handle_list_keys(&mut self, ctx: &egui::Context) {
        let keys = ctx.input_mut(|input| {
            let shift = input.modifiers.shift;
            let alt = input.modifiers.alt;

            // Tab：consume_key 在逻辑上忽略多余的 Shift，因此 Shift+Tab 也走这条路径，
            // 方向用 shift 判断；消费掉它，输入框就不会把 Tab 当成焦点切换。
            let tab = input.consume_key(egui::Modifiers::NONE, egui::Key::Tab);
            let arrow_down = input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
            let arrow_up = input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
            // Alt+J / Alt+K：只有真的按着 Alt 才消费，避免吞掉输入框里正常的 j / k。
            let alt_j = alt && input.consume_key(egui::Modifiers::ALT, egui::Key::J);
            let alt_k = alt && input.consume_key(egui::Modifiers::ALT, egui::Key::K);
            // Ctrl+W / Alt+X：关闭选中窗口。同样在输入框拿到事件之前就消费，
            // 两个组合键都不是 TextEdit 的编辑快捷键，不会与输入冲突。
            let ctrl_w = input.consume_key(egui::Modifiers::CTRL, egui::Key::W);
            let alt_x = input.consume_key(egui::Modifiers::ALT, egui::Key::X);
            // 回车「按下」直接消费掉（不关心返回值）：单行 TextEdit 因此不会失焦。
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            // 回车「松开」只识别、不消费，切换在松开这一帧才发生。
            let enter_released = input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        pressed: false,
                        ..
                    }
                )
            });

            ListKeys {
                down: arrow_down || alt_j || (tab && !shift),
                up: arrow_up || alt_k || (tab && shift),
                enter_released,
                close: ctrl_w || alt_x,
            }
        });

        let len = self.ranked.len();
        if len == 0 {
            // 没有结果时回车 = 清空查询（界面提示里也这么写），其余按键不产生副作用。
            if keys.enter_released && !self.query.is_empty() {
                self.query.clear();
                self.refresh_ranked(true);
                log::info!("当前没有匹配结果，回车已清空查询");
            }
            return;
        }

        if keys.down {
            self.selection.next(len);
            self.scroll_to_selected = true;
        }
        if keys.up {
            self.selection.previous(len);
            self.scroll_to_selected = true;
        }

        if keys.close {
            self.close_selected(ctx);
        }

        if keys.enter_released {
            if let Some(row) = self.selection.index() {
                self.activate(row, ctx);
            }
        }
    }

    /// 参数：`count` 本次画出的窗口数（只用于日志）
    /// 核心流程：只在首帧记一次——取走计时起点，算出「热键回调 → 列表画出」的毫秒数，
    /// 打一行 info 日志后丢弃（界面上不再显示耗时）。
    fn record_first_frame(&mut self, count: usize) {
        let Some(started) = self.first_frame_started.take() else {
            return;
        };

        let ms = started.elapsed().as_secs_f32() * 1000.0;
        log::info!("首帧耗时 {ms:.1} ms，窗口数 {count}");
    }

    /// 参数：`ui` 卡片内 UI
    /// 核心流程：把单实例、热键注册、切换 / 关闭失败、面板里没保存成功的原因与配置保存失败的
    /// 提示拼成若干行红字，**仅在存在错误时**画在搜索框上方；全部正常时一行都不占，界面上只看
    /// 得到搜索框与列表。
    fn show_errors(&self, ui: &mut egui::Ui) {
        let palette = self.palette();
        let messages = [
            self.single_instance_error
                .as_ref()
                .map(|err| format!("单实例获取失败：{err}")),
            self.hotkey_error
                .as_ref()
                .map(|err| format!("热键注册失败：{err}")),
            self.switch_error.clone(),
            // 托盘里的切换也会落盘，失败原因同样要能看见（面板开着时由面板自己显示）。
            self.save_error.clone(),
            // 热键冲突 / 自启失败：面板开着时显示在面板里，面板关着（托盘改自启）时显示在这里。
            self.options_error.clone(),
        ];

        let mut shown = false;
        for message in messages.into_iter().flatten() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(message)
                        .size(TITLE_SIZE - 1.0)
                        .color(palette.error_text),
                )
                .wrap(),
            );
            shown = true;
        }
        if shown {
            ui.add_space(SEARCH_LIST_GAP);
        }
    }

    /// 参数：`ui` 卡片内 UI
    /// 核心流程：画顶部唯一的控件行——左侧细边搜索框占满除 `?` 按钮外的整行宽度，右侧是
    /// 圆角无边框的 `?` 按钮（点击翻转 [`SwitcherooApp::show_help`]）→ 输入框保持焦点并把
    /// Tab / 方向键锁在本浮层（否则 egui 会先按 TextEdit 的默认行为切走焦点）→
    /// 文本一变立即重算排名；返回整行矩形，供帮助面板贴在它下方（不遮挡搜索框）。
    fn show_search_row(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        let palette = self.palette();
        let spacing = ui.spacing().item_spacing.x;
        // 搜索框宽度 = 整行宽度 - `?` 按钮 - 一个间距；窗口很窄时留一个下限免得算成负数。
        let search_width = (ui.available_width() - HELP_BUTTON_SIZE - spacing).max(80.0);

        let row = ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("搜索窗口，可用「进程.标题」或拼音首字母")
                    .font(egui::FontId::proportional(TITLE_SIZE))
                    .text_color(palette.text_primary)
                    .desired_width(search_width)
                    // 自定义外框：主题底色 + 1px 描边 + 小圆角，内边距压到约 28 高。
                    .frame(
                        egui::Frame::new()
                            .fill(palette.search_fill)
                            .stroke(egui::Stroke::new(1.0, palette.search_stroke))
                            .corner_radius(egui::CornerRadius::same(ROW_RADIUS))
                            .inner_margin(egui::Margin::symmetric(8, 5)),
                    ),
            );
            // 记下搜索框的 id：打开选项面板时要靠它交还焦点（见 `open_options`）。
            self.search_focus = Some(edit.id);
            if edit.has_focus() {
                // 锁住 Tab / 方向键：否则 egui 会在下一帧开头把焦点当「Tab 切焦点」挪走。
                ui.memory_mut(|mem| mem.set_focus_lock_filter(edit.id, NAV_KEYS_LOCK));
            } else {
                // 首次显示即聚焦 -> 对应浮层唤起后输入框自动获得焦点。
                edit.request_focus();
            }
            if edit.changed() {
                // 输入即过滤，无需确认键；过滤后自动选中第 1 条。
                // 排名只在这里重算一次，不放进每帧渲染路径。
                self.refresh_ranked(true);
            }

            let help = ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("?")
                            .size(TITLE_SIZE)
                            .color(palette.text_muted),
                    )
                    .frame(false)
                    .min_size(egui::vec2(HELP_BUTTON_SIZE, HELP_BUTTON_SIZE)),
                )
                .on_hover_text("快捷键");
            if help.clicked() {
                self.show_help = !self.show_help;
            }

            edit.rect.union(help.rect)
        });

        row.inner
    }

    /// 参数：`ui` 卡片内 UI；`anchor` 搜索行的矩形（面板贴在它下方，绝不遮挡搜索框）
    /// 核心流程：在 `egui::Area`（前景层）里画一张主题底色 + 细边框的小卡片，逐行列出搜索、
    /// `进程.标题` 复合查询、拼音首字母、上下移动、切换、关闭、选项、收起与退出的快捷键；内部再包一层与卡片
    /// 同款的边框 Frame，宽度跟随搜索行，因此不会盖住输入框，也不会把列表挤走。底部一个
    /// 「选项…」按钮直接打开选项面板（US-047），省得用户去记 `Ctrl+,`。
    fn show_help_panel(&mut self, ui: &egui::Ui, anchor: egui::Rect) {
        let palette = self.palette();
        /// 帮助面板的逐条内容：左侧按键、右侧说明。
        const HELP_ROWS: [(&str, &str); 10] = [
            ("输入", "即时过滤，无需回车确认"),
            (
                "进程.标题",
                "`.` 前限定进程，`.` 后只搜标题；`.标题` 用唤起时的前台进程",
            ),
            ("wj / yy", "拼音首字母：文件 / 音乐"),
            ("↓ / Alt+J / Tab", "下一项（循环）"),
            ("↑ / Alt+K / Shift+Tab", "上一项（循环）"),
            ("回车 / 双击", "切换到选中窗口"),
            ("Ctrl+W / Alt+X", "关闭选中窗口"),
            ("Ctrl+,", "打开选项（热键 / 自启 / 主题）"),
            ("Esc / Alt+Space", "收起浮层"),
            ("Ctrl+Q", "退出"),
        ];

        let pos = egui::pos2(anchor.left(), anchor.bottom() + SEARCH_LIST_GAP);
        // 比搜索行窄一些：看起来是小卡片，也不至于把整列列表盖住。
        let width = anchor.width().clamp(280.0, 460.0);
        let mut open_options = false;

        egui::Area::new(egui::Id::new("shortcut_help"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(palette.help_fill)
                    .stroke(egui::Stroke::new(1.0, palette.help_stroke))
                    .corner_radius(egui::CornerRadius::same(CARD_RADIUS))
                    .inner_margin(egui::Margin::same(CARD_INNER_MARGIN))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
                        ui.label(
                            egui::RichText::new("快捷键")
                                .size(PROCESS_SIZE + 1.0)
                                .color(palette.text_muted),
                        );
                        for (keys, desc) in HELP_ROWS {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("{keys}："))
                                        .size(PROCESS_SIZE + 0.5)
                                        .color(palette.text_primary),
                                );
                                ui.label(
                                    egui::RichText::new(desc)
                                        .size(PROCESS_SIZE + 0.5)
                                        .color(palette.text_muted),
                                );
                            });
                        }
                        ui.add_space(2.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("选项…")
                                        .size(PROCESS_SIZE + 0.5)
                                        .color(palette.text_primary),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            open_options = true;
                        }
                    });
            });

        // 借用到 `self` 的绘制闭包已经结束，这里再真正打开面板（它会改 `self` 的多个字段）。
        if open_options {
            self.open_options(ui.ctx());
        }
    }

    /// 参数：`ui` 卡片内 UI（面板铺满卡片，与搜索框 / 列表互斥）
    /// 核心流程：自上而下画「启用全局热键」复选框 →（关闭时整组置灰）组合键显示 + `录制` 按钮 →
    /// 「开机自启」复选框 → 主题三选一 → 失败提示（热键被占用 / 自启写注册表失败 / 配置保存
    /// 失败，都是红字，面板保持打开以便重试）→ 底部 `确定` / `取消`。所有控件只改草稿；
    /// 两个按钮的点击先记成 [`OptionsAction`]，等草稿的可变借用结束后再执行，这样它们可以安全地
    /// 走会改整个 `self` 的保存 / 取消流程。
    fn show_options_panel(&mut self, ui: &mut egui::Ui) {
        let palette = self.palette();
        let mut action = None;

        ui.label(
            egui::RichText::new("选项")
                .size(TITLE_SIZE + 1.0)
                .color(palette.text_primary),
        );
        ui.add_space(SEARCH_LIST_GAP);

        if let Some(draft) = self.draft.as_mut() {
            ui.add(egui::Checkbox::new(
                &mut draft.enable_hotkey,
                egui::RichText::new("启用全局热键")
                    .size(TITLE_SIZE)
                    .color(palette.text_primary),
            ));
            ui.add_space(4.0);

            // 热键关闭时整组置灰：录制按钮在禁用状态下点不动，视觉上也说不通。
            let enabled = draft.enable_hotkey;
            let recording = draft.recording;
            let hotkey = draft.hotkey;
            let mut start_recording = false;
            ui.add_enabled_ui(enabled, |ui| {
                ui.horizontal(|ui| {
                    let (text, color) = if recording {
                        (RECORDING_HINT.to_string(), palette.text_muted)
                    } else {
                        (hotkey_text(&hotkey), palette.text_primary)
                    };
                    egui::Frame::new()
                        .fill(palette.search_fill)
                        .stroke(egui::Stroke::new(1.0, palette.search_stroke))
                        .corner_radius(egui::CornerRadius::same(ROW_RADIUS))
                        .inner_margin(egui::Margin::symmetric(8, 5))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(text).size(TITLE_SIZE).color(color));
                        });

                    let record = ui.add_enabled(
                        !recording,
                        egui::Button::new(
                            egui::RichText::new("录制")
                                .size(TITLE_SIZE)
                                .color(palette.text_primary),
                        ),
                    );
                    if record.clicked() {
                        start_recording = true;
                    }
                });
            });
            // 录制按钮的点击延后写回草稿：闭包里只置一个局部标记，避免和外层的可变借用打架。
            if start_recording {
                draft.recording = true;
            }

            ui.label(
                egui::RichText::new("录制时请按住至少一个修饰键（Ctrl / Shift / Alt / Win）")
                    .size(PROCESS_SIZE)
                    .color(palette.text_muted),
            );
            ui.add_space(SEARCH_LIST_GAP);

            ui.add(egui::Checkbox::new(
                &mut draft.autostart,
                egui::RichText::new("开机自启")
                    .size(TITLE_SIZE)
                    .color(palette.text_primary),
            ));
            ui.add_space(SEARCH_LIST_GAP);

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("主题")
                        .size(TITLE_SIZE)
                        .color(palette.text_primary),
                );
                for theme in [
                    ThemeSetting::System,
                    ThemeSetting::Light,
                    ThemeSetting::Dark,
                ] {
                    ui.radio_value(
                        &mut draft.theme,
                        theme,
                        egui::RichText::new(theme_name(theme))
                            .size(TITLE_SIZE)
                            .color(palette.text_primary),
                    );
                }
            });
        }

        // 面板里的红字：热键被占用 / 自启写注册表失败（options_error）与配置保存失败（save_error）。
        for err in [self.options_error.as_ref(), self.save_error.as_ref()]
            .into_iter()
            .flatten()
        {
            ui.add_space(SEARCH_LIST_GAP);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(err.clone())
                        .size(TITLE_SIZE - 1.0)
                        .color(palette.error_text),
                )
                .wrap(),
            );
        }

        ui.add_space(SEARCH_LIST_GAP * 2.0);
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(
                    egui::RichText::new("确定").size(TITLE_SIZE),
                ))
                .clicked()
            {
                action = Some(OptionsAction::Save);
            }
            if ui
                .add(egui::Button::new(
                    egui::RichText::new("取消").size(TITLE_SIZE),
                ))
                .clicked()
            {
                action = Some(OptionsAction::Cancel);
            }
        });

        match action {
            Some(OptionsAction::Save) => self.save_options(ui.ctx()),
            Some(OptionsAction::Cancel) => self.cancel_options(ui.ctx()),
            None => {}
        }
    }

    /// 参数：`ui` 卡片内的 UI
    /// 核心流程：快照为空只画一行灰字占位；有快照但查询无命中时提示「回车清空查询」；否则用
    /// `ScrollArea::show_rows` 只渲染可见行（200 个窗口也走同一条路径，列表高度取卡片剩余
    /// 空间），行内容按排名结果回查快照（`RankedWindow.index`）→ 逐行画图标占位、标题与
    /// 进程名，关闭中的行置灰 → 单击把 `selection` 移到该行（只选中）→ 键盘移动后把选中行
    /// 滚到可视区中央 → 把鼠标双击的那一行交给 `activate`。行与行之间不画分隔线。
    fn show_list(&mut self, ui: &mut egui::Ui) {
        let palette = self.palette();
        if self.window_snapshot.is_empty() {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("没有可切换的窗口")
                    .size(TITLE_SIZE)
                    .color(palette.text_muted),
            );
            self.record_first_frame(0);
            return;
        }

        // 有快照但当前查询无命中：清空查询的动作在 `handle_list_keys` 里。
        if self.ranked.is_empty() {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("没有匹配的窗口（回车清空查询）")
                    .size(TITLE_SIZE)
                    .color(palette.text_muted),
            );
            self.record_first_frame(0);
            return;
        }

        let len = self.ranked.len();
        let selected = self.selection.index();
        // 滚入可视区只在键盘移动 / 刚唤起后需要一次，取走标记避免每帧都滚。
        let scroll_to_selected = std::mem::take(&mut self.scroll_to_selected);
        let mut double_clicked = None;
        // 列表吃掉卡片剩余高度：卡片本身铺满窗口，所以这里就是一个固定高度的可视区。
        let max_height = ui.available_height().max(ROW_HEIGHT);

        // 行与行之间不留间距 -> 行高严格等于 ROW_HEIGHT，show_rows 才能按行高定位。
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical()
            .id_salt("window_list")
            .max_height(max_height)
            .auto_shrink([false, false])
            .show_rows(ui, ROW_HEIGHT, len, |ui, range| {
                // 选中行不在本次渲染范围内（列表刚重置/跳转）时，按行高解析出它的位置再滚过去。
                if let (true, Some(sel)) = (scroll_to_selected, selected) {
                    if !range.contains(&sel) {
                        let offset = sel as f32 - range.start as f32;
                        let top = ui.max_rect().top() + offset * ROW_HEIGHT;
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(ui.max_rect().left(), top),
                            egui::vec2(ui.max_rect().width(), ROW_HEIGHT),
                        );
                        ui.scroll_to_rect(rect, Some(egui::Align::Center));
                    }
                }

                for row in range {
                    // 行号 -> 排名结果 -> 快照原下标；越界（快照刚被移除）直接跳过这一行。
                    let Some(window) = self.window_at(row) else {
                        continue;
                    };
                    // US-042：命中区间来自排名结果（标题与进程名各一份），空查询时两者都为空。
                    let no_ranges: &[Range<usize>] = &[];
                    let (title_ranges, process_ranges) = match self.ranked.get(row) {
                        Some(ranked) => (
                            ranked.title.ranges.as_slice(),
                            ranked.process.ranges.as_slice(),
                        ),
                        None => (no_ranges, no_ranges),
                    };
                    let closing = self.is_closing(window.id);
                    let is_selected = selected == Some(row);
                    // US-044：还没取回来的图标是 `None`，`draw_row` 会继续画占位方块。
                    let icon = self.icons.texture(window.id);
                    let response = draw_row(
                        ui,
                        RowStyle {
                            palette: &palette,
                            selected: is_selected,
                            closing,
                        },
                        row,
                        icon,
                        RowText {
                            text: &window.title,
                            ranges: title_ranges,
                        },
                        RowText {
                            text: &window.process_name,
                            ranges: process_ranges,
                        },
                    );

                    if is_selected && scroll_to_selected {
                        ui.scroll_to_rect(response.rect, Some(egui::Align::Center));
                    }
                    // 单击只选中（对齐原版 ListBox 行为），不触发切换。
                    if response.clicked() {
                        // `Selection` 没有单点 setter，`reset(row + 1, true)` 的
                        // pick_last 正好落到第 row 项。
                        self.selection.reset(row + 1, true);
                    }
                    if response.double_clicked() {
                        double_clicked = Some(row);
                    }
                }
            });

        // 首帧基准：热键回调 → 列表第一次真正画出。
        self.record_first_frame(len);

        if let Some(row) = double_clicked {
            self.activate(row, ui.ctx());
        }
    }

    /// 参数：`ctx` egui 上下文（读本帧的 [`egui::ViewportInfo`]，并下发 `OuterPosition`）
    /// 核心流程：取本帧 `ViewportInfo` 交给 [`centered_outer_position`] 算出居中后的左上角 →
    /// 拿到坐标就下发 [`egui::ViewportCommand::OuterPosition`] 并清掉待居中的标记 → 拿不到
    /// （首帧监视器信息为空 / 后端不报位置）就**不**设置位置，保持系统默认，只累加尝试次数，
    /// 未超过 [`CENTER_RETRY_MAX`] 时请求下一帧再试一次，绝不 panic。
    /// 返回 true 表示本帧已经下发了居中位置。
    fn center_on_current_monitor(&mut self, ctx: &egui::Context) -> bool {
        // `ViewportInfo` 只在帧内有效，克隆一份（每帧至多一次，拷贝成本可忽略）。
        let info = ctx.input(|input| input.viewport().clone());

        if let Some(pos) = centered_outer_position(&info) {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
            self.center_pending = false;
            self.center_tries = 0;
            log::info!(
                "浮层居中：监视器 {:?}，窗口 {:?}，左上角 ({:.0}, {:.0})",
                info.monitor_size,
                window_size_of(&info),
                pos.x,
                pos.y
            );
            return true;
        }

        self.center_tries = self.center_tries.saturating_add(1);
        if self.center_tries >= CENTER_RETRY_MAX {
            // 例如窗口被最小化、或后端不报位置：放弃居中，保持系统默认位置，下次唤起再试。
            self.center_pending = false;
            log::warn!("连续 {CENTER_RETRY_MAX} 次拿不到监视器信息，本次放弃居中");
            return false;
        }

        log::debug!("暂时拿不到监视器信息，本帧不居中，下一帧可见时再试");
        ctx.request_repaint();
        false
    }

    /// 参数：`ctx` egui 上下文
    /// 核心流程：可见性状态一变就走这一条入口——**变可见**时先清空上一次的筛选条件（查询串），
    /// 让紧接着的 `load_windows` → `refresh_ranked` 以空查询重算（列表回到全部窗口、选中第 1
    /// 项、滚回顶部），再把空快照重试次数复位、重新加载窗口快照（此刻浮层还没显示，前台仍是
    /// 用户原来的窗口），然后居中，最后才下发 `Visible(true)` + `Focus`，所以热键唤起、降级
    /// 直接显示和以后新增的入口都自动覆盖；
    /// **变隐藏**时只发 `Visible(false)`；状态没变则在「已可见但还没居中成功」时补一次居中。
    fn sync_window_visibility(&mut self, ctx: &egui::Context) {
        if self.applied_visible != Some(self.visible) {
            if self.visible {
                // 重新唤起从干净状态开始：先清查询再加载，随后的排名自然用的是空查询。
                // 清空必须留在这里，不能挪进 `load_windows` 内部——空快照重试与 Ctrl+W 关闭
                // 窗口后的刷新也会调用 `load_windows`，那些属于同一次唤起，要保留过滤条件。
                if should_reset_query(self.applied_visible, self.visible) && !self.query.is_empty()
                {
                    log::debug!("重新唤起：清空上一次的查询「{}」", self.query);
                    self.query.clear();
                }
                // 每次显示都重新加载 + 重新居中（US-024）：列表不会是上一次的残留，也不会是空表。
                self.empty_retry_left = EMPTY_SNAPSHOT_RETRY_MAX;
                self.empty_retry_due = None;
                self.load_windows(ctx);
                self.center_pending = true;
                self.center_tries = 0;
                // 先居中再显示：同一帧内两条命令按序下发，窗口不会先闪到系统默认位置。
                self.center_on_current_monitor(ctx);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                log::debug!("浮层已唤起");
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                log::debug!("浮层已收起");
            }

            self.applied_visible = Some(self.visible);
            // 显示之后需要再画一帧，否则窗口会以空白内容出现。
            ctx.request_repaint();
            return;
        }

        // 已经可见但上一帧没拿到监视器信息（例如首帧）：这一帧再补一次居中。
        if self.visible && self.center_pending {
            self.center_on_current_monitor(ctx);
        }
    }
}

/// 参数：`info` 本帧的 [`egui::ViewportInfo`]
/// 核心流程：优先用真实窗口的 `outer_rect` 尺寸——egui-winit 已经把它按 `pixels_per_point`
/// 折算成逻辑点，与 `OuterPosition` 的期望单位一致，所以这里**不能**再乘一次 DPI；
/// 拿不到或尺寸退化成 0 时退回 [`FALLBACK_WINDOW_SIZE`]（600×640 逻辑点），保证居中不会因为
/// 缺数据而失败。
fn window_size_of(info: &egui::ViewportInfo) -> egui::Vec2 {
    info.outer_rect
        .map(|rect| rect.size())
        .filter(|size| size.x > 0.0 && size.y > 0.0)
        .unwrap_or(FALLBACK_WINDOW_SIZE)
}

/// 参数：`window_min` 当前窗口左上角（逻辑点，桌面坐标系；拿不到时为 `None`）；
/// `monitor` 当前监视器尺寸；`window` 窗口尺寸
/// 核心流程：egui 0.35 的 [`egui::ViewportInfo`] 只给监视器**尺寸**、不给原点，所以这里用
/// 「虚拟桌面按监视器尺寸平铺」的口径近似：看窗口中心落在第几个「监视器格子」，据此反推
/// 当前监视器的左上角。单显示器（原点 0,0）以及常见的左右 / 上下等尺寸拼接都得到正确原点；
/// 拼接方式不规则时可能偏一格，调用方会把结果夹回监视器范围内，因此不会跑到屏幕外。
fn monitor_origin(
    window_min: Option<egui::Pos2>,
    monitor: egui::Vec2,
    window: egui::Vec2,
) -> egui::Pos2 {
    let Some(min) = window_min else {
        return egui::Pos2::ZERO;
    };

    let center = min + window / 2.0;
    let column = (center.x / monitor.x).floor();
    let row = (center.y / monitor.y).floor();
    egui::pos2(column * monitor.x, row * monitor.y)
}

/// 参数：`info` 本帧的 [`egui::ViewportInfo`]
/// 核心流程：监视器尺寸缺失（首帧 / 后端不报）返回 `None`，调用方据此保持系统默认位置；
/// 否则取窗口尺寸 → 用 [`monitor_origin`] 估算监视器左上角 → 左上角 + (监视器尺寸 − 窗口尺寸)/2
/// 得到居中位置 → 取整到逻辑像素（避免半像素抖动）→ 再夹回监视器范围内，防止窗口被推出屏幕。
fn centered_outer_position(info: &egui::ViewportInfo) -> Option<egui::Pos2> {
    let monitor = info.monitor_size?;
    if monitor.x <= 0.0 || monitor.y <= 0.0 {
        return None;
    }

    let window = window_size_of(info);
    let origin = monitor_origin(info.outer_rect.map(|rect| rect.min), monitor, window);
    // 窗口比监视器还大时剩余空间按 0 处理，居中退化成左上角对齐。
    let slack = (monitor - window).max(egui::Vec2::ZERO);
    let centered = (origin + slack / 2.0).round();
    Some(centered.clamp(origin, origin + slack))
}

/// 参数：`windows` 窗口快照；`ranked` 当前排名结果；`id` 要找的窗口标识
/// 核心流程：先按 id 在快照里定位原始下标，再在排名结果里反查它当前的行号；
/// 窗口已被移除（或没命中当前查询）时返回 `None`，调用方据此保留 `Selection::clamp` 的结果。
fn ranked_row_of(windows: &[WindowInfo], ranked: &[RankedWindow], id: WindowId) -> Option<usize> {
    let index = windows.iter().position(|window| window.id == id)?;
    ranked.iter().position(|ranked| ranked.index == index)
}

/// 一段要渲染的文本：原文 + 命中区间。
///
/// 参数：`text` 原文（标题或进程名）；`ranges` 命中字符的字节区间（空切片 = 全段不加粗）
/// 核心流程：把「文本」与「哪些字要加粗」绑在一起传给 [`draw_row`]，让行渲染函数既保持
/// 参数个数精简，又不必依赖 core 的匹配结构体。
#[derive(Clone, Copy)]
struct RowText<'a> {
    text: &'a str,
    ranges: &'a [Range<usize>],
}

/// 单行的绘制上下文：主题配色 + 交互状态。
///
/// 参数：无（纯数据）
/// 核心流程：把配色与「选中 / 关闭中」两个状态打包传给 [`draw_row`]（与 [`RowText`] 同一思路），
/// 行渲染的参数个数因此不会随主题字段增加而膨胀。
#[derive(Clone, Copy)]
struct RowStyle<'a> {
    /// 当前主题的一套颜色。
    palette: &'a Palette,
    /// 是否选中。
    selected: bool,
    /// 是否正在关闭（置灰）。
    closing: bool,
}

/// 参数：`text` 原文；`ranges` 命中字节区间（来自 core 的 `MatchResult::ranges`）；
/// `base` 普通段的字体与颜色；`bold` 命中段要用的字体
/// 核心流程：按区间把文本切成「普通段 / 加粗段」，逐段 `LayoutJob::append` 拼装——命中段
/// 用 `bold` 字体（egui 没有合成粗体，只能换字体族），其余用 `base`；越界、落在字符中间、
/// 乱序或重叠的区间整段跳过（退回普通文本），`<`、`&` 在 `LayoutJob` 里只是普通字符。
fn highlight_job(
    text: &str,
    ranges: &[Range<usize>],
    base: (&egui::FontId, egui::Color32),
    bold: &egui::FontId,
) -> LayoutJob {
    let plain = egui::TextFormat::simple(base.0.clone(), base.1);
    let strong = egui::TextFormat::simple(bold.clone(), base.1);
    let mut job = LayoutJob::default();
    // 已渲染到的字节位置：同时挡掉乱序与重叠的区间（它们会让分片不连续）。
    let mut cursor = 0;

    for range in ranges {
        let (start, end) = (range.start, range.end);
        // 空区间 / 越界 / 非字符边界 / 与已渲染部分重叠（含乱序）一律跳过，绝不切片 panic。
        if start >= end
            || end > text.len()
            || start < cursor
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            continue;
        }
        if cursor < start {
            job.append(&text[cursor..start], 0.0, plain.clone());
        }
        job.append(&text[start..end], 0.0, strong.clone());
        cursor = end;
    }

    // 尾段（以及完全没命中 / 空文本的情况）补一段普通文本，保证分片覆盖全文。
    if cursor < text.len() || job.sections.is_empty() {
        job.append(&text[cursor..], 0.0, plain);
    }
    job
}

/// 参数：`ui` 列表内 UI；`style` 本行的配色与状态（见 [`RowStyle`]）；`row` 行号（用来生成
/// 稳定的交互 id）；`icon` 该窗口的真实图标（还没取回来 / 没有图标时为 `None`）；
/// `title` 标题原文与命中区间；`process` 进程名原文与命中区间
/// 核心流程：分配整行矩形并画选中 / 悬停底色（选中再加 1px 描边，行间不画分隔线）→
/// 用 `right_to_left` 先把进程名贴到最右（11 号次要色小字）→ 剩余空间切回 `left_to_right`，
/// 依次放 16×16 图标与 13 号正文标题（超长截断）→ 最后用同一个矩形注册整行点击：
/// 后注册的交互优先，双击不会被行内文字抢走。图标有纹理时按 [`crate::icons::fit_icon_size`]
/// 等比缩放画进 16×16 方框，没有纹理时画主题的占位方块。两段文本都由 [`highlight_job`]
/// 拼装，命中字符加粗；关闭中的条目整体降饱和：颜色换成 `palette.closing_text` 且清空命中
/// 区间（不再加粗），图标同时整体变淡。
fn draw_row(
    ui: &mut egui::Ui,
    style: RowStyle<'_>,
    row: usize,
    icon: Option<&egui::TextureHandle>,
    title: RowText<'_>,
    process: RowText<'_>,
) -> egui::Response {
    let palette = style.palette;
    let selected = style.selected;
    let closing = style.closing;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, ROW_HEIGHT), egui::Sense::hover());

    // 选中 / 悬停底色：选中用主题的选中填充 + 1px 描边，悬停只给一档悬停填充。
    if selected {
        ui.painter()
            .rect_filled(rect, ROW_RADIUS, palette.row_selected_fill);
        ui.painter().rect_stroke(
            rect,
            ROW_RADIUS,
            egui::Stroke::new(1.0, palette.row_selected_stroke),
            egui::StrokeKind::Inside,
        );
    } else if ui.rect_contains_pointer(rect) {
        ui.painter()
            .rect_filled(rect, ROW_RADIUS, palette.row_hover_fill);
    }

    // 关闭中整体降饱和：标题与进程名换成更淡的颜色，并且只渲染普通文本。
    let title_color = if closing {
        palette.closing_text
    } else {
        palette.text_primary
    };
    let process_color = if closing {
        palette.closing_text
    } else {
        palette.text_muted
    };
    let icon_color = if closing {
        palette.closing_icon
    } else {
        palette.icon_placeholder
    };
    let no_ranges: &[Range<usize>] = &[];
    let title_ranges = if closing { no_ranges } else { title.ranges };
    let process_ranges = if closing { no_ranges } else { process.ranges };

    // 字体：标题 13 号、进程名 11 号；命中段换到 `bold` 族（同一字号、同一字色）。
    let title_font = egui::FontId::proportional(TITLE_SIZE);
    let process_font = egui::FontId::proportional(PROCESS_SIZE);
    let bold_family = crate::fonts::bold_family();
    let title_bold = egui::FontId::new(TITLE_SIZE, bold_family.clone());
    let process_bold = egui::FontId::new(PROCESS_SIZE, bold_family);
    let title_job = highlight_job(
        title.text,
        title_ranges,
        (&title_font, title_color),
        &title_bold,
    );
    let process_job = highlight_job(
        process.text,
        process_ranges,
        (&process_font, process_color),
        &process_bold,
    );

    // 行内三列：右侧进程名（right_to_left）+ 左侧图标与标题（left_to_right）。
    let inner = rect.shrink2(egui::vec2(ROW_PADDING_X, 0.0));
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    row_ui.add(egui::Label::new(process_job).truncate().selectable(false));
    row_ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let (icon_rect, _) =
            ui.allocate_exact_size(egui::vec2(ICON_SIZE, ICON_SIZE), egui::Sense::hover());
        match icon {
            // US-044：真图标——等比缩放到 16×16 方框内居中绘制，关闭中的条目整体变淡。
            Some(texture) => {
                let size = crate::icons::fit_icon_size(texture.size_vec2(), ICON_SIZE);
                let tint = if closing {
                    egui::Color32::from_white_alpha(CLOSING_ICON_ALPHA)
                } else {
                    // 白色 = 不着色，真图标按原样画。
                    egui::Color32::WHITE
                };
                if size.x > 0.0 && size.y > 0.0 {
                    ui.painter().image(
                        texture.id(),
                        egui::Rect::from_center_size(icon_rect.center(), size),
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        tint,
                    );
                }
            }
            // 图标还没回来（或这个窗口没有图标）：继续画主题的圆角方块占位。
            None => {
                ui.painter().rect_filled(icon_rect, 3.0, icon_color);
            }
        }
        ui.add_space(COLUMN_GAP);
        ui.add(egui::Label::new(title_job).truncate().selectable(false));
    });

    ui.interact(
        rect,
        ui.id().with(("window_row", row)),
        egui::Sense::click(),
    )
}

impl eframe::App for SwitcherooApp {
    /// 参数：无
    /// 核心流程：把窗口清成全透明，只有卡片区域不透明，让浮层看起来是悬浮的。
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    /// 参数：`ctx` egui 上下文；`_frame` eframe 帧句柄（本页未用到）
    /// 核心流程：`logic` 在窗口隐藏时也会被调用，所以状态机放在这里：先非阻塞收割托盘命令
    /// （US-048）→ 录制态下本帧按键只喂给热键录制（US-047），消费掉就直接同步窗口状态收工 →
    /// 处理退出快捷键 → `Ctrl+,` 打开选项面板、`Ctrl+T` 切换明暗 → 消费热键标志翻转期望可见
    /// 状态（浮层打开时要丢弃这次切换，真正的加载、居中与显示由最后的 `sync_window_visibility`
    /// 统一完成）→ 处理 `Esc`（面板打开时只关面板）→ 空快照兜底重试与一次关闭轮询（US-043）→
    /// 把后台线程解好的图标收进缓存（US-044）→ 最后把最终状态同步给窗口系统。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 0) 托盘命令：非阻塞收割，命令只改状态，不发窗口命令（留给最后的 sync 统一生效）。
        self.poll_tray_commands(ctx);

        // 0.5) 录制优先：录制态下这一帧的按键只属于热键录制，不再当快捷键解释。
        if self.capture_record(ctx) {
            self.sync_window_visibility(ctx);
            return;
        }

        // 1) 退出：托盘与退出菜单之外，键盘仍然用 Ctrl+Q 结束进程。
        let quit = ctx.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::Q));
        if quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // 2) Ctrl+,：直接打开选项面板（面板已经开着就不再重复初始化草稿）。
        let open_options =
            ctx.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::Comma));
        if open_options && !self.options_open {
            self.open_options(ctx);
        }

        // 3) Ctrl+T：快速切换明暗。与选项面板的 theme 同步写盘，避免「临时切换」和配置打架；
        //    面板打开时交给面板里的三选一，这里不再响应。
        let toggle_theme = ctx.input(|input| {
            input.modifiers.ctrl && input.key_pressed(egui::Key::T) && !self.options_open
        });
        if toggle_theme {
            let dark = !self.dark;
            self.set_dark(ctx, dark);
            self.settings.theme = if dark {
                ThemeSetting::Dark
            } else {
                ThemeSetting::Light
            };
            let snapshot = self.settings.clone();
            self.persist_settings(&snapshot, "快捷键：切换明暗");
        }

        // 4) 热键回调只置位标志，真正的显示切换必须在 UI 线程做；这里只翻转期望状态，
        //    加载快照（此刻前台还是用户原来的窗口）与居中都在 `sync_window_visibility` 里
        //    跟着「从隐藏变可见」一起做，任何入口都不会漏掉加载。面板打开时丢弃这次切换，
        //    免得用户正在填表时浮层被收走。
        if self.toggle_requested.swap(false, Ordering::SeqCst) {
            if self.options_open {
                log::debug!("选项面板打开中，忽略本次全局热键");
            } else if self.visible {
                self.hide(ctx);
            } else {
                self.visible = true;
            }
        }

        // 5) Esc：面板打开时只关面板（回到列表），否则收起浮层并清掉本次唤起的选中项。
        if self.visible && ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            if self.options_open {
                self.cancel_options(ctx);
            } else {
                self.hide(ctx);
            }
        }

        // 6) 空快照兜底：到点后再枚举一次（最多两次），避免刚开机 / 刚登录时用户看到空列表。
        if self.visible {
            if let Some(due) = self.empty_retry_due {
                if Instant::now() >= due {
                    self.empty_retry_due = None;
                    log::info!("空快照重试：重新枚举窗口");
                    self.load_windows(ctx);
                }
            }
        }

        // 7) 关闭轮询：只在有「关闭中」条目时真正探测；集合在隐藏 / 退出时已清空，
        //    所以不会有残留的轮询任务，也不需要另开线程。
        self.poll_closing(ctx);

        // 8) 图标：把后台线程已经解好的位图不阻塞地收进纹理缓存；有新图标就请求重绘，
        //    本帧/下一帧的列表就能把占位方块换成真图标。没有新图标时这一步是空转。
        if self.icons.poll(ctx) > 0 {
            ctx.request_repaint();
        }

        // 9) 下发窗口命令（窗口隐藏时按热键同样会走到这里）。
        self.sync_window_visibility(ctx);
    }

    /// 参数：`ui` eframe 给出的根 UI；`_frame` eframe 帧句柄（本页未用到）
    /// 核心流程：本帧已发起隐藏时直接返回，不再画列表（否则会闪一帧空列表）→ 面板没开时先处理
    /// 列表按键（必须早于输入框渲染，否则 Tab / 回车会被 TextEdit 抢走，且回车切换成功后同样
    /// 直接返回；面板打开时按键归面板，列表选中项不动）→ 在透明窗口里画一张铺满可用区域的卡片
    /// （底色随主题）：面板打开时整张卡片换成选项面板，否则是「出错时才有的一行红字 → 搜索框与
    /// `?` 按钮 → 展开时的快捷键浮层 → 窗口列表」。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 热键收起 / Esc 已在 `logic` 里发起隐藏：本帧不画任何内容。
        if std::mem::take(&mut self.skip_render_this_frame) {
            return;
        }

        if !self.options_open {
            self.handle_list_keys(ui.ctx());
        }

        // 回车切换成功会在按键处理里收起浮层并清空快照：同帧同样不再继续画空列表。
        if std::mem::take(&mut self.skip_render_this_frame) {
            return;
        }

        let palette = self.palette();
        egui::Frame::new()
            .fill(palette.card_fill)
            .stroke(egui::Stroke::new(1.0, palette.card_stroke))
            .corner_radius(egui::CornerRadius::same(CARD_RADIUS))
            .outer_margin(egui::Margin::same(CARD_OUTER_MARGIN))
            .inner_margin(egui::Margin::same(CARD_INNER_MARGIN))
            .show(ui, |ui| {
                // 卡片吃掉整窗可用空间，外边距之外保持透明，看起来才像悬浮的卡片。
                ui.take_available_space();

                // 选项面板与搜索框 / 列表互斥：同一时刻只有一套控件在抢焦点与按键。
                if self.options_open {
                    self.show_options_panel(ui);
                    return;
                }

                self.show_errors(ui);
                let search_rect = self.show_search_row(ui);
                ui.add_space(SEARCH_LIST_GAP);
                if self.show_help {
                    self.show_help_panel(ui, search_rect);
                }
                // 帮助面板里的「选项…」可能在本帧中途打开了选项面板：这一帧就不再画列表，
                // 免得面板和列表挤在同一帧里闪一下。
                if self.options_open {
                    return;
                }
                self.show_list(ui);
            });

        // 双击发生在列表已经画完之后，标记只对当帧有效，不能留给下一帧。
        self.skip_render_this_frame = false;
    }
}

/// 参数：无
/// 核心流程：先清空关闭中集合（进程退出后不再需要轮询），再显式取走并释放托盘（其 Drop 会
/// 摘掉通知区图标、销毁隐藏窗口并 join 托盘线程），接着显式取走并释放热键监听
/// （其 Drop 会向热键线程投递退出消息并 join，所以即使窗口处于隐藏状态退出也不会留下
/// 孤儿线程），最后释放单实例互斥量。
impl Drop for SwitcherooApp {
    fn drop(&mut self) {
        self.closing.clear();
        let _tray = self.tray.take();
        let _hotkey = self.hotkey.take();
        let _single_instance = self.single_instance.take();
        log::info!("Switcheroo 退出：关闭轮询已停止，托盘、热键监听与单实例互斥量已释放");
    }
}

#[cfg(test)]
mod tests {
    // 命中区间天然可能是单元素（对应 core 的 `MatchResult::ranges`），`&[0..6]` 就是真实取值；
    // clippy 的 `single_range_in_vec_init` 针对的是把 `[a; n]` 误写成 `vec![a..b]` 的笔误。
    #![allow(clippy::single_range_in_vec_init)]

    use super::*;
    // 顶部只导入 UI 侧要用的 `rank_windows_with_foreground`，单测仍按旧签名断言排名顺序。
    use switcheroo_core::rank_windows;

    /// 构造一个窗口：句柄按序号递增，标题与进程名由参数给出。
    fn window(raw: u64, title: &str, process_name: &str) -> WindowInfo {
        WindowInfo::new(WindowId::new(raw), title, process_name)
    }

    /// 居中坐标：单显示器（原点 0,0）与右侧副屏都按「监视器左上角 + 剩余空间的一半」算。
    #[test]
    fn centered_position_uses_monitor_grid() {
        let on_primary = egui::ViewportInfo {
            monitor_size: Some(egui::vec2(1920.0, 1080.0)),
            outer_rect: Some(egui::Rect::from_min_size(
                egui::pos2(100.0, 50.0),
                egui::vec2(600.0, 640.0),
            )),
            ..Default::default()
        };
        // (1920 - 600) / 2 = 660，(1080 - 640) / 2 = 220。
        assert_eq!(
            centered_outer_position(&on_primary),
            Some(egui::pos2(660.0, 220.0))
        );

        // 窗口落在右侧第二块（x ≥ 1920）时原点按 1920 算，仍然在第二块里居中。
        let on_secondary = egui::ViewportInfo {
            outer_rect: Some(egui::Rect::from_min_size(
                egui::pos2(2000.0, 100.0),
                egui::vec2(600.0, 640.0),
            )),
            ..on_primary.clone()
        };
        assert_eq!(
            centered_outer_position(&on_secondary),
            Some(egui::pos2(2580.0, 220.0))
        );
    }

    /// 缺数据时只降级、不 panic：没有监视器信息就保持系统默认位置，没有窗口尺寸就用 600×640。
    #[test]
    fn centered_position_degrades_without_monitor_info() {
        // 首帧还没有监视器信息：返回 None，调用方不设置位置。
        assert_eq!(
            centered_outer_position(&egui::ViewportInfo::default()),
            None
        );

        // 监视器尺寸为 0（后端异常）：同样放弃，不做除零计算。
        let zero_monitor = egui::ViewportInfo {
            monitor_size: Some(egui::Vec2::ZERO),
            ..Default::default()
        };
        assert_eq!(centered_outer_position(&zero_monitor), None);

        // 没有 outer_rect：窗口尺寸退回 600×640 的逻辑尺寸。
        let no_window = egui::ViewportInfo {
            monitor_size: Some(egui::vec2(1920.0, 1080.0)),
            ..Default::default()
        };
        assert_eq!(
            centered_outer_position(&no_window),
            Some(egui::pos2(660.0, 220.0))
        );

        // 窗口比监视器还大：剩余空间按 0 处理，位置被夹回监视器内，不会算出负坐标。
        let big_window = egui::ViewportInfo {
            monitor_size: Some(egui::vec2(800.0, 600.0)),
            outer_rect: Some(egui::Rect::from_min_size(
                egui::pos2(10.0, 10.0),
                egui::vec2(600.0, 640.0),
            )),
            ..Default::default()
        };
        assert_eq!(
            centered_outer_position(&big_window),
            Some(egui::pos2(100.0, 0.0))
        );
    }

    /// 行号必须跟着排名结果走：空查询是快照顺序，有查询时只剩命中项。
    #[test]
    fn ranked_row_follows_filter_order() {
        let windows = vec![
            window(1, "alpha", "a.exe"),
            window(2, "beta", "b.exe"),
            window(3, "gamma", "c.exe"),
        ];

        let ranked = rank_windows("", &windows);
        assert_eq!(ranked_row_of(&windows, &ranked, WindowId::new(2)), Some(1));

        // 只有 beta 命中：它的行号变成 0，其余窗口在当前查询下没有行号。
        let ranked = rank_windows("beta", &windows);
        assert_eq!(ranked_row_of(&windows, &ranked, WindowId::new(2)), Some(0));
        assert_eq!(ranked_row_of(&windows, &ranked, WindowId::new(1)), None);
    }

    /// 关闭选中行后的选中位置：优先下一条，没有下一条时落到上一条。
    #[test]
    fn closing_selected_row_picks_next_then_previous() {
        let mut windows = vec![
            window(1, "alpha", "a.exe"),
            window(2, "beta", "b.exe"),
            window(3, "gamma", "c.exe"),
        ];
        let mut ranked = rank_windows("", &windows);
        let mut selection = Selection::new(ranked.len());
        selection.next(ranked.len());
        assert_eq!(selection.index(), Some(1));

        // 关掉行号 1 的 beta：重算排名后 clamp 保住行号 1，落到原来的第 3 项（下一条）。
        windows.retain(|w| w.id != WindowId::new(2));
        ranked = rank_windows("", &windows);
        selection.clamp(ranked.len());
        assert_eq!(selection.index(), Some(1));
        assert_eq!(windows[1].id, WindowId::new(3));

        // 再关掉末项 gamma：没有下一条，clamp 收敛到新的末项（上一条 alpha）。
        windows.retain(|w| w.id != WindowId::new(3));
        ranked = rank_windows("", &windows);
        selection.clamp(ranked.len());
        assert_eq!(selection.index(), Some(0));
        assert_eq!(windows[0].id, WindowId::new(1));
    }

    /// 测试用的普通字体与加粗字体（只用于构造 LayoutJob，不需要真的注册字体族）。
    fn test_fonts() -> (egui::FontId, egui::FontId) {
        (
            egui::FontId::proportional(14.0),
            egui::FontId::new(14.0, egui::FontFamily::Name("bold".into())),
        )
    }

    /// 把 LayoutJob 的分片还原成 `(文本片段, 是否加粗)`，便于逐段断言。
    fn segments<'a>(job: &'a LayoutJob, bold: &egui::FontId) -> Vec<(&'a str, bool)> {
        job.sections
            .iter()
            .map(|section| {
                let text = &job.text[section.byte_range.start.0..section.byte_range.end.0];
                (text, section.format.font_id == *bold)
            })
            .collect()
    }

    /// 普通命中：区间把文本切成「普通 / 加粗 / 普通」三段，且文本内容不被改写。
    #[test]
    fn highlight_job_splits_matched_range() {
        let (plain, bold) = test_fonts();
        let job = highlight_job(
            "文件 - PowerPoint",
            &[0..6],
            (&plain, egui::Color32::WHITE),
            &bold,
        );
        assert_eq!(job.text, "文件 - PowerPoint");
        assert_eq!(
            segments(&job, &bold),
            vec![("文件", true), (" - PowerPoint", false)]
        );

        // 中间命中：两侧各留一段普通文本。
        let job = highlight_job("abcde", &[1..3], (&plain, egui::Color32::WHITE), &bold);
        assert_eq!(
            segments(&job, &bold),
            vec![("a", false), ("bc", true), ("de", false)]
        );

        // 未命中（空区间）：整段普通文本，只有一段。
        let job = highlight_job("abcde", &[], (&plain, egui::Color32::WHITE), &bold);
        assert_eq!(segments(&job, &bold), vec![("abcde", false)]);
    }

    /// `<` / `&` 在 LayoutJob 里是普通文本：既不当作标记吞掉，也不改变原文。
    #[test]
    fn highlight_job_keeps_special_characters_literal() {
        let (plain, bold) = test_fonts();
        let text = "a<b>&c</b>";
        let job = highlight_job(text, &[2..3], (&plain, egui::Color32::WHITE), &bold);
        // 文本原样进入 job.text，没有被解析成标记。
        assert_eq!(job.text, text);
        assert_eq!(
            segments(&job, &bold),
            vec![("a<", false), ("b", true), (">&c</b>", false)]
        );

        // 命中段正好是 `<` 本身：也照常加粗，不触发任何转义。
        let job = highlight_job("<&>", &[0..1], (&plain, egui::Color32::WHITE), &bold);
        assert_eq!(segments(&job, &bold), vec![("<", true), ("&>", false)]);
    }

    /// 越界、非字符边界、乱序的区间一律被忽略（退回普通文本），绝不 panic。
    #[test]
    fn highlight_job_ignores_invalid_ranges() {
        let (plain, bold) = test_fonts();

        // 完全越界与末尾越界：都不产生加粗段。
        let job = highlight_job("abc", &[0..99, 3..4], (&plain, egui::Color32::WHITE), &bold);
        assert_eq!(job.text, "abc");
        assert_eq!(segments(&job, &bold), vec![("abc", false)]);

        // 落在多字节字符中间（「中」占 3 字节，1..2 不是字符边界）：整段跳过。
        let job = highlight_job("中文", &[1..2], (&plain, egui::Color32::WHITE), &bold);
        assert_eq!(segments(&job, &bold), vec![("中文", false)]);

        // 乱序 / 重叠 + 一个合法区间：合法的那段照常加粗，出错的被跳过。
        let job = highlight_job(
            "abcdef",
            &[1..3, 0..2, 5..99],
            (&plain, egui::Color32::WHITE),
            &bold,
        );
        assert_eq!(
            segments(&job, &bold),
            vec![("a", false), ("bc", true), ("def", false)]
        );

        // 空文本 + 越界区间：不 panic，分片为空文本。
        let job = highlight_job("", &[0..5], (&plain, egui::Color32::WHITE), &bold);
        assert_eq!(job.text, "");
        assert_eq!(segments(&job, &bold), vec![("", false)]);
    }

    /// 端到端冒烟：在真实字体的 egui 上下文里排版高亮 job 不 panic，且能排出一行。
    /// 这条测试同时覆盖 `bold` 字体族注册（未注册的族会让 epaint panic）与截断前的排版路径。
    #[test]
    fn highlight_job_layouts_with_real_fonts() {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        // 普通段用默认比例族、命中段用 `bold` 族：与真实渲染时的字体组合一致。
        let plain = egui::FontId::proportional(14.0);
        let bold = egui::FontId::new(14.0, crate::fonts::bold_family());
        let job = highlight_job("文件 abc", &[0..6], (&plain, egui::Color32::WHITE), &bold);
        let mut rows = 0;
        // 只借 `run_ui` 把字体系统初始化起来；返回的绘制输出这里用不到。
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            rows = ui
                .ctx()
                .fonts_mut(|fonts| fonts.layout_job(job.clone()).rows.len());
        });
        assert!(rows > 0, "高亮 job 应能排版出至少一行");
    }

    /// 暗色卡片必须比浅色暗：否则「深色模式」只是换了描边，内容区仍是白底。
    #[test]
    fn palette_dark_card_is_darker_than_light() {
        let luma = |color: egui::Color32| {
            u32::from(color.r()) + u32::from(color.g()) + u32::from(color.b())
        };
        assert!(luma(palette(true).card_fill) < luma(palette(false).card_fill));
    }

    /// 选中描边在两套配色里必须不同：暗色下沿用浅蓝描边会糊在深底上，看不出选中行。
    #[test]
    fn palette_selected_stroke_differs_between_themes() {
        assert_ne!(
            palette(true).row_selected_stroke,
            palette(false).row_selected_stroke
        );
    }

    /// 构造一个按键事件：录制逻辑只关心 `key` / `pressed` / `modifiers`。
    fn key_event(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        }
    }

    /// 按键到虚拟键码：字母 / 数字 / 功能键三段连续区按固定起点换算，修饰键与合成键不录制。
    #[test]
    fn vk_code_maps_letters_digits_and_function_keys() {
        assert_eq!(vk_code(egui::Key::A), Some(0x41));
        assert_eq!(vk_code(egui::Key::Z), Some(0x5A));
        assert_eq!(vk_code(egui::Key::Num0), Some(0x30));
        assert_eq!(vk_code(egui::Key::Num9), Some(0x39));
        assert_eq!(vk_code(egui::Key::F1), Some(0x70));
        assert_eq!(vk_code(egui::Key::F12), Some(0x7B));
        assert_eq!(vk_code(egui::Key::Space), Some(0x20));

        // 修饰键本身、以及 Copy / Cut / Paste 这类合成键都不能当全局热键。
        assert_eq!(vk_code(egui::Key::ShiftLeft), None);
        assert_eq!(vk_code(egui::Key::ControlRight), None);
        assert_eq!(vk_code(egui::Key::Copy), None);
    }

    /// 格式化：修饰键顺序固定 Ctrl → Shift → Alt → Win，未知键码退化成十六进制。
    #[test]
    fn hotkey_text_orders_modifiers_and_falls_back() {
        let alt_space = HotkeySetting {
            key: 0x20,
            modifiers: MOD_ALT,
        };
        assert_eq!(hotkey_text(&alt_space), "Alt + Space");

        let everything = HotkeySetting {
            key: 0x41,
            modifiers: MOD_ALT | MOD_CONTROL | MOD_SHIFT | MOD_WIN,
        };
        assert_eq!(hotkey_text(&everything), "Ctrl + Shift + Alt + Win + A");

        // 组合键为空时只剩主键；键码不认识时给十六进制，界面照样看得懂。
        assert_eq!(
            hotkey_text(&HotkeySetting {
                key: 0x0D,
                modifiers: 0,
            }),
            "Enter"
        );
        assert_eq!(
            hotkey_text(&HotkeySetting {
                key: 0x07,
                modifiers: MOD_CONTROL,
            }),
            "Ctrl + 键码 0x07"
        );
    }

    /// egui 按键 + 修饰键 → `HotkeySetting`：必须有修饰键，Win 位单独由 Super 键补。
    #[test]
    fn hotkey_from_key_requires_modifier_and_adds_win_bit() {
        // 光秃稚的普通键不做全局热键。
        assert_eq!(
            hotkey_from_key(egui::Key::Space, egui::Modifiers::NONE, false),
            None
        );

        assert_eq!(
            hotkey_from_key(egui::Key::Space, egui::Modifiers::ALT, false),
            Some(HotkeySetting {
                key: 0x20,
                modifiers: MOD_ALT,
            })
        );

        // Windows 上 Win 键不会进 `Modifiers`，只能靠 Super 键的按下状态补 `MOD_WIN`。
        assert_eq!(
            hotkey_from_key(egui::Key::Num1, egui::Modifiers::CTRL, true),
            Some(HotkeySetting {
                key: 0x31,
                modifiers: MOD_CONTROL | MOD_WIN,
            })
        );

        // 修饰键本身永远折算不出结果。
        assert_eq!(
            hotkey_from_key(egui::Key::AltLeft, egui::Modifiers::ALT, false),
            None
        );
    }

    /// 录制：组合键生效；单独 Tab / 修饰键继续等；Esc 取消；Win 键先按后组合也能录到。
    #[test]
    fn record_input_captures_skips_and_cancels() {
        // 只按修饰键 / 单独按 Tab：都不改变显示，继续等。
        let mut input = RecordInput::default();
        assert_eq!(
            input.feed(&[key_event(
                egui::Key::ShiftLeft,
                true,
                egui::Modifiers::SHIFT
            )]),
            Recorded::Pending
        );
        assert_eq!(
            input.feed(&[key_event(egui::Key::Tab, true, egui::Modifiers::NONE)]),
            Recorded::Pending
        );

        // Alt+Space：直接录到。
        assert_eq!(
            input.feed(&[key_event(egui::Key::Space, true, egui::Modifiers::ALT)]),
            Recorded::Captured(HotkeySetting {
                key: 0x20,
                modifiers: MOD_ALT,
            })
        );

        // Ctrl+W：同样能录到（录制优先于「关闭窗口」）。
        assert_eq!(
            input.feed(&[key_event(egui::Key::W, true, egui::Modifiers::CTRL)]),
            Recorded::Captured(HotkeySetting {
                key: 0x57,
                modifiers: MOD_CONTROL,
            })
        );

        // 先按下 Win，再按 Ctrl+1：录到的是 Ctrl + Win + 1。
        assert_eq!(
            input.feed(&[key_event(egui::Key::SuperLeft, true, egui::Modifiers::NONE)]),
            Recorded::Pending
        );
        assert_eq!(
            input.feed(&[key_event(egui::Key::Num1, true, egui::Modifiers::CTRL)]),
            Recorded::Captured(HotkeySetting {
                key: 0x31,
                modifiers: MOD_CONTROL | MOD_WIN,
            })
        );

        // Esc 取消录制（松开的 Esc 不算，只看按下）。
        assert_eq!(
            input.feed(&[key_event(egui::Key::Escape, false, egui::Modifiers::NONE)]),
            Recorded::Pending
        );
        assert_eq!(
            input.feed(&[key_event(egui::Key::Escape, true, egui::Modifiers::NONE)]),
            Recorded::Cancel
        );
    }

    /// 主题：`System` 跟随系统，托盘切换永远在浅 / 深之间翻面。
    #[test]
    fn theme_resolution_and_toggle() {
        assert!(dark_for(ThemeSetting::System, true));
        assert!(!dark_for(ThemeSetting::System, false));
        assert!(!dark_for(ThemeSetting::Light, true));
        assert!(dark_for(ThemeSetting::Dark, false));

        // 跟随系统且系统是深色 -> 切到浅色；再切回去就是深色。
        assert_eq!(
            toggled_theme(ThemeSetting::System, true),
            ThemeSetting::Light
        );
        assert_eq!(
            toggled_theme(ThemeSetting::System, false),
            ThemeSetting::Dark
        );
        assert_eq!(
            toggled_theme(ThemeSetting::Light, false),
            ThemeSetting::Dark
        );
        assert_eq!(
            toggled_theme(ThemeSetting::Dark, false),
            ThemeSetting::Light
        );
    }

    /// 自启以注册表为准：不一致才修正，一致时不动（避免每次启动都写一次盘）。
    #[test]
    fn autostart_follows_registry() {
        let mut settings = Settings {
            autostart: false,
            ..Settings::default()
        };
        assert!(reconcile_autostart(&mut settings, true));
        assert!(settings.autostart);

        // 已经一致：不修正。
        assert!(!reconcile_autostart(&mut settings, true));

        // 反过来也一样：配置文件说开、注册表说关，以注册表为准。
        assert!(reconcile_autostart(&mut settings, false));
        assert!(!settings.autostart);
    }

    /// 冲突回滚：改了组合键必须先试注册，注册失败就不落盘、草稿退回旧值；组合键没变且旧监听
    /// 还在时连注册都不做（否则会撞上自己占着的同一个键）；关掉热键则只释放旧监听。
    #[test]
    fn hotkey_conflict_rolls_back_before_persist() {
        let saved = Settings::default();

        // 1) 组合键变了：必须试注册。
        let mut draft = Settings {
            hotkey: HotkeySetting {
                key: 0x41,
                modifiers: MOD_CONTROL,
            },
            ..saved.clone()
        };
        let plan = plan_hotkey_rebind(&saved, &draft, true);
        assert_eq!(plan, HotkeyPlan::Register);
        // 注册失败 -> 回滚（不写配置）；注册成功 -> 落盘。
        assert_eq!(decide_after_register(plan, false), SaveDecision::Rollback);
        assert_eq!(decide_after_register(plan, true), SaveDecision::Persist);

        // 2) 组合键没变且旧监听在：旧监听已经占着它，不再注册，直接落盘。
        let plan = plan_hotkey_rebind(&saved, &saved, true);
        assert_eq!(plan, HotkeyPlan::Keep);
        assert_eq!(decide_after_register(plan, true), SaveDecision::Persist);

        // 3) 组合键没变但旧监听不在（启动时就没注册上）：同一个组合键也要重新试一次。
        assert_eq!(
            plan_hotkey_rebind(&saved, &saved, false),
            HotkeyPlan::Register
        );

        // 4) 关掉热键：不注册，落盘后才释放旧监听。
        draft.enable_hotkey = false;
        let plan = plan_hotkey_rebind(&saved, &draft, true);
        assert_eq!(plan, HotkeyPlan::Release);
        assert_eq!(decide_after_register(plan, true), SaveDecision::Persist);
    }

    /// 启动可见性：只有「没有任何唤起入口」时浮层才自己冒出来。
    #[test]
    fn start_visibility_covers_degraded_cases() {
        // 热键正常：启动常驻后台。
        assert!(!should_start_visible(true, true, true, false));
        assert!(!should_start_visible(true, true, false, false));
        // 热键要开却注册失败：直接显示并把原因写在界面上。
        assert!(should_start_visible(true, false, true, false));
        // 热键关掉但托盘可用：仍可常驻，靠托盘「打开」。
        assert!(!should_start_visible(false, false, true, false));
        // 热键关掉又没有托盘：没有任何入口，直接显示。
        assert!(should_start_visible(false, false, false, false));
        // 单实例出错：必须显示并向用户解释。
        assert!(should_start_visible(true, true, true, true));
    }

    /// 查询清空：只有「从隐藏 / 未下发过变可见」这一次重新唤起才清，其余情况一律保留。
    #[test]
    fn query_reset_only_on_fresh_show() {
        // 首次下发（None）与收起后再次唤起：都算新一次唤起，清空上一次的筛选条件。
        assert!(should_reset_query(None, true));
        assert!(should_reset_query(Some(false), true));
        // 窗口已可见（正在输入、选项面板切走再回来、Ctrl+W 后刷新）：保持过滤条件。
        assert!(!should_reset_query(Some(true), true));
        // 正在收起：与查询无关，绝不清空。
        assert!(!should_reset_query(Some(true), false));
        assert!(!should_reset_query(Some(false), false));
    }

    /// 草稿与设置能相互转换，面板打开看到的就是已保存的值。
    #[test]
    fn options_draft_roundtrips_settings() {
        let settings = Settings {
            enable_hotkey: false,
            hotkey: HotkeySetting {
                key: 0x42,
                modifiers: MOD_CONTROL | MOD_SHIFT,
            },
            autostart: true,
            theme: ThemeSetting::Dark,
        };
        let draft = OptionsDraft::new(&settings);
        assert!(!draft.recording, "打开面板不应处于录制态");
        assert_eq!(draft.to_settings(), settings);
    }
}
