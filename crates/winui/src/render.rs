//! Direct2D / DirectWrite 的最小封装：把圆角卡片连同列表画进 32 位 DIB，再用
//! `UpdateLayeredWindow` 贴到分层窗口上，得到「卡片外完全透明、只有卡片区域不透明」的浮层。
//!
//! 参数：无（模块级说明）
//! 核心流程：`Renderer::new` 建 D2D1 工厂与 DWrite 工厂并预建四套文本格式（标题 / 进程名 /
//! 空列表提示 / 错误提示）→ 首次 `present` 时建一块 32bpp 预乘 DIB 画布 + 一个绑定到它的
//! `ID2D1DCRenderTarget`，两者一起缓存在 [`Device`] 里（只有尺寸或 DPI 变了才重建）→ 每帧：
//! 清成全透明 → 白色圆角卡片 → 逐可见行画选中底色、真图标（未就绪时只留浅灰占位块）、标题
//! 与进程名 → `EndDraw` → 让原生输入框把自己画进同一块位图 → `UpdateLayeredWindow(ULW_ALPHA)`。
//!
//! 画布与渲染目标为什么跨帧复用：`ID2D1Bitmap` 是「设备相关资源」，只有在创建它的渲染目标
//! 还活着、并且用同一个目标绘制时才有意义。每帧新建渲染目标等于每帧都要重建图标位图，
//! 所以这里把画布 + 渲染目标做成一套长期设备，图标位图才能真正缓存下来。
//!
//! 文本为什么用 `IDWriteTextLayout`：S6 的命中高亮只对「某几个区间」加粗，而
//! `ID2D1RenderTarget::DrawText` 只能整段一个格式。`CreateTextLayout` 之后可以对每个命中
//! 区间调 `SetFontWeight(BOLD, range)`，再用 `DrawTextLayout` 一次画出；区间来自 core 的
//! UTF-8 字节区间，必须经 [`crate::highlight::utf8_range_to_utf16_range`] 换成 UTF-16 偏移
//! （CJK 1 个 unit、emoji 2 个 unit）。
//!
//! 图标：解码在后台线程（[`crate::icons`]），这里只负责把回传的 RGBA 像素转成预乘 BGRA 的
//! `ID2D1Bitmap` 并缓存（[`Renderer::insert_icon`]）；快照变小时裁剪
//! （[`Renderer::retain_icons`]），窗口销毁时随 `Renderer` 一起释放。
//!
//! 虚拟化：调用方只把「当前可见的若干行」交进来（[`Frame::rows`]），这里不做任何裁剪计算，
//! 于是一万个窗口和一屏窗口的绘制成本完全一样。
//!
//! 输入框为什么要自己画：`UpdateLayeredWindow` 模式下系统只合成我们交给它的那张位图，
//! **它的子窗口（EDIT）虽然能收发消息、能拿焦点，但像素不会出现在屏幕上**。所以
//! [`paint_edit`] 在 `EndDraw` 之后用 `WM_PRINTCLIENT` 让 EDIT 把自己画进同一块 DIB，
//! 再把这块区域的 alpha 强制成不透明——输入框因此既能用原生控件（输入法、选区、剪贴板），
//! 又能显示在逐像素透明的卡片上。
//!
//! windows-sys 只导出 C 风格 API，不导出 Direct2D / DirectWrite 这类 COM 接口，因此这里
//! 手写 extern 声明 + 最小 vtable 调用：用到的接口只有 `ID2D1Factory`、`ID2D1DCRenderTarget`、
//! `IDWriteFactory`、`IDWriteTextFormat`、`IDWriteTextLayout`，每个调用点的槽位序号都按
//! `d2d1.h` / `dwrite.h` 的 vtable 顺序标注（已逐个对照 mingw-w64 头文件核对），改动前请
//! 再核对一遍。

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ops::Range;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use switcheroo_core::{HotkeySetting, ThemeSetting, WindowId};
use switcheroo_platform_win::IconImage;
use windows_sys::Win32::Foundation::{GetLastError, HWND, POINT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, IntersectClipRect, RestoreDC,
    SaveDC, SelectObject, SetViewportOrgEx, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SendMessageW, UpdateLayeredWindow, PRF_CLIENT, PRF_ERASEBKGND, ULW_ALPHA, WM_PRINTCLIENT,
};

use crate::highlight;
use crate::icons::premultiply_bgra;
use crate::layout;

// ---------------------------------------------------------------------------
// 配色（S9）：浅色 / 深色两套值，绘制、WM_CTLCOLOREDIT 与选项面板都从这里取色
// ---------------------------------------------------------------------------

/// 一套配色。
///
/// 参数：`card_fill` 卡片底；`card_stroke` 卡片 / 控件描边；`search_fill` 搜索框底；
/// `text_primary` 正文；`text_muted` 次要文字；`row_selected_fill` / `row_selected_stroke`
/// 选中行底与描边；`row_hover_fill` 悬停底；`icon_placeholder` 图标占位块；
/// `closing_text` 关闭中文字；`closing_icon_opacity` 关闭中图标不透明度；
/// `error_text` 错误文字；`panel_fill` 选项面板底色
/// 核心流程：由 [`palette`] 按明暗产出一份，`app` 用它给 `WM_CTLCOLOREDIT` 设色、
/// `render` 用它建 D2D 画刷；两处取同一个字段，深色下输入框与卡片才不会一块白一块黑。
#[derive(Clone, Copy)]
pub struct Palette {
    pub card_fill: D2d1ColorF,
    pub card_stroke: D2d1ColorF,
    pub search_fill: D2d1ColorF,
    pub text_primary: D2d1ColorF,
    pub text_muted: D2d1ColorF,
    pub row_selected_fill: D2d1ColorF,
    pub row_selected_stroke: D2d1ColorF,
    pub row_hover_fill: D2d1ColorF,
    pub icon_placeholder: D2d1ColorF,
    pub closing_text: D2d1ColorF,
    pub closing_icon_opacity: f32,
    pub error_text: D2d1ColorF,
    pub panel_fill: D2d1ColorF,
}

impl Palette {
    /// 参数：无
    /// 核心流程：把图标占位块的颜色降到 `closing_icon_opacity`（深色下 0.4），
    /// 「关闭中」那一行的占位块因此和真图标一起变淡，不会一块灰一块亮。
    pub fn closing_icon_fill(&self) -> D2d1ColorF {
        D2d1ColorF {
            a: self.closing_icon_opacity,
            ..self.icon_placeholder
        }
    }
}

/// 参数：`dark` 为真取深色一套，为假取浅色一套
/// 核心流程：两套值字段一一对应；浅色沿用改造前的观感（卡片纯白、选中浅蓝、悬停极浅灰、
/// 正文 #1F1F1F、次要 #808080、关闭中 #A6A6A6、占位块 #DDDDDD、错误红），深色按需求给定的
/// 一组值（卡片 #202020、选中 #2C4A63、悬停 #2A2A2A、正文 #E8E8E8 …），面板底色比卡片亮
/// 一档，作为压在上面的前景卡片才分得清层次。
pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            card_fill: D2d1ColorF::from_rgb8(0x20, 0x20, 0x20),
            card_stroke: D2d1ColorF::from_rgb8(0x3A, 0x3A, 0x3A),
            search_fill: D2d1ColorF::from_rgb8(0x1A, 0x1A, 0x1A),
            text_primary: D2d1ColorF::from_rgb8(0xE8, 0xE8, 0xE8),
            text_muted: D2d1ColorF::from_rgb8(0x90, 0x90, 0x90),
            row_selected_fill: D2d1ColorF::from_rgb8(0x2C, 0x4A, 0x63),
            row_selected_stroke: D2d1ColorF::from_rgb8(0x3E, 0x6B, 0x8F),
            row_hover_fill: D2d1ColorF::from_rgb8(0x2A, 0x2A, 0x2A),
            icon_placeholder: D2d1ColorF::from_rgb8(0x3C, 0x3C, 0x3C),
            closing_text: D2d1ColorF::from_rgb8(0x5E, 0x5E, 0x5E),
            closing_icon_opacity: 0.4,
            error_text: D2d1ColorF::from_rgb8(0xFF, 0x7B, 0x72),
            panel_fill: D2d1ColorF::from_rgb8(0x26, 0x26, 0x26),
        }
    } else {
        Palette {
            card_fill: D2d1ColorF::from_rgb8(0xFF, 0xFF, 0xFF),
            card_stroke: D2d1ColorF::from_rgb8(0xE0, 0xE0, 0xE0),
            search_fill: D2d1ColorF::from_rgb8(0xFF, 0xFF, 0xFF),
            text_primary: D2d1ColorF::from_rgb8(0x1F, 0x1F, 0x1F),
            text_muted: D2d1ColorF::from_rgb8(0x80, 0x80, 0x80),
            row_selected_fill: D2d1ColorF::from_rgb8(0xCC, 0xE4, 0xF7),
            row_selected_stroke: D2d1ColorF::from_rgb8(0xA8, 0xCE, 0xEE),
            row_hover_fill: D2d1ColorF::from_rgb8(0xF5, 0xF5, 0xF5),
            icon_placeholder: D2d1ColorF::from_rgb8(0xDD, 0xDD, 0xDD),
            closing_text: D2d1ColorF::from_rgb8(0xA6, 0xA6, 0xA6),
            closing_icon_opacity: 0.4,
            error_text: D2d1ColorF::from_rgb8(0xCD, 0x2D, 0x2D),
            panel_fill: D2d1ColorF::from_rgb8(0xFF, 0xFF, 0xFF),
        }
    }
}

// ---------------------------------------------------------------------------
// 固定文案与尺寸（选项面板、搜索框占位）
// ---------------------------------------------------------------------------

/// 搜索框占位文案的字号：比行文本（13 DIP）小一档，与 EDIT 里的 lfMessageFont 观感一致。
const HINT_TEXT_SIZE: f32 = 12.0;
/// 占位文案相对输入框左边的内缩：与 EDIT 无边框时的文字起点对齐。
const HINT_TEXT_INSET: f32 = 4.0;
/// 面板标题。
const PANEL_TITLE_TEXT: &str = "选项";
/// 「启用全局热键」复选框。
const PANEL_ENABLE_TEXT: &str = "启用全局热键";
/// 「开机自启」复选框。
const PANEL_AUTOSTART_TEXT: &str = "开机自启";
/// 主题三选一的标签。
const PANEL_THEME_TEXT: &str = "主题";
/// 录制中的值框提示。
const PANEL_RECORDING_TEXT: &str = "请按下新的组合键…";
/// 录制说明。
const PANEL_RECORD_HINT: &str = "录制时请按住至少一个修饰键（Ctrl / Shift / Alt / Win），Esc 取消";
/// 「录制」按钮。
const PANEL_RECORD_BUTTON: &str = "录制";
/// 「确定」按钮。
const PANEL_SAVE_BUTTON: &str = "确定";
/// 「取消」按钮。
const PANEL_CANCEL_BUTTON: &str = "取消";

// ---------------------------------------------------------------------------
// 一帧要画的内容
// ---------------------------------------------------------------------------

/// 列表的一行（已经裁剪到可见区内，行的先后顺序就是上屏顺序）。
///
/// 参数：`id` 窗口 id（图标缓存的键）；`title` / `process` 标题与进程名；
/// `title_ranges` / `process_ranges` 命中字符的 UTF-8 字节区间（未命中时为空切片）；
/// `closing` 该窗口已请求关闭、正在等它消失（文字变灰、图标变淡）。
/// 核心流程：由 `app` 从 `ListState` 里取出来拼好，`render` 只读，不回头查快照。
pub struct RowText<'a> {
    pub id: WindowId,
    pub title: &'a str,
    pub process: &'a str,
    pub title_ranges: &'a [Range<usize>],
    pub process_ranges: &'a [Range<usize>],
    pub closing: bool,
}

/// 原生输入框在本帧位图里的位置（逻辑像素，相对窗口左上角）。
///
/// 参数：`hwnd` EDIT 句柄；`left` / `top` / `width` / `height` 由 `layout::edit_box` 算出的
/// DIP 矩形。
/// 核心流程：由 `app` 传给 [`Renderer::present`]，[`paint_edit`] 据此把 EDIT 的内容
/// `WM_PRINTCLIENT` 到同一块位图上——分层窗口的子控件不会被系统合成，必须自己画。
#[derive(Clone, Copy)]
pub struct EditSurface {
    pub hwnd: HWND,
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

/// 选项面板这一帧要画的内容（S10）。
///
/// 参数：`enable_hotkey` / `autostart` 两个复选框的草稿值；`recording` 是否正在录制热键；
/// `hotkey` 草稿里的组合键（不录制时显示成 `Alt + Space` 这样的文本）；`theme` 三选一的
/// 当前项；`error` 面板内部的红字（热键冲突 / 自启写注册表失败 / 配置保存失败）。
/// 核心流程：由 `app` 从草稿里抄一份过来，`render` 只读——面板上的任何点击都不会在这里落地。
pub struct OptionsView<'a> {
    pub enable_hotkey: bool,
    pub recording: bool,
    pub hotkey: HotkeySetting,
    pub autostart: bool,
    pub theme: ThemeSetting,
    pub error: Option<&'a str>,
}

/// 一帧的绘制输入。
///
/// 参数：`rows` 可见行（最多一屏）；`selected` 选中行在 `rows` 里的下标；`hover` 鼠标悬停行
/// 在 `rows` 里的下标；`message` 空列表时要显示的灰字；`error` 需要以红字显示的提示
/// （热键 / 渲染初始化失败 / 关闭失败）；`edit` 输入框位置（没有输入框 / 面板打开时为
/// `None`）；`edit_hint` 输入框为空时要自己画的占位文案（面板打开时也为 `None`）；
/// `dark` 本帧用深色还是浅色配色；`panel` 面板打开时的面板内容（`None` 表示画列表）。
/// 核心流程：`app` 在每次状态变化（显示、查询变化、选中 / 悬停变化、图标到货、关闭轮询移除
/// 条目、面板开关与草稿变化）时组一份交给 [`Renderer::present`]。
pub struct Frame<'a> {
    pub rows: &'a [RowText<'a>],
    pub selected: Option<usize>,
    pub hover: Option<usize>,
    pub message: Option<&'a str>,
    pub error: Option<&'a str>,
    pub edit: Option<EditSurface>,
    pub edit_hint: Option<&'a str>,
    pub dark: bool,
    pub panel: Option<&'a OptionsView<'a>>,
}

// ---------------------------------------------------------------------------
// 手写的最小 COM / Direct2D / DirectWrite 声明
// ---------------------------------------------------------------------------

/// Win32 的 `GUID`：与 COM 的 16 字节布局一致。
#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// `IID_ID2D1Factory`：{06152247-6F50-465A-9245-118BFD3B6007}。
const IID_ID2D1_FACTORY: Guid = Guid {
    data1: 0x0615_2247,
    data2: 0x6f50,
    data3: 0x465a,
    data4: [0x92, 0x45, 0x11, 0x8b, 0xfd, 0x3b, 0x60, 0x07],
};

/// `IID_IDWriteFactory`：{B859EE5A-D838-4B5B-A2E8-1ADC7D93DB48}。
const IID_IDWRITE_FACTORY: Guid = Guid {
    data1: 0xb859_ee5a,
    data2: 0xd838,
    data3: 0x4b5b,
    data4: [0xa2, 0xe8, 0x1a, 0xdc, 0x7d, 0x93, 0xdb, 0x48],
};

/// `D2D1_FACTORY_TYPE_SINGLE_THREADED`：所有 D2D 调用都在窗口线程，单线程档省锁。
const D2D1_FACTORY_TYPE_SINGLE_THREADED: i32 = 0;
/// `D2D1_RENDER_TARGET_TYPE_DEFAULT`。
const D2D1_RENDER_TARGET_TYPE_DEFAULT: i32 = 0;
/// `DXGI_FORMAT_B8G8R8A8_UNORM`：32 位 BGRA，和 `CreateDIBSection` 的 BI_RGB 布局一致。
const DXGI_FORMAT_B8G8R8A8_UNORM: i32 = 87;
/// `D2D1_ALPHA_MODE_PREMULTIPLIED`：分层窗口要求预乘 alpha。
const D2D1_ALPHA_MODE_PREMULTIPLIED: i32 = 1;
/// `D2D1_RENDER_TARGET_USAGE_NONE`。
const D2D1_RENDER_TARGET_USAGE_NONE: i32 = 0;
/// `D2D1_FEATURE_LEVEL_DEFAULT`。
const D2D1_FEATURE_LEVEL_DEFAULT: i32 = 0;
/// `D2D1_DRAW_TEXT_OPTIONS_NONE`。
const D2D1_DRAW_TEXT_OPTIONS_NONE: i32 = 0;
/// `D2D1_BITMAP_INTERPOLATION_MODE_LINEAR`（NEAREST_NEIGHBOR=0、LINEAR=1）：32×32 图标缩到
/// 16×16 时用线性插值，边缘才不会有台阶。
const D2D1_BITMAP_INTERPOLATION_MODE_LINEAR: i32 = 1;
/// `DWRITE_FACTORY_TYPE_SHARED`。
const DWRITE_FACTORY_TYPE_SHARED: i32 = 0;
/// `DWRITE_FONT_WEIGHT_NORMAL`。
const DWRITE_FONT_WEIGHT_NORMAL: i32 = 400;
/// `DWRITE_FONT_WEIGHT_BOLD`：命中字符用粗体。
const DWRITE_FONT_WEIGHT_BOLD: i32 = 700;
/// `DWRITE_FONT_STYLE_NORMAL`。
const DWRITE_FONT_STYLE_NORMAL: i32 = 0;
/// `DWRITE_FONT_STRETCH_NORMAL`（UNDEFINED=0 … NORMAL=5）。
const DWRITE_FONT_STRETCH_NORMAL: i32 = 5;
/// `DWRITE_TEXT_ALIGNMENT_LEADING`（左对齐，标题与提示文字用）。
const DWRITE_TEXT_ALIGNMENT_LEADING: i32 = 0;
/// `DWRITE_TEXT_ALIGNMENT_TRAILING`（右对齐，进程名用）。
const DWRITE_TEXT_ALIGNMENT_TRAILING: i32 = 1;
/// `DWRITE_TEXT_ALIGNMENT_CENTER`。
const DWRITE_TEXT_ALIGNMENT_CENTER: i32 = 2;
/// `DWRITE_PARAGRAPH_ALIGNMENT_NEAR`（垂直靠上，错误提示用）。
const DWRITE_PARAGRAPH_ALIGNMENT_NEAR: i32 = 0;
/// `DWRITE_PARAGRAPH_ALIGNMENT_CENTER`。
const DWRITE_PARAGRAPH_ALIGNMENT_CENTER: i32 = 2;
/// `DWRITE_WORD_WRAPPING_NO_WRAP`。
const DWRITE_WORD_WRAPPING_NO_WRAP: i32 = 1;
/// `DWRITE_MEASURING_MODE_NATURAL`。
const DWRITE_MEASURING_MODE_NATURAL: i32 = 0;
/// `DWRITE_TRIMMING_GRANULARITY_CHARACTER`（NONE=0、CHARACTER=1、WORD=2）：超长文本按字符
/// 截断，绝不会只画出半个字，也不会溢出到进程名列。
const DWRITE_TRIMMING_GRANULARITY_CHARACTER: u32 = 1;
/// `D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE`（DEFAULT=0、CLEARTYPE=1、GRAYSCALE=2）：
/// 分层窗口用逐像素 alpha，ClearType 会在透明背景上留彩边。
const D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE: i32 = 2;

/// `D2D1_PIXEL_FORMAT`。
#[repr(C)]
struct D2d1PixelFormat {
    format: i32,
    alpha_mode: i32,
}

/// `D2D1_RENDER_TARGET_PROPERTIES`。
#[repr(C)]
struct D2d1RenderTargetProperties {
    target_type: i32,
    pixel_format: D2d1PixelFormat,
    dpi_x: f32,
    dpi_y: f32,
    usage: i32,
    min_level: i32,
}

/// `D2D1_BITMAP_PROPERTIES`：位图的像素格式与 DPI（32×32 图标按 96 DPI 解释，再缩放到
/// 16 DIP 的方框里画）。
#[repr(C)]
struct D2d1BitmapProperties {
    pixel_format: D2d1PixelFormat,
    dpi_x: f32,
    dpi_y: f32,
}

/// `D2D1_SIZE_U`（按值传递的 8 字节结构体）。
#[repr(C)]
struct D2d1SizeU {
    width: u32,
    height: u32,
}

/// `D2D1_POINT_2F`（按值传递的 8 字节结构体）。
#[repr(C)]
struct D2d1PointF {
    x: f32,
    y: f32,
}

/// `DWRITE_TEXT_RANGE`：`start` / `length` 都是 UTF-16 code unit 偏移（按值传递）。
#[repr(C)]
#[derive(Clone, Copy)]
struct DwriteTextRange {
    start: u32,
    length: u32,
}

/// `DWRITE_TRIMMING`：截断策略（按字符截断）。
#[repr(C)]
struct DwriteTrimming {
    granularity: u32,
    delimiter: u32,
    delimiter_count: u32,
}

/// `D2D1_COLOR_F`。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct D2d1ColorF {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl D2d1ColorF {
    /// 参数：`r` / `g` / `b` 8 位三原色
    /// 核心流程：各通道除以 255 换成 D2D 的 0.0–1.0 浮点，alpha 固定 1.0（不透明）——
    /// 配色表因此可以直接按设计师给的十六进制写。
    pub fn from_rgb8(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: 1.0,
        }
    }

    /// 参数：无
    /// 核心流程：换成 GDI 的 `COLORREF`（`0x00BBGGRR`：低字节红、次低字节绿、第三字节蓝），
    /// `SetTextColor` / `SetBkColor` / `CreateSolidBrush` 都吃这个格式。alpha 被丢弃——
    /// GDI 的颜色没有透明度。
    pub fn to_colorref(self) -> u32 {
        let scale = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
        (scale(self.b) << 16) | (scale(self.g) << 8) | scale(self.r)
    }
}

/// `D2D1_RECT_F`。
#[repr(C)]
#[derive(Clone, Copy)]
struct D2d1RectF {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

/// `D2D1_ROUNDED_RECT`。
#[repr(C)]
struct D2d1RoundedRect {
    rect: D2d1RectF,
    radius_x: f32,
    radius_y: f32,
}

/// `D2D1_BRUSH_PROPERTIES`（`transform` 是 3×2 矩阵）。
#[repr(C)]
struct D2d1BrushProperties {
    opacity: f32,
    transform: [f32; 6],
}

#[link(name = "d2d1")]
extern "system" {
    /// `D2D1CreateFactory`：按 IID 创建 D2D 工厂。
    fn D2D1CreateFactory(
        factory_type: i32,
        riid: *const Guid,
        factory_options: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;
}

#[link(name = "dwrite")]
extern "system" {
    /// `DWriteCreateFactory`：按 IID 创建 DWrite 工厂。
    fn DWriteCreateFactory(factory_type: i32, iid: *const Guid, factory: *mut *mut c_void) -> i32;
}

/// `ID2D1Factory::CreateDCRenderTarget`（vtable 第 16 槽）。
type FnCreateDcRenderTarget = unsafe extern "system" fn(
    *mut c_void,
    *const D2d1RenderTargetProperties,
    *mut *mut c_void,
) -> i32;
/// `ID2D1DCRenderTarget::BindDC`（vtable 第 57 槽，即 `ID2D1RenderTarget` 的 57 个槽之后）。
type FnBindDc = unsafe extern "system" fn(*mut c_void, HDC, *const RECT) -> i32;
/// `ID2D1RenderTarget::SetDpi`（vtable 第 51 槽）。
type FnSetDpi = unsafe extern "system" fn(*mut c_void, f32, f32);
/// `ID2D1RenderTarget::SetTextAntialiasMode`（vtable 第 34 槽）。
type FnSetTextAntialiasMode = unsafe extern "system" fn(*mut c_void, i32);
/// `ID2D1RenderTarget::Clear`（vtable 第 47 槽）。
type FnClear = unsafe extern "system" fn(*mut c_void, *const D2d1ColorF);
/// `ID2D1RenderTarget::BeginDraw`（vtable 第 48 槽）。
type FnBeginDraw = unsafe extern "system" fn(*mut c_void);
/// `ID2D1RenderTarget::EndDraw`（vtable 第 49 槽）。
type FnEndDraw = unsafe extern "system" fn(*mut c_void, *mut u64, *mut u64) -> i32;
/// `ID2D1RenderTarget::CreateBitmap`（vtable 第 4 槽）。
type FnCreateBitmap = unsafe extern "system" fn(
    *mut c_void,
    D2d1SizeU,
    *const c_void,
    u32,
    *const D2d1BitmapProperties,
    *mut *mut c_void,
) -> i32;
/// `ID2D1RenderTarget::CreateSolidColorBrush`（vtable 第 8 槽）。
type FnCreateSolidColorBrush = unsafe extern "system" fn(
    *mut c_void,
    *const D2d1ColorF,
    *const D2d1BrushProperties,
    *mut *mut c_void,
) -> i32;
/// `ID2D1RenderTarget::FillRoundedRectangle`（vtable 第 19 槽）。
type FnFillRoundedRect =
    unsafe extern "system" fn(*mut c_void, *const D2d1RoundedRect, *mut c_void);
/// `ID2D1RenderTarget::DrawRoundedRectangle`（vtable 第 18 槽，画描边用）。
type FnDrawRoundedRect =
    unsafe extern "system" fn(*mut c_void, *const D2d1RoundedRect, *mut c_void, f32, *mut c_void);
/// `ID2D1RenderTarget::DrawBitmap`（vtable 第 26 槽）。
type FnDrawBitmap = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *const D2d1RectF,
    f32,
    i32,
    *const D2d1RectF,
);
/// `ID2D1RenderTarget::DrawText`（vtable 第 27 槽，只有提示文字走这条）。
type FnDrawText = unsafe extern "system" fn(
    *mut c_void,
    *const u16,
    u32,
    *mut c_void,
    *const D2d1RectF,
    *mut c_void,
    i32,
    i32,
);
/// `ID2D1RenderTarget::DrawTextLayout`（vtable 第 28 槽，行文本走这条）。
type FnDrawTextLayout =
    unsafe extern "system" fn(*mut c_void, D2d1PointF, *mut c_void, *mut c_void, i32);
/// `IDWriteFactory::CreateTextFormat`（vtable 第 15 槽）。
type FnCreateTextFormat = unsafe extern "system" fn(
    *mut c_void,
    *const u16,
    *mut c_void,
    i32,
    i32,
    i32,
    f32,
    *const u16,
    *mut *mut c_void,
) -> i32;
/// `IDWriteFactory::CreateTextLayout`（vtable 第 18 槽）。
type FnCreateTextLayout = unsafe extern "system" fn(
    *mut c_void,
    *const u16,
    u32,
    *mut c_void,
    f32,
    f32,
    *mut *mut c_void,
) -> i32;
/// `IDWriteTextFormat::SetTextAlignment`（vtable 第 3 槽）。
type FnSetTextAlignment = unsafe extern "system" fn(*mut c_void, i32) -> i32;
/// `IDWriteTextFormat::SetTrimming`（vtable 第 9 槽）。
type FnSetTrimming =
    unsafe extern "system" fn(*mut c_void, *const DwriteTrimming, *mut c_void) -> i32;
/// `IDWriteTextLayout::SetMaxWidth`（vtable 第 28 槽）。
type FnSetMaxWidth = unsafe extern "system" fn(*mut c_void, f32) -> i32;
/// `IDWriteTextLayout::SetFontWeight`（vtable 第 32 槽）。
type FnSetFontWeight = unsafe extern "system" fn(*mut c_void, i32, DwriteTextRange) -> i32;
/// `IDWriteTextLayout::SetFontSize`（vtable 第 35 槽，粗体不可用时的退路）。
type FnSetFontSize = unsafe extern "system" fn(*mut c_void, f32, DwriteTextRange) -> i32;

/// 从 COM 对象的 vtable 里取出第 `index` 个函数指针。
///
/// 参数：`object` 任意 COM 接口指针（不能为空）；`index` 槽位序号（QueryInterface=0、
/// AddRef=1、Release=2，其余按各接口头文件顺序）。
/// 核心流程：COM 对象的第一个字段就是 vtable 指针 → 读出第 `index` 个槽 →
/// 转成对应的 `extern "system" fn` 调用。
unsafe fn vtable_fn<F: Copy>(object: *mut c_void, index: usize) -> F {
    let vtable = *(object as *mut *const *const c_void);
    let slot = *vtable.add(index);
    std::mem::transmute_copy(&slot)
}

/// 调用 COM 对象的 `IUnknown::Release`（vtable 第 2 槽）。
///
/// 参数：`object` 任意 COM 接口指针，不能为空。
/// 核心流程：取第 2 槽的函数指针并调用，引用计数减一；计数归零时对象自行析构。
unsafe fn release(object: *mut c_void) {
    type FnRelease = unsafe extern "system" fn(*mut c_void) -> u32;
    let release_fn: FnRelease = vtable_fn(object, 2);
    release_fn(object);
}

/// 一个「Drop 即 Release」的 COM 接口指针。
///
/// 参数：`0` 为任意 COM 接口指针（可以为空，空指针在 Drop 时跳过）。
/// 核心流程：所有 COM 对象都装进这个壳里，任何提前返回的分支都会自动 Release，
/// 不需要在每个错误路径上手写释放。
struct Com(*mut c_void);

/// 参数：无。
/// 核心流程：非空指针调用 `IUnknown::Release`，空指针直接跳过。
impl Drop for Com {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: self.0 是成功创建、尚未释放过的 COM 接口指针；
            // Release 之后不再使用它（结构体随后就被丢弃）。
            unsafe { release(self.0) };
        }
    }
}

// ---------------------------------------------------------------------------
// 内存 DC + 32bpp DIB 画布
// ---------------------------------------------------------------------------

/// 一块「内存 DC + 32bpp 位图」的画布（跟随 [`Device`] 长期存活）。
///
/// 参数：`dc` 为 `CreateCompatibleDC` 的句柄；`bitmap` 为 `CreateDIBSection` 的位图；
/// `previous` 是位图选进 DC 时被替换掉的旧 GDI 对象；`pixels` 是位图内存首地址（32bpp、
/// 自上而下，行跨距 = `width × 4`）；`width` / `height` 位图尺寸（物理像素）。
/// 核心流程：Drop 时按 GDI 的固定顺序释放——先 `SelectObject` 还原旧对象（位图被选中时
/// 删不掉），再 `DeleteObject(bitmap)`，最后 `DeleteDC(dc)`。渲染目标绑定着这块 DC，所以
/// 画布必须比渲染目标活得久，这由 [`Device`] 的字段顺序保证（target 在前、canvas 在后）。
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    pixels: *mut u8,
    width: i32,
    height: i32,
}

impl Canvas {
    /// 参数：`width` / `height` 位图尺寸（物理像素）。
    /// 核心流程：`CreateCompatibleDC(NULL)` 建内存 DC → 填 `BITMAPINFO`（32bpp、BI_RGB、
    /// 负高度表示自上而下，与分层窗口的行序一致）→ `CreateDIBSection` 建位图 →
    /// `SelectObject` 把它选进 DC 并记下旧对象。
    fn new(width: i32, height: i32) -> Result<Self, String> {
        // SAFETY: CreateCompatibleDC(NULL) 建一个与屏幕兼容的内存 DC，无输入指针。
        let dc = unsafe { CreateCompatibleDC(ptr::null_mut()) };
        if dc.is_null() {
            return Err("CreateCompatibleDC 失败".to_string());
        }

        // SAFETY: BITMAPINFO 是纯 POD，全零是合法初值；随后逐字段填写，
        // 其中 bmiColors 不使用（BI_RGB 下不需要调色板）。
        let mut info: BITMAPINFO = unsafe { zeroed() };
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };

        let mut pixels: *mut c_void = ptr::null_mut();
        // SAFETY: CreateDIBSection 只读取栈上的 &info；pixels 用于接收位图内存指针（本函数
        // 不需要它，但 API 要求非空）；hsection 传空、offset 传 0 表示匿名内存位图。
        let bitmap =
            unsafe { CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut pixels, ptr::null_mut(), 0) };
        if bitmap.is_null() {
            // SAFETY: dc 是上面刚创建、尚未交给别人的句柄，这里创建失败，只关一次。
            unsafe { DeleteDC(dc) };
            return Err("CreateDIBSection 失败".to_string());
        }

        // SAFETY: 把 bitmap 选进本函数独占的 dc，返回旧对象用于 Drop 时还原；
        // 两个句柄都有效且都归本结构体所有。
        let previous = unsafe { SelectObject(dc, bitmap) };
        // SelectObject 失败时返回 NULL 或 HGDI_ERROR（(HGDIOBJ)-1），两者都要当成失败处理。
        if previous.is_null() || previous as isize == -1 {
            // SAFETY: bitmap 与 dc 都是本函数刚创建、尚未交给别人的句柄，按「先删位图再删 DC」
            // 的顺序释放；位图此时还没被选进 DC（SelectObject 失败），可以直接删。
            unsafe {
                DeleteObject(bitmap);
                DeleteDC(dc);
            }
            return Err("SelectObject 失败".to_string());
        }

        Ok(Self {
            dc,
            bitmap,
            previous,
            pixels: pixels as *mut u8,
            width,
            height,
        })
    }

    /// 参数：`x` / `y` 区域左上角（物理像素）；`width` / `height` 区域尺寸（物理像素）
    /// 核心流程：把矩形内每个像素的第 4 个字节（alpha）置 255。GDI 往 32bpp DIB 上画东西时
    /// 只写 BGR、alpha 字节留 0，而分层窗口要预乘 alpha——不补这一步，`WM_PRINTCLIENT` 画上去
    /// 的内容会被当成全透明。矩形先与位图边界求交，越界部分直接跳过。
    fn make_opaque(&self, x: i32, y: i32, width: i32, height: i32) {
        if self.pixels.is_null() {
            return;
        }
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + width).min(self.width);
        let y1 = (y + height).min(self.height);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        for row in y0..y1 {
            for column in x0..x1 {
                let offset = (row as usize * self.width as usize + column as usize) * 4 + 3;
                // SAFETY: pixels 指向本 Canvas 的位图内存，尺寸由 CreateDIBSection 的
                // BITMAPINFO 确定（width × height × 4 字节）；offset 已按边界求交，不会越界。
                unsafe { *self.pixels.add(offset) = 255 };
            }
        }
    }
}

/// 参数：无。
/// 核心流程：还原旧 GDI 对象 → 删除位图 → 删除 DC，三步各调用一次，保证 GDI 对象不泄漏。
impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: 三个句柄都由 Canvas::new 成功创建且只在这里释放；顺序必须是
        // 先还原旧对象（否则位图仍被 DC 选中，DeleteObject 会失败），再删位图，最后删 DC。
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

// ---------------------------------------------------------------------------
// 图标缓存
// ---------------------------------------------------------------------------

/// 一个窗口的图标：预乘 BGRA 像素 + 按当前渲染目标建出来的 D2D 位图。
///
/// 参数：`pixels` 预乘 BGRA8 像素（`width × height × 4`）；`width` / `height` 像素尺寸；
/// `bitmap` 属于 `generation` 那个渲染目标的 `ID2D1Bitmap`（还没建或已失效时为 `None`）；
/// `generation` 位图所属的渲染目标代次。
/// 核心流程：像素只在后台结果到货时转换一次；位图按代次懒建——尺寸 / DPI 变化会让渲染
/// 目标重建、旧位图作废，这时只丢位图、保留像素，下一帧再 `CreateBitmap` 一次即可。
struct IconEntry {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    bitmap: Option<Com>,
    generation: u64,
}

// ---------------------------------------------------------------------------
// 渲染器
// ---------------------------------------------------------------------------

/// 参数：`dwf` 为 `IDWriteFactory` 指针；`size` 字号（DIP）；`h_align` 水平对齐
/// （`DWRITE_TEXT_ALIGNMENT_*`）；`v_align` 垂直对齐（`DWRITE_PARAGRAPH_ALIGNMENT_*`）。
/// 核心流程：`CreateTextFormat`（第 15 槽）建格式 → `SetTextAlignment`（第 3 槽）、
/// `SetParagraphAlignment`（第 4 槽）、`SetWordWrapping(NO_WRAP)`（第 5 槽）与
/// `SetTrimming(按字符截断)`（第 9 槽）设段落行为；族名用 "Segoe UI"、locale 用 zh-CN。
/// 粗体不另建格式：DirectWrite 会在同一个族里按字符权重自动挑粗体成员，行文本只需对命中
/// 区间 `SetFontWeight(BOLD)`。失败时返回中文错误串，已创建的格式由 [`Com`] 释放。
fn create_text_format(
    dwf: *mut c_void,
    size: f32,
    h_align: i32,
    v_align: i32,
) -> Result<Com, String> {
    let family: Vec<u16> = "Segoe UI"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let locale: Vec<u16> = "zh-CN".encode_utf16().chain(std::iter::once(0)).collect();

    let mut format: *mut c_void = ptr::null_mut();
    // SAFETY: CreateTextFormat 只读取 family / locale 指向的 NUL 结尾 UTF-16 字符串
    // （两个 Vec 在调用期间都存活），fontCollection 传空表示用系统字体集合。
    let create: FnCreateTextFormat = unsafe { vtable_fn(dwf, 15) };
    let hr = unsafe {
        create(
            dwf,
            family.as_ptr(),
            ptr::null_mut(),
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            locale.as_ptr(),
            &mut format,
        )
    };
    if hr < 0 || format.is_null() {
        return Err(format!("CreateTextFormat 失败，HRESULT 0x{hr:08X}"));
    }
    let format = Com(format);

    // SAFETY: format 是刚创建成功的 IDWriteTextFormat，槽位 3 / 4 / 5 分别是
    // SetTextAlignment / SetParagraphAlignment / SetWordWrapping，参数是合法枚举值。
    let set_text_alignment: FnSetTextAlignment = unsafe { vtable_fn(format.0, 3) };
    let hr = unsafe { set_text_alignment(format.0, h_align) };
    if hr < 0 {
        return Err(format!("SetTextAlignment 失败，HRESULT 0x{hr:08X}"));
    }
    // SAFETY: 同上，第 4 槽是 SetParagraphAlignment。
    let set_paragraph_alignment: FnSetTextAlignment = unsafe { vtable_fn(format.0, 4) };
    let hr = unsafe { set_paragraph_alignment(format.0, v_align) };
    if hr < 0 {
        return Err(format!("SetParagraphAlignment 失败，HRESULT 0x{hr:08X}"));
    }
    // SAFETY: 同上，第 5 槽是 SetWordWrapping，取值 NO_WRAP，避免长标题被折行。
    let set_word_wrapping: FnSetTextAlignment = unsafe { vtable_fn(format.0, 5) };
    let hr = unsafe { set_word_wrapping(format.0, DWRITE_WORD_WRAPPING_NO_WRAP) };
    if hr < 0 {
        return Err(format!("SetWordWrapping 失败，HRESULT 0x{hr:08X}"));
    }

    // 截断策略挂在「格式」上（布局继承它）：超出 maxWidth 的文本按字符截断，trimmingSign
    // 传空表示不加省略号——进程名列就在右边，加省略号反而更挤。
    let trimming = DwriteTrimming {
        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
        delimiter: 0,
        delimiter_count: 0,
    };
    // SAFETY: 第 9 槽是 SetTrimming，只读栈上的 trimming 与可空的 trimmingSign。
    let set_trimming: FnSetTrimming = unsafe { vtable_fn(format.0, 9) };
    let hr = unsafe { set_trimming(format.0, &trimming, ptr::null_mut()) };
    if hr < 0 {
        // 截断只是「超长文本画得好看一点」，失败不值得让整个渲染器起不来（那会让浮层
        // 只剩一行红字）：记一条 warn，继续用默认的不截断行为。
        log::warn!("SetTrimming 失败（HRESULT 0x{hr:08X}），超长文本按默认方式裁剪");
    }

    Ok(format)
}

/// 一套「画布 + 渲染目标」，尺寸或 DPI 不变就一直复用。
///
/// 参数：`target` `ID2D1DCRenderTarget`（已绑定 `canvas` 的 DC）；`canvas` DIB 画布；
/// `width` / `height` 物理像素尺寸；`scale` DPI 缩放；`generation` 代次（每次重建 +1，
/// 用来判断图标位图是否还能用）。
/// 核心流程：字段顺序即 Drop 顺序——先释放渲染目标（它引用着下面的 DC），再释放画布。
struct Device {
    target: Com,
    canvas: Canvas,
    width: i32,
    height: i32,
    scale: f32,
    generation: u64,
}

/// Direct2D / DirectWrite 的长期对象（跟随进程生命周期，`WM_DESTROY` 时释放）。
///
/// 参数：`title_format` / `process_format` / `message_format` / `error_format` / `hint_format` /
/// `center_format` 是预建的六套文本格式；`device` 画布与渲染目标（首次绘制时创建）；`icons`
/// 图标缓存；`generation` 下一个渲染目标的代次；`dwrite` / `factory` 是两个 COM 工厂。
/// 核心流程：字段按「依赖方在前、被依赖方在后」声明，Drop 顺序因此是
/// 图标位图 → 渲染目标 → 画布 → 文本格式 → DWrite 工厂 → D2D 工厂。
pub struct Renderer {
    title_format: Com,
    process_format: Com,
    message_format: Com,
    error_format: Com,
    hint_format: Com,
    center_format: Com,
    device: Option<Device>,
    icons: HashMap<WindowId, IconEntry>,
    generation: u64,
    dwrite: Com,
    factory: Com,
}

impl Renderer {
    /// 参数：无（调用前必须先在本线程执行过 `CoInitializeEx`）。
    /// 核心流程：`D2D1CreateFactory` 建 D2D 工厂 → `DWriteCreateFactory` 建 DWrite 工厂 →
    /// 预建四套文本格式：标题 13 DIP 左对齐、进程名 11 DIP 右对齐、空列表提示 13 DIP 左对齐、
    /// 错误提示 13 DIP 居中，全部垂直居中（错误提示靠上）且不换行、超长按字符截断。渲染目标
    /// 要等第一次 `present` 知道窗口尺寸时才创建。任一步失败都返回可直接显示的中文错误串，
    /// 已创建的对象由 [`Com`] 自动释放。
    pub fn new() -> Result<Self, String> {
        let mut factory: *mut c_void = ptr::null_mut();
        // SAFETY: D2D1CreateFactory 只读取 'static 的 GUID（options 传空表示默认选项），
        // 并把工厂指针写进栈上的 factory；factory_type 用单线程档，与全程单线程的假设一致。
        let hr = unsafe {
            D2D1CreateFactory(
                D2D1_FACTORY_TYPE_SINGLE_THREADED,
                &IID_ID2D1_FACTORY,
                ptr::null(),
                &mut factory,
            )
        };
        if hr < 0 || factory.is_null() {
            return Err(format!("D2D1CreateFactory 失败，HRESULT 0x{hr:08X}"));
        }
        let factory = Com(factory);

        let mut dwrite: *mut c_void = ptr::null_mut();
        // SAFETY: DWriteCreateFactory 的调用约定与上面一致：只读 'static GUID、写栈上指针。
        let hr = unsafe {
            DWriteCreateFactory(
                DWRITE_FACTORY_TYPE_SHARED,
                &IID_IDWRITE_FACTORY,
                &mut dwrite,
            )
        };
        if hr < 0 || dwrite.is_null() {
            return Err(format!("DWriteCreateFactory 失败，HRESULT 0x{hr:08X}"));
        }
        let dwrite = Com(dwrite);

        let title_format = create_text_format(
            dwrite.0,
            layout::TITLE_SIZE,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )?;
        let process_format = create_text_format(
            dwrite.0,
            layout::PROCESS_SIZE,
            DWRITE_TEXT_ALIGNMENT_TRAILING,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )?;
        let message_format = create_text_format(
            dwrite.0,
            layout::HINT_SIZE,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )?;
        let error_format = create_text_format(
            dwrite.0,
            layout::HINT_SIZE,
            DWRITE_TEXT_ALIGNMENT_CENTER,
            DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
        )?;
        // 搜索框占位文案：字号比行文本小一档（12 DIP），垂直居中，靠左与 EDIT 里的文字对齐。
        let hint_format = create_text_format(
            dwrite.0,
            HINT_TEXT_SIZE,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )?;
        // 按钮 / 主题选项的文字：水平垂直都居中。
        let center_format = create_text_format(
            dwrite.0,
            layout::TITLE_SIZE,
            DWRITE_TEXT_ALIGNMENT_CENTER,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )?;

        Ok(Self {
            title_format,
            process_format,
            message_format,
            error_format,
            hint_format,
            center_format,
            device: None,
            icons: HashMap::new(),
            generation: 0,
            dwrite,
            factory,
        })
    }

    /// 参数：`id` 窗口 id；`image` 后台线程解出来的图标（RGBA8、非预乘）。
    /// 核心流程：校验尺寸（宽高非零、像素数组够长）→ 用 [`premultiply_bgra`] 一次性换成
    /// 预乘 BGRA 并缓存；D2D 位图留到下一次绘制时按当时的渲染目标懒建（此刻可能还没有渲染
    /// 目标）。尺寸不合法只记 warn，不影响其它行。
    pub fn insert_icon(&mut self, id: WindowId, image: &IconImage) {
        let expected = image.width as usize * image.height as usize * 4;
        if image.width == 0 || image.height == 0 || image.rgba.len() < expected {
            log::warn!(
                "图标数据不合法：{}×{} / {} 字节（{:#X}）",
                image.width,
                image.height,
                image.rgba.len(),
                id.raw()
            );
            return;
        }
        self.icons.insert(
            id,
            IconEntry {
                pixels: premultiply_bgra(&image.rgba[..expected]),
                width: image.width,
                height: image.height,
                bitmap: None,
                generation: 0,
            },
        );
    }

    /// 参数：`keep` 仍然存在于快照里的窗口 id 集合。
    /// 核心流程：把不在集合里的图标（窗口已关闭 / 已从快照消失）连像素带位图一起丢掉，
    /// 缓存因此随快照收缩，不会长成一张只增不减的表。
    pub fn retain_icons(&mut self, keep: &HashSet<WindowId>) {
        let before = self.icons.len();
        self.icons.retain(|id, _| keep.contains(id));
        if self.icons.len() != before {
            log::debug!("图标缓存随快照收缩：{before} → {}", self.icons.len());
        }
    }

    /// 参数：无。
    /// 核心流程：返回缓存的图标条目数，只用于 `WM_DESTROY` 的释放日志。
    pub fn icon_count(&self) -> usize {
        self.icons.len()
    }

    /// 画一帧并贴到分层窗口。
    ///
    /// 参数：`hwnd` 目标分层窗口；`position` 窗口左上角（屏幕物理像素）；`width` / `height`
    /// 位图与窗口尺寸（物理像素）；`scale` DPI 缩放（1.0 表示 96 DPI）；`frame` 这一帧要画的
    /// 可见行 / 选中项 / 空列表提示 / 错误文本 / 输入框位置。
    /// 核心流程：取（必要时重建）画布 + 渲染目标 → 给本帧可见行补齐图标位图 → 把渲染目标的
    /// DPI 当作 96×scale，于是绘制坐标直接是逻辑像素 → `Clear` 成全透明 → 画白色圆角卡片与
    /// 列表 → `EndDraw` → 让原生输入框用 `WM_PRINTCLIENT` 把自己画进同一块位图并补上 alpha →
    /// `UpdateLayeredWindow(ULW_ALPHA)` 把整张位图连同逐像素 alpha 交给系统。画布、渲染目标
    /// 与图标位图都靠 Drop 释放，任何一条失败路径都不会漏掉 GDI / COM 资源。
    pub fn present(
        &mut self,
        hwnd: HWND,
        position: POINT,
        width: i32,
        height: i32,
        scale: f32,
        frame: &Frame<'_>,
    ) -> Result<(), String> {
        let generation = self.ensure_device(width, height, scale)?;
        self.ensure_icon_bitmaps(frame, generation);
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| "渲染目标未就绪".to_string())?;

        self.draw(device.target.0, width, height, scale, frame)?;
        // D2D 的绘制必须等 EndDraw 之后才能让 GDI 往同一块 DC 上画，所以这一步放在 draw 之后。
        if let Some(edit) = frame.edit {
            paint_edit(&device.canvas, scale, edit);
            // 占位文案（S9）：EDIT 自己的 EM_SETCUEBANNER 颜色跟随系统、深色下不可读，所以
            // 由我们自己在这块位图上补画——必须排在 WM_PRINTCLIENT 之后，否则会被 EDIT 的
            // 背景擦掉。
            if let Some(hint) = frame.edit_hint {
                let colors = palette(frame.dark);
                paint_edit_hint(
                    device.target.0,
                    edit,
                    hint,
                    self.hint_format.0,
                    &colors.text_muted,
                )?;
            }
        }

        let size = SIZE {
            cx: width,
            cy: height,
        };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        // SAFETY: UpdateLayeredWindow 是 user32 的 API：hwnd 是本进程刚创建的分层窗口，
        // hdcdst 传空表示窗口 DC 交给系统、位置与尺寸由 pptdst/psize 指定（两者都指向栈上
        // 变量，调用期间有效）；hdcsrc 是 Device 持有的画布 DC（比本函数活得久），位图已由
        // D2D 画好；crkey 在 ULW_ALPHA 下被忽略，blend 指定 AC_SRC_ALPHA 逐像素合成。
        let ok = unsafe {
            UpdateLayeredWindow(
                hwnd,
                ptr::null_mut(),
                &position,
                &size,
                device.canvas.dc,
                &source,
                0,
                &blend,
                ULW_ALPHA,
            )
        };
        if ok == 0 {
            // SAFETY: 紧邻 UpdateLayeredWindow 失败之后读取 last-error，中间没有其它 Win32 调用。
            let code = unsafe { GetLastError() };
            return Err(format!("UpdateLayeredWindow 失败，错误码 {code}"));
        }
        Ok(())
    }

    /// 参数：本帧位图的物理尺寸与 DPI 缩放。
    /// 核心流程：尺寸 / 缩放都没变就复用现有 [`Device`]（连同它的渲染目标与图标位图）→
    /// 否则先丢弃旧的（Drop 顺序保证渲染目标先于画布释放）→ 建画布 → 建渲染目标并 `BindDC`
    /// → 设 DPI → 代次 +1，返回新代次。返回的代次用来判断图标位图是否还能继续用。
    fn ensure_device(&mut self, width: i32, height: i32, scale: f32) -> Result<u64, String> {
        if let Some(device) = self.device.as_ref() {
            if device.width == width && device.height == height && device.scale == scale {
                return Ok(device.generation);
            }
        }

        self.device = None;
        let canvas = Canvas::new(width, height)?;
        let target = self.create_render_target(canvas.dc, width, height, scale)?;
        let generation = self.generation;
        self.generation = self.generation.wrapping_add(1);
        self.device = Some(Device {
            target,
            canvas,
            width,
            height,
            scale,
            generation,
        });
        log::debug!(
            "渲染目标重建：{width}×{height} 像素，DPI 缩放 {}%，代次 {generation}",
            layout::scale_percent(scale)
        );
        Ok(generation)
    }

    /// 参数：`frame` 本帧内容；`generation` 当前渲染目标代次。
    /// 核心流程：遍历可见行的 id，凡是「还没有位图」或「位图属于旧渲染目标」的条目，都用它
    /// 缓存好的预乘像素在当前渲染目标上 `CreateBitmap` 一次并记下代次；建失败只记 warn，
    /// 该行继续显示浅灰占位块。
    fn ensure_icon_bitmaps(&mut self, frame: &Frame<'_>, generation: u64) {
        let Some(target) = self.device.as_ref().map(|device| device.target.0) else {
            return;
        };
        for row in frame.rows {
            let Some(entry) = self.icons.get_mut(&row.id) else {
                continue;
            };
            if entry.bitmap.is_some() && entry.generation == generation {
                continue;
            }
            match create_icon_bitmap(target, entry) {
                Ok(bitmap) => {
                    entry.bitmap = Some(bitmap);
                    entry.generation = generation;
                }
                Err(err) => log::warn!("创建图标位图失败（{:#X}）：{err}", row.id.raw()),
            }
        }
    }

    /// 参数：`dc` 已选中本帧位图的内存 DC；`width` / `height` 位图尺寸（像素）；
    /// `scale` DPI 缩放。
    /// 核心流程：填 `D2D1_RENDER_TARGET_PROPERTIES`（B8G8R8A8 + 预乘 alpha）→
    /// `ID2D1Factory::CreateDCRenderTarget`（第 16 槽）→ `BindDC`（第 57 槽）覆盖整张位图 →
    /// `SetDpi(96×scale)`（第 51 槽）让后续绘制单位回到逻辑像素。
    fn create_render_target(
        &self,
        dc: HDC,
        width: i32,
        height: i32,
        scale: f32,
    ) -> Result<Com, String> {
        let properties = D2d1RenderTargetProperties {
            target_type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixel_format: D2d1PixelFormat {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alpha_mode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            // dpi 传 0 表示沿用 DC 的 DPI，随后再用 SetDpi 显式覆盖成屏幕缩放。
            dpi_x: 0.0,
            dpi_y: 0.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            min_level: D2D1_FEATURE_LEVEL_DEFAULT,
        };

        let mut target: *mut c_void = ptr::null_mut();
        // SAFETY: CreateDCRenderTarget 是 ID2D1Factory 的第 16 槽，只读 &properties、
        // 写栈上的 target；properties 里的枚举都是 D2D 1.0 的合法取值。
        let create: FnCreateDcRenderTarget = unsafe { vtable_fn(self.factory.0, 16) };
        let hr = unsafe { create(self.factory.0, &properties, &mut target) };
        if hr < 0 || target.is_null() {
            return Err(format!("CreateDCRenderTarget 失败，HRESULT 0x{hr:08X}"));
        }
        let target = Com(target);

        let rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        // SAFETY: BindDC 是 ID2D1DCRenderTarget 的第 57 槽（ID2D1RenderTarget 的 57 个槽之后），
        // dc 是 Device 持有的画布 DC（生命周期覆盖本渲染目标），rect 覆盖整张位图，
        // 两者在调用期间都有效。
        let bind: FnBindDc = unsafe { vtable_fn(target.0, 57) };
        let hr = unsafe { bind(target.0, dc, &rect) };
        if hr < 0 {
            return Err(format!("BindDC 失败，HRESULT 0x{hr:08X}"));
        }

        // SAFETY: 第 51 槽是 ID2D1RenderTarget::SetDpi，只改渲染目标的 DPI 标量。
        let set_dpi: FnSetDpi = unsafe { vtable_fn(target.0, 51) };
        let dpi = 96.0 * scale;
        unsafe { set_dpi(target.0, dpi, dpi) };

        Ok(target)
    }

    /// 参数：`target` 已绑定 DIB 的渲染目标；`width` / `height` 位图尺寸（像素）；
    /// `scale` DPI 缩放；`frame` 这一帧的内容。
    /// 核心流程：按 DPI 缩放算出逻辑尺寸与卡片矩形 → 按 `frame.dark` 取一套 [`Palette`] 并建好
    /// 全部画刷 → 透明清屏 → 卡片底色 + 1px 描边 → 面板打开时画选项面板、否则画列表 →
    /// `EndDraw` 检查 HRESULT。深色 / 浅色在这里只差一份配色，绘制代码不分叉。
    fn draw(
        &self,
        target: *mut c_void,
        width: i32,
        height: i32,
        scale: f32,
        frame: &Frame<'_>,
    ) -> Result<(), String> {
        let logical_width = width as f32 / scale;
        let logical_height = height as f32 / scale;
        let card = D2d1RectF {
            left: layout::CARD_MARGIN,
            top: layout::CARD_MARGIN,
            right: logical_width - layout::CARD_MARGIN,
            bottom: logical_height - layout::CARD_MARGIN,
        };
        let colors = palette(frame.dark);
        let brushes = Brushes::new(target, &colors)?;

        // SAFETY: 第 34 槽 SetTextAntialiasMode、第 48 槽 BeginDraw，都只作用于本帧渲染目标；
        // 此时所有画刷都已创建完成，BeginDraw 之后只做绘制，不再创建/释放资源。
        let set_text_aa: FnSetTextAntialiasMode = unsafe { vtable_fn(target, 34) };
        unsafe { set_text_aa(target, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE) };
        let begin_draw: FnBeginDraw = unsafe { vtable_fn(target, 48) };
        unsafe { begin_draw(target) };

        // SAFETY: 第 47 槽 Clear，clear_color 指向栈上的全透明色（预乘 alpha 下 0,0,0,0 即透明）。
        let clear: FnClear = unsafe { vtable_fn(target, 47) };
        let clear_color = D2d1ColorF {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
        unsafe { clear(target, &clear_color) };

        fill_rounded(target, card, layout::CARD_RADIUS, brushes.card.0);
        stroke_rounded(
            target,
            card,
            layout::CARD_RADIUS,
            1.0,
            brushes.card_stroke.0,
        );

        match frame.panel {
            Some(panel) => {
                self.draw_panel(target, logical_width, logical_height, panel, &brushes);
            }
            None => self.draw_list(target, card, frame, &brushes, &colors),
        }

        // SAFETY: 第 49 槽 EndDraw，两个 tag 指针传空表示不收集调试标签；
        // 与上面的 BeginDraw 严格配对。EndDraw 返回后绘制命令已经落到 DIB 上。
        let end_draw: FnEndDraw = unsafe { vtable_fn(target, 49) };
        let hr = unsafe { end_draw(target, ptr::null_mut(), ptr::null_mut()) };
        if hr < 0 {
            return Err(format!("EndDraw 失败，HRESULT 0x{hr:08X}"));
        }

        // 全部画刷在这里之后才 Drop（EndDraw 已完成），资源生命周期与 D2D 的批处理要求一致。
        Ok(())
    }

    /// 参数：`target` 本帧渲染目标；`card` 卡片矩形（逻辑像素）；`frame` 本帧内容；
    /// `brushes` 本帧画刷；`colors` 本帧配色（关闭中图标的不透明度从这里取）。
    /// 核心流程：逐可见行画选中 / 悬停底色、图标占位块 / 真图标、标题（左对齐，到进程名列之前
    /// 为止）与进程名（右对齐；关闭中的行文字变灰、图标降到配置的不透明度）→ 空列表时画一行
    /// 灰字 → 有错误时在卡片底部画红字。
    fn draw_list(
        &self,
        target: *mut c_void,
        card: D2d1RectF,
        frame: &Frame<'_>,
        brushes: &Brushes,
        colors: &Palette,
    ) {
        let row_left = card.left + layout::ROW_PAD_X;
        let row_right = card.right - layout::ROW_PAD_X;
        let title_left = row_left + layout::ICON_SIZE + layout::COLUMN_GAP;
        // 标题右边界停在进程名列左侧，长标题由 DWrite 按字符截断，不会压到进程名。
        let title_right = (row_right - layout::PROCESS_COLUMN).max(title_left);

        let list_top = layout::list_top();
        for (row, text) in frame.rows.iter().enumerate() {
            let top = list_top + row as f32 * layout::ROW_HEIGHT;
            let bottom = top + layout::ROW_HEIGHT;

            if frame.selected == Some(row) || frame.hover == Some(row) {
                let highlight = D2d1RectF {
                    left: card.left + layout::ROW_INSET_X,
                    top,
                    right: card.right - layout::ROW_INSET_X,
                    bottom,
                };
                // 选中（浅蓝）优先于悬停（极浅灰）：两者同时命中时只画选中色，避免叠加变脏。
                let selected = frame.selected == Some(row);
                let brush = if selected {
                    brushes.selected.0
                } else {
                    brushes.hover.0
                };
                fill_rounded(target, highlight, layout::ROW_RADIUS, brush);
                if selected {
                    // 选中行再加 1px 选中描边：浅色下是一圈更深的蓝，深色下是更亮的一圈，
                    // 与 egui 版的选中观感一致（用 palette.row_selected_stroke）。
                    stroke_rounded(
                        target,
                        highlight,
                        layout::ROW_RADIUS,
                        1.0,
                        brushes.selected_stroke.0,
                    );
                }
            }

            // 图标位：先铺 16×16 圆角占位块，真图标到了就按等比缩放盖在上面。
            // 关闭中的行整块降透明度（占位块用带 alpha 的颜色，真图标用 DrawBitmap 的 opacity）。
            let icon_top = top + (layout::ROW_HEIGHT - layout::ICON_SIZE) / 2.0;
            let icon_rect = D2d1RectF {
                left: row_left,
                top: icon_top,
                right: row_left + layout::ICON_SIZE,
                bottom: icon_top + layout::ICON_SIZE,
            };
            let icon_fill = if text.closing {
                brushes.closing_icon.0
            } else {
                brushes.icon.0
            };
            fill_rounded(target, icon_rect, layout::ICON_RADIUS, icon_fill);
            if let Some(entry) = self.icons.get(&text.id) {
                if let Some(bitmap) = entry.bitmap.as_ref() {
                    let destination = fit_icon(icon_rect, entry.width, entry.height);
                    let opacity = if text.closing {
                        colors.closing_icon_opacity
                    } else {
                        1.0
                    };
                    draw_bitmap(target, bitmap.0, &destination, opacity);
                }
            }

            // 关闭中的行：标题与进程名一起换成 palette.closing_text，表达「已经在关，别切了」。
            let title_fill = if text.closing {
                brushes.closing_text.0
            } else {
                brushes.title.0
            };
            let process_fill = if text.closing {
                brushes.closing_text.0
            } else {
                brushes.muted.0
            };

            let title_rect = D2d1RectF {
                left: title_left,
                top,
                right: title_right,
                bottom,
            };
            if let Err(err) = draw_row_text(
                self.dwrite.0,
                target,
                text.title,
                self.title_format.0,
                &title_rect,
                text.title_ranges,
                layout::TITLE_SIZE,
                title_fill,
            ) {
                log::warn!("标题绘制失败：{err}");
            }

            let process_rect = D2d1RectF {
                left: row_right - layout::PROCESS_COLUMN,
                top,
                right: row_right,
                bottom,
            };
            if let Err(err) = draw_row_text(
                self.dwrite.0,
                target,
                text.process,
                self.process_format.0,
                &process_rect,
                text.process_ranges,
                layout::PROCESS_SIZE,
                process_fill,
            ) {
                log::warn!("进程名绘制失败：{err}");
            }
        }

        if let Some(message) = frame.message {
            let message_rect = D2d1RectF {
                left: row_left,
                top: list_top,
                right: row_right,
                bottom: list_top + layout::ROW_HEIGHT,
            };
            draw_text(
                target,
                message,
                self.message_format.0,
                &message_rect,
                brushes.muted.0,
            );
        }

        if let Some(error) = frame.error {
            let error_rect = D2d1RectF {
                left: card.left + layout::CARD_PAD,
                top: card.bottom - 4.0 - layout::HINT_SIZE,
                right: card.right - layout::CARD_PAD,
                bottom: card.bottom - 4.0,
            };
            draw_text(
                target,
                error,
                self.error_format.0,
                &error_rect,
                brushes.error.0,
            );
        }
    }

    /// 参数：`target` 本帧渲染目标；`logical_width` / `logical_height` 窗口逻辑尺寸；
    /// `panel` 面板内容（草稿快照）；`brushes` 本帧画刷。
    /// 核心流程：按 [`layout::panel_layout`] 的矩形依次画面板底色 + 描边、标题、两个复选框、
    /// 热键值框与「录制」按钮、录制说明、主题三选一、面板红字与确定 / 取消按钮。布局与命中
    /// 测试共用同一份 `PanelLayout`，画在哪就能点在哪。
    fn draw_panel(
        &self,
        target: *mut c_void,
        logical_width: f32,
        logical_height: f32,
        panel: &OptionsView<'_>,
        brushes: &Brushes,
    ) {
        let layout::PanelLayout {
            panel: panel_rect,
            title,
            enable_row,
            hotkey_value,
            record_button,
            hint,
            autostart_row,
            theme_label,
            theme_choices,
            error,
            save_button,
            cancel_button,
        } = layout::panel_layout(logical_width, logical_height);

        fill_rounded(
            target,
            rect_of(panel_rect),
            layout::PANEL_RADIUS,
            brushes.panel.0,
        );
        stroke_rounded(
            target,
            rect_of(panel_rect),
            layout::PANEL_RADIUS,
            1.0,
            brushes.card_stroke.0,
        );

        draw_text(
            target,
            PANEL_TITLE_TEXT,
            self.title_format.0,
            &rect_of(title),
            brushes.title.0,
        );
        draw_checkbox_row(
            target,
            enable_row,
            PANEL_ENABLE_TEXT,
            panel.enable_hotkey,
            self.title_format.0,
            brushes,
        );
        draw_checkbox_row(
            target,
            autostart_row,
            PANEL_AUTOSTART_TEXT,
            panel.autostart,
            self.title_format.0,
            brushes,
        );

        // 热键值框：录制中显示提示（次要色），否则显示当前组合键。
        fill_rounded(
            target,
            rect_of(hotkey_value),
            layout::ROW_RADIUS,
            brushes.search.0,
        );
        stroke_rounded(
            target,
            rect_of(hotkey_value),
            layout::ROW_RADIUS,
            1.0,
            brushes.card_stroke.0,
        );
        // 热键被关掉时整组置灰：值框与「录制」按钮都显示成「按不动」的样子。
        let (value_text, value_brush) = if panel.recording || !panel.enable_hotkey {
            (PANEL_RECORDING_TEXT.to_string(), brushes.muted.0)
        } else {
            (crate::options::hotkey_text(&panel.hotkey), brushes.title.0)
        };
        draw_text(
            target,
            &value_text,
            self.title_format.0,
            &rect_of(hotkey_value.inset(8.0, 0.0)),
            value_brush,
        );
        draw_button(
            target,
            record_button,
            PANEL_RECORD_BUTTON,
            self.center_format.0,
            brushes,
            false,
            panel.enable_hotkey,
        );
        draw_text(
            target,
            PANEL_RECORD_HINT,
            self.hint_format.0,
            &rect_of(hint),
            brushes.muted.0,
        );

        // 主题三选一：当前项用选中色 + 选中描边，点当前项会循环到下一项。
        draw_text(
            target,
            PANEL_THEME_TEXT,
            self.title_format.0,
            &rect_of(theme_label),
            brushes.title.0,
        );
        for (index, theme) in crate::theme::THEME_CHOICES.iter().enumerate() {
            draw_theme_choice(
                target,
                theme_choices[index],
                crate::theme::theme_label(*theme),
                panel.theme == *theme,
                self.center_format.0,
                brushes,
            );
        }

        if let Some(err) = panel.error {
            draw_text(
                target,
                err,
                self.message_format.0,
                &rect_of(error),
                brushes.error.0,
            );
        }

        draw_button(
            target,
            save_button,
            PANEL_SAVE_BUTTON,
            self.center_format.0,
            brushes,
            true,
            true,
        );
        draw_button(
            target,
            cancel_button,
            PANEL_CANCEL_BUTTON,
            self.center_format.0,
            brushes,
            false,
            true,
        );
    }
}

/// 一帧用到的全部 D2D 画刷。
///
/// 参数：每个字段对应 [`Palette`] 里的一个颜色（另有面板底色与描边色）
/// 核心流程：在 `BeginDraw` 之前一次性建好、到 `EndDraw` 之后才随作用域 Drop——D2D 的绘制命令
/// 要到 `EndDraw` 才真正提交，提前释放会让这一批命令引用到已销毁的画刷。
struct Brushes {
    card: Com,
    card_stroke: Com,
    panel: Com,
    search: Com,
    selected: Com,
    selected_stroke: Com,
    hover: Com,
    icon: Com,
    closing_icon: Com,
    title: Com,
    closing_text: Com,
    muted: Com,
    error: Com,
}

impl Brushes {
    /// 参数：`target` 本帧渲染目标；`colors` 本帧配色
    /// 核心流程：逐个 `CreateSolidColorBrush`，任一失败就返回错误串（已建好的画刷由 [`Com`]
    /// 的 Drop 自动释放，不会漏）。
    fn new(target: *mut c_void, colors: &Palette) -> Result<Self, String> {
        Ok(Self {
            card: solid_brush(target, &colors.card_fill)?,
            card_stroke: solid_brush(target, &colors.card_stroke)?,
            panel: solid_brush(target, &colors.panel_fill)?,
            search: solid_brush(target, &colors.search_fill)?,
            selected: solid_brush(target, &colors.row_selected_fill)?,
            selected_stroke: solid_brush(target, &colors.row_selected_stroke)?,
            hover: solid_brush(target, &colors.row_hover_fill)?,
            icon: solid_brush(target, &colors.icon_placeholder)?,
            closing_icon: solid_brush(target, &colors.closing_icon_fill())?,
            title: solid_brush(target, &colors.text_primary)?,
            closing_text: solid_brush(target, &colors.closing_text)?,
            muted: solid_brush(target, &colors.text_muted)?,
            error: solid_brush(target, &colors.error_text)?,
        })
    }
}

/// 参数：`rect` 布局矩形（DIP）
/// 核心流程：换成 D2D 的左上右下四个浮点，供 `fill_rounded` / `stroke_rounded` / `draw_text`
/// 使用。布局与命中用的是 [`layout::Rect`]，绘制只在这一处换算，两边不会各自算一遍。
fn rect_of(rect: layout::Rect) -> D2d1RectF {
    D2d1RectF {
        left: rect.x,
        top: rect.y,
        right: rect.right(),
        bottom: rect.bottom(),
    }
}

/// 参数：`target` 渲染目标；`rect` 矩形（逻辑像素）；`radius` 圆角半径；`stroke_width` 线宽；
/// `brush` 描边画刷
/// 核心流程：包成 `D2D1_ROUNDED_RECT` 后调用 `ID2D1RenderTarget::DrawRoundedRectangle`
/// （vtable 第 18 槽）。
fn stroke_rounded(
    target: *mut c_void,
    rect: D2d1RectF,
    radius: f32,
    stroke_width: f32,
    brush: *mut c_void,
) {
    let rounded = D2d1RoundedRect {
        rect,
        radius_x: radius,
        radius_y: radius,
    };
    // SAFETY: 第 18 槽 DrawRoundedRectangle 只读 &rounded 与本帧内仍然存活的画刷，
    // stroke_style 传空表示用默认（实线）样式。
    let draw: FnDrawRoundedRect = unsafe { vtable_fn(target, 18) };
    unsafe { draw(target, &rounded, brush, stroke_width, ptr::null_mut()) };
}

/// 参数：`target` 渲染目标；`rect` 按钮矩形；`label` 按钮文字；`format` 居中的文字格式；
/// `brushes` 本帧画刷；`accent` 是否用强调色（「确定」用）；`enabled` 是否可用（不可用时文字
/// 走次要色，视觉上「按不动」）
/// 核心流程：铺底色 + 1px 描边，再把文字水平垂直居中画上去。
fn draw_button(
    target: *mut c_void,
    rect: layout::Rect,
    label: &str,
    format: *mut c_void,
    brushes: &Brushes,
    accent: bool,
    enabled: bool,
) {
    let (fill, stroke) = if accent && enabled {
        (brushes.selected.0, brushes.selected_stroke.0)
    } else {
        (brushes.search.0, brushes.card_stroke.0)
    };
    let text = if enabled {
        brushes.title.0
    } else {
        brushes.muted.0
    };
    let box_rect = rect_of(rect);
    fill_rounded(target, box_rect, layout::ROW_RADIUS, fill);
    stroke_rounded(target, box_rect, layout::ROW_RADIUS, 1.0, stroke);
    draw_text(target, label, format, &box_rect, text);
}

/// 参数：`target` 渲染目标；`row` 整行矩形；`label` 文字；`checked` 是否勾选；
/// `format` 文字格式；`brushes` 本帧画刷
/// 核心流程：左边画 14×14 方框（输入框底色 + 1px 描边），勾选时在方框里再填一个小一号的强调
/// 色方块；右边画文字。命中区是整行（见 [`layout::PanelLayout::hit`]），比只点小方框好按。
fn draw_checkbox_row(
    target: *mut c_void,
    row: layout::Rect,
    label: &str,
    checked: bool,
    format: *mut c_void,
    brushes: &Brushes,
) {
    let side = layout::CHECK_BOX;
    let box_rect = layout::Rect::new(row.x, row.y + (row.height - side) / 2.0, side, side);
    let box_d2d = rect_of(box_rect);
    fill_rounded(target, box_d2d, layout::ICON_RADIUS, brushes.search.0);
    stroke_rounded(
        target,
        box_d2d,
        layout::ICON_RADIUS,
        1.0,
        brushes.card_stroke.0,
    );
    if checked {
        fill_rounded(
            target,
            rect_of(box_rect.inset(3.0, 3.0)),
            2.0,
            brushes.selected_stroke.0,
        );
    }
    let label_x = box_rect.right() + layout::CHECK_LABEL_GAP;
    let label_rect =
        layout::Rect::new(label_x, row.y, (row.right() - label_x).max(0.0), row.height);
    draw_text(target, label, format, &rect_of(label_rect), brushes.title.0);
}

/// 参数：`target` 渲染目标；`rect` 选项矩形；`label` 文字；`selected` 是否当前选中；
/// `format` 居中的文字格式；`brushes` 本帧画刷
/// 核心流程：选中项用「选中底色 + 选中描边」，未选中项用「输入框底色 + 普通描边」；文字居中。
fn draw_theme_choice(
    target: *mut c_void,
    rect: layout::Rect,
    label: &str,
    selected: bool,
    format: *mut c_void,
    brushes: &Brushes,
) {
    let (fill, stroke) = if selected {
        (brushes.selected.0, brushes.selected_stroke.0)
    } else {
        (brushes.search.0, brushes.card_stroke.0)
    };
    let box_rect = rect_of(rect);
    fill_rounded(target, box_rect, layout::ROW_RADIUS, fill);
    stroke_rounded(target, box_rect, layout::ROW_RADIUS, 1.0, stroke);
    draw_text(target, label, format, &box_rect, brushes.title.0);
}

/// 参数：`target` 渲染目标；`edit` 输入框矩形（DIP）；`hint` 占位文案；
/// `format` 文字格式；`color` 文字颜色
/// 核心流程：单独开一轮 `BeginDraw` / `EndDraw`，把占位文案画在输入框上。这一步必须排在
/// [`paint_edit`] 之后：EDIT 用 `WM_PRINTCLIENT` 画自己时会把背景刷成卡片底色，先画的文案会被
/// 擦掉。画刷在 `EndDraw` 之后才 Drop，与 D2D 的批处理要求一致。
fn paint_edit_hint(
    target: *mut c_void,
    edit: EditSurface,
    hint: &str,
    format: *mut c_void,
    color: &D2d1ColorF,
) -> Result<(), String> {
    let brush = solid_brush(target, color)?;
    let rect = D2d1RectF {
        left: edit.left + HINT_TEXT_INSET,
        top: edit.top,
        right: (edit.left + edit.width).max(edit.left),
        bottom: edit.top + edit.height,
    };
    // SAFETY: 第 48 槽 BeginDraw 只作用于本帧渲染目标；这一轮里只画一行文字，不再创建资源。
    let begin: FnBeginDraw = unsafe { vtable_fn(target, 48) };
    unsafe { begin(target) };
    draw_text(target, hint, format, &rect, brush.0);
    // SAFETY: 第 49 槽 EndDraw，与上面的 BeginDraw 严格配对；tag 指针传空表示不收集调试标签。
    let end: FnEndDraw = unsafe { vtable_fn(target, 49) };
    let hr = unsafe { end(target, ptr::null_mut(), ptr::null_mut()) };
    if hr < 0 {
        return Err(format!("EndDraw(占位文案) 失败，HRESULT 0x{hr:08X}"));
    }
    Ok(())
}

/// 参数：`canvas` 本帧画布（其 DC 上已经画好卡片与列表）；`scale` DPI 缩放；
/// `edit` 输入框句柄与 DIP 矩形。
/// 核心流程：`SaveDC` 保存 DC 状态 → 把视口原点挪到输入框左上角并 `IntersectClipRect` 限制在
/// 输入框范围内 → `SendMessageW(WM_PRINTCLIENT, hdc, PRF_CLIENT | PRF_ERASEBKGND)` 让 EDIT
/// 按自己当前的文本 / 字体 / 占位文案画一遍（背景色来自父窗口的 `WM_CTLCOLOREDIT`）→
/// `RestoreDC` 还原 → 把这块区域的 alpha 补成 255。
fn paint_edit(canvas: &Canvas, scale: f32, edit: EditSurface) {
    let left = (edit.left * scale).round() as i32;
    let top = (edit.top * scale).round() as i32;
    let width = (edit.width * scale).round() as i32;
    let height = (edit.height * scale).round() as i32;
    if width <= 0 || height <= 0 {
        return;
    }

    // SAFETY: 四个调用都只作用于本帧 Canvas 的 DC：SaveDC 返回状态句柄供 RestoreDC 还原；
    // SetViewportOrgEx 只改坐标原点；IntersectClipRect 只收窄裁剪区；SendMessageW 让 EDIT 画到
    // 这块 DC 上——EDIT 是本进程的活控件，同步调用期间句柄有效。
    unsafe {
        let saved = SaveDC(canvas.dc);
        if saved == 0 {
            return;
        }
        SetViewportOrgEx(canvas.dc, left, top, ptr::null_mut());
        IntersectClipRect(canvas.dc, 0, 0, width, height);
        SendMessageW(
            edit.hwnd,
            WM_PRINTCLIENT,
            canvas.dc as usize,
            (PRF_CLIENT | PRF_ERASEBKGND) as isize,
        );
        RestoreDC(canvas.dc, saved);
    }

    canvas.make_opaque(left, top, width, height);
}

/// 参数：`target` 渲染目标指针；`color` 画刷颜色。
/// 核心流程：`ID2D1RenderTarget::CreateSolidColorBrush`（第 8 槽），brush_properties
/// 传空表示不透明 + 单位矩阵。
fn solid_brush(target: *mut c_void, color: &D2d1ColorF) -> Result<Com, String> {
    let mut brush: *mut c_void = ptr::null_mut();
    // SAFETY: 第 8 槽 CreateSolidColorBrush 只读 color（'static 常量或调用方栈变量）
    // 与可空的 brush_properties，写栈上的 brush。
    let create: FnCreateSolidColorBrush = unsafe { vtable_fn(target, 8) };
    let hr = unsafe { create(target, color, ptr::null(), &mut brush) };
    if hr < 0 || brush.is_null() {
        return Err(format!("CreateSolidColorBrush 失败，HRESULT 0x{hr:08X}"));
    }
    Ok(Com(brush))
}

/// 参数：`target` 渲染目标；`rect` 矩形（逻辑像素）；`radius` 圆角半径；`brush` 填充画刷。
/// 核心流程：把矩形与半径包成 `D2D1_ROUNDED_RECT` 后调用 `FillRoundedRectangle`（第 19 槽）。
fn fill_rounded(target: *mut c_void, rect: D2d1RectF, radius: f32, brush: *mut c_void) {
    let rounded = D2d1RoundedRect {
        rect,
        radius_x: radius,
        radius_y: radius,
    };
    // SAFETY: 第 19 槽 FillRoundedRectangle 只读 &rounded 与本帧内仍然存活的画刷。
    let fill: FnFillRoundedRect = unsafe { vtable_fn(target, 19) };
    unsafe { fill(target, &rounded, brush) };
}

/// 参数：`box_rect` 行内 16×16 的图标方框；`pixel_width` / `pixel_height` 图标像素尺寸。
/// 核心流程：按「短边贴边」算等比缩放，再把结果在方框里居中——32×32 的图标正好铺满 16×16；
/// 非正方形的图标不会被拉变形，只是左右或上下留白。像素尺寸非法时原样返回方框。
fn fit_icon(box_rect: D2d1RectF, pixel_width: u32, pixel_height: u32) -> D2d1RectF {
    if pixel_width == 0 || pixel_height == 0 {
        return box_rect;
    }
    let box_width = box_rect.right - box_rect.left;
    let box_height = box_rect.bottom - box_rect.top;
    let scale = (box_width / pixel_width as f32).min(box_height / pixel_height as f32);
    let width = pixel_width as f32 * scale;
    let height = pixel_height as f32 * scale;
    let left = box_rect.left + (box_width - width) / 2.0;
    let top = box_rect.top + (box_height - height) / 2.0;
    D2d1RectF {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

/// 参数：`target` 渲染目标；`entry` 已经备好预乘 BGRA 像素的图标条目。
/// 核心流程：填 `D2D1_BITMAP_PROPERTIES`（B8G8R8A8 + 预乘 alpha、96 DPI）→
/// `ID2D1RenderTarget::CreateBitmap`（第 4 槽）把像素拷进一张 D2D 位图，行跨距 = 宽 × 4。
/// 位图必须由**当前**渲染目标创建：D2D 位图是设备相关资源，跨目标使用会被拒绝。
fn create_icon_bitmap(target: *mut c_void, entry: &IconEntry) -> Result<Com, String> {
    let properties = D2d1BitmapProperties {
        pixel_format: D2d1PixelFormat {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alpha_mode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpi_x: 96.0,
        dpi_y: 96.0,
    };
    let size = D2d1SizeU {
        width: entry.width,
        height: entry.height,
    };
    let mut bitmap: *mut c_void = ptr::null_mut();
    // SAFETY: 第 4 槽 CreateBitmap 只读 entry.pixels（长度在 insert_icon 里已确保
    // ≥ 宽 × 高 × 4）与 &properties，写栈上的 bitmap；pitch 按 32bpp 计算。
    let create: FnCreateBitmap = unsafe { vtable_fn(target, 4) };
    let hr = unsafe {
        create(
            target,
            size,
            entry.pixels.as_ptr().cast::<c_void>(),
            entry.width * 4,
            &properties,
            &mut bitmap,
        )
    };
    if hr < 0 || bitmap.is_null() {
        return Err(format!("CreateBitmap 失败，HRESULT 0x{hr:08X}"));
    }
    Ok(Com(bitmap))
}

/// 参数：`target` 渲染目标；`bitmap` 图标位图；`destination` 目标矩形（逻辑像素，等比缩放）；
/// `opacity` 不透明度（关闭中的行传 0.4，其余传 1.0）。
/// 核心流程：`ID2D1RenderTarget::DrawBitmap`（第 26 槽），线性插值，source 传空表示整张位图。
fn draw_bitmap(target: *mut c_void, bitmap: *mut c_void, destination: &D2d1RectF, opacity: f32) {
    // SAFETY: 第 26 槽 DrawBitmap 只读取位图与 &destination（都是本帧内仍然存活的对象）。
    let draw: FnDrawBitmap = unsafe { vtable_fn(target, 26) };
    unsafe {
        draw(
            target,
            bitmap,
            destination,
            opacity,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            ptr::null(),
        );
    }
}

/// 参数：`dwrite` DWrite 工厂；`target` 渲染目标；`text` 要画的文本；`format` 文本格式；
/// `rect` 排版矩形（逻辑像素）；`ranges` 命中字符的 UTF-8 字节区间；`size` 字号（粗体不可用
/// 时的退路要用）；`brush` 前景画刷。
/// 核心流程：文本转 UTF-16 → `IDWriteFactory::CreateTextLayout`（第 18 槽，maxWidth /
/// maxHeight 就是排版盒子的宽高）→ 显式 `SetMaxWidth`（第 28 槽）把宽度钉死在盒子里 →
/// 命中区间换算成 UTF-16 偏移后逐段 `SetFontWeight(BOLD)`（第 32 槽；失败时退到
/// `SetFontSize` +0.5）→ `ID2D1RenderTarget::DrawTextLayout`（第 28 槽）从矩形左上角画出。
/// 格式里已经设了 `NO_WRAP` + 按字符截断，超长文本因此只会被截短，不会溢出到别的列。
///
/// 粗体为什么不用第二个 `IDWriteTextFormat`：`SetFontWeight` 只改区间内字符的字重，
/// DirectWrite 会在「同一族 / 该字符的回退族」里挑对应的粗体字面（中文回退到
/// Microsoft YaHei，就会选中它的 Bold 成员），所以整个行仍然只有一套格式、一次布局。
///
/// 参数就是一次 DirectWrite 排版需要的全部输入（工厂、目标、文本、格式、盒子、命中区间、
/// 字号、画刷），都是裸指针 / 借用，把其中几个打包成结构体只会多一层解构，故直接允许 8 个参数。
#[allow(clippy::too_many_arguments)]
fn draw_row_text(
    dwrite: *mut c_void,
    target: *mut c_void,
    text: &str,
    format: *mut c_void,
    rect: &D2d1RectF,
    ranges: &[Range<usize>],
    size: f32,
    brush: *mut c_void,
) -> Result<(), String> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let mut layout: *mut c_void = ptr::null_mut();
    // SAFETY: 第 18 槽 CreateTextLayout 只读 wide（长度由 wide.len() 给出，不要求 NUL 结尾）
    // 与 format，写栈上的 layout；wide / format 在调用期间都存活。
    let create: FnCreateTextLayout = unsafe { vtable_fn(dwrite, 18) };
    let hr = unsafe {
        create(
            dwrite,
            wide.as_ptr(),
            wide.len() as u32,
            format,
            width,
            height,
            &mut layout,
        )
    };
    if hr < 0 || layout.is_null() {
        return Err(format!("CreateTextLayout 失败，HRESULT 0x{hr:08X}"));
    }
    let layout = Com(layout);

    // CreateTextLayout 的 maxWidth 已经限制过宽度，这里再显式设一次：将来若调整
    // CreateTextLayout 的参数，宽度限制也不会跟着丢。
    // SAFETY: 第 28 槽 SetMaxWidth 只改本布局的宽度上限，参数是上面算出的矩形宽度。
    let set_max_width: FnSetMaxWidth = unsafe { vtable_fn(layout.0, 28) };
    let hr = unsafe { set_max_width(layout.0, width) };
    if hr < 0 {
        return Err(format!("SetMaxWidth 失败，HRESULT 0x{hr:08X}"));
    }

    // UTF-8 字节区间 → UTF-16 code unit 区间（CJK 1 个 unit、emoji 2 个 unit）。
    for hit in highlight::utf8_range_to_utf16_range(text, ranges) {
        let text_range = DwriteTextRange {
            start: hit.start as u32,
            length: (hit.end - hit.start) as u32,
        };
        // SAFETY: 第 32 槽 SetFontWeight 只改本布局里 [start, start+length) 这段字符的权重；
        // 区间来自 highlight 的边界校验，落在 wide.len() 之内。
        let set_weight: FnSetFontWeight = unsafe { vtable_fn(layout.0, 32) };
        let hr = unsafe { set_weight(layout.0, DWRITE_FONT_WEIGHT_BOLD, text_range) };
        if hr < 0 {
            bold_fallback(layout.0, text_range, size);
        }
    }

    let origin = D2d1PointF {
        x: rect.left,
        y: rect.top,
    };
    // SAFETY: 第 28 槽 DrawTextLayout 只读 origin（按值）、布局与画刷，三者在本帧内都存活。
    let draw: FnDrawTextLayout = unsafe { vtable_fn(target, 28) };
    unsafe {
        draw(target, origin, layout.0, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
    Ok(())
}

/// 参数：`layout` 文本布局；`range` 加粗失败的区间；`size` 该行的原字号。
/// 核心流程：`SetFontWeight(BOLD)` 失败（系统字体集合里没有这一族的粗体成员）时，退回
/// 「同一个格式、字号 +0.5」来制造视觉差异，并用一个进程级的 [`AtomicBool`] 保证这条 warn
/// 只记一次——否则每帧每行都会刷屏。
fn bold_fallback(layout: *mut c_void, range: DwriteTextRange, size: f32) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        log::warn!("SetFontWeight(BOLD) 失败，命中字符改用字号 +0.5 区分（只提示一次）");
    }
    // SAFETY: 第 35 槽 SetFontSize 只改本布局里该区间的字号，区间与上一次调用相同。
    let set_size: FnSetFontSize = unsafe { vtable_fn(layout, 35) };
    unsafe { set_size(layout, size + 0.5, range) };
}

/// 参数：`target` 渲染目标；`text` 要画的文本；`format` 文本格式；`rect` 排版矩形（逻辑像素）；
/// `brush` 前景画刷。
/// 核心流程：把文本转成 UTF-16 缓冲（调用期间存活）→ `ID2D1RenderTarget::DrawText`
/// （第 27 槽），options 与 measuring_mode 用默认值。只有「没有命中区间」的提示文字走这里，
/// 列表行一律走 [`draw_row_text`]。
fn draw_text(
    target: *mut c_void,
    text: &str,
    format: *mut c_void,
    rect: &D2d1RectF,
    brush: *mut c_void,
) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: 第 27 槽 DrawText 只读取 wide 指向的 UTF-16 缓冲（长度由 wide.len() 给出，
    // 不要求 NUL 结尾）、rect 与 format；缓冲与矩形都在调用期间存活。
    let draw: FnDrawText = unsafe { vtable_fn(target, 27) };
    unsafe {
        draw(
            target,
            wide.as_ptr(),
            wide.len() as u32,
            format,
            rect,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16×16 方框里放 32×32 图标：等比缩放后正好铺满。
    #[test]
    fn square_icon_fills_the_box() {
        let box_rect = D2d1RectF {
            left: 10.0,
            top: 20.0,
            right: 26.0,
            bottom: 36.0,
        };
        let fitted = fit_icon(box_rect, 32, 32);
        assert_eq!(fitted.left, 10.0);
        assert_eq!(fitted.top, 20.0);
        assert_eq!(fitted.right, 26.0);
        assert_eq!(fitted.bottom, 36.0);
    }

    /// 两套配色的关键值：浅色沿用改造前的观感，深色用需求给定的一组值。
    #[test]
    fn light_and_dark_palettes_carry_the_expected_colours() {
        let light = palette(false);
        assert_eq!(light.card_fill.to_colorref(), 0x00FF_FFFF);
        assert_eq!(light.row_selected_fill.to_colorref(), 0x00F7_E4CC);
        assert_eq!(light.row_hover_fill.to_colorref(), 0x00F5_F5F5);
        assert_eq!(light.text_primary.to_colorref(), 0x001F_1F1F);
        assert_eq!(light.text_muted.to_colorref(), 0x0080_8080);
        assert_eq!(light.icon_placeholder.to_colorref(), 0x00DD_DDDD);
        assert_eq!(light.closing_text.to_colorref(), 0x00A6_A6A6);
        assert_eq!(light.panel_fill.to_colorref(), 0x00FF_FFFF);

        let dark = palette(true);
        assert_eq!(dark.card_fill.to_colorref(), 0x0020_2020);
        assert_eq!(dark.card_stroke.to_colorref(), 0x003A_3A3A);
        assert_eq!(dark.search_fill.to_colorref(), 0x001A_1A1A);
        assert_eq!(dark.text_primary.to_colorref(), 0x00E8_E8E8);
        assert_eq!(dark.text_muted.to_colorref(), 0x0090_9090);
        assert_eq!(dark.row_selected_fill.to_colorref(), 0x0063_4A2C);
        assert_eq!(dark.row_selected_stroke.to_colorref(), 0x008F_6B3E);
        assert_eq!(dark.row_hover_fill.to_colorref(), 0x002A_2A2A);
        assert_eq!(dark.icon_placeholder.to_colorref(), 0x003C_3C3C);
        assert_eq!(dark.closing_text.to_colorref(), 0x005E_5E5E);
        assert_eq!(dark.error_text.to_colorref(), 0x0072_7BFF);
        assert_eq!(dark.panel_fill.to_colorref(), 0x0026_2626);
        assert_eq!(dark.closing_icon_opacity, 0.4);
        // 关闭中的图标占位块 = 占位块颜色 + 0.4 不透明度，颜色本身不变。
        let closing = dark.closing_icon_fill();
        assert_eq!(closing.to_colorref(), dark.icon_placeholder.to_colorref());
        assert_eq!(closing.a, 0.4);
    }

    /// 8 位三原色与 COLORREF（0x00BBGGRR）的双向口径：白 / 黑 / 中灰。
    #[test]
    fn colour_ref_uses_bgra_byte_order() {
        assert_eq!(
            D2d1ColorF::from_rgb8(0xFF, 0xFF, 0xFF).to_colorref(),
            0x00FF_FFFF
        );
        assert_eq!(
            D2d1ColorF::from_rgb8(0x00, 0x00, 0x00).to_colorref(),
            0x0000_0000
        );
        assert_eq!(
            D2d1ColorF::from_rgb8(0x1A, 0x1A, 0x1A).to_colorref(),
            0x001A_1A1A
        );
        // 红在最低字节、蓝在第三字节：COLORREF 的字节序与 RGB 相反。
        assert_eq!(
            D2d1ColorF::from_rgb8(0x12, 0x34, 0x56).to_colorref(),
            0x0056_3412
        );
    }

    /// 32×16 的图标：按短边贴边，上下居中，宽度只占一半。
    #[test]
    fn wide_icon_is_centered_without_distortion() {
        let box_rect = D2d1RectF {
            left: 0.0,
            top: 0.0,
            right: 16.0,
            bottom: 16.0,
        };
        let fitted = fit_icon(box_rect, 32, 16);
        assert_eq!(fitted.left, 4.0);
        assert_eq!(fitted.top, 0.0);
        assert_eq!(fitted.right, 12.0);
        assert_eq!(fitted.bottom, 16.0);
    }
}
