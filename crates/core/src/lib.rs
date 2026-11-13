//! Switcheroo 的纯逻辑内核。
//!
//! 本 crate 只承载与界面、操作系统无关的数据结构与算法：窗口模型、查询解析、
//! 匹配器与排序。UI 与平台层都依赖它，它不反向依赖任何一方。

pub mod matcher;
pub mod model;
pub mod pinyin;
pub mod selection;
pub mod settings;
pub mod window;

pub use matcher::{
    match_text, rank_windows, rank_windows_with_foreground, MatchResult, RankedWindow,
};
pub use model::{move_foreground_to_end, WindowId, WindowInfo};
pub use selection::Selection;
pub use settings::{config_path, load_settings, save_settings, Settings, ThemeSetting};
pub use window::{is_alt_tab_window, WindowAttributes};

/// 产品名，界面标题与托盘提示统一使用。
pub const APP_NAME: &str = "Switcheroo";

/// 默认全局热键的虚拟键码（Windows 的 `VK_SPACE`）。
pub const DEFAULT_HOTKEY_KEY: u32 = 0x20;

/// 修饰键位：Alt（Win32 的 `MOD_ALT`）。
pub const MOD_ALT: u32 = 0x1;

/// 修饰键位：Ctrl（Win32 的 `MOD_CONTROL`）。
pub const MOD_CONTROL: u32 = 0x2;

/// 修饰键位：Shift（Win32 的 `MOD_SHIFT`）。
pub const MOD_SHIFT: u32 = 0x4;

/// 修饰键位：Windows 键（Win32 的 `MOD_WIN`）。
pub const MOD_WIN: u32 = 0x8;

/// 全局热键的组合键配置。
///
/// 参数：`key` 为虚拟键码；`modifiers` 为 [`MOD_ALT`] / [`MOD_CONTROL`] /
/// [`MOD_SHIFT`] / [`MOD_WIN`] 的位或
/// 核心流程：作为配置层与平台层之间的稳定类型，US-046 读写配置时以它为落点，
/// 平台层再把它转成自己的热键描述（如 Windows 的 `HotkeySpec`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeySetting {
    pub key: u32,
    pub modifiers: u32,
}

impl Default for HotkeySetting {
    /// 参数：无
    /// 核心流程：返回默认组合键 Alt + 空格（`MOD_ALT` + `VK_SPACE`）。
    fn default() -> Self {
        Self {
            key: DEFAULT_HOTKEY_KEY,
            modifiers: MOD_ALT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认热键必须是 Alt + 空格，且修饰键常量与 Win32 的 MOD_* 取值一致。
    #[test]
    fn default_hotkey_setting_is_alt_space() {
        let setting = HotkeySetting::default();
        assert_eq!(setting.key, 0x20);
        assert_eq!(setting.modifiers, 0x1);
        assert_eq!(
            (MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN),
            (0x1, 0x2, 0x4, 0x8)
        );
    }
}
