//! Win32 原始值到可移植值的纯逻辑。
//!
//! 参数：只接收字符串、整数等普通值，不接收任何窗口/进程句柄
//! 核心流程：把 `QueryFullProcessImageNameW` 的完整路径削成进程名，把 `GetPropW`
//! 读到的属性值折成 cloak 类型，把 `GetWindowTextW` 的裸标题折成「是否有可读标题」。
//! 因为完全不碰 Win32，这些函数在 Linux/macOS 上也能直接单测。

/// 进程名取不到时的统一降级值。
pub const UNKNOWN_PROCESS_NAME: &str = "unknown";

/// 参数：`title` `GetWindowTextW` 取回的原始标题
/// 核心流程：去掉首尾空白再判空。真机上有窗口（如 msedge 的壳）只拿到空白，
/// 按原始串判空会把这种“用户读不出名字”的窗口放进切换列表，因此 `has_title`
/// 一律按 trim 之后的结果取值（纯符号标题不在此处过滤，不自作主张加规则）。
pub fn has_meaningful_title(title: &str) -> bool {
    !title.trim().is_empty()
}

/// 参数：`path` 完整可执行文件路径，例如 `C:\Windows\explorer.exe`
/// 核心流程：取最后一个 `\` 或 `/` 之后的部分作为文件名 → 去掉结尾的 `.exe`
/// （忽略大小写）→ 文件名为空或本身就是空串时返回 [`UNKNOWN_PROCESS_NAME`]。
/// 其余扩展名（如 `.dll`）原样保留，大小写不做归一。
pub fn process_name_from_exe_path(path: &str) -> String {
    let file_name = path.rsplit(['\\', '/']).next().unwrap_or_default();
    let stem = if file_name.len() >= 4
        && file_name.as_bytes()[file_name.len() - 4..].eq_ignore_ascii_case(b".exe")
    {
        // 结尾 4 字节确认是 ASCII 的 ".exe"，因此按字节切分一定落在字符边界上。
        &file_name[..file_name.len() - 4]
    } else {
        file_name
    };

    if stem.is_empty() {
        UNKNOWN_PROCESS_NAME.to_string()
    } else {
        stem.to_string()
    }
}

/// 参数：`raw` `GetPropW` 读到的属性原始整数，属性不存在时为 `None`
/// 核心流程：缺失按 0（未 cloak）处理，超出 `u32` 的脏数据也按 0 处理，
/// 保证 cloak 类型永远是可用数值而不是错误。
pub fn cloak_type_from_prop(raw: Option<usize>) -> u32 {
    raw.and_then(|value| u32::try_from(value).ok()).unwrap_or(0)
}

/// 参数：`width` 位图宽度（像素）；`bit_count` 每像素位数（图标只用到 1 与 32）
/// 核心流程：DIB 的每一行都按 4 字节对齐，行字节数 = `((width * bit_count + 31) / 32) * 4`；
/// 宽度为 0 或位数不在 1..=32 时返回 `None`，让调用方直接放弃，而不是算出一个坏 stride。
pub fn dib_row_stride(width: u32, bit_count: u16) -> Option<usize> {
    if width == 0 || !(1..=32).contains(&bit_count) {
        return None;
    }
    let bits = (width as usize).checked_mul(bit_count as usize)?;
    bits.div_ceil(32).checked_mul(4)
}

/// 参数：`bi_height` 位图信息头里的 `biHeight`（可为负）
/// 核心流程：`GetDIBits` 回写的 `biHeight` 为正表示像素自下而上（行序要翻转），为负表示
/// 自上而下（原样使用）。0 不是合法高度，调用方会先把它当脏数据挡掉，这里按
/// `GetDIBits` 的默认输出（自下而上）处理，避免多一条无用的错误分支。
pub fn dib_is_bottom_up(bi_height: i32) -> bool {
    bi_height >= 0
}

/// 参数：`width`/`height` 像素尺寸
/// 核心流程：用 checked 乘法算出 RGBA8 位图应有的字节数 `width * height * 4`，溢出
/// （脏句柄给出的巨大尺寸）时返回 `None`，绝不按溢出后的长度分配内存。
pub fn rgba_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
}

/// 参数：`width`/`height` 像素尺寸；`len` 实际缓冲长度
/// 核心流程：[`rgba_len`] 算出的期望长度与 `len` 完全相等才算合法——egui 的
/// `ColorImage::from_rgba_unmultiplied` 遇到长度不符会 panic，脏数据必须在这里挡掉。
pub fn rgba_len_matches(width: u32, height: u32, len: usize) -> bool {
    rgba_len(width, height) == Some(len)
}

/// 参数：`bgra` 32 位 BGRA 像素缓冲；`width`/`height` 尺寸；`bottom_up` 该 DIB 是否自下而上
/// （`GetDIBits` 的 `biHeight` 为正时给出的就是自下而上）
/// 核心流程：先按 [`rgba_len_matches`] 校验长度（不符直接 `None`，绝不越界读）→
/// 逐像素把 B 与 R 对调成 RGBA，`bottom_up` 时按行倒序拷贝成自上而下，alpha 原样保留。
pub fn bgra_to_rgba(bgra: &[u8], width: u32, height: u32, bottom_up: bool) -> Option<Vec<u8>> {
    if !rgba_len_matches(width, height, bgra.len()) {
        return None;
    }

    let (w, h) = (width as usize, height as usize);
    let mut rgba = vec![0u8; bgra.len()];
    for row in 0..h {
        // 目标行始终自上而下：源自下而上时，第 row 行对应源的第 h-1-row 行。
        let src_row = if bottom_up { h - 1 - row } else { row };
        let src = &bgra[src_row * w * 4..(src_row + 1) * w * 4];
        let dst = &mut rgba[row * w * 4..(row + 1) * w * 4];
        for (from, to) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
            to[0] = from[2];
            to[1] = from[1];
            to[2] = from[0];
            to[3] = from[3];
        }
    }
    Some(rgba)
}

/// 参数：`bgra` 32 位 BGRA 像素缓冲
/// 核心流程：只要有一个像素的 alpha 字节非 0 就认为这张位图自带 alpha 通道；全是 0 说明
/// 这是没有 alpha 通道的老式图标（GDI 把 alpha 位填 0），需要退回 AND 掩码合成。
pub fn bgra_has_alpha(bgra: &[u8]) -> bool {
    bgra.chunks_exact(4).any(|pixel| pixel[3] != 0)
}

/// 参数：`rgba` 已转成 RGBA 的缓冲（就地改第 4 字节）；`alpha` 统一写入的值
/// 核心流程：给每个像素的 alpha 写同一个值，用于「没有 alpha 通道」的图标先把整张图置为
/// 不透明，再由掩码挖洞；长度不足 4 的尾巴忽略。
pub fn fill_alpha(rgba: &mut [u8], alpha: u8) {
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[3] = alpha;
    }
}

/// 参数：`rgba` 目标 RGBA 缓冲（自上而下）；`mask` 1 位 AND 掩码缓冲；`width`/`height` 尺寸；
/// `mask_bottom_up` 掩码是否自下而上
/// 核心流程：先校验两边长度（掩码行宽取 [`dib_row_stride`]），不匹配就返回 `false` 且
/// 不动 `rgba` → 按位读掩码：位为 1 表示"露出背景"（alpha 0），位为 0 表示该像素可见
/// （alpha 255）。颜色本身不动，透明度完全由掩码决定。
pub fn apply_mask_alpha(
    rgba: &mut [u8],
    mask: &[u8],
    width: u32,
    height: u32,
    mask_bottom_up: bool,
) -> bool {
    if !rgba_len_matches(width, height, rgba.len()) {
        return false;
    }
    let Some(stride) = dib_row_stride(width, 1) else {
        return false;
    };
    let needed = stride.saturating_mul(height as usize);
    if mask.len() < needed {
        return false;
    }

    let (w, h) = (width as usize, height as usize);
    for row in 0..h {
        let mask_row = if mask_bottom_up { h - 1 - row } else { row };
        let bits = &mask[mask_row * stride..(mask_row + 1) * stride];
        let dst = &mut rgba[row * w * 4..(row + 1) * w * 4];
        for (column, pixel) in dst.chunks_exact_mut(4).enumerate() {
            // 掩码位序是高位在前：第 column 个像素对应所在字节的第 (7 - column % 8) 位。
            let transparent = bits[column / 8] & (0x80 >> (column % 8)) != 0;
            pixel[3] = if transparent { 0 } else { 255 };
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 带目录、带 `.exe` 的常见路径应只剩文件主干名。
    #[test]
    fn strips_directory_and_exe_suffix() {
        assert_eq!(
            process_name_from_exe_path(r"C:\Windows\System32\notepad.exe"),
            "notepad"
        );
        assert_eq!(
            process_name_from_exe_path(r"C:\Program Files\Foo Bar\powerpnt.EXE"),
            "powerpnt"
        );
        assert_eq!(
            process_name_from_exe_path(r"\\?\C:\Windows\EXPLORER.EXE"),
            "EXPLORER"
        );
        // 顺带覆盖正斜杠分隔符，便于在开发机上复用同一份逻辑。
        assert_eq!(process_name_from_exe_path("/usr/bin/code.exe"), "code");
    }

    /// 没有扩展名的路径不应被截断。
    #[test]
    fn keeps_names_without_exe_suffix() {
        assert_eq!(process_name_from_exe_path(r"C:\Tools\app"), "app");
        assert_eq!(
            process_name_from_exe_path(r"C:\Tools\shell32.dll"),
            "shell32.dll"
        );
        assert_eq!(process_name_from_exe_path("single"), "single");
    }

    /// 空串、纯目录、只有 `.exe` 的异常输入都降级为 unknown，不能 panic。
    #[test]
    fn empty_input_degrades_to_unknown() {
        assert_eq!(process_name_from_exe_path(""), UNKNOWN_PROCESS_NAME);
        assert_eq!(
            process_name_from_exe_path(r"C:\Windows\"),
            UNKNOWN_PROCESS_NAME
        );
        assert_eq!(
            process_name_from_exe_path(r"C:\Windows\.exe"),
            UNKNOWN_PROCESS_NAME
        );
    }

    /// cloak 属性缺失按 0 处理，正常取值原样透出。
    #[test]
    fn cloak_type_defaults_to_zero() {
        assert_eq!(cloak_type_from_prop(None), 0);
        assert_eq!(cloak_type_from_prop(Some(0)), 0);
        assert_eq!(cloak_type_from_prop(Some(1)), 1);
        assert_eq!(cloak_type_from_prop(Some(2)), 2);
    }

    /// 带实际文字的标题算有标题；首尾空白被忽略。
    #[test]
    fn meaningful_title_accepts_real_text() {
        assert!(has_meaningful_title("记事本"));
        assert!(has_meaningful_title("  记事本 - 无标题  "));
        assert!(has_meaningful_title("x"));
    }

    /// 空串与纯空白（含制表符、换行、不换行空格）都不算有标题，即 US-036 的脏数据。
    /// 这里只按 trim 判空，纯符号标题仍然算有标题，不额外加过滤规则。
    #[test]
    fn meaningful_title_rejects_blank() {
        assert!(!has_meaningful_title(""));
        assert!(!has_meaningful_title("   "));
        assert!(!has_meaningful_title("\t\r\n"));
        assert!(!has_meaningful_title("\u{00A0}"));
    }

    /// DIB 行宽：32 位位图每行天然 4 字节对齐；1 位掩码按 32 位一组向上取整。
    #[test]
    fn dib_row_stride_aligns_to_four_bytes() {
        assert_eq!(dib_row_stride(32, 32), Some(128));
        assert_eq!(dib_row_stride(16, 32), Some(64));
        // 1 位掩码：16 像素 = 2 字节 → 补齐到 4 字节。
        assert_eq!(dib_row_stride(16, 1), Some(4));
        assert_eq!(dib_row_stride(32, 1), Some(4));
        // 33 像素 = 33 位 → 两组 32 位 → 8 字节。
        assert_eq!(dib_row_stride(33, 1), Some(8));
        // 脏输入直接放弃。
        assert_eq!(dib_row_stride(0, 32), None);
        assert_eq!(dib_row_stride(8, 0), None);
    }

    /// biHeight 符号决定行序：正=自下而上（要翻转），负=自上而下（原样用）。
    #[test]
    fn dib_is_bottom_up_follows_bi_height_sign() {
        assert!(dib_is_bottom_up(32), "正数应视为自下而上，需要翻行");
        assert!(!dib_is_bottom_up(-32), "负数应视为自上而下，不翻行");
        // 0 不合法，调用方会先挡掉；这里按 GetDIBits 的默认输出处理。
        assert!(dib_is_bottom_up(0));
    }

    /// RGBA 缓冲长度校验：正好 w*h*4 才合法，且巨大尺寸不会溢出成小数。
    #[test]
    fn rgba_len_validates_exact_buffer_size() {
        assert_eq!(rgba_len(2, 2), Some(16));
        assert!(rgba_len_matches(2, 2, 16));
        assert!(!rgba_len_matches(2, 2, 15));
        assert!(!rgba_len_matches(2, 2, 0));
        // 溢出保护：脏句柄给出 u32::MAX 尺寸时返回 None 而不是绕回小数字。
        assert_eq!(rgba_len(u32::MAX, u32::MAX), None);
        assert!(!rgba_len_matches(u32::MAX, u32::MAX, 0));
    }

    /// BGRA→RGBA：B/R 交换、alpha 保留，自下而上的源被翻成自上而下。
    #[test]
    fn bgra_to_rgba_swaps_channels_and_flips_rows() {
        // 2×1 自下而上：源第 0 行是最下面那行，转换后应落到第 1 行。
        let bgra = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let rgba = bgra_to_rgba(&bgra, 2, 1, true).expect("长度合法");
        assert_eq!(rgba, vec![3, 2, 1, 4, 7, 6, 5, 8]);

        // 2 行自下而上：行序整体倒过来，行内像素顺序不变。
        let bgra = [
            10u8, 0, 0, 255, 20, 0, 0, 255, // 源第 0 行（最下面）
            30, 0, 0, 255, 40, 0, 0, 255, // 源第 1 行（最上面）
        ];
        let rgba = bgra_to_rgba(&bgra, 2, 2, true).expect("长度合法");
        assert_eq!(rgba[0], 0);
        assert_eq!(rgba[2], 30);
        assert_eq!(rgba[8], 0);
        assert_eq!(rgba[10], 10);

        // 自上而下时不翻转。
        let bgra = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let rgba = bgra_to_rgba(&bgra, 2, 1, false).expect("长度合法");
        assert_eq!(rgba, vec![3, 2, 1, 4, 7, 6, 5, 8]);

        // 长度不符：返回 None，绝不越界读。
        assert_eq!(bgra_to_rgba(&[1, 2, 3], 2, 2, true), None);
    }

    /// alpha 通道探测：任一像素 alpha 非 0 就算自带 alpha；全 0 说明要走掩码。
    #[test]
    fn bgra_has_alpha_detects_real_alpha_channel() {
        assert!(bgra_has_alpha(&[0, 0, 0, 255]));
        assert!(bgra_has_alpha(&[0, 0, 0, 0, 0, 0, 0, 1]));
        assert!(!bgra_has_alpha(&[1, 2, 3, 0, 4, 5, 6, 0]));
        assert!(!bgra_has_alpha(&[]));
    }

    /// 统一 alpha：每个像素第 4 字节被覆盖，颜色不动。
    #[test]
    fn fill_alpha_overwrites_only_alpha_bytes() {
        let mut rgba = vec![1u8, 2, 3, 0, 4, 5, 6, 0];
        fill_alpha(&mut rgba, 255);
        assert_eq!(rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }

    /// 掩码合成：位为 1 的像素置全透明，位为 0 的不透明；行序按 `mask_bottom_up` 对齐。
    #[test]
    fn apply_mask_alpha_maps_bits_to_transparency() {
        // 8×1：掩码字节 0b1010_0000 → 第 0、2 个像素透明。1 位掩码每行按 4 字节对齐。
        let mut rgba = vec![255u8; 8 * 4];
        assert!(apply_mask_alpha(
            &mut rgba,
            &[0b1010_0000, 0, 0, 0],
            8,
            1,
            false
        ));
        let alpha: Vec<u8> = rgba.chunks_exact(4).map(|p| p[3]).collect();
        assert_eq!(alpha, vec![0, 255, 0, 255, 255, 255, 255, 255]);

        // 2×2 自下而上：掩码第 0 行（最下面）对应 rgba 的第 1 行。
        // 每行 4 字节：0b1000_0000（第 0 个像素透明），第二行全 0（不透明）。
        let mut rgba = vec![255u8; 2 * 2 * 4];
        let mask = [0b1000_0000u8, 0, 0, 0, 0, 0, 0, 0];
        assert!(apply_mask_alpha(&mut rgba, &mask, 2, 2, true));
        assert_eq!(rgba[3], 255, "第 0 行（上面）应保持不透明");
        assert_eq!(rgba[2 * 4 + 3], 0, "第 1 行（下面）的第 0 个像素应透明");
    }

    /// 掩码长度不够时拒绝合成，且不改动已有 alpha（失败也要保持原样）。
    #[test]
    fn apply_mask_alpha_rejects_short_mask() {
        let mut rgba = vec![7u8; 8 * 4];
        // 8 像素的 1 位掩码需要 4 字节（按行 4 字节对齐），这里只给 1 字节。
        assert!(!apply_mask_alpha(&mut rgba, &[0xFF], 8, 1, false));
        assert!(rgba.chunks_exact(4).all(|pixel| pixel[3] == 7));
        // rgba 本身长度不符也拒绝。
        assert!(!apply_mask_alpha(&mut [0u8; 3], &[0, 0, 0, 0], 8, 1, false));
    }
}
