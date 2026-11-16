//! 浮动列表的全部状态：窗口快照、排名结果、查询串、选中项与滚动偏移。
//!
//! 参数：无（模块级说明）
//! 核心流程：热键唤起时 [`ListState::reload`] 取一次前台窗口 + 快照，用 core 的
//! `move_foreground_to_end` 把唤起前的前台窗口挪到末尾，然后再算一次排名；之后只有
//! 「查询变化」或「重新唤起」才重算排名，渲染路径只读结果，绝不每帧重算。
//! 键盘导航交给 core 的 [`Selection`]（越界自动回绕），滚动偏移由
//! [`ListState::ensure_selected_visible`] 在选中项跑出可视区时补齐。

use switcheroo_core::{
    move_foreground_to_end, rank_windows_with_foreground, RankedWindow, Selection, WindowId,
    WindowInfo,
};
use switcheroo_platform_win::{foreground_window, snapshot};

/// 列表的完整状态。
///
/// 参数：`windows` 本次唤起的窗口快照（顺序即切换顺序）；`ranked` 按查询算出的排名结果；
/// `foreground_process` 唤起瞬间的前台进程名（`.标题` 复合查询要用）；`query` 当前查询串；
/// `selection` 选中项；`offset` 首个可见行下标（虚拟滚动的起点）。
/// 核心流程：由 [`ListState::reload`] 在每次唤起时整体重置，之后由查询变化与键盘导航推进。
pub struct ListState {
    windows: Vec<WindowInfo>,
    ranked: Vec<RankedWindow>,
    foreground_process: Option<String>,
    query: String,
    selection: Selection,
    offset: usize,
}

impl Default for ListState {
    /// 参数：无
    /// 核心流程：空快照 + 空排名 + 无选中，`offset` 归零。
    fn default() -> Self {
        Self {
            windows: Vec::new(),
            ranked: Vec::new(),
            foreground_process: None,
            query: String::new(),
            selection: Selection::new(0),
            offset: 0,
        }
    }
}

impl ListState {
    /// 参数：无
    /// 核心流程：等价于 `Default`，单独给出来是因为 `app` 里读起来更直白。
    pub fn new() -> Self {
        Self::default()
    }

    /// 参数：无
    /// 核心流程：先取唤起前的前台窗口（此刻浮层还没显示，前台仍是用户原来的窗口）→ 再取真实
    /// 窗口快照 → 用 core 把前台窗口移到末尾，让列表首项落在「下一个窗口」→ 记下前台进程名 →
    /// 清空查询、选中第 1 项、偏移归零 → 重算排名。空快照时不 panic，只留下空列表。
    pub fn reload(&mut self) {
        let foreground = foreground_window();
        let mut windows = snapshot();

        if let Some(fg) = &foreground {
            let moved = move_foreground_to_end(&mut windows, fg.id, &fg.process_name);
            log::debug!(
                "唤起前的前台窗口 {}({:#X}) 置底：{}",
                fg.process_name,
                fg.id.raw(),
                moved
            );
        } else {
            log::debug!("唤起时没有前台窗口，列表顺序保持不变");
        }

        // 必须在 refresh 之前赋值：`.标题` 这类复合查询靠它补全进程片段。
        self.foreground_process = foreground.map(|fg| fg.process_name);
        let count = windows.len();
        self.windows = windows;
        self.query.clear();
        self.offset = 0;
        self.refresh(true);
        log::info!(
            "窗口加载完成：快照 {count} 个，查询无，选中第 1 项（前台进程 {}）",
            self.foreground_process.as_deref().unwrap_or("-")
        );
    }

    /// 参数：`query` 输入框里的最新全文
    /// 核心流程：与当前查询相同则什么都不做（`EN_CHANGE` 在光标移动等场景也会触发，避免无谓
    /// 重排）；否则替换查询 → 重算排名 → 选中项收敛进新列表（`Selection::clamp`，不清零）。
    pub fn set_query(&mut self, query: &str) {
        if self.query == query {
            return;
        }
        self.query.clear();
        self.query.push_str(query);
        self.refresh(false);
    }

    /// 参数：`reset_selection` 为真时选中第 1 项并清零偏移，否则只把选中项收敛进新列表
    /// 核心流程：用查询串、快照与前台进程名调 core 的 `rank_windows_with_foreground`（空查询走
    /// 它自己的「返回全部」分支）→ 按参数重置或收敛选中项 → 偏移同样清零或收敛。
    fn refresh(&mut self, reset_selection: bool) {
        self.ranked = rank_windows_with_foreground(
            &self.query,
            &self.windows,
            self.foreground_process.as_deref(),
        );
        let len = self.ranked.len();
        if reset_selection {
            self.selection.reset(len, false);
            self.offset = 0;
        } else {
            self.selection.clamp(len);
            self.offset = self.offset.min(len.saturating_sub(1));
        }
        log::debug!(
            "排名结果 {len} / 共 {} 个窗口（查询「{}」）",
            self.windows.len(),
            self.query
        );
    }

    /// 参数：无
    /// 核心流程：选中项下移一位，末项回绕到首项；列表为空时 `Selection` 自己保持无选中。
    pub fn next(&mut self) {
        self.selection.next(self.ranked.len());
    }

    /// 参数：无
    /// 核心流程：选中项上移一位，首项回绕到末项。
    pub fn previous(&mut self) {
        self.selection.previous(self.ranked.len());
    }

    /// 参数：`row` 目标行号（按排名结果计，即鼠标点中的那一行）
    /// 核心流程：把选中项直接挪到这一行；越界（列表变短、行号来自旧状态）由
    /// [`Selection::clamp`] 收敛回末项。滚动偏移不动——点击本来就落在可视区内，不需要跟着滚。
    pub fn set_selected(&mut self, row: usize) {
        self.select_index(row);
    }

    /// 参数：`index` 目标下标（越界自动收敛）
    /// 核心流程：core 的 [`Selection`] 只提供「相对移动」，没有直接赋值——先 `reset` 到首项，
    /// 再按需 `next` 若干步，最后 `clamp` 兜底。列表只有几十行，这个循环的成本可以忽略。
    fn select_index(&mut self, index: usize) {
        let len = self.ranked.len();
        self.selection.reset(len, false);
        let target = index.min(len.saturating_sub(1));
        for _ in 0..target {
            self.selection.next(len);
        }
        self.selection.clamp(len);
    }

    /// 参数：`delta` 滚动步数（正数向下翻、负数向上翻）；`visible_rows` 一屏能画下的行数
    /// 核心流程：一屏放得下（或一屏为零）时直接忽略，不产生空滚 → 偏移按 `delta` 增减并夹到
    /// `[0, len - visible_rows]` → 返回偏移是否真的变了，调用方据此决定要不要重绘。
    pub fn scroll(&mut self, delta: isize, visible_rows: usize) -> bool {
        let len = self.ranked.len();
        if visible_rows == 0 || len <= visible_rows {
            return false;
        }
        let max = len - visible_rows;
        let current = self.offset.min(max);
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta as usize).min(max)
        };
        if next == self.offset {
            return false;
        }
        self.offset = next;
        true
    }

    /// 参数：`id` 要从列表里移除的窗口（关闭轮询确认它已经消失）
    /// 核心流程：在快照里找到它的位置 → 从 `windows` 里删掉，并把 `ranked` 里指向它的那条一起
    /// 删掉、其余条目的快照下标整体前移一位（两份数据必须同步，否则排名会指向错的窗口）→
    /// 修正选中项：被删的正是选中行时下标原地就是「下一条」（后面的条目整体前移），越过末尾时
    /// `clamp` 收敛到末项，也就是「没有下一条就选上一条」→ 滚动偏移收进新长度，返回是否删掉。
    pub fn remove_window(&mut self, id: WindowId) -> bool {
        let Some(position) = self.windows.iter().position(|window| window.id == id) else {
            return false;
        };
        let selected = self.selection.index();
        let removed_rank = self.ranked.iter().position(|row| row.index == position);

        self.windows.remove(position);
        // 顺序很重要：先按「原始下标」删掉指向被删窗口的那一条，再把更大的下标整体减一；
        // 反过来做会把「原本排在后面、减一之后正好等于 position」的窗口误删。
        self.ranked.retain(|row| row.index != position);
        for row in &mut self.ranked {
            if row.index > position {
                row.index -= 1;
            }
        }

        if let Some(selected) = selected {
            let target = match removed_rank {
                // 删除位置在选中行之前：选中行整体前移一位。
                Some(rank) if selected > rank => selected - 1,
                // 删的就是选中行，或它在选中行之后：下标原地就是「下一条」。
                _ => selected,
            };
            self.select_index(target);
        }
        self.offset = self.offset.min(self.ranked.len().saturating_sub(1));
        log::debug!(
            "已从列表移除窗口 {:#X}（快照剩 {} 个，排名剩 {} 个）",
            id.raw(),
            self.windows.len(),
            self.ranked.len()
        );
        true
    }

    /// 参数：无
    /// 核心流程：快照已经空得一个窗口都不剩——关闭轮询据此决定「列表被关空，隐藏浮层」。
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// 参数：无
    /// 核心流程：整体回到 `Default`：清空查询串、窗口快照、排名结果、选中项与滚动偏移。
    /// 切换成功隐藏浮层后调用，保证列表里不留上一次的状态（下次唤起 `reload` 会重新取快照）。
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// 参数：`visible_rows` 一屏能画下的行数
    /// 核心流程：选中项在可视区上方就把它顶到第一行；在下方就把视窗下移，让选中项正好落在
    /// 最后一行；最后把偏移夹在 `[0, len - visible_rows]` 内，避免列表变短后露出空白。
    pub fn ensure_selected_visible(&mut self, visible_rows: usize) {
        let len = self.ranked.len();
        if visible_rows == 0 || len == 0 {
            self.offset = 0;
            return;
        }
        if let Some(selected) = self.selection.index() {
            if selected < self.offset {
                self.offset = selected;
            } else if selected >= self.offset + visible_rows {
                self.offset = selected + 1 - visible_rows;
            }
        }
        self.offset = self.offset.min(len.saturating_sub(visible_rows.max(1)));
    }

    /// 参数：`visible_rows` 一屏能画下的行数
    /// 核心流程：从偏移量开始取最多 `visible_rows` 行，返回 `[start, end)` 半开区间；
    /// 越界端一律夹到 `len`，调用方直接用这个区间做虚拟化渲染。
    pub fn visible_range(&self, visible_rows: usize) -> (usize, usize) {
        let len = self.ranked.len();
        let start = self.offset.min(len);
        let end = start.saturating_add(visible_rows).min(len);
        (start, end)
    }

    /// 参数：`row` 行号（按排名结果计）
    /// 核心流程：行号 → 排名结果里的快照下标 → 快照项；任何一步越界都返回 `None`。
    pub fn row(&self, row: usize) -> Option<&WindowInfo> {
        let index = self.ranked.get(row)?.index;
        self.windows.get(index)
    }

    /// 参数：`row` 行号（按排名结果计）
    /// 核心流程：直接返回该行的排名信息（含标题 / 进程名的命中字节区间），越界返回 `None`。
    /// 渲染时与 [`ListState::row`] 配对使用：前者给文本，后者给要高亮的区间。
    pub fn ranked(&self, row: usize) -> Option<&RankedWindow> {
        self.ranked.get(row)
    }

    /// 参数：无
    /// 核心流程：返回本次唤起的窗口快照（顺序即切换顺序），供图标预取与缓存裁剪使用。
    pub fn windows(&self) -> &[WindowInfo] {
        &self.windows
    }

    /// 参数：无
    /// 核心流程：返回当前选中行号，`None` 表示列表为空。
    pub fn selected(&self) -> Option<usize> {
        self.selection.index()
    }

    /// 参数：无
    /// 核心流程：快照为空提示「没有可切换的窗口」；有快照但查询无命中提示「没有匹配的窗口」；
    /// 有结果时返回 `None`，由调用方去画列表。
    pub fn message(&self) -> Option<&'static str> {
        if self.windows.is_empty() {
            Some("没有可切换的窗口")
        } else if self.ranked.is_empty() {
            Some("没有匹配的窗口")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switcheroo_core::MatchResult;

    /// 参数：`raw` 窗口句柄数值
    /// 核心流程：造一个标题、进程名都可读的窗口快照，供断言里比对。
    fn window(raw: u64) -> WindowInfo {
        WindowInfo::new(WindowId::new(raw), format!("窗口{raw}"), "app")
    }

    /// 参数：`index` 该行在快照里的下标
    /// 核心流程：造一条未命中任何查询的排名结果（分数与命中区间都不参与本组测试）。
    fn ranked(index: usize) -> RankedWindow {
        RankedWindow {
            index,
            score: 0,
            title: MatchResult::default(),
            process: MatchResult::default(),
        }
    }

    /// 参数：`count` 快照里的窗口个数
    /// 核心流程：造一个「排名顺序 = 快照顺序」且选中第 1 项的列表，测试再按需移动选中项。
    fn list(count: u64) -> ListState {
        ListState {
            windows: (1..=count).map(window).collect(),
            ranked: (0..count as usize).map(ranked).collect(),
            foreground_process: None,
            query: String::new(),
            selection: Selection::new(count as usize),
            offset: 0,
        }
    }

    /// 参数：`state` 列表；`row` 目标选中行
    /// 核心流程：把选中项挪到 `row`，用于构造「删掉某一行之前先选中它」的场景。
    fn select(state: &mut ListState, row: usize) {
        state.set_selected(row);
        assert_eq!(state.selected(), Some(row));
    }

    /// 删掉选中行：新选中项落在原来的「下一条」上（后面的条目整体前移一位）。
    #[test]
    fn removing_selected_row_selects_next_row() {
        let mut state = list(3);
        select(&mut state, 1);
        let removed = window(2).id;
        assert!(state.remove_window(removed));
        // 快照变 [1, 3]，排名变 [窗口1, 窗口3]，选中项还是下标 1 —— 也就是「下一条」窗口3。
        assert_eq!(state.windows().len(), 2);
        assert_eq!(state.selected(), Some(1));
        assert_eq!(state.row(1).map(|w| w.id), Some(WindowId::new(3)));
        assert_eq!(state.row(1).map(|w| w.id.raw()), Some(3));
    }

    /// 删掉最后一条且它正被选中：没有下一条，回落到上一条。
    #[test]
    fn removing_last_selected_row_selects_previous_row() {
        let mut state = list(3);
        select(&mut state, 2);
        assert!(state.remove_window(window(3).id));
        assert_eq!(state.windows().len(), 2);
        assert_eq!(state.selected(), Some(1));
        assert_eq!(state.row(1).map(|w| w.id.raw()), Some(2));
    }

    /// 删掉选中行之前的行：选中项前移一位，仍然指着同一个窗口。
    #[test]
    fn removing_earlier_row_keeps_same_window_selected() {
        let mut state = list(3);
        select(&mut state, 2);
        assert!(state.remove_window(window(1).id));
        assert_eq!(state.selected(), Some(1));
        assert_eq!(state.row(1).map(|w| w.id.raw()), Some(3));
    }

    /// 排名结果里的快照下标必须跟着删除一起前移，否则会指向错的窗口。
    #[test]
    fn ranked_indices_stay_in_sync_with_snapshot() {
        let mut state = list(4);
        assert!(state.remove_window(window(2).id));
        // 快照剩 [1, 3, 4]；排名同步成 [0, 1, 2]，逐行取到的窗口依次是 1、3、4。
        let raw_ids: Vec<u64> = (0..state.windows().len())
            .map(|row| state.row(row).map(|window| window.id.raw()).unwrap())
            .collect();
        assert_eq!(raw_ids, vec![1, 3, 4]);
        assert_eq!(state.ranked.len(), 3);
        // 排名里的 index 就是快照下标，逐条都必须落在快照范围内。
        assert!(state
            .ranked
            .iter()
            .all(|row| row.index < state.windows().len()));
    }

    /// 列表被关空：快照与排名都空，选中项变 `None`。
    #[test]
    fn removing_every_row_empties_the_list() {
        let mut state = list(1);
        assert!(state.remove_window(window(1).id));
        assert!(state.is_empty());
        assert_eq!(state.selected(), None);
        assert_eq!(state.message(), Some("没有可切换的窗口"));
    }

    /// 一屏放得下时滚轮无效；放不下时按步长移动并夹在 [0, len - visible] 内。
    #[test]
    fn scroll_is_clamped_and_ignored_when_everything_fits() {
        let mut state = list(5);
        assert!(!state.scroll(1, 5));
        assert!(!state.scroll(-1, 8));
        assert!(state.scroll(2, 2));
        assert_eq!(state.visible_range(2), (2, 4));
        assert!(state.scroll(99, 2));
        assert_eq!(state.visible_range(2), (3, 5));
        assert!(!state.scroll(1, 2));
        assert!(state.scroll(-99, 2));
        assert_eq!(state.visible_range(2), (0, 2));
    }

    /// 移除一条不存在的窗口：返回 false，状态完全不动。
    #[test]
    fn removing_unknown_window_is_a_no_op() {
        let mut state = list(2);
        assert!(!state.remove_window(WindowId::new(99)));
        assert_eq!(state.windows().len(), 2);
        assert_eq!(state.selected(), Some(0));
    }
}
