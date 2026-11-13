//! 窗口模型：UI、平台层与内核共用的稳定数据结构。
//!
//! 参数：无（纯数据结构）
//! 核心流程：平台层枚举窗口后填充 [`WindowInfo`]，内核基于它做匹配与排序，
//! UI 只读取这些字段做渲染，三方因此不产生直接依赖。

/// 顶层窗口句柄的可移植表示。
///
/// 参数：`0` 为保留值，表示"无窗口"
/// 核心流程：Windows 平台把 `HWND` 的数值存进 `u64`，避免上层接触到平台类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(u64);

impl WindowId {
    /// 参数：`raw` 平台原始句柄数值
    /// 核心流程：包成 [`WindowId`] 供上层传递。
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// 参数：无
    /// 核心流程：返回原始句柄数值，供平台层重新调用 Win32 API。
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// 一个"可被切换"的窗口快照。
///
/// 参数：`id` 窗口句柄、`title` 窗口标题、`process_name` 进程名
/// 核心流程：由平台层枚举产生，检索、排序、渲染都以它为单位。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: WindowId,
    pub title: String,
    pub process_name: String,
}

impl WindowInfo {
    /// 参数：窗口句柄、窗口标题、进程名
    /// 核心流程：组装快照，标题与进程名按住户输入的匹配口径原样保存。
    pub fn new(id: WindowId, title: impl Into<String>, process_name: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            process_name: process_name.into(),
        }
    }
}

/// 把唤起前的前台窗口移到列表末尾。
///
/// 参数：`items` 待调整的窗口列表（原地修改）；`foreground_id` 唤起瞬间的前台窗口句柄；
/// `foreground_process` 唤起瞬间的前台窗口进程名（按 ASCII 忽略大小写比较，空串不参与匹配）
/// 核心流程：找到首个「句柄相同或进程名相同」的项，取出后追加到末尾，让列表首项落在
/// 「下一个窗口」上（对应原版把前台窗口排到最后）；命中项本就在末尾时顺序不变，不算移动。
pub fn move_foreground_to_end(
    items: &mut Vec<WindowInfo>,
    foreground_id: WindowId,
    foreground_process: &str,
) -> bool {
    let Some(index) = items.iter().position(|item| {
        item.id == foreground_id
            || (!foreground_process.is_empty()
                && item.process_name.eq_ignore_ascii_case(foreground_process))
    }) else {
        return false;
    };

    // 已在末尾：列表顺序不变，不产生移动。
    if index + 1 == items.len() {
        return false;
    }

    let item = items.remove(index);
    items.push(item);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个列表项：句柄按序号递增，进程名随参数给出。
    fn window(raw: u64, process_name: &str) -> WindowInfo {
        WindowInfo::new(WindowId::new(raw), format!("窗口{raw}"), process_name)
    }

    #[test]
    fn keeps_raw_handle_roundtrip() {
        assert_eq!(WindowId::new(0x1234).raw(), 0x1234);
    }

    #[test]
    fn stores_title_and_process_name_as_given() {
        let window = WindowInfo::new(WindowId::new(1), "文件 - PowerPoint", "powerpnt");
        assert_eq!(window.title, "文件 - PowerPoint");
        assert_eq!(window.process_name, "powerpnt");
    }

    #[test]
    fn move_first_item_matching_id_to_end() {
        let mut items = vec![window(1, "alpha"), window(2, "beta"), window(3, "gamma")];
        assert!(move_foreground_to_end(&mut items, WindowId::new(1), ""));
        assert_eq!(
            items.iter().map(|item| item.id.raw()).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn move_item_matching_process_name_ignoring_case() {
        let mut items = vec![window(1, "Explorer"), window(2, "beta"), window(3, "gamma")];
        assert!(move_foreground_to_end(
            &mut items,
            WindowId::new(99),
            "explorer"
        ));
        assert_eq!(
            items.iter().map(|item| item.id.raw()).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn leave_order_when_foreground_is_absent() {
        let mut items = vec![window(1, "alpha"), window(2, "beta")];
        assert!(!move_foreground_to_end(
            &mut items,
            WindowId::new(7),
            "zeta"
        ));
        assert_eq!(
            items.iter().map(|item| item.id.raw()).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn empty_list_reports_no_move() {
        let mut items: Vec<WindowInfo> = Vec::new();
        assert!(!move_foreground_to_end(
            &mut items,
            WindowId::new(1),
            "alpha"
        ));
        assert!(items.is_empty());
    }

    #[test]
    fn already_last_item_reports_no_move() {
        let mut items = vec![window(1, "alpha"), window(2, "beta")];
        assert!(!move_foreground_to_end(&mut items, WindowId::new(2), ""));
        assert_eq!(
            items.iter().map(|item| item.id.raw()).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
}
