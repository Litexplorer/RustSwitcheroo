#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Switcheroo 的 egui 入口。
//!
//! 参数：无（配置从 `config.ini` 读取，路径由 core 的 `config_path()` 给出）
//! 核心流程：初始化日志 → 读取配置（US-046）→ 抢占单实例互斥量（已有实例则静默退出；平台
//! 调用失败则 fail-open 继续启动）→ 组装"无边框 / 透明 / 置顶 / 不进任务栏 / 初始隐藏"的
//! viewport → 用 eframe 驱动 [`app::SwitcherooApp`]，由它按配置注册全局热键、建托盘并负责
//! 唤起与收起。

mod app;
mod fonts;
mod icons;

use switcheroo_core::{load_settings, APP_NAME};
use switcheroo_platform_win::{SingleInstance, SingleInstanceError};

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // 先读配置再建窗口：热键、主题与开机自启都由它决定；文件不存在或读取失败时
    // `load_settings` 自己回落默认值，不会让启动失败。
    let settings = load_settings();
    log::info!(
        "已读取配置：启用热键 {}（{}），自启 {}，主题 {:?}",
        settings.enable_hotkey,
        app::hotkey_text(&settings.hotkey),
        settings.autostart,
        settings.theme
    );

    // US-034：抢命名互斥量。已有实例在跑时以退出码 0 静默结束（不建窗口、不注册热键），
    // 避免第二个实例打扰用户；平台调用失败则不能无声退出，打印错误后把原因交给界面。
    let single_instance = match SingleInstance::acquire() {
        Ok(instance) => Ok(instance),
        Err(SingleInstanceError::AlreadyRunning) => {
            log::info!("已有 Switcheroo 实例在运行，本次启动直接退出");
            return Ok(());
        }
        Err(err) => {
            log::error!("单实例互斥量获取失败，按 fail-open 继续启动：{err}");
            Err(err)
        }
    };

    // 一次性配好浮层形态 -> 后续窗口装饰、任务栏、置顶行为都由这里决定；
    // `with_visible(false)` 让进程启动后常驻后台，等热键第一次唤起才出现。
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        // 浮层是一张 600×640 的白卡片（约 20 行窗口列表），窗口本身固定不可缩放。
        .with_inner_size([600.0, 640.0])
        .with_decorations(false)
        .with_transparent(true)
        .with_taskbar(false)
        .with_resizable(false)
        .with_always_on_top()
        .with_visible(false)
        // 窗口/任务栏图标取自 `assets/app-icon-32.rgba`（32×32 原始 RGBA，4096 字节）：
        // 编译期 `include_bytes!` 进来，路径相对本文件（`src/main.rs`），运行时不读文件。
        .with_icon(eframe::egui::IconData {
            rgba: include_bytes!("../assets/app-icon-32.rgba").to_vec(),
            width: 32,
            height: 32,
        });

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    // 启动 eframe 事件循环 -> 窗口显示与按键分发交给 egui/winit；
    // 单实例结果（成功句柄或失败原因）与配置一起移交给 app，句柄在进程退出时随 app 的 Drop 释放。
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::SwitcherooApp::new(
                cc,
                single_instance,
                settings,
            )))
        }),
    )
}
