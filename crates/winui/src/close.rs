//! 「关闭中」窗口集合：投递关闭请求之后、确认窗口真的消失之前的那段时间。
//!
//! 参数：无（模块级说明）
//! 核心流程：[`App::close_selected`](crate::app) 对选中窗口调 `request_close` 成功后，把窗口 id
//! 与请求时刻记进 [`ClosingWindows`] → 列表把这些行画成灰色 → 窗口线程的 125ms 定时器
//! （`WM_TIMER`）逐个 `is_window_alive` 轮询，确认消失就从集合里删掉并移除列表行。
//! 集合本身不碰 Win32、不开线程，只负责「哪些窗口在等关闭 + 等了多久」这一份状态。

use std::collections::HashMap;
use std::time::Instant;

use switcheroo_core::WindowId;

/// 轮询「关闭中的窗口是否已经消失」的周期（毫秒）。
pub const POLL_INTERVAL_MS: u32 = 125;

/// `WM_TIMER` 用的定时器 id：全程序只跑一个关闭轮询定时器，固定 id 就够区分。
pub const POLL_TIMER_ID: usize = 0x5710;

/// 正在等待关闭的窗口。
///
/// 参数：`entries` 窗口 id → 投递关闭请求的时刻。
/// 核心流程：`request_close` 成功后由 [`ClosingWindows::insert`] 记一条；轮询确认窗口消失、
/// 投递失败、浮层隐藏或进程退出时由 [`ClosingWindows::remove`] / [`ClosingWindows::clear`] 清掉。
/// 用一张 `HashMap<WindowId, Instant>` 同时承担「是否关闭中」的集合查询与「已经等了多久」的计时，
/// 而不是 `HashSet` + 时间表两份数据：两份数据每次增删都要同步，漏一处就会出现「关了但永远不轮询」
/// 或「轮询了一个早已移除的 id」。
#[derive(Default)]
pub struct ClosingWindows {
    entries: HashMap<WindowId, Instant>,
}

impl ClosingWindows {
    /// 参数：无
    /// 核心流程：给 [`App`](crate::app) 一个空集合，等价于 `Default`。
    pub fn new() -> Self {
        Self::default()
    }

    /// 参数：`id` 窗口 id
    /// 核心流程：集合里有没有这个窗口——渲染时用它决定该行要不要画成灰色。
    pub fn contains(&self, id: WindowId) -> bool {
        self.entries.contains_key(&id)
    }

    /// 参数：`id` 已成功投递关闭请求的窗口
    /// 核心流程：以当前 `Instant` 记一条；重复插入（理论上不会发生，调用方先查了 `contains`）
    /// 只覆盖时间，不会产生第二条记录。
    pub fn insert(&mut self, id: WindowId) {
        self.entries.insert(id, Instant::now());
    }

    /// 参数：`id` 窗口 id
    /// 核心流程：移除一条并返回当初记下的时刻，供调用方算「关闭用了多久」；不存在时返回 `None`。
    pub fn remove(&mut self, id: WindowId) -> Option<Instant> {
        self.entries.remove(&id)
    }

    /// 参数：无
    /// 核心流程：没有任何窗口在等关闭——定时器可以停了。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 参数：无
    /// 核心流程：当前等待关闭的窗口个数，只用于日志。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 参数：无
    /// 核心流程：把 id 抄成一份快照再返回，让轮询可以一边遍历一边从集合里删（不能边迭代边改）。
    pub fn ids(&self) -> Vec<WindowId> {
        self.entries.keys().copied().collect()
    }

    /// 参数：无
    /// 核心流程：整表清空——浮层隐藏、列表被关空或进程退出时用，避免残留的 id 在下次唤起时
    /// 被当成「关闭中」而一直画灰色。
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
