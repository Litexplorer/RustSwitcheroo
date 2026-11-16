//! 选项面板的纯逻辑（S10）：草稿、热键显示文本、按键 → 组合键、保存前的热键处置。
//!
//! 参数：无（只依赖 core 的可移植类型，不碰任何 Win32 类型与句柄）
//! 核心流程：面板上的勾选 / 单选 / 录制都只改 [`OptionsDraft`]，`确定` 时才折回
//! [`Settings`] 交给 `app` 去试注册 / 落盘。按键 → 组合键的折算规则（[`hotkey_from_key`]）、
//! 组合键 → 显示文本（[`hotkey_text`]）与保存时的处置判定（[`plan_hotkey_rebind`]）都是纯函数，
//! 所以「哪些键不能录」「冲突时不落盘」这些规则可以在 Linux 上直接单测。

use switcheroo_core::{
    HotkeySetting, Settings, ThemeSetting, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};

/// 选项面板里的一份草稿。
///
/// 参数：字段与 [`Settings`] 一一对应，外加只属于界面的录制状态
/// 核心流程：面板上的勾选 / 单选 / 录制都只改草稿，绝不直接动运行时；`确定` 才把草稿落到
/// 运行时设置并生效，`取消` 时草稿随面板一起丢掉，运行时状态保持不变。
pub struct OptionsDraft {
    /// 是否启用全局热键。
    pub enable_hotkey: bool,
    /// 草稿里的组合键（录制结果写这里）。
    pub hotkey: HotkeySetting,
    /// 是否开机自启。
    pub autostart: bool,
    /// 主题偏好。
    pub theme: ThemeSetting,
    /// 是否正在录制热键。
    pub recording: bool,
}

impl OptionsDraft {
    /// 参数：`settings` 当前生效的设置
    /// 核心流程：逐字段复制，录制状态一律从「未录制」开始，所以每次打开面板看到的都是真实配置。
    pub fn new(settings: &Settings) -> Self {
        Self {
            enable_hotkey: settings.enable_hotkey,
            hotkey: settings.hotkey,
            autostart: settings.autostart,
            theme: settings.theme,
            recording: false,
        }
    }

    /// 参数：无
    /// 核心流程：把草稿折回 [`Settings`]，面板里的界面状态（录制态）不参与。
    pub fn to_settings(&self) -> Settings {
        Settings {
            enable_hotkey: self.enable_hotkey,
            hotkey: self.hotkey,
            autostart: self.autostart,
            theme: self.theme,
        }
    }
}

/// 保存选项时热键这一步的处置方式。
///
/// 参数：无
/// 核心流程：把「试注册新组合键 / 继续用旧监听 / 释放旧监听」三种处置分开，保存流程据此决定
/// 要不要 spawn 新监听；判定本身是纯函数，所以「冲突时不落盘」这条路径可以脱离真机单测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyPlan {
    /// 组合键没变且旧监听还在：不重注册（同一个组合键会撞上自己占着的那一个），只落盘。
    Keep,
    /// 组合键变了、或旧监听不在：先试注册新组合键。
    Register,
    /// 配置里关了热键：不注册，落盘后才释放旧监听。
    Release,
}

/// 参数：`current` 当前生效的设置；`next` 面板里的新设置；`listener_alive` 当前是否有监听在跑
/// 核心流程：不启用热键 → [`HotkeyPlan::Release`]；启用且组合键与当前一致、旧监听还在 →
/// [`HotkeyPlan::Keep`]；其余（组合键变了 / 旧监听不在）→ [`HotkeyPlan::Register`]。
pub fn plan_hotkey_rebind(current: &Settings, next: &Settings, listener_alive: bool) -> HotkeyPlan {
    if !next.enable_hotkey {
        return HotkeyPlan::Release;
    }
    if listener_alive && current.hotkey == next.hotkey {
        return HotkeyPlan::Keep;
    }
    HotkeyPlan::Register
}

/// 参数：`setting` 一条组合键配置
/// 核心流程：按固定顺序 `Ctrl → Shift → Alt → Win → 主键` 拼成人类可读文本（例如
/// `Alt + Space`），键名由 [`key_label`] 提供；未识别的键码退化成 `键码 0x2C` 这样的十六进制，
/// 界面上仍然看得出按了哪个键。
pub fn hotkey_text(setting: &HotkeySetting) -> String {
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

/// 参数：`vk` Win32 虚拟键码
/// 核心流程：给[`hotkey_from_key`]能产出的键码反查一个短名字（`Space` / `F5` / `A` / `0`）；
/// 查不到时返回 `None`，由 [`hotkey_text`] 退化成十六进制键码，不丢信息也不 panic。
pub fn key_label(vk: u32) -> Option<String> {
    // 字母 / 数字 / 功能键：直接按虚拟键码段反算，不用再写一张表。
    if (0x41..=0x5A).contains(&vk) || (0x30..=0x39).contains(&vk) {
        return Some(char::from(vk as u8).to_string());
    }
    if (0x70..=0x87).contains(&vk) {
        return Some(format!("F{}", vk - 0x70 + 1));
    }

    let label = match vk {
        0x20 => "Space",
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

/// 参数：`vk` 虚拟键码
/// 核心流程：判断这个键是不是「修饰键本身」——Shift / Ctrl / Alt / Win 的通用键码与左右键码，
/// 录热键时它们只用来凑修饰位，绝不能当主键（否则会录出 `Ctrl + Ctrl` 这种东西）。
pub fn is_modifier_key(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

/// 参数：`vk` 虚拟键码
/// 核心流程：判断这个键能不能当全局热键的主键——只认字母、数字、功能键、空格与主键盘上一排
/// 标点；方向键 / Tab / 回车 / Esc / 鼠标键都不认：它们要么会抢走系统的键盘导航，要么会让
/// 浮层自己的按键失效，录了也只能算误触。
pub fn is_recordable_key(vk: u32) -> bool {
    (0x41..=0x5A).contains(&vk)      // A–Z
        || (0x30..=0x39).contains(&vk) // 0–9
        || (0x70..=0x87).contains(&vk) // F1–F24
        || vk == 0x20                  // Space
        || (0xBA..=0xC0).contains(&vk) // ; = , - . / `
        || (0xDB..=0xDE).contains(&vk) // [ \ ] '
}

/// 参数：`vk` 按下的虚拟键码；`modifiers` 由 `GetKeyState` 折出来的 `MOD_*` 位或
/// 核心流程：修饰键本身、以及不带任何修饰键的普通键一律返回 `None`（一个光秃秃的字母做全局
/// 热键会把所有程序的打字都抢走，宁可不录），其余可录制的键直接组成 [`HotkeySetting`]。
pub fn hotkey_from_key(vk: u32, modifiers: u32) -> Option<HotkeySetting> {
    if modifiers == 0 || is_modifier_key(vk) || !is_recordable_key(vk) {
        return None;
    }
    Some(HotkeySetting { key: vk, modifiers })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认组合键 Alt + Space 的显示文本，与需求里的例子一致。
    #[test]
    fn default_hotkey_text_is_alt_space() {
        assert_eq!(hotkey_text(&HotkeySetting::default()), "Alt + Space");
    }

    /// 修饰键按固定顺序拼接，字母 / 功能键名字正确。
    #[test]
    fn full_combo_orders_modifiers_predictably() {
        let setting = HotkeySetting {
            key: 0x41,
            modifiers: MOD_WIN | MOD_ALT | MOD_SHIFT | MOD_CONTROL,
        };
        assert_eq!(hotkey_text(&setting), "Ctrl + Shift + Alt + Win + A");

        let function = HotkeySetting {
            key: 0x74,
            modifiers: MOD_CONTROL,
        };
        assert_eq!(hotkey_text(&function), "Ctrl + F5");
    }

    /// 认识不了的键码退化成十六进制，信息不丢。
    #[test]
    fn unknown_key_falls_back_to_hex() {
        let setting = HotkeySetting {
            key: 0x0D,
            modifiers: MOD_ALT,
        };
        assert_eq!(hotkey_text(&setting), "Alt + 键码 0x0D");
    }

    /// 录到 Ctrl + Shift + A：修饰位与主键都要保留。
    #[test]
    fn records_letter_with_modifiers() {
        let captured = hotkey_from_key(0x41, MOD_CONTROL | MOD_SHIFT);
        assert_eq!(
            captured,
            Some(HotkeySetting {
                key: 0x41,
                modifiers: MOD_CONTROL | MOD_SHIFT,
            })
        );
    }

    /// 不带修饰键、只按修饰键、按方向键 / Tab 都不该录出组合键。
    #[test]
    fn refuses_bare_modifier_and_navigation_keys() {
        assert_eq!(hotkey_from_key(0x41, 0), None);
        assert_eq!(hotkey_from_key(0x11, MOD_CONTROL), None);
        assert_eq!(hotkey_from_key(0xA2, MOD_CONTROL), None);
        assert_eq!(hotkey_from_key(0x09, MOD_ALT), None);
        assert_eq!(hotkey_from_key(0x28, MOD_ALT), None);
        assert_eq!(hotkey_from_key(0x0D, MOD_CONTROL), None);
    }

    /// 空格 / 数字 / 标点都能录，这是「Alt + Space」这类默认组合的前提。
    #[test]
    fn accepts_space_digits_and_punctuation() {
        for vk in [0x20, 0x31, 0xBC, 0xC0, 0x87] {
            assert!(
                hotkey_from_key(vk, MOD_ALT).is_some(),
                "键码 0x{vk:02X} 应该可录"
            );
        }
    }

    /// 组合键没变且旧监听还在 → 不重注册（否则会撞上自己）；变了 → 重注册。
    #[test]
    fn plan_keeps_same_hotkey_and_registers_changed_one() {
        let current = Settings::default();
        let same = OptionsDraft::new(&current).to_settings();
        assert_eq!(plan_hotkey_rebind(&current, &same, true), HotkeyPlan::Keep);
        // 旧监听不在（启动时注册失败过）→ 即使组合键一样也要试注册。
        assert_eq!(
            plan_hotkey_rebind(&current, &same, false),
            HotkeyPlan::Register
        );

        let mut changed = same.clone();
        changed.hotkey = HotkeySetting {
            key: 0x42,
            modifiers: MOD_CONTROL,
        };
        assert_eq!(
            plan_hotkey_rebind(&current, &changed, true),
            HotkeyPlan::Register
        );
    }

    /// 关掉热键时既不注册也不保留监听，落盘后才释放。
    #[test]
    fn plan_releases_when_hotkey_disabled() {
        let current = Settings::default();
        let mut next = current.clone();
        next.enable_hotkey = false;
        assert_eq!(
            plan_hotkey_rebind(&current, &next, true),
            HotkeyPlan::Release
        );
    }
}
