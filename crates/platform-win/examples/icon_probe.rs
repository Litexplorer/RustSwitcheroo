//! 命令行诊断：前 10 个窗口的图标取不到 / 解不开分别出在哪一步（US-044 真机排查）。
//!
//! 参数：无（直接运行，不需要命令行参数）
//! 核心流程：`snapshot()` 取当前 Alt+Tab 口径窗口 → 取前 [`PROBE_LIMIT`] 个逐个调用
//! [`window_icon`]，一行一条打印 `序号 HWND 进程名 icon=ok WxH 耗时` 或 `icon=none 耗时`；
//! 同时用本文件里的极简 logger 把平台层 `icon:` 前缀的 debug 日志打到 stderr，
//! 因此不用设 `RUST_LOG` 也能看到每一级拿没拿到句柄、解码卡在哪一步。
//!
//! 用法：`cargo run -p switcheroo-platform-win --example icon_probe`（Windows 上执行）
//!
//! 读法：某行 `icon=none` 之后紧跟几条 `[DEBUG] icon:` 日志——只有 `tier=… 0x0` 没有
//! `decode start` 是「拿不到句柄」；有 `decode start` 但 `decode fail … reason=…`
//! 是「拿到句柄但解码失败」。

use log::{LevelFilter, Metadata, Record};
use switcheroo_platform_win::{snapshot, window_icon};

/// 只探测前这么多个窗口，输出短到能整段贴进问题单。
const PROBE_LIMIT: usize = 10;

/// 极简 logger：把日志原样写到 stderr。
///
/// 参数：无
/// 核心流程：平台层只依赖 `log` 门面，示例里实现一个只打印的 `Log` 就能把 debug 日志
/// 带出来，不必为诊断工具新增任何依赖。
struct ProbeLogger;

impl log::Log for ProbeLogger {
    /// 参数：`_metadata` 日志元信息
    /// 核心流程：诊断工具要的就是全量输出，一律放行。
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    /// 参数：`record` 一条日志
    /// 核心流程：写成 `[级别] 正文`，与平台层日志里的 `icon:` 前缀配合即可 grep。
    fn log(&self, record: &Record<'_>) {
        eprintln!("[{}] {}", record.level(), record.args());
    }

    /// 参数：无
    /// 核心流程：stderr 无缓冲，不需要额外冲刷。
    fn flush(&self) {}
}

fn main() {
    if !cfg!(windows) {
        println!("当前平台没有窗口图标可取（非 Windows 构建），请在 Windows 上运行本示例");
        return;
    }

    // 全局 logger 只能设置一次；已被别处设置时忽略错误，下面的探测不受影响。
    let _ = log::set_logger(&ProbeLogger);
    log::set_max_level(LevelFilter::Debug);

    let windows = snapshot();
    let count = windows.len().min(PROBE_LIMIT);
    println!("共 {} 个可切换窗口，检查前 {} 个", windows.len(), count);

    for (index, info) in windows.iter().take(PROBE_LIMIT).enumerate() {
        let started = std::time::Instant::now();
        let icon = window_icon(info.id);
        let elapsed = started.elapsed().as_millis();
        match icon {
            Some(image) => println!(
                "{:>3}. HWND=0x{:X} 进程={} icon=ok {}x{} 耗时={}ms",
                index + 1,
                info.id.raw(),
                info.process_name,
                image.width,
                image.height,
                elapsed
            ),
            None => println!(
                "{:>3}. HWND=0x{:X} 进程={} icon=none 耗时={}ms",
                index + 1,
                info.id.raw(),
                info.process_name,
                elapsed
            ),
        }
    }
}
