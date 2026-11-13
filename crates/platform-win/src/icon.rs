//! 窗口图标解码（US-044）。
//!
//! 参数：只接收上层的 [`WindowId`]，不接收也不暴露任何 Win32 句柄
//! 核心流程：`WM_GETICON`（ICON_BIG → ICON_SMALL，`SendMessageTimeoutW` 100ms 超时）→
//! 窗口类图标（GCLP_HICON → GCLP_HICONSM）→ 可执行文件图标（`ExtractIconExW`，失败再退
//! `SHGetFileInfoW`），任一步失败就降级到下一步，全部失败返回 `None` 且不 panic。
//! 拿到 HICON 后用 `GetIconInfo` + `GetDIBits` 读出 32 位 BGRA，再交给
//! [`crate::win32_parse`] 里的纯函数换成 RGBA（没有 alpha 通道时用 AND 掩码合成）。
//! 解码在调用线程上同步完成，本模块不建线程——放到后台线程是上层的事。
//!
//! 诊断：每一级的结果都打 `debug` 级日志，前缀统一是 `icon:`，便于真机 `grep`：
//! - `icon: tier=wm_geticon … big=0x… small=0x…`（句柄是不是 0）
//! - `icon: tier=class_icon … big=0x… small=0x…`
//! - `icon: tier=exe_icon path=… count=… large=0x… small=0x…`
//! - `icon: tier=shell_icon path=… ok=… hicon=0x…`
//! - `icon: decode start …`、`icon: decode result ok …`、`icon: decode fail … reason=…`
//! - `icon: result=ok|none id=0x…`
//!
//! 判读方法：有 `decode start` 却没有 `result ok` = 「拿到句柄但解码失败」；连
//! `tier=` 行都只有 `0x0` = 「拿不到句柄」。

use switcheroo_core::WindowId;

/// 一张已解码的图标位图（RGBA8，非预乘）。
///
/// 参数：`width`/`height` 像素尺寸，`rgba` 长度为 `width * height * 4` 的自上而下像素
/// 核心流程：只承载"能直接交给纹理上传"的裸数据；尺寸原样保留，缩放交给上层，
/// 平台层不做任何重采样。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 参数：`id` 目标窗口 id
/// 核心流程：`IsWindow` 校验窗口还在 → `WM_GETICON` 取 ICON_BIG / ICON_SMALL
/// （两次都用 `SendMessageTimeoutW` 限时 100ms，卡住的窗口不会阻塞调用方）→ 都拿不到就
/// 读窗口类图标 GCLP_HICON / GCLP_HICONSM → 还是没有就去可执行文件里先 `ExtractIconExW`
/// 抽第一个图标，再退到 `SHGetFileInfoW` 要一个图标。每一级拿到的 HICON 都先解码，
/// 解码失败继续降级；全部失败返回 `None`。每一级的结果都打 `icon:` 前缀的 debug 日志。
#[cfg(windows)]
pub fn window_icon(id: WindowId) -> Option<IconImage> {
    use windows::Win32::UI::WindowsAndMessaging::{
        IsWindow, GCLP_HICON, GCLP_HICONSM, ICON_BIG, ICON_SMALL,
    };

    let hwnd = crate::window_enum::hwnd_from_id(id);

    // SAFETY: IsWindow 只校验句柄是否仍指向一个窗口，只读、不转移所有权；窗口已被销毁时返回 FALSE。
    if hwnd.is_invalid() || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        log::debug!("icon: tier=window_check id={:#X} alive=false", id.raw());
        return None;
    }

    // 第 1 步：窗口自己登记的图标，先大后小。超时由 SendMessageTimeoutW 保证。
    let big = message_icon(hwnd, ICON_BIG);
    let small = message_icon(hwnd, ICON_SMALL);
    log::debug!(
        "icon: tier=wm_geticon id={:#X} big={:#X} small={:#X}",
        id.raw(),
        handle_of(big),
        handle_of(small)
    );
    if let Some((tier, image)) =
        first_decodable([("wm_geticon_big", big), ("wm_geticon_small", small)])
    {
        return report_ok(id, tier, image);
    }

    // 第 2 步：窗口类里登记的图标（老程序只设 WNDCLASS，不处理 WM_GETICON）。
    let big = class_icon(hwnd, GCLP_HICON);
    let small = class_icon(hwnd, GCLP_HICONSM);
    log::debug!(
        "icon: tier=class_icon id={:#X} big={:#X} small={:#X}",
        id.raw(),
        handle_of(big),
        handle_of(small)
    );
    if let Some((tier, image)) =
        first_decodable([("class_icon_big", big), ("class_icon_small", small)])
    {
        return report_ok(id, tier, image);
    }

    // 第 3 步：可执行文件里的图标（ExtractIconExW，失败再 SHGetFileInfoW 兜底）。
    if let Some((tier, image)) = exe_icon(hwnd) {
        return report_ok(id, tier, image);
    }

    log::debug!(
        "icon: result=none id={:#X} reason=all_tiers_failed",
        id.raw()
    );
    None
}

/// 参数：`id` 目标窗口 id
/// 核心流程：非 Windows 平台没有窗口图标可取，恒返回 `None`，让上层继续画占位方块，
/// 保证 workspace 在开发机上也能编译通过。
#[cfg(not(windows))]
pub fn window_icon(id: WindowId) -> Option<IconImage> {
    log::debug!("icon: result=none id={:#X} reason=not_windows", id.raw());
    None
}

/// 参数：`id` 目标窗口；`tier` 命中的降级级名；`image` 解好的位图
/// 核心流程：打一条成功后统一格式的日志再原样返回，避免每一级各写一遍日志格式。
#[cfg(windows)]
fn report_ok(id: WindowId, tier: &'static str, image: IconImage) -> Option<IconImage> {
    log::debug!(
        "icon: result=ok id={:#X} tier={tier} w={} h={}",
        id.raw(),
        image.width,
        image.height
    );
    Some(image)
}

/// 参数：`hicon` 可选的图标句柄
/// 核心流程：日志里只想看到数值，空句柄统一打成 `0x0`，免得 `Option` 的 Debug 输出在
/// 真机日志里难以 grep。
#[cfg(windows)]
fn handle_of(hicon: Option<windows::Win32::UI::WindowsAndMessaging::HICON>) -> usize {
    hicon.map_or(0, |handle| handle.0 as usize)
}

/// 参数：`candidates` 按降级顺序排列的 `(级名, 句柄)` 对，句柄为 `None` 表示这一级没拿到
/// 核心流程：依次解码，第一个解码成功的连同它的级名一起返回；全部失败返回 `None`。
/// 只做"挑一个能解的"，不碰任何所有权——句柄属于窗口或窗口类，都不由这里销毁。
#[cfg(windows)]
fn first_decodable<const N: usize>(
    candidates: [(
        &'static str,
        Option<windows::Win32::UI::WindowsAndMessaging::HICON>,
    ); N],
) -> Option<(&'static str, IconImage)> {
    for (tier, hicon) in candidates {
        let Some(hicon) = hicon else {
            continue;
        };
        if let Some(image) = icon_image(hicon, tier) {
            return Some((tier, image));
        }
    }
    None
}

/// `WM_GETICON` 的单次超时（毫秒）：目标窗口卡住时最多等这么久就降级到下一步。
#[cfg(windows)]
const ICON_TIMEOUT_MS: u32 = 100;

/// 单边最大像素：超过这个值的尺寸按脏数据处理（避免按异常句柄分配巨量内存）。
#[cfg(windows)]
const MAX_ICON_SIDE: i32 = 256;

/// 参数：`hwnd` 目标窗口；`which` `ICON_BIG` 或 `ICON_SMALL`
/// 核心流程：`SendMessageTimeoutW(WM_GETICON)` 把 wparam 设成图标类型、限时
/// [`ICON_TIMEOUT_MS`]，目标卡死时立刻返回 `None`；返回值非 0 且输出槽非 0 才算拿到 HICON。
#[cfg(windows)]
fn message_icon(
    hwnd: windows::Win32::Foundation::HWND,
    which: u32,
) -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HICON, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_GETICON,
    };

    let mut result: usize = 0;
    // SAFETY: SendMessageTimeoutW 只向目标窗口同步投递一条只读查询消息，超时即返回；
    // result 是本函数的栈变量，在调用期间一直有效。
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETICON,
            WPARAM(which as usize),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            ICON_TIMEOUT_MS,
            Some(&mut result),
        )
    };

    if sent.0 == 0 || result == 0 {
        return None;
    }
    Some(HICON(result as *mut core::ffi::c_void))
}

/// 参数：`hwnd` 目标窗口；`index` `GCLP_HICON`（大图标）或 `GCLP_HICONSM`（小图标）
/// 核心流程：`GetClassLongPtrW` 读窗口类里登记的图标句柄，0 表示这个类没设图标。
/// 该句柄属于窗口类，**不**由我们销毁。
#[cfg(windows)]
fn class_icon(
    hwnd: windows::Win32::Foundation::HWND,
    index: windows::Win32::UI::WindowsAndMessaging::GET_CLASS_LONG_INDEX,
) -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    use windows::Win32::UI::WindowsAndMessaging::{GetClassLongPtrW, HICON};

    // SAFETY: GetClassLongPtrW 只读窗口类的指定值（这里是图标句柄），不转移所有权。
    let value = unsafe { GetClassLongPtrW(hwnd, index) };
    if value == 0 {
        None
    } else {
        Some(HICON(value as *mut core::ffi::c_void))
    }
}

/// 参数：`hwnd` 目标窗口
/// 核心流程：复用 [`crate::window_enum::process_image_path`] 拿 exe 全路径 →
/// `ExtractIconExW` 取第 0 个大图标（没有再退回小图标）→ 都拿不到就用
/// `SHGetFileInfoW`（带 `SHGFI_USEFILEATTRIBUTES`，不读文件属性也能给图标）兜底 → 解码。
/// `ExtractIconExW` / `SHGetFileInfoW` 返回的句柄所有权归调用方，无论解码成败都要
/// `DestroyIcon`。连 exe 路径都拿不到时返回 `None`。
#[cfg(windows)]
fn exe_icon(hwnd: windows::Win32::Foundation::HWND) -> Option<(&'static str, IconImage)> {
    let path = match crate::window_enum::process_image_path(hwnd) {
        Some(path) => path,
        None => {
            log::debug!("icon: tier=exe_icon path=<none> reason=process_path_unavailable");
            return None;
        }
    };
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    if let Some(result) = extract_icon(&path, &wide) {
        return Some(result);
    }

    shell_icon(&path, &wide)
}

/// 参数：`path` exe 全路径（只用于日志）；`wide` 以 NUL 结尾的宽字符路径
/// 核心流程：`ExtractIconExW` 取第 0 个大图标与小图标 → 逐个解码 → 无论解码成败都用
/// `DestroyIcon` 释放本次创建的句柄（`ExtractIconExW` 返回的句柄所有权归调用方）。
#[cfg(windows)]
fn extract_icon(path: &str, wide: &[u16]) -> Option<(&'static str, IconImage)> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

    let mut large = HICON(std::ptr::null_mut());
    let mut small = HICON(std::ptr::null_mut());
    // SAFETY: ExtractIconExW 把新建的图标句柄写进栈上变量并返回图标个数；句柄所有权归
    // 调用方，下面用 DestroyIcon 释放。
    let count = unsafe {
        ExtractIconExW(
            PCWSTR(wide.as_ptr()),
            0,
            Some(&mut large),
            Some(&mut small),
            1,
        )
    };
    log::debug!(
        "icon: tier=exe_icon path={path} count={count} large={:#X} small={:#X}",
        large.0 as usize,
        small.0 as usize
    );

    for (tier, hicon) in [("exe_icon_large", large), ("exe_icon_small", small)] {
        if hicon.is_invalid() {
            continue;
        }
        let image = icon_image(hicon, tier);
        // SAFETY: 这个句柄是上面 ExtractIconExW 新建的，只有我们持有，用完必须销毁。
        unsafe {
            let _ = DestroyIcon(hicon);
        }
        if let Some(image) = image {
            return Some((tier, image));
        }
    }
    None
}

/// 参数：`path` exe 全路径（只用于日志）；`wide` 以 NUL 结尾的宽字符路径
/// 核心流程：最后兜底——`SHGetFileInfoW` 带 `SHGFI_ICON | SHGFI_LARGEICON |
/// SHGFI_USEFILEATTRIBUTES` 直接向 shell 要一个图标（不打开文件、不读属性，exe 属性读不到
/// 或权限受限时也能拿到图标）→ 解码 → 无论成败都 `DestroyIcon`（该句柄归调用方）。
#[cfg(windows)]
fn shell_icon(path: &str, wide: &[u16]) -> Option<(&'static str, IconImage)> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::UI::Shell::{
        SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
    };
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;

    let mut info = SHFILEINFOW::default();
    let size = std::mem::size_of::<SHFILEINFOW>() as u32;
    // SAFETY: SHGetFileInfoW 把图标句柄与文件信息写进栈上的 info（最多 size 字节），
    // wide 是本次调用期间有效的 NUL 结尾宽字符串；成功时 info.hIcon 归调用方所有。
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            size,
            SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES,
        )
    };
    log::debug!(
        "icon: tier=shell_icon path={path} ok={ok} hicon={:#X}",
        info.hIcon.0 as usize
    );

    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }

    let image = icon_image(info.hIcon, "shell_icon");
    // SAFETY: SHGetFileInfoW 返回的图标句柄只有我们持有，用完必须 DestroyIcon。
    unsafe {
        let _ = DestroyIcon(info.hIcon);
    }
    image.map(|image| ("shell_icon", image))
}

/// 参数：`hicon` 图标句柄（系统窗口图标，或本模块通过 `ExtractIconExW` / `SHGetFileInfoW`
/// 新建的图标）；`tier` 命中的降级级名（只用于日志）
/// 核心流程：`GetIconInfo` 复制出颜色位图与 AND 掩码 → [`decode_icon`] 解码 →
/// 无论成功与否都用 `DeleteObject` 释放这两块副本。这里**不**销毁 `hicon` 本身：
/// 系统窗口图标的所有权不在我们手里，销毁它会打坏别的窗口的图标。
#[cfg(windows)]
fn icon_image(
    hicon: windows::Win32::UI::WindowsAndMessaging::HICON,
    tier: &'static str,
) -> Option<IconImage> {
    use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

    let mut info = ICONINFO::default();
    // SAFETY: GetIconInfo 把图标信息写进栈上结构体；成功时 hbmColor/hbmMask 是新建的位图
    // 副本，按文档由调用方 DeleteObject。
    if let Err(err) = unsafe { GetIconInfo(hicon, &mut info) } {
        log::debug!("icon: decode fail tier={tier} reason=get_icon_info err={err:?}");
        return None;
    }

    let image = decode_icon(&info, tier);

    // SAFETY: 释放 GetIconInfo 复制出来的两块位图；空句柄会被 GDI 直接拒绝，无副作用。
    unsafe {
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ::from(info.hbmColor));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ::from(info.hbmMask));
        }
    }

    image
}

/// 参数：`info` `GetIconInfo` 填好的图标信息（颜色位图 + AND 掩码）；`tier` 日志用的级名
/// 核心流程：读颜色位图尺寸 → `GetDIBits` 读成 32 位 BGRA（按回写的 `biHeight` 符号决定
/// 是否翻转行序）→ 转 RGBA；自带 alpha 就直接用，没有 alpha 时先把整张图置为不透明，
/// 再用同尺寸的 1 位掩码把"露出背景"的像素挖成透明（掩码尺寸对不上就按不透明处理）。
/// 颜色位图为空（单色图标）或任何一步失败都返回 `None` 并记一条带 `reason=` 的日志，
/// 绝不 panic。
#[cfg(windows)]
fn decode_icon(
    info: &windows::Win32::UI::WindowsAndMessaging::ICONINFO,
    tier: &'static str,
) -> Option<IconImage> {
    // 单色图标没有颜色位图，只有 AND/XOR 两个半区的掩码；这条解码路径本模块不实现，
    // 直接放弃并记下原因（上层继续降级，最终画占位方块），不 panic。
    if info.hbmColor.is_invalid() {
        log::debug!("icon: decode fail tier={tier} reason=no_color_bitmap(monochrome)");
        return None;
    }

    let Some(color) = bitmap_dims(info.hbmColor) else {
        log::debug!("icon: decode fail tier={tier} reason=color_bitmap_size_unavailable");
        return None;
    };
    let (width, height) = (color.width, color.height);
    let mask_dims = bitmap_dims(info.hbmMask);
    log::debug!(
        "icon: decode start tier={tier} w={width} h={height} color_bpp={} mask_w={} mask_h={}",
        color.bits_per_pixel,
        mask_dims.map_or(0, |dims| dims.width),
        mask_dims.map_or(0, |dims| dims.height),
    );

    let Some(dib) = read_dib(info.hbmColor, width, height, 32) else {
        log::debug!("icon: decode fail tier={tier} reason=color_getdibits");
        return None;
    };
    let Some(mut rgba) =
        crate::win32_parse::bgra_to_rgba(&dib.bits, width as u32, height as u32, dib.bottom_up)
    else {
        log::debug!("icon: decode fail tier={tier} reason=bgra_len_mismatch");
        return None;
    };

    let has_alpha = crate::win32_parse::bgra_has_alpha(&dib.bits);
    log::debug!("icon: decode alpha tier={tier} has_alpha={has_alpha}");

    if !has_alpha {
        // 没有 alpha 通道：GDI 会把 alpha 位全填 0，直接用会得到一张全透明的图，
        // 所以先整体置为不透明，再由掩码决定哪些像素透明。
        crate::win32_parse::fill_alpha(&mut rgba, 255);
        match mask_dims {
            Some(dims) if dims.width == width && dims.height == height => {
                match read_dib(info.hbmMask, width, height, 1) {
                    Some(mask) => {
                        let applied = crate::win32_parse::apply_mask_alpha(
                            &mut rgba,
                            &mask.bits,
                            width as u32,
                            height as u32,
                            mask.bottom_up,
                        );
                        if !applied {
                            log::debug!("icon: decode mask tier={tier} result=length_mismatch");
                        }
                    }
                    None => log::debug!("icon: decode mask tier={tier} result=read_failed"),
                }
            }
            _ => log::debug!("icon: decode mask tier={tier} result=size_unavailable"),
        }
    }

    log::debug!("icon: decode result ok tier={tier} w={width} h={height}");
    Some(IconImage {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

/// 位图头的关键尺寸（宽、高、每像素位数）。
///
/// 参数：`width`/`height` 像素尺寸（高度已取绝对值），`bits_per_pixel` 原始位数
/// 核心流程：只承载解码需要的最小信息，日志里能看出"到底是几位图标"，不碰像素数据。
#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct BitmapDims {
    width: i32,
    height: i32,
    bits_per_pixel: u16,
}

/// 参数：`bitmap` 位图句柄（可能为空）
/// 核心流程：`GetObjectW` 读 `BITMAP` 头拿宽高与位数；句柄为空、读取失败、宽高为 0 或
/// 超过 [`MAX_ICON_SIDE`] 都返回 `None`（异常图标直接放弃，不做巨量分配）。
#[cfg(windows)]
fn bitmap_dims(bitmap: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<BitmapDims> {
    use windows::Win32::Graphics::Gdi::{GetObjectW, BITMAP, HGDIOBJ};

    if bitmap.is_invalid() {
        return None;
    }

    let mut info = BITMAP::default();
    let size = std::mem::size_of::<BITMAP>() as i32;
    // SAFETY: GetObjectW 只把位图头写进栈上的 BITMAP 结构体（最多 size 字节），不转移所有权。
    let written = unsafe {
        GetObjectW(
            HGDIOBJ::from(bitmap),
            size,
            Some(&mut info as *mut BITMAP as *mut core::ffi::c_void),
        )
    };
    if written == 0 {
        return None;
    }

    // 自下而上的 DIB 段 bmHeight 为负，尺寸判断只关心绝对值。
    let (width, height) = (info.bmWidth, info.bmHeight.abs());
    if !(1..=MAX_ICON_SIDE).contains(&width) || !(1..=MAX_ICON_SIDE).contains(&height) {
        log::debug!("icon: bitmap 尺寸异常，放弃：{width}x{height}");
        return None;
    }
    Some(BitmapDims {
        width,
        height,
        bits_per_pixel: info.bmBitsPixel,
    })
}

/// 一次 `GetDIBits` 的结果：像素缓冲 + 行序。
///
/// 参数：`bits` 从 `GetDIBits` 读出的原始字节；`bottom_up` 该缓冲是否自下而上
/// （由回写的 `biHeight` 符号决定，见 [`crate::win32_parse::dib_is_bottom_up`]）
/// 核心流程：把"读出来的字节"和"要不要翻行"绑在一起返回，调用方不用再自己判断符号。
#[cfg(windows)]
struct DibPixels {
    bits: Vec<u8>,
    bottom_up: bool,
}

/// 参数：`bitmap` 位图句柄；`width`/`height` 像素尺寸；`bit_count` 期望的每像素位数
/// （颜色位图要 32，掩码要 1）
/// 核心流程：按 [`crate::win32_parse::dib_row_stride`] 算出缓冲大小 →
/// `CreateCompatibleDC` 建内存 DC → `GetDIBits` 把像素读进缓冲（请求 `biHeight` 为正，
/// 即自下而上）→ 立刻读 `GetLastError`（之后的 `DeleteDC` 会覆盖它）→ `DeleteDC` →
/// 用回写的 `biHeight` 符号判断实际行序（正=自下而上，负=自上而下）。
/// 只有 `GetDIBits` 返回 0 才放弃；行数与回写高度对不上只记日志，按请求的行序继续解。
#[cfg(windows)]
fn read_dib(
    bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    width: i32,
    height: i32,
    bit_count: u16,
) -> Option<DibPixels> {
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC, GetDIBits, DIB_RGB_COLORS};

    let stride = crate::win32_parse::dib_row_stride(width as u32, bit_count)?;
    let size = stride.checked_mul(height as usize)?;
    let mut bits = vec![0u8; size];

    // SAFETY: CreateCompatibleDC(None) 只新建一个兼容内存 DC，失败时返回空句柄。
    let dc = unsafe { CreateCompatibleDC(None) };
    if dc.is_invalid() {
        log::debug!("icon: read_dib 失败 reason=no_dc");
        return None;
    }

    let mut info = BitmapInfoBuffer::new(width, height, bit_count);
    // SAFETY: bits 长度为 stride * height，GetDIBits 最多写这么多字节；info 按 BITMAPINFO
    // 的布局（#[repr(C)]）预留了调色板空间；dc 是本函数刚建的内存 DC。
    let copied = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(bits.as_mut_ptr() as *mut core::ffi::c_void),
            info.as_mut_ptr(),
            DIB_RGB_COLORS,
        )
    };
    // SAFETY: GetLastError 只读当前线程的错误码，无参数、无副作用；必须在其它 GDI 调用
    // 之前取，否则会被覆盖。
    let error = unsafe { GetLastError() };

    // SAFETY: 释放本函数新建的 DC；位图句柄不归它所有，不在这里销毁。
    unsafe {
        let _ = DeleteDC(dc);
    }

    if copied == 0 {
        log::debug!(
            "icon: read_dib 失败 reason=getdibits_zero err={:#X}",
            error.0
        );
        return None;
    }
    // 行数对不上只记日志不放弃：缓冲是零初始化的，缺的行按全 0（透明）算，宁可读出
    // 一张稍微残的图标，也不要因为驱动回了个怪数字就把整张图丢掉。
    if copied != height {
        log::debug!("icon: read_dib 行数异常 copied={copied} expect={height}");
    }

    // 回写的 biHeight：正=自下而上（要翻行），负=自上而下（原样用）。回写的值不可用
    // （0 或与请求宽度不符）时退回我们请求的 +height，也就是自下而上。
    let returned_height = info.returned_height();
    let bottom_up = if returned_height == 0 || returned_height.unsigned_abs() != height as u32 {
        log::debug!(
            "icon: read_dib 高度回写异常 biHeight={returned_height} expect={height}，按自下而上处理"
        );
        true
    } else {
        crate::win32_parse::dib_is_bottom_up(returned_height)
    };

    Some(DibPixels { bits, bottom_up })
}

/// `GetDIBits` 需要的 BITMAPINFO 缓冲：头部 + 256 项调色板。
///
/// 参数：`header` 位图信息头（`GetDIBits` 会回写其中的 `biHeight`/`biSizeImage`），
/// `_colors` 调色板（1 位掩码转换时 GDI 要写 2 项）
/// 核心流程：标准 `BITMAPINFO` 只声明了 1 项调色板，而 1 位位图要 2 项，直接传
/// `BITMAPINFO` 会被 GDI 写到结构体之外；这里按同样的头布局多留空间，再转成
/// `*mut BITMAPINFO` 交给 `GetDIBits`。`#[repr(C)]` 是硬要求：字段顺序一旦被 Rust 重排，
/// GDI 读到的就是垃圾头，`GetDIBits` 会稳定返回 0。
#[cfg(windows)]
#[repr(C)]
struct BitmapInfoBuffer {
    header: windows::Win32::Graphics::Gdi::BITMAPINFOHEADER,
    _colors: [windows::Win32::Graphics::Gdi::RGBQUAD; 256],
}

#[cfg(windows)]
impl BitmapInfoBuffer {
    /// 参数：`width`/`height` 像素尺寸；`bit_count` 每像素位数
    /// 核心流程：填 `BITMAPINFOHEADER` 的必填字段（结构体大小、尺寸、1 平面、位数、
    /// `BI_RGB` 不压缩、图像字节数），高度请求为正（希望拿到自下而上的像素，实际行序
    /// 以 `GetDIBits` 回写的符号为准）；调色板留空由 GDI 按原位图填写。
    fn new(width: i32, height: i32, bit_count: u16) -> Self {
        use windows::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB, RGBQUAD};

        let stride = crate::win32_parse::dib_row_stride(width as u32, bit_count).unwrap_or(0);
        Self {
            header: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: height,
                biPlanes: 1,
                biBitCount: bit_count,
                biCompression: BI_RGB.0,
                biSizeImage: (stride * height as usize) as u32,
                ..Default::default()
            },
            _colors: [RGBQUAD::default(); 256],
        }
    }

    /// 参数：无
    /// 核心流程：把本结构体当成 `*mut BITMAPINFO` 交出去——`#[repr(C)]` 保证头部布局与
    /// `BITMAPINFO` 完全一致，多出来的调色板空间只是安全余量。
    fn as_mut_ptr(&mut self) -> *mut windows::Win32::Graphics::Gdi::BITMAPINFO {
        use windows::Win32::Graphics::Gdi::BITMAPINFO;

        self as *mut Self as *mut BITMAPINFO
    }

    /// 参数：无
    /// 核心流程：读 `GetDIBits` 回写后的 `biHeight`——正数表示像素自下而上，负数表示
    /// 自上而下（源是 top-down DIB 段时会出现）。
    fn returned_height(&self) -> i32 {
        self.header.biHeight
    }
}
