//! 命令行验收：打印当前 Alt+Tab 口径下的窗口列表（US-036）。
//!
//! 参数：无（直接运行，不需要命令行参数）
//! 核心流程：调用 `switcheroo_platform_win::snapshot_detailed()` 取回
//! `(WindowInfo, WindowAttributes)`，逐行打印序号、HWND、标题、进程名与可见性；
//! 非 Windows 平台没有窗口枚举能力，只打印一行提示。

fn main() {
    if !cfg!(windows) {
        println!("当前平台不支持窗口枚举");
        return;
    }

    let windows = switcheroo_platform_win::snapshot_detailed();
    println!("共 {} 个可切换窗口", windows.len());

    for (index, (info, attrs)) in windows.iter().enumerate() {
        println!(
            "{:>3}. HWND=0x{:X} 进程={} 可见={} 标题={}",
            index + 1,
            info.id.raw(),
            info.process_name,
            attrs.is_visible,
            info.title
        );
    }
}
