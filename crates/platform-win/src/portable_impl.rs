//! 非 Windows 平台的占位实现。
//!
//! 参数：无
//! 核心流程：保持与 Windows 实现相同的函数签名，返回空数据，
//! 使 `cargo check` 在开发机（Linux/macOS）上也能通过。

use switcheroo_core::{WindowAttributes, WindowInfo};

/// 参数：无
/// 核心流程：非 Windows 平台没有可枚举的桌面窗口，直接返回空快照。
pub fn snapshot() -> Vec<WindowInfo> {
    log::debug!("platform-win stubbed on this target");
    Vec::new()
}

/// 参数：无
/// 核心流程：与 [`snapshot`] 同理，非 Windows 平台没有窗口属性可采集，返回空列表。
pub fn snapshot_detailed() -> Vec<(WindowInfo, WindowAttributes)> {
    log::debug!("platform-win window enumeration stubbed on this target");
    Vec::new()
}
