//! 设置模型与配置读写。
//!
//! 参数：无（纯逻辑，只额外使用 `std::fs` / `std::env`）
//! 核心流程：app 启动时用 [`load_settings`] 取一份 [`Settings`]，用户在选项面板
//! 改动后调用 [`save_settings`] 写回 `%APPDATA%\Switcheroo\config.ini`。配置文件是
//! 面向用户的纯文本 `key=value`，键名固定、输出顺序稳定，方便手工编辑。

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{HotkeySetting, APP_NAME};

/// 配置文件名。
const CONFIG_FILE_NAME: &str = "config.ini";

/// 主题偏好。
///
/// 参数：无
/// 核心流程：只有三态，UI 据此决定配色；[`ThemeSetting::System`] 表示跟随系统。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeSetting {
    /// 跟随系统。
    #[default]
    System,
    /// 浅色。
    Light,
    /// 深色。
    Dark,
}

impl ThemeSetting {
    /// 参数：无
    /// 核心流程：返回写进配置文件的稳定文本（`system` / `light` / `dark`）。
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// 参数：配置值文本
    /// 核心流程：只认三种稳定取值，其余返回 `None`，由调用方回落到默认值。
    fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// 一份完整配置。
///
/// 参数：字段即配置项，含义见各字段注释
/// 核心流程：由 [`Settings::default`] 给出首启默认值，由 [`Settings::parse`] 从文本
/// 还原、由 [`Settings::to_text`] 序列化为文本，二者保证往返一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// 是否启用全局热键。
    pub enable_hotkey: bool,
    /// 全局热键的组合键。
    pub hotkey: HotkeySetting,
    /// 是否开机自启。
    pub autostart: bool,
    /// 主题偏好。
    pub theme: ThemeSetting,
}

impl Default for Settings {
    /// 参数：无
    /// 核心流程：首启默认值——启用热键、Alt+空格、不开机自启、跟随系统主题。
    fn default() -> Self {
        Self {
            enable_hotkey: true,
            hotkey: HotkeySetting::default(),
            autostart: false,
            theme: ThemeSetting::System,
        }
    }
}

impl Settings {
    /// 参数：`text` 配置文件文本
    /// 核心流程：逐行解析 `key=value`，跳过空行与 `#` / `;` 注释；未知键忽略；
    /// 合法值覆盖默认值，非法值回落到该字段的默认值。
    pub fn parse(text: &str) -> Settings {
        let defaults = Settings::default();
        let mut settings = defaults.clone();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            // 没有 '=' 的行构不成键值对，直接忽略。
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();

            match key {
                "enable_hotkey" => {
                    settings.enable_hotkey = parse_bool(value).unwrap_or(defaults.enable_hotkey);
                }
                "hotkey_key" => {
                    settings.hotkey.key = value.parse::<u32>().unwrap_or(defaults.hotkey.key);
                }
                "hotkey_modifiers" => {
                    settings.hotkey.modifiers =
                        value.parse::<u32>().unwrap_or(defaults.hotkey.modifiers);
                }
                "autostart" => {
                    settings.autostart = parse_bool(value).unwrap_or(defaults.autostart);
                }
                "theme" => {
                    settings.theme = ThemeSetting::parse(value).unwrap_or(defaults.theme);
                }
                // 未知键忽略，给后续版本留余地，也容忍用户手写的多余项。
                _ => {}
            }
        }

        settings
    }

    /// 参数：无
    /// 核心流程：按固定顺序输出 `key=value` 文本（带 `# ` 注释头），
    /// 保证 `Settings::parse(&self.to_text()) == *self`。
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        text.push_str("# Switcheroo 配置，修改后重启生效\n");
        text.push_str("# 布尔值取 true / false，theme 取 system / light / dark\n");
        text.push_str(&format!("enable_hotkey={}\n", self.enable_hotkey));
        text.push_str(&format!("hotkey_key={}\n", self.hotkey.key));
        text.push_str(&format!("hotkey_modifiers={}\n", self.hotkey.modifiers));
        text.push_str(&format!("autostart={}\n", self.autostart));
        text.push_str(&format!("theme={}\n", self.theme.as_str()));
        text
    }
}

/// 参数：配置值文本
/// 核心流程：只认 `true` / `false`，其余返回 `None`（交给调用方回落默认值）。
fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// 参数：`appdata` 为环境变量 `APPDATA` 的原始值，`None` 表示未设置
/// 核心流程：`Some` 且非空时拼成 `<appdata>/Switcheroo/config.ini`；
/// 否则退化为当前目录下的 `config.ini`。
fn config_path_from(appdata: Option<&OsStr>) -> PathBuf {
    match appdata {
        Some(dir) if !dir.is_empty() => Path::new(dir).join(APP_NAME).join(CONFIG_FILE_NAME),
        _ => PathBuf::from(CONFIG_FILE_NAME),
    }
}

/// 参数：无
/// 核心流程：优先 `%APPDATA%\Switcheroo\config.ini`；`APPDATA` 不存在时
/// 退化为当前目录 `config.ini`。
pub fn config_path() -> PathBuf {
    config_path_from(std::env::var_os("APPDATA").as_deref())
}

/// 参数：`path` 配置文件路径
/// 核心流程：读取并解析；文件不存在或读取失败都返回默认配置，不向上报错。
fn load_settings_from(path: &Path) -> Settings {
    match fs::read_to_string(path) {
        Ok(text) => Settings::parse(&text),
        Err(_) => Settings::default(),
    }
}

/// 参数：无
/// 核心流程：读取 [`config_path`]；文件不存在或解析失败都返回
/// [`Settings::default`]（解析本身不会失败，非法值按字段回落）。
pub fn load_settings() -> Settings {
    load_settings_from(&config_path())
}

/// 参数：`path` 目标文件路径；`settings` 要保存的设置
/// 核心流程：先确保父目录存在（父目录为空时跳过，兼容当前目录的相对路径），
/// 再把 [`Settings::to_text`] 写入文件，返回写入结果。
fn save_settings_to(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, settings.to_text())
}

/// 参数：`settings` 要保存的设置
/// 核心流程：确保父目录存在后写入 [`config_path`]，返回写入结果。
pub fn save_settings(settings: &Settings) -> std::io::Result<()> {
    save_settings_to(&config_path(), settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MOD_CONTROL, MOD_SHIFT};
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 参数：无
    /// 核心流程：在系统临时目录下生成一个唯一子目录路径（不创建目录），
    /// 用进程号 + 时间戳 + 自增序号避免并行测试互相踩踏。
    fn unique_temp_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "switcheroo-core-settings-{}-{}-{}",
            std::process::id(),
            stamp,
            seq
        ))
    }

    #[test]
    fn default_settings_match_first_run_values() {
        let settings = Settings::default();
        assert!(settings.enable_hotkey);
        assert_eq!(settings.hotkey, HotkeySetting::default());
        assert!(!settings.autostart);
        assert_eq!(settings.theme, ThemeSetting::System);
    }

    #[test]
    fn parse_of_to_text_roundtrips() {
        let settings = Settings {
            enable_hotkey: false,
            hotkey: HotkeySetting {
                key: 0x41,
                modifiers: MOD_CONTROL | MOD_SHIFT,
            },
            autostart: true,
            theme: ThemeSetting::Dark,
        };
        assert_eq!(Settings::parse(&settings.to_text()), settings);
    }

    #[test]
    fn parse_ignores_comments_blank_lines_and_unknown_keys() {
        let text = "\
# 井号注释
; 分号注释

unknown_key=whatever
没有等号的一行
enable_hotkey=false
autostart=true
hotkey_key=65
hotkey_modifiers=6
";
        let settings = Settings::parse(text);
        assert!(!settings.enable_hotkey);
        assert!(settings.autostart);
        assert_eq!(settings.hotkey.key, 65);
        assert_eq!(settings.hotkey.modifiers, 6);
    }

    #[test]
    fn invalid_values_fall_back_to_defaults() {
        // 先给一个合法值，再用非法值覆盖同一键：结果仍应回到默认值。
        let text = "\
enable_hotkey=false
enable_hotkey=maybe
autostart=perhaps
hotkey_key=not-a-number
hotkey_modifiers=0x10
theme=rainbow
";
        assert_eq!(Settings::parse(text), Settings::default());
    }

    #[test]
    fn theme_parses_all_three_values() {
        assert_eq!(Settings::parse("theme=system").theme, ThemeSetting::System);
        assert_eq!(Settings::parse("theme=light").theme, ThemeSetting::Light);
        assert_eq!(Settings::parse("theme=dark").theme, ThemeSetting::Dark);
        assert_eq!(Settings::parse("theme=unknown").theme, ThemeSetting::System);
    }

    #[test]
    fn config_path_uses_appdata_when_present() {
        let path = config_path_from(Some(OsStr::new("appdata-root")));
        assert!(path.to_string_lossy().contains("Switcheroo"));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("config.ini")
        );

        // 空值等同于未设置，退化到当前目录。
        assert_eq!(
            config_path_from(Some(OsStr::new(""))),
            PathBuf::from("config.ini")
        );
    }

    #[test]
    fn config_path_falls_back_without_appdata() {
        assert_eq!(config_path_from(None), PathBuf::from("config.ini"));
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = unique_temp_dir();
        let path = dir.join("nested").join("config.ini");
        let settings = Settings {
            enable_hotkey: false,
            hotkey: HotkeySetting {
                key: 0x42,
                modifiers: MOD_CONTROL,
            },
            autostart: true,
            theme: ThemeSetting::Light,
        };

        save_settings_to(&path, &settings).expect("保存配置应成功");
        assert_eq!(load_settings_from(&path), settings);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_missing_file_returns_default() {
        let path = unique_temp_dir().join("config.ini");
        assert_eq!(load_settings_from(&path), Settings::default());
    }
}
