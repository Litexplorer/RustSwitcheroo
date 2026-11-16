//! 约 40 行的极简日志器，用来替换 `env_logger`（后者为了体积背了一堆格式化与过滤器代
//! 码，在 dist 里能占到几十 KB）。
//!
//! 参数：无（模块级说明）
//! 核心流程：`init` 把 [`StderrLogger`] 注册为全局 logger 并按 `RUST_LOG`（没设就用编译期
//! 默认值）设置最大级别 → 每次 `log::info!` 之类调用由 [`StderrLogger::log`] 拼成
//! `时:分:秒.毫秒 级别 正文` 一行写进 stderr。级别支持 `info` / `debug` / `off` 这种单值，
//! 也支持 `crate=debug` 取等号右边的部分（只取第一段能解析成级别的指令，不做 per-target
//! 过滤——对只有两三个模块的小程序足够）。
//!
//! 时间戳用的是 UTC：只为了日志有先后次序，不追求与本地时钟一致，因此不引入 chrono。
//!
//! stderr 不可用时（GUI 子系统构建双击启动时根本没有控制台，`std::io::stderr()` 拿到的句柄
//! 无效）写入会返回 `Err`，这里一律用 `let _ =` 忽略：日志只用于排查，绝不允许因为写不出去而
//! panic 或影响浮层。想看日志就从命令行启动并把 stderr 重定向到文件（见 `main.rs` 的说明）。

use std::io::Write;

/// 没有设置 `RUST_LOG` 时使用的默认级别。
const DEFAULT_LEVEL: log::LevelFilter = log::LevelFilter::Info;

/// 全局 logger 本体：无状态，一个零大小类型就够。
struct StderrLogger;

/// 单例：`log::set_logger` 需要 `'static` 引用。
static LOGGER: StderrLogger = StderrLogger;

/// 参数：无
/// 核心流程：注册全局 logger 并按环境变量设最大级别。重复初始化（例如测试里调两次）直接忽略，
/// 不会 panic。
pub fn init() {
    // 已经装过 logger 时返回错误，这里无所谓：级别还是要照设，否则默认是 Off 什么都不打。
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level_from_env());
}

/// 参数：无
/// 核心流程：读 `RUST_LOG`，按逗号拆指令、按 `=` 取右半段，返回第一个能解析成级别的值；
/// 变量缺失或整串都解析不出来时退回 [`DEFAULT_LEVEL`]。
fn level_from_env() -> log::LevelFilter {
    let Ok(spec) = std::env::var("RUST_LOG") else {
        return DEFAULT_LEVEL;
    };
    spec.split(',')
        .find_map(|directive| {
            // `switcheroo_winui=debug` 取 `debug`；单值 `debug` 原样取回。
            let value = directive.split('=').next_back().unwrap_or(directive).trim();
            value.parse().ok()
        })
        .unwrap_or(DEFAULT_LEVEL)
}

/// 参数：无
/// 核心流程：取当前 UTC 时间，格式化成 `HH:MM:SS.mmm`（只保留一天内的部分，够看先后）。
fn timestamp() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let seconds = (millis / 1000) % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60,
        millis % 1000
    )
}

impl log::Log for StderrLogger {
    /// 参数：`metadata` 本条日志的级别等信息
    /// 核心流程：级别不超过全局最大值才输出。
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    /// 参数：`record` 一条日志（级别 + 正文）
    /// 核心流程：拼成一行后一次 `write_all` 写进 stderr——单次写入可以避免多线程下同一行被
    /// 别的输出插在中间。写失败（stderr 被关掉）直接忽略，日志不允许影响主流程。
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("{} {:<5} {}\n", timestamp(), record.level(), record.args());
        let _ = std::io::stderr().write_all(line.as_bytes());
    }

    /// 参数：无
    /// 核心流程：stderr 通常无缓冲，这里只把可能存在的缓冲刷掉，失败同样忽略。
    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 时间戳形如 `12:34:56.789`：长度固定 12，冒号与点各就各位。
    #[test]
    fn timestamp_has_fixed_shape() {
        let stamp = timestamp();
        assert_eq!(stamp.len(), 12, "{stamp}");
        assert_eq!(stamp.as_bytes()[2], b':');
        assert_eq!(stamp.as_bytes()[5], b':');
        assert_eq!(stamp.as_bytes()[8], b'.');
    }
}
