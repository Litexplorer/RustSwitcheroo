//! Windows 分支入口。
//!
//! 参数：无
//! 核心流程：真实枚举在 [`crate::window_enum`] 里完成（`EnumWindows` + Win32 属性
//! 采集），这里只做转发，让 `lib.rs` 保持"一个 `imp` 两种实现"的 cfg 分发形状。

pub use crate::window_enum::{snapshot, snapshot_detailed};
