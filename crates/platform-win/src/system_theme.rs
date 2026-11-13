//! 系统主题（深浅色）查询。
//!
//! 参数：无
//! 核心流程：读当前用户个性化设置里的 `AppsUseLightTheme` 这一个 DWORD——它是 Windows
//! 「应用模式」开关的真身（设置 → 个性化 → 颜色）。值 1 表示应用用浅色、0 表示用深色。
//! 只读 HKCU，不需要管理员权限。判断不了一律按「浅色」处理：Windows 默认就是浅色，
//! 上层拿到的 false 与真实默认状态一致，不会把用户界面误翻成深色。

/// 个性化设置键的路径（相对 `HKEY_CURRENT_USER`）。
#[cfg(windows)]
const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// 键下表示「应用是否使用浅色主题」的值名。
#[cfg(windows)]
const VALUE_NAME: &str = "AppsUseLightTheme";

/// 参数：`apps_use_light_theme` 注册表 `AppsUseLightTheme` 的原始值，读不到时为 `None`
/// 核心流程：唯一的判定规则——`0` 是深色，返回 `true`；`1` 是浅色，返回 `false`。
/// `None`（键/值读不到）以及 0/1 之外的脏数据都属于「判断不了」，按浅色降级，
/// 避免把浅色系统误判成深色。纯逻辑，可跨平台单测。
pub fn is_dark_from_light_theme(apps_use_light_theme: Option<u32>) -> bool {
    matches!(apps_use_light_theme, Some(0))
}

/// 参数：`text` 任意 Rust 字符串
/// 核心流程：转成 Win32 需要的「以 NUL 结尾的 UTF-16」缓冲区。
#[cfg(windows)]
fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 参数：无
/// 核心流程：用 `RegGetValueW`（`RRF_RT_REG_DWORD`）一步读出 HKCU 个性化键下的
/// `AppsUseLightTheme`。返回 `None` 覆盖所有失败情形：键打不开、值不存在、值类型不是
/// DWORD、以及读到的字节数不是 4（脏数据）。不打开句柄，因此没有句柄泄漏风险。
#[cfg(windows)]
fn read_apps_use_light_theme() -> Option<u32> {
    use std::ffi::c_void;
    use std::mem::size_of;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let key = to_wide(PERSONALIZE_KEY);
    let name = to_wide(VALUE_NAME);
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;

    // SAFETY: key/name 都是以 NUL 结尾的宽字符缓冲，活的足够久；value 是 4 字节的 u32，
    // size 初始为它的字节数，正好是 RegGetValueW 对 DWORD 期望的输入/输出；pdwtype 传
    // None 表示不关心类型（RRF_RT_REG_DWORD 已经限定了类型）。该调用不需要关闭句柄。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };

    if status != ERROR_SUCCESS || size != size_of::<u32>() as u32 {
        log::debug!(
            "系统主题：读不到 {} 值，错误码 {}，字节数 {}",
            VALUE_NAME,
            status.0,
            size
        );
        return None;
    }

    Some(value)
}

/// 参数：无
/// 核心流程：读 `AppsUseLightTheme` 后交给 [`is_dark_from_light_theme`] 判定；
/// 非 Windows 平台没有这个概念，直接返回 `false`（浅色）。
#[cfg(windows)]
pub fn is_system_dark() -> bool {
    is_dark_from_light_theme(read_apps_use_light_theme())
}

/// 参数：无
/// 核心流程：非 Windows 平台上没有系统主题可读，恒返回 `false`（当作浅色），
/// 保证上层调用方无需按平台分支。
#[cfg(not(windows))]
pub fn is_system_dark() -> bool {
    is_dark_from_light_theme(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0 是深色、1 是浅色，这是契约里唯一确定的正向映射。
    #[test]
    fn maps_light_theme_flag_to_dark() {
        assert!(is_dark_from_light_theme(Some(0)));
        assert!(!is_dark_from_light_theme(Some(1)));
    }

    /// 读不到（None）或脏数据都按浅色处理，绝不误判成深色。
    #[test]
    fn missing_or_dirty_value_degrades_to_light() {
        assert!(!is_dark_from_light_theme(None));
        assert!(!is_dark_from_light_theme(Some(2)));
        assert!(!is_dark_from_light_theme(Some(u32::MAX)));
    }
}
