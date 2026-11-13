//! 列表选中项：键盘导航的当前位置与循环规则。
//!
//! 参数：无（纯逻辑，只接收列表长度）
//! 核心流程：UI 层把列表长度交给 [`Selection`]，由它维护当前下标；上下移动越界时
//! 回绕，列表长度变化时把下标收敛回合法区间，UI 只读下标决定高亮哪一行。

/// 列表中当前选中的一项。
///
/// 参数：`index` 为选中的下标，`None` 表示无选中（列表为空）
/// 核心流程：由 [`Selection::new`] / [`Selection::reset`] 初始化，之后由
/// [`Selection::next`] / [`Selection::previous`] / [`Selection::clamp`] 演进。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    index: Option<usize>,
}

impl Selection {
    /// 参数：`len` 列表长度
    /// 核心流程：空列表为 `None`；非空列表默认选中第 0 项。
    pub fn new(len: usize) -> Self {
        Self {
            index: if len == 0 { None } else { Some(0) },
        }
    }

    /// 参数：`len` 新的列表长度；`pick_last` 为真时选中最后一项
    /// （对应 `Alt+Shift+Tab` 唤起时从末项开始）
    /// 核心流程：长度归零则为 `None`，否则按 `pick_last` 取末项或首项。
    pub fn reset(&mut self, len: usize, pick_last: bool) {
        self.index = if len == 0 {
            None
        } else if pick_last {
            Some(len - 1)
        } else {
            Some(0)
        };
    }

    /// 参数：无
    /// 核心流程：返回当前下标，`None` 表示没有选中项。
    pub fn index(&self) -> Option<usize> {
        self.index
    }

    /// 参数：`len` 当前列表长度
    /// 核心流程：下标加一；末项再下移回绕到第 0 项；列表为空时保持 `None`。
    pub fn next(&mut self, len: usize) {
        if len == 0 {
            self.index = None;
            return;
        }

        self.index = Some(match self.index {
            Some(i) if i + 1 < len => i + 1,
            // 末项回绕到首项；下标失效（无选中或残留越界）时同样落到首项。
            _ => 0,
        });
    }

    /// 参数：`len` 当前列表长度
    /// 核心流程：下标减一；首项再上移回绕到最后一项；列表为空时保持 `None`。
    pub fn previous(&mut self, len: usize) {
        if len == 0 {
            self.index = None;
            return;
        }

        self.index = Some(match self.index {
            Some(i) if i > 0 && i < len => i - 1,
            // 首项回绕到末项；下标失效（无选中或残留越界）时同样落到末项。
            _ => len - 1,
        });
    }

    /// 参数：`len` 变化后的列表长度
    /// 核心流程：长度为 0 时清空为 `None`；非空时把越界下标收敛到末项，
    /// 无选中时补齐为首项，保证非空列表始终有选中项。
    pub fn clamp(&mut self, len: usize) {
        if len == 0 {
            self.index = None;
            return;
        }

        self.index = Some(match self.index {
            Some(i) if i < len => i,
            // 下标越界（列表变短）收敛到末项。
            Some(_) => len - 1,
            None => 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_empty_list_has_no_index() {
        assert_eq!(Selection::new(0).index(), None);
    }

    #[test]
    fn new_non_empty_list_selects_first_item() {
        assert_eq!(Selection::new(3).index(), Some(0));
    }

    #[test]
    fn next_wraps_from_last_to_first() {
        let mut selection = Selection::new(3);
        selection.next(3);
        assert_eq!(selection.index(), Some(1));
        selection.next(3);
        assert_eq!(selection.index(), Some(2));
        selection.next(3);
        assert_eq!(selection.index(), Some(0));
    }

    #[test]
    fn previous_wraps_from_first_to_last() {
        let mut selection = Selection::new(3);
        selection.previous(3);
        assert_eq!(selection.index(), Some(2));
        selection.previous(3);
        assert_eq!(selection.index(), Some(1));
        selection.previous(3);
        assert_eq!(selection.index(), Some(0));
    }

    #[test]
    fn empty_list_keeps_none_and_does_not_panic() {
        let mut selection = Selection::new(0);
        selection.next(0);
        assert_eq!(selection.index(), None);
        selection.previous(0);
        assert_eq!(selection.index(), None);
        selection.clamp(0);
        assert_eq!(selection.index(), None);
    }

    #[test]
    fn empty_list_clears_existing_selection() {
        let mut selection = Selection::new(2);
        selection.next(0);
        assert_eq!(selection.index(), None);
    }

    #[test]
    fn clamp_moves_out_of_range_index_to_last_item() {
        let mut selection = Selection::new(5);
        // 先把下标推到第 4 项，再把列表长度缩到 3，下标 4 越界。
        selection.previous(5);
        selection.clamp(3);
        assert_eq!(selection.index(), Some(2));
    }

    #[test]
    fn clamp_keeps_valid_index() {
        let mut selection = Selection::new(5);
        selection.next(5);
        selection.next(5);
        selection.clamp(4);
        assert_eq!(selection.index(), Some(2));
    }

    #[test]
    fn clamp_fills_first_item_when_no_selection() {
        let mut selection = Selection::new(0);
        selection.clamp(2);
        assert_eq!(selection.index(), Some(0));
    }

    #[test]
    fn clamp_to_empty_list_clears_index() {
        let mut selection = Selection::new(2);
        selection.clamp(0);
        assert_eq!(selection.index(), None);
    }

    #[test]
    fn reset_with_pick_last_selects_last_item() {
        let mut selection = Selection::new(3);
        selection.reset(4, true);
        assert_eq!(selection.index(), Some(3));
    }

    #[test]
    fn reset_without_pick_last_selects_first_item() {
        let mut selection = Selection::new(3);
        selection.reset(4, false);
        assert_eq!(selection.index(), Some(0));
    }

    #[test]
    fn reset_to_empty_list_clears_index() {
        let mut selection = Selection::new(3);
        selection.reset(0, true);
        assert_eq!(selection.index(), None);
        selection.reset(0, false);
        assert_eq!(selection.index(), None);
    }
}
