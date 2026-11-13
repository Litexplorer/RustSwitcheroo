//! 开机自启（US-048）。
//!
//! 参数：无
//! 核心流程：把「开机自启」这件事完全落在当前用户的注册表 Run 键上——
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下有一个名为 `Switcheroo` 的
//! `REG_SZ` 值，值就是带双引号的 exe 完整路径。`is_enabled` 查这个值在不在，`enable`
//! 写它，`disable` 删它（本来就没有也算成功）。用 HKCU 而不是 HKLM：不需要管理员权限，
//! 也只影响当前用户，符合托盘小工具的行为。非 Windows 平台一律返回「不支持」。

// 引号规则、REG_SZ 字节编码这些纯逻辑在非 Windows 上没有调用方，但单测要在 Linux 上跑，
// 所以整体允许未使用告警；Win32 代码本身仍由 cfg(windows) 隔离。
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::Path;

/// Run 键的路径（相对 `HKEY_CURRENT_USER`）。
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Run 键下的值名：任务管理器「启动」页显示的就是它。
const VALUE_NAME: &str = "Switcheroo";

/// 参数：`exe` 可执行文件路径
/// 核心流程：给路径套上一对双引号。Run 键的值是一条命令行，路径含空格
/// （`C:\Program Files\…`）时没有引号会被拆成「程序 + 参数」，开机时直接报「找不到文件」。
/// 纯逻辑，可跨平台单测。
pub(crate) fn quote_exe_path(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// 参数：`value` 要写进注册表的字符串
/// 核心流程：编成 `REG_SZ` 需要的字节序列——UTF-16LE，并且末尾带一个 NUL（`RegSetValueExW`
/// 要的字节数含这个结尾 NUL，少了会被读成乱码/截断）。纯逻辑，可跨平台单测。
pub(crate) fn reg_sz_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((value.encode_utf16().count() + 1) * 2);
    for unit in value.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// 参数：`text` 任意 Rust 字符串
/// 核心流程：转成 Win32 需要的「以 NUL 结尾的 UTF-16」缓冲区。
#[cfg(windows)]
fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
/// 参数：无
/// 核心流程：以 `KEY_READ` 打开 HKCU 下的 Run 键 → 用 `RegQueryValueExW` 只查大小（`lpData`
/// 传空）判断 `Switcheroo` 这个值是否存在 → 关键。值不存在、键打不开或任何错误都按
/// 「未启用」处理：托盘勾选状态宁可少勾一次，也不要谎报已启用。
pub fn is_enabled() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, KEY_READ, REG_VALUE_TYPE,
    };

    let key_path = to_wide(RUN_KEY);
    let name = to_wide(VALUE_NAME);
    let mut key = HKEY::default();

    // SAFETY: RegOpenKeyExW 打开 HKCU 下一个子键，只读权限；key_path 是以 NUL 结尾的宽字符
    // 缓冲，key 是栈上变量用来接收句柄。成功时必须 RegCloseKey。
    let status = unsafe {
        RegOpenKeyExW(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            None,
            KEY_READ,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        log::debug!("开机自启：打不开 Run 键，错误码 {}", status.0);
        return false;
    }

    let mut kind = REG_VALUE_TYPE::default();
    let mut size = 0u32;
    // SAFETY: lpData 传空、lpcbData 非空时 RegQueryValueExW 只回填数据大小并返回
    // ERROR_SUCCESS——正好用来判断「值存在与否」，不读也不复制值内容。
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };

    // SAFETY: key 是上面 RegOpenKeyExW 成功返回的句柄，这里关闭一次，之后不再使用。
    unsafe {
        let _ = RegCloseKey(key);
    }

    if status != ERROR_SUCCESS {
        log::debug!("开机自启：未找到 {} 值，错误码 {}", VALUE_NAME, status.0);
    }
    status == ERROR_SUCCESS
}

#[cfg(windows)]
/// 参数：无
/// 核心流程：`current_exe` 取本进程 exe 全路径 → 加双引号 → 以 `KEY_SET_VALUE` 打开 Run 键
/// → `RegSetValueExW(REG_SZ)` 写入 UTF-16LE（含结尾 NUL）→ 关键。取路径失败、开键失败、
/// 写值失败都把原因作为 `Err` 返回，上层可以拿去提示用户。
pub fn enable() -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegSetValueExW, HKEY, KEY_SET_VALUE, REG_SZ,
    };

    let exe = std::env::current_exe().map_err(|err| format!("取当前可执行文件路径失败：{err}"))?;
    let command = quote_exe_path(&exe);
    let data = reg_sz_bytes(&command);

    let key_path = to_wide(RUN_KEY);
    let name = to_wide(VALUE_NAME);
    let mut key = HKEY::default();

    // SAFETY: 以 KEY_SET_VALUE 打开 HKCU 下的 Run 键（该键由系统预建，正常一定存在）；
    // key_path 是以 NUL 结尾的宽字符缓冲，key 接收句柄。
    let status = unsafe {
        RegOpenKeyExW(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            None,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        let reason = format!("打开 Run 键失败，错误码 {}", status.0);
        log::warn!("开机自启：{reason}");
        return Err(reason);
    }

    // SAFETY: 往已打开的键写一个 REG_SZ 值；data 是本次调用期间有效的 UTF-16LE 字节（含结尾
    // NUL），name 是以 NUL 结尾的宽字符值名。REG_SZ 是固定字符串，不需要 REG_OPTION_* 参数。
    let status = unsafe {
        RegSetValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            REG_SZ,
            Some(data.as_slice()),
        )
    };

    // SAFETY: key 是上面成功打开的句柄，无论写入成败都在这里关闭一次。
    unsafe {
        let _ = RegCloseKey(key);
    }

    if status != ERROR_SUCCESS {
        let reason = format!("写入 Run 键失败，错误码 {}", status.0);
        log::warn!("开机自启：{reason}");
        return Err(reason);
    }

    log::info!("开机自启已写入：{command}");
    Ok(())
}

#[cfg(windows)]
/// 参数：无
/// 核心流程：以 `KEY_SET_VALUE` 打开 Run 键 → `RegDeleteValueW` 删掉 `Switcheroo` 值 →
/// 关键。值本来就不存在（`ERROR_FILE_NOT_FOUND`）也算成功——「关掉自启」的目标状态已经达成，
/// 不该因为重复点击而报错。
pub fn disable() -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, HKEY, KEY_SET_VALUE,
    };

    let key_path = to_wide(RUN_KEY);
    let name = to_wide(VALUE_NAME);
    let mut key = HKEY::default();

    // SAFETY: 以 KEY_SET_VALUE 打开 HKCU 下的 Run 键；key_path 是以 NUL 结尾的宽字符缓冲。
    let status = unsafe {
        RegOpenKeyExW(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            None,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        let reason = format!("打开 Run 键失败，错误码 {}", status.0);
        log::warn!("开机自启：{reason}");
        return Err(reason);
    }

    // SAFETY: 删除已打开键下的一个值；name 是以 NUL 结尾的宽字符值名，不存在的值会返回
    // ERROR_FILE_NOT_FOUND，这不是错误。
    let status = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };

    // SAFETY: key 是上面成功打开的句柄，在这里关闭一次。
    unsafe {
        let _ = RegCloseKey(key);
    }

    if status == ERROR_SUCCESS {
        log::info!("开机自启已删除");
        Ok(())
    } else if status == ERROR_FILE_NOT_FOUND {
        log::debug!("开机自启：值本来就不存在，视为已关闭");
        Ok(())
    } else {
        let reason = format!("删除 Run 键值失败，错误码 {}", status.0);
        log::warn!("开机自启：{reason}");
        Err(reason)
    }
}

#[cfg(not(windows))]
/// 参数：无
/// 核心流程：非 Windows 平台没有注册表 Run 键，恒返回 `false`。
pub fn is_enabled() -> bool {
    false
}

#[cfg(not(windows))]
/// 参数：无
/// 核心流程：非 Windows 平台不支持开机自启，返回带平台名的错误描述，由上层决定是否提示。
pub fn enable() -> Result<(), String> {
    Err(format!("{} 平台暂不支持开机自启", std::env::consts::OS))
}

#[cfg(not(windows))]
/// 参数：无
/// 核心流程：同 [`enable`]，非 Windows 平台直接返回「不支持」。
pub fn disable() -> Result<(), String> {
    Err(format!("{} 平台暂不支持开机自启", std::env::consts::OS))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路径含空格时必须整体加引号，否则 Run 键会把它拆成「程序 + 参数」。
    #[test]
    fn quote_exe_path_wraps_in_quotes() {
        assert_eq!(
            quote_exe_path(Path::new(r"C:\Program Files\Switcheroo\switcheroo.exe")),
            r#""C:\Program Files\Switcheroo\switcheroo.exe""#
        );
        assert_eq!(
            quote_exe_path(Path::new("/usr/bin/switcheroo")),
            "\"/usr/bin/switcheroo\""
        );
    }

    /// 引号成对出现，不会出现只加半边的情况。
    #[test]
    fn quoted_path_starts_and_ends_with_quote() {
        let quoted = quote_exe_path(Path::new("a b c.exe"));
        assert!(quoted.starts_with('"'));
        assert!(quoted.ends_with('"'));
        assert_eq!(quoted.matches('"').count(), 2);
    }

    /// REG_SZ 必须是 UTF-16LE 且以两个 0 字节结尾，长度 = (UTF-16 单元数 + 1) * 2。
    #[test]
    fn reg_sz_bytes_are_utf16le_with_nul() {
        let bytes = reg_sz_bytes("ab");
        assert_eq!(bytes, vec![b'a', 0, b'b', 0, 0, 0]);
        assert_eq!(bytes.len(), 6);

        // 中文同样按 UTF-16 编码，不会被截成单字节。
        let bytes = reg_sz_bytes("自启");
        assert_eq!(bytes.len(), (2 + 1) * 2);
        assert_eq!(&bytes[bytes.len() - 2..], &[0, 0]);
    }

    /// 空字符串也要写入一个 NUL，不能写 0 字节（那样等于没写值）。
    #[test]
    fn reg_sz_bytes_of_empty_string_is_one_nul() {
        assert_eq!(reg_sz_bytes(""), vec![0, 0]);
    }

    /// 非 Windows 平台必须给出确定的降级行为，不 panic。
    #[cfg(not(windows))]
    #[test]
    fn autostart_is_unsupported_off_windows() {
        assert!(!is_enabled());
        assert!(enable().is_err());
        assert!(disable().is_err());
    }
}
