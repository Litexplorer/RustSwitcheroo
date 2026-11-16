//! 明暗主题的纯逻辑（S9）。
//!
//! 参数：无（只依赖 core 的 [`ThemeSetting`]，不碰任何 Win32 类型）
//! 核心流程：把「配置里的三态 + 系统当前明暗」折算成最终的一档（[`dark_for`]）、给出
//! `Ctrl+T` 的浅深翻面结果（[`toggled_theme`]）、面板三选一的循环顺序（[`cycle_theme`]）与
//! 中文标签（[`theme_label`]）。真正的颜色表在 `render::Palette`，这里只决定「该用哪一档」，
//! 因此这几个函数可以在 Linux 上直接单测。

use switcheroo_core::ThemeSetting;

/// 选项面板里三选一的固定顺序（也是 [`cycle_theme`] 的顺序）。
pub const THEME_CHOICES: [ThemeSetting; 3] = [
    ThemeSetting::System,
    ThemeSetting::Light,
    ThemeSetting::Dark,
];

/// 参数：`theme` 配置里的主题偏好；`system_dark` 系统当前是否深色
/// 核心流程：`System` 跟随系统，`Light` / `Dark` 直接给出结果；纯函数，启动、`Ctrl+T` 与
/// 选项面板共用同一处判断。
pub fn dark_for(theme: ThemeSetting, system_dark: bool) -> bool {
    match theme {
        ThemeSetting::System => system_dark,
        ThemeSetting::Light => false,
        ThemeSetting::Dark => true,
    }
}

/// 参数：`theme` 当前主题偏好；`system_dark` 系统当前是否深色
/// 核心流程：`Ctrl+T` 永远在浅 / 深之间翻面——`System` 先按系统实际观感落到一侧再翻转，
/// 于是切完就脱离「跟随系统」（与 egui 版托盘「切换明暗」同一条规则）。
pub fn toggled_theme(theme: ThemeSetting, system_dark: bool) -> ThemeSetting {
    if dark_for(theme, system_dark) {
        ThemeSetting::Light
    } else {
        ThemeSetting::Dark
    }
}

/// 参数：`theme` 主题偏好
/// 核心流程：按 [`THEME_CHOICES`] 的顺序走到下一档（`System → Light → Dark → System`），
/// 面板里点「当前已选中的那一项」时用它循环，避免多做一个控件。
pub fn cycle_theme(theme: ThemeSetting) -> ThemeSetting {
    match theme {
        ThemeSetting::System => ThemeSetting::Light,
        ThemeSetting::Light => ThemeSetting::Dark,
        ThemeSetting::Dark => ThemeSetting::System,
    }
}

/// 参数：`theme` 主题偏好
/// 核心流程：给选项面板的三选一与日志提供中文标签，三种取值与配置文件的
/// `system` / `light` / `dark` 一一对应。
pub fn theme_label(theme: ThemeSetting) -> &'static str {
    match theme {
        ThemeSetting::System => "跟随系统",
        ThemeSetting::Light => "浅色",
        ThemeSetting::Dark => "深色",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三态 × 系统明暗 共六种组合，只有 `System` 看系统。
    #[test]
    fn system_only_follows_the_os() {
        assert!(dark_for(ThemeSetting::System, true));
        assert!(!dark_for(ThemeSetting::System, false));
        assert!(!dark_for(ThemeSetting::Light, true));
        assert!(!dark_for(ThemeSetting::Light, false));
        assert!(dark_for(ThemeSetting::Dark, true));
        assert!(dark_for(ThemeSetting::Dark, false));
    }

    /// `Ctrl+T` 翻面：`System` 先落到系统那一侧再翻，`Light` / `Dark` 直接互换。
    #[test]
    fn toggle_flips_current_look_and_leaves_system() {
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
        assert_eq!(toggled_theme(ThemeSetting::Dark, true), ThemeSetting::Light);
    }

    /// 三态循环走一圈回到原点，顺序与面板从左到右一致。
    #[test]
    fn cycle_visits_three_states_and_returns() {
        let mut theme = ThemeSetting::System;
        let mut seen = vec![theme];
        for _ in 0..2 {
            theme = cycle_theme(theme);
            seen.push(theme);
        }
        assert_eq!(seen, THEME_CHOICES.to_vec());
        assert_eq!(cycle_theme(theme), ThemeSetting::System);
    }

    /// 中文标签不重复、不为空，面板按 [`THEME_CHOICES`] 顺序绘制时恰好一行三个。
    #[test]
    fn labels_are_distinct_and_non_empty() {
        let labels: Vec<&str> = THEME_CHOICES.iter().map(|t| theme_label(*t)).collect();
        assert_eq!(labels, vec!["跟随系统", "浅色", "深色"]);
    }
}
