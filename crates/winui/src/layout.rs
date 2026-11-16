//! 卡片内的几何常量与两个小换算函数。
//!
//! 参数：无（全部是 DIP 常量与纯函数）
//! 核心流程：`render`（D2D 绘制）与 `app` / `edit`（Win32 子控件定位）共用这里的同一套数字，
//! 避免「绘制的行高」与「滚动用的行高」各写一遍而对不上。所有数值单位都是 DIP（96 DPI 下的
//! 逻辑像素），转物理像素一律乘调用方算出的 `scale`。

/// 卡片与窗口边缘之间的透明留白：留白区域必须是全透明的，才能看出圆角。
pub const CARD_MARGIN: f32 = 12.0;
/// 卡片圆角半径。
pub const CARD_RADIUS: f32 = 12.0;
/// 卡片内边距：输入框、列表的左/右/上边距都用它。
pub const CARD_PAD: f32 = 10.0;
/// 列表与卡片底边之间的留白。
pub const LIST_BOTTOM_PAD: f32 = 10.0;
/// 输入框高度（DIP）。
pub const EDIT_HEIGHT: f32 = 28.0;
/// 输入框与列表之间的间距。
pub const SEARCH_LIST_GAP: f32 = 8.0;
/// 列表行高（DIP）：行与行之间不留间距，虚拟化按它换算下标。
pub const ROW_HEIGHT: f32 = 30.0;
/// 行内左内边距。
pub const ROW_PAD_X: f32 = 10.0;
/// 选中行底色的左右缩进：比整行略窄一点，看起来才贴着卡片内边距。
pub const ROW_INSET_X: f32 = 4.0;
/// 选中行底色圆角。
pub const ROW_RADIUS: f32 = 4.0;
/// 左侧图标占位块的边长。
pub const ICON_SIZE: f32 = 16.0;
/// 图标占位块的圆角。
pub const ICON_RADIUS: f32 = 3.0;
/// 图标与标题之间的列间距。
pub const COLUMN_GAP: f32 = 8.0;
/// 右侧进程名列的宽度：进程名在这个固定宽度的盒子里右对齐，标题到它左边为止（过长自动裁掉）。
pub const PROCESS_COLUMN: f32 = 160.0;
/// 窗口标题字号。
pub const TITLE_SIZE: f32 = 13.0;
/// 进程名字号。
pub const PROCESS_SIZE: f32 = 11.0;
/// 空列表 / 错误提示字号。
pub const HINT_SIZE: f32 = 13.0;
/// 错误提示在卡片底部预留的高度：有错误时列表少画一行，红字才不会压住最后一行。
pub const ERROR_RESERVE: f32 = 20.0;

// ---------------------------------------------------------------------------
// 选项面板（S10）：面板底色框、控件行与命中测试都用这里的同一套数字
// ---------------------------------------------------------------------------

/// 选项面板底色框的圆角。
pub const PANEL_RADIUS: f32 = 8.0;
/// 面板底色框与里面控件之间的内边距。
pub const PANEL_PAD: f32 = 16.0;
/// 面板标题高度。
pub const PANEL_TITLE_HEIGHT: f32 = 22.0;
/// 标题与第一行控件之间的间距。
pub const PANEL_TITLE_GAP: f32 = 16.0;
/// 复选框 / 主题三选一这类「一行文字 + 一个小方块」的行高。
pub const CHECK_ROW_HEIGHT: f32 = 24.0;
/// 复选框方块的边长。
pub const CHECK_BOX: f32 = 14.0;
/// 复选框方块与右边文字的间距。
pub const CHECK_LABEL_GAP: f32 = 8.0;
/// 热键值框与按钮的高度。
pub const CONTROL_HEIGHT: f32 = 28.0;
/// 「录制」按钮的宽度。
pub const RECORD_BUTTON_WIDTH: f32 = 76.0;
/// 热键值框与「录制」按钮之间的间距。
pub const CONTROL_GAP: f32 = 8.0;
/// 主题三选一里每一项的宽度。
pub const THEME_CHOICE_WIDTH: f32 = 84.0;
/// 主题三选一各项之间的间距。
pub const THEME_CHOICE_GAP: f32 = 8.0;
/// 「主题」两个字占的宽度。
pub const THEME_LABEL_WIDTH: f32 = 48.0;
/// 面板底部按钮（确定 / 取消）的宽度。
pub const PANEL_BUTTON_WIDTH: f32 = 88.0;
/// 面板底部按钮的高度。
pub const PANEL_BUTTON_HEIGHT: f32 = 30.0;
/// 两个底部按钮之间的间距。
pub const PANEL_BUTTON_GAP: f32 = 10.0;
/// 面板里一行说明 / 红字的高度。
pub const PANEL_TEXT_HEIGHT: f32 = 18.0;
/// 面板里各行之间的大间距。
pub const PANEL_ROW_GAP: f32 = 12.0;
/// 说明文字与上一行之间的紧凑间距。
pub const PANEL_TIGHT_GAP: f32 = 6.0;

/// 面板内一块矩形（DIP：左上角 + 宽高）。
///
/// 参数：`x` / `y` 左上角；`width` / `height` 尺寸，单位都是 DIP
/// 核心流程：只做几何运算，不涉及 D2D / Win32 类型，因此绘制端（`render`）与命中端
/// （`app`）能共用同一份布局结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// 参数：见字段说明
    /// 核心流程：直接按字段构图，不做任何夹取。
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// 参数：无
    /// 核心流程：返回右边界（`x + width`）。
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    /// 参数：无
    /// 核心流程：返回下边界（`y + height`）。
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// 参数：`dx` 水平内缩量；`dy` 垂直内缩量
    /// 核心流程：四边各缩进对应距离，缩到负数时夹成 0，避免出现反向矩形。
    pub fn inset(&self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            width: (self.width - 2.0 * dx).max(0.0),
            height: (self.height - 2.0 * dy).max(0.0),
        }
    }

    /// 参数：`x` / `y` 待测点（DIP）
    /// 核心流程：左闭右开判定矩形是否包含该点——相邻的两块控件因此不会同时命中。
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// 面板上被点中的可交互区域。
///
/// 参数：无
/// 核心流程：由 [`PanelLayout::hit`] 产出，`app` 按它改草稿 / 执行保存取消；
/// [`PanelHit::Nothing`] 表示点在面板空白处或面板外。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelHit {
    /// 「启用全局热键」复选框整行。
    EnableHotkey,
    /// 「录制」按钮。
    Record,
    /// 热键值框本身（点它等同于点「录制」）。
    HotkeyValue,
    /// 「开机自启」复选框整行。
    Autostart,
    /// 主题三选一里的第 `index` 项（0 = 跟随系统）。
    Theme(usize),
    /// 「确定」。
    Save,
    /// 「取消」。
    Cancel,
    /// 面板空白处或面板外。
    Nothing,
}

/// 选项面板的完整布局（DIP）。
///
/// 参数：`panel` 面板底色框；其余字段是各个控件行 / 按钮的矩形
/// 核心流程：由 [`panel_layout`] 按窗口逻辑尺寸自上而下排好；`render` 只按它画，`app` 只按它
/// 命中，两边因此不会错位。
pub struct PanelLayout {
    pub panel: Rect,
    pub title: Rect,
    pub enable_row: Rect,
    pub hotkey_value: Rect,
    pub record_button: Rect,
    pub hint: Rect,
    pub autostart_row: Rect,
    pub theme_label: Rect,
    pub theme_choices: [Rect; 3],
    pub error: Rect,
    pub save_button: Rect,
    pub cancel_button: Rect,
}

impl PanelLayout {
    /// 参数：`x` / `y` 鼠标位置（DIP）
    /// 核心流程：面板外的点一律 [`PanelHit::Nothing`]；面板内按「复选框整行 → 录制按钮 →
    /// 主题选项 → 确定 / 取消 → 热键值框」的顺序逐个判包含。复选框整行都可点，比只点
    /// 14×14 的小方块好按。
    pub fn hit(&self, x: f32, y: f32) -> PanelHit {
        if !self.panel.contains(x, y) {
            return PanelHit::Nothing;
        }
        if self.enable_row.contains(x, y) {
            return PanelHit::EnableHotkey;
        }
        if self.record_button.contains(x, y) {
            return PanelHit::Record;
        }
        if self.autostart_row.contains(x, y) {
            return PanelHit::Autostart;
        }
        for (index, choice) in self.theme_choices.iter().enumerate() {
            if choice.contains(x, y) {
                return PanelHit::Theme(index);
            }
        }
        if self.save_button.contains(x, y) {
            return PanelHit::Save;
        }
        if self.cancel_button.contains(x, y) {
            return PanelHit::Cancel;
        }
        if self.hotkey_value.contains(x, y) {
            return PanelHit::HotkeyValue;
        }
        PanelHit::Nothing
    }
}

/// 参数：`window_width_dip` 窗口宽度（DIP）；`logical_height` 窗口高度（DIP）
/// 核心流程：面板底色框铺满卡片内边距以内（与搜索框同一条左边线）→ 再按 [`PANEL_PAD`] 收出
/// 内容区 → 从上到下依次排：标题、启用热键、热键值框 + 录制、说明文字、开机自启、主题三选一、
/// 红字行、确定 / 取消。所有间距都来自本模块常量，绘制与命中共用。
pub fn panel_layout(window_width_dip: f32, logical_height: f32) -> PanelLayout {
    let panel = Rect::new(
        CARD_MARGIN + CARD_PAD,
        CARD_MARGIN + CARD_PAD,
        (window_width_dip - 2.0 * (CARD_MARGIN + CARD_PAD)).max(0.0),
        (logical_height - 2.0 * (CARD_MARGIN + CARD_PAD)).max(0.0),
    );
    let content = panel.inset(PANEL_PAD, PANEL_PAD);

    let title = Rect::new(content.x, content.y, content.width, PANEL_TITLE_HEIGHT);
    let enable_row = Rect::new(
        content.x,
        title.bottom() + PANEL_TITLE_GAP,
        content.width,
        CHECK_ROW_HEIGHT,
    );

    let record_y = enable_row.bottom() + PANEL_ROW_GAP;
    let record_button = Rect::new(
        content.right() - RECORD_BUTTON_WIDTH,
        record_y,
        RECORD_BUTTON_WIDTH,
        CONTROL_HEIGHT,
    );
    let hotkey_value = Rect::new(
        content.x,
        record_y,
        (record_button.x - content.x - CONTROL_GAP).max(0.0),
        CONTROL_HEIGHT,
    );

    let hint = Rect::new(
        content.x,
        hotkey_value.bottom() + PANEL_TIGHT_GAP,
        content.width,
        PANEL_TEXT_HEIGHT,
    );
    let autostart_row = Rect::new(
        content.x,
        hint.bottom() + PANEL_ROW_GAP,
        content.width,
        CHECK_ROW_HEIGHT,
    );

    let theme_y = autostart_row.bottom() + PANEL_ROW_GAP;
    let theme_label = Rect::new(content.x, theme_y, THEME_LABEL_WIDTH, CHECK_ROW_HEIGHT);
    let first_choice_x = theme_label.right() + CHECK_LABEL_GAP;
    let theme_choices = [0, 1, 2].map(|index| {
        Rect::new(
            first_choice_x + index as f32 * (THEME_CHOICE_WIDTH + THEME_CHOICE_GAP),
            theme_y,
            THEME_CHOICE_WIDTH,
            CHECK_ROW_HEIGHT,
        )
    });

    let error = Rect::new(
        content.x,
        theme_y + CHECK_ROW_HEIGHT + PANEL_ROW_GAP,
        content.width,
        PANEL_TEXT_HEIGHT,
    );
    let button_y = error.bottom() + PANEL_ROW_GAP;
    let save_button = Rect::new(content.x, button_y, PANEL_BUTTON_WIDTH, PANEL_BUTTON_HEIGHT);
    let cancel_button = Rect::new(
        save_button.right() + PANEL_BUTTON_GAP,
        button_y,
        PANEL_BUTTON_WIDTH,
        PANEL_BUTTON_HEIGHT,
    );

    PanelLayout {
        panel,
        title,
        enable_row,
        hotkey_value,
        record_button,
        hint,
        autostart_row,
        theme_label,
        theme_choices,
        error,
        save_button,
        cancel_button,
    }
}

/// 参数：无
/// 核心流程：卡片顶边 = 透明留白；输入框顶边再让出卡片内边距。
pub fn search_top() -> f32 {
    CARD_MARGIN + CARD_PAD
}

/// 参数：无
/// 核心流程：输入框底边 + 行间距，就是列表第一行的顶边。
pub fn list_top() -> f32 {
    search_top() + EDIT_HEIGHT + SEARCH_LIST_GAP
}

/// 参数：`window_width_dip` 窗口宽度（DIP）
/// 核心流程：左右各让出「卡片留白 + 卡片内边距」，返回 `(left, top, width, height)`，
/// 由调用方乘 `scale` 得到 `MoveWindow` 需要的物理像素。
pub fn edit_box(window_width_dip: f32) -> (f32, f32, f32, f32) {
    let left = CARD_MARGIN + CARD_PAD;
    let width = window_width_dip - 2.0 * left;
    (left, search_top(), width, EDIT_HEIGHT)
}

/// 参数：`logical_height` 窗口高度（DIP）；`reserve_bottom` 底部要额外预留的高度（有错误红字时传
/// [`ERROR_RESERVE`]，否则传 0）
/// 核心流程：可用的列表高度 = 窗口高 − 卡片下留白 − 底部预留 − 列表顶边，再除以行高向下取整；
/// 结果就是「一屏最多能画多少行」，虚拟化与滚动都拿它当窗口大小。
pub fn visible_rows(logical_height: f32, reserve_bottom: f32) -> usize {
    let available = logical_height - CARD_MARGIN - LIST_BOTTOM_PAD - reserve_bottom - list_top();
    if available <= 0.0 {
        return 0;
    }
    (available / ROW_HEIGHT).floor() as usize
}

/// 参数：`scale` DPI 缩放（1.0 = 96 DPI）
/// 核心流程：乘 100 四舍五入成整数百分比，供日志使用——`{:.2}` 这种浮点格式化会把
/// `core::fmt` 的 grisu 算法（约 15 KB）拉进 dist，日志里 125% 已经够用。
pub fn scale_percent(scale: f32) -> i32 {
    (scale * 100.0).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 600×640 的默认窗口：列表顶边 58 DIP、一屏 18 行、输入框宽 576 DIP。
    #[test]
    fn default_window_fits_expected_layout() {
        assert_eq!(search_top(), 22.0);
        assert_eq!(list_top(), 58.0);
        assert_eq!(visible_rows(640.0, 0.0), 18);
        let (left, top, width, height) = edit_box(600.0);
        assert_eq!((left, top, width, height), (22.0, 22.0, 556.0, 28.0));
    }

    /// 底部预留错误行时少一行；窗口太矮时返回 0 而不是下溢。
    #[test]
    fn reserve_bottom_reduces_rows_and_never_underflows() {
        assert_eq!(visible_rows(640.0, ERROR_RESERVE), 18);
        assert_eq!(visible_rows(80.0, 0.0), 0);
        assert_eq!(visible_rows(0.0, 0.0), 0);
    }

    /// 面板里所有控件都落在底色框内部、互不重叠，且按钮下方还留着空白。
    #[test]
    fn panel_controls_fit_inside_the_panel_without_overlap() {
        let layout = panel_layout(600.0, 640.0);
        let controls = [
            layout.title,
            layout.enable_row,
            layout.hotkey_value,
            layout.record_button,
            layout.hint,
            layout.autostart_row,
            layout.theme_label,
            layout.theme_choices[0],
            layout.theme_choices[1],
            layout.theme_choices[2],
            layout.error,
            layout.save_button,
            layout.cancel_button,
        ];
        for (index, rect) in controls.iter().enumerate() {
            assert!(
                rect.x >= layout.panel.x
                    && rect.right() <= layout.panel.right()
                    && rect.y >= layout.panel.y
                    && rect.bottom() <= layout.panel.bottom(),
                "第 {index} 块控件越出了面板底色框"
            );
            for other in &controls[index + 1..] {
                let disjoint = rect.right() <= other.x
                    || other.right() <= rect.x
                    || rect.bottom() <= other.y
                    || other.bottom() <= rect.y;
                assert!(
                    disjoint,
                    "第 {index} 块控件与后面的控件重叠：{rect:?} / {other:?}"
                );
            }
        }
        assert!(layout.cancel_button.bottom() < layout.panel.bottom());
        // 值框与录制按钮在同一行，中间留 CONTROL_GAP。
        assert_eq!(layout.hotkey_value.y, layout.record_button.y);
        assert_eq!(
            layout.record_button.x - layout.hotkey_value.right(),
            CONTROL_GAP
        );
    }

    /// 命中测试：每个控件的中心点都能命中自己，面板外的点什么都不命中。
    #[test]
    fn hit_test_maps_each_control_to_its_own_action() {
        let layout = panel_layout(600.0, 640.0);
        let center = |rect: Rect| (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);

        let cases = [
            (layout.enable_row, PanelHit::EnableHotkey),
            (layout.record_button, PanelHit::Record),
            (layout.autostart_row, PanelHit::Autostart),
            (layout.theme_choices[0], PanelHit::Theme(0)),
            (layout.theme_choices[1], PanelHit::Theme(1)),
            (layout.theme_choices[2], PanelHit::Theme(2)),
            (layout.save_button, PanelHit::Save),
            (layout.cancel_button, PanelHit::Cancel),
        ];
        for (rect, expected) in cases {
            let (x, y) = center(rect);
            assert_eq!(layout.hit(x, y), expected);
        }
        assert_eq!(layout.hit(1.0, 1.0), PanelHit::Nothing);
        assert_eq!(
            layout.hit(layout.panel.right() + 1.0, layout.panel.y),
            PanelHit::Nothing
        );
    }

    /// 窗口缩到极小时布局不会算出负尺寸（公式里的 `max(0.0)` 兜底）。
    #[test]
    fn panel_layout_survives_tiny_windows() {
        let layout = panel_layout(20.0, 20.0);
        assert!(layout.panel.width >= 0.0 && layout.panel.height >= 0.0);
        assert!(layout.hotkey_value.width >= 0.0);
        assert_eq!(layout.hit(0.0, 0.0), PanelHit::Nothing);
    }
}
