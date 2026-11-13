//! Alt+Tab 窗口口径：判定一个顶层窗口是否应出现在切换列表里。
//!
//! 参数：无（纯逻辑，只接收已经采集好的窗口属性快照）
//! 核心流程：平台层把 Win32 查询结果填进 [`WindowAttributes`]，本模块按原版
//! （Windows 任务视图 / Alt+Tab）的规则顺序给出保留或排除的结论。

/// 判定 Alt+Tab 口径所需的窗口属性快照。
///
/// 参数：由平台层通过 Win32 查询采集到的原始事实（可见性、扩展样式、Owner、窗口属性、类名），
/// 这里不做任何推断
/// 核心流程：作为 [`is_alt_tab_window`] 的唯一输入，按值拷贝，避免借用到平台句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowAttributes {
    /// `IsWindowVisible` 为真。
    pub is_visible: bool,
    /// 窗口标题非空。
    pub has_title: bool,
    /// 带 `WS_EX_APPWINDOW` 扩展样式。
    pub is_appwindow: bool,
    /// 带 `WS_EX_TOOLWINDOW` 扩展样式。
    pub is_toolwindow: bool,
    /// 带 `WS_EX_NOACTIVATE` 扩展样式。
    pub is_noactivate: bool,
    /// 其 owner 窗口当前可见。
    pub owner_is_visible: bool,
    /// 带 `WS_EX_TASKLIST_DELETED`（任务栏移除）标记。
    pub is_tasklist_deleted: bool,
    /// 类名为 `Windows.UI.Core.CoreWindow`。
    pub is_core_window: bool,
    /// 来自窗口属性 `ApplicationViewCloakType`（缺失按 0），1 表示对应应用已退出。
    pub cloak_type: u32,
}

/// 判断窗口是否符合 Alt+Tab 口径（应出现在切换列表里）。
///
/// 参数：`attrs` 平台层采集到的窗口属性快照
/// 核心流程：逐条按原版规则顺序短路返回，顺序即优先级：
/// 1. 不可见或无标题 → 排除（原版只切换用户看得见、认得出的窗口）；
/// 2. 已被任务栏移除（`WS_EX_TASKLIST_DELETED`）→ 排除；
/// 3. 带 `WS_EX_APPWINDOW` → 保留（原版认为它明确要求出现在任务栏与切换列表）；
/// 4. UWP 的 `Windows.UI.Core.CoreWindow` → 排除（原版用它去重，真正的宿主是 ApplicationFrameWindow）；
/// 5. 工具窗口、不可激活窗口、owner 可见的附属窗口 → 排除（原版只保留顶层可激活窗口）；
/// 6. cloak 类型为 1（ApplicationFrameWindow 对应应用已退出）→ 排除；0 或 2 → 保留；
/// 7. 其余情况 → 保留。
pub fn is_alt_tab_window(attrs: &WindowAttributes) -> bool {
    // 规则 1：不可见或无标题的窗口用户既看不到也叫不出名字，直接排除。
    if !attrs.is_visible || !attrs.has_title {
        return false;
    }

    // 规则 2：被标记为已从任务栏移除的窗口不再参与切换。
    if attrs.is_tasklist_deleted {
        return false;
    }

    // 规则 3：带 APPWINDOW 扩展样式的窗口明确要求出现在任务栏，直接保留。
    if attrs.is_appwindow {
        return true;
    }

    // 规则 4：UWP 的 CoreWindow 与它的 ApplicationFrameWindow 成对出现，这里做去重排除。
    if attrs.is_core_window {
        return false;
    }

    // 规则 5：工具窗口、不可激活窗口，以及 owner 仍可见的附属窗口都不是独立顶层窗口。
    if attrs.is_toolwindow || attrs.is_noactivate || attrs.owner_is_visible {
        return false;
    }

    // 规则 6：cloak 类型 1 表示 ApplicationFrameWindow 对应的应用已退出，排除；
    // cloak 类型 0（未 cloak）或 2（被 DWM 以其他方式 cloak）仍视为可切换。
    match attrs.cloak_type {
        1 => false,
        // 规则 7：0 / 2 以及原版未覆盖的其他取值都保守保留。
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个"普通可见、有标题、无任何排除标记"的基线窗口。
    fn baseline() -> WindowAttributes {
        WindowAttributes {
            is_visible: true,
            has_title: true,
            ..WindowAttributes::default()
        }
    }

    #[test]
    fn ordinary_visible_window_with_title_is_kept() {
        assert!(is_alt_tab_window(&baseline()));
    }

    #[test]
    fn invisible_window_is_excluded() {
        let attrs = WindowAttributes {
            is_visible: false,
            ..baseline()
        };
        assert!(!is_alt_tab_window(&attrs));
    }

    #[test]
    fn untitled_window_is_excluded() {
        let attrs = WindowAttributes {
            has_title: false,
            ..baseline()
        };
        assert!(!is_alt_tab_window(&attrs));
    }

    #[test]
    fn tasklist_deleted_window_is_excluded() {
        let attrs = WindowAttributes {
            is_tasklist_deleted: true,
            ..baseline()
        };
        assert!(!is_alt_tab_window(&attrs));
    }

    #[test]
    fn appwindow_is_kept() {
        let attrs = WindowAttributes {
            is_appwindow: true,
            ..baseline()
        };
        assert!(is_alt_tab_window(&attrs));
    }

    #[test]
    fn appwindow_precedes_later_exclusions() {
        let attrs = WindowAttributes {
            is_appwindow: true,
            is_toolwindow: true,
            cloak_type: 1,
            ..baseline()
        };
        assert!(is_alt_tab_window(&attrs));
    }

    #[test]
    fn core_window_is_excluded() {
        let attrs = WindowAttributes {
            is_core_window: true,
            ..baseline()
        };
        assert!(!is_alt_tab_window(&attrs));
    }

    #[test]
    fn core_window_is_excluded_before_toolwindow_check() {
        let attrs = WindowAttributes {
            is_core_window: true,
            is_toolwindow: true,
            ..baseline()
        };
        assert!(!is_alt_tab_window(&attrs));
    }

    #[test]
    fn toolwindow_is_excluded() {
        let attrs = WindowAttributes {
            is_toolwindow: true,
            ..baseline()
        };
        assert!(!is_alt_tab_window(&attrs));
    }

    #[test]
    fn noactivate_window_is_excluded() {
        let attrs = WindowAttributes {
            is_noactivate: true,
            ..baseline()
        };
        assert!(!is_alt_tab_window(&attrs));
    }

    #[test]
    fn owned_visible_window_is_excluded() {
        let attrs = WindowAttributes {
            owner_is_visible: true,
            ..baseline()
        };
        assert!(!is_alt_tab_window(&attrs));
    }

    #[test]
    fn cloak_type_zero_is_kept() {
        let attrs = WindowAttributes {
            cloak_type: 0,
            ..baseline()
        };
        assert!(is_alt_tab_window(&attrs));
    }

    #[test]
    fn cloak_type_one_is_excluded() {
        let attrs = WindowAttributes {
            cloak_type: 1,
            ..baseline()
        };
        assert!(!is_alt_tab_window(&attrs));
    }

    #[test]
    fn cloak_type_two_is_kept() {
        let attrs = WindowAttributes {
            cloak_type: 2,
            ..baseline()
        };
        assert!(is_alt_tab_window(&attrs));
    }
}
