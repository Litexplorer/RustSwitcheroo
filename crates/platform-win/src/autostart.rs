//! 开机自启（US-048 计划任务版）。
//!
//! 参数：无
//! 核心流程：把「开机自启」落在**计划任务**上，而不是注册表 Run 键——
//! `schtasks /create /tn Switcheroo /tr "\"<exe 全路径>\"" /sc onlogon /rl HIGHEST /f`
//! 建一个登录时触发、以最高权限运行的 Task Scheduler 任务。`is_enabled` 用
//! `schtasks /query` 的退出码判定，`enable` 建任务，`disable` 删任务。
//!
//! 为什么换成计划任务：程序 manifest 要求管理员权限，靠 HKCU Run 键启动时会弹 UAC 确认框；
//! `/rl HIGHEST` 的计划任务由任务计划程序以提权方式拉起，登录时不再弹 UAC。
//! 建 `/rl HIGHEST` 的任务本身要求在已提权的进程里做——本程序正常就是以管理员身份运行的，
//! 所以 `enable()` 不需要额外提权，失败时把退出码报给上层提示即可。
//!
//! 调用 `schtasks.exe` 一律走 `CreateProcessW` + `CREATE_NO_WINDOW`：不用 `ShellExecute`
//! （会过 shell、可能弹窗），也不新建控制台（不会闪黑框）。命令行拼装与引号转义是纯函数
//! （[`quote_arg`]、[`create_args`]、[`build_command_line`]），单测在任意平台上都不会真的
//! 执行 `schtasks`、也不会碰注册表。
//!
//! [`migrate_legacy_run_value`] 负责把旧 egui 版写在
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下的 `Switcheroo` 值迁到计划任务：
//! 建任务成功后才删旧值，失败就保留旧值（宁可开机时多弹一次 UAC，也别让自启凭空消失）。
//!
//! 写进任务的 exe 路径一律取 [`canonical_exe_path`] 展开后的**长路径**，不再直接用
//! `current_exe()`：进程从 8.3 短名目录里被拉起时，`current_exe()` 回的是
//! `C:\Users\chromechen\ALWAYS~1\…\SWITCH~1\switcheroo-win.exe` 这种短名，短名编号会随目录
//! 内容重排，卷关掉 8.3 名称生成后更会直接失效，任务就此断链。机器上已经落下的旧任务由
//! [`reconcile_task_target`] 校正：读任务 XML 比对目标路径，不一致就 `/create … /f` 覆盖一次，
//! 一致时零副作用，所以每次启动都能调。

// 引号规则、参数拼装这些纯逻辑在非 Windows 上没有调用方，但单测要在 Linux 上跑，
// 所以整体允许未使用告警；Win32 代码本身仍由 cfg(windows) 隔离。
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

/// 计划任务名：`schtasks` 的 `/tn`，任务计划程序里显示的就是它。
const TASK_NAME: &str = "Switcheroo";

/// 旧版（HKCU Run 键）用的值名，迁移时按这个名字找旧值。
const VALUE_NAME: &str = "Switcheroo";

/// 旧版自启写入的 Run 键路径（相对 `HKEY_CURRENT_USER`）。
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// `%SystemRoot%` 读不到时的兜底值。
const FALLBACK_SYSTEM_ROOT: &str = r"C:\Windows";

/// 参数：`arg` 命令行里的一个参数
/// 核心流程：按 Windows 的 `CommandLineToArgvW` 规则无条件给参数加一对双引号——
/// 参数内部的 `"` 变成 `\"`（它前面的反斜杠要翻倍），参数结尾的反斜杠也要翻倍，
/// 否则会被当成「转义收尾引号」的反斜杠，把引号吃掉。纯逻辑，可跨平台单测。
pub(crate) fn quote_arg_always(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                // 引号前的反斜杠翻倍，再多一个用来转义这个引号本身。
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    // 收尾引号前的反斜杠同样要翻倍。
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// 参数：`arg` 命令行里的一个参数
/// 核心流程：只在需要时加引号的版本——空串、含空格、制表符或双引号时转交
/// [`quote_arg_always`]，其余原样返回（`/create` 这类开关不加引号更好读）。
pub(crate) fn quote_arg(arg: &str) -> String {
    let needs_quotes = arg.is_empty() || arg.chars().any(|ch| matches!(ch, ' ' | '\t' | '"'));
    if needs_quotes {
        quote_arg_always(arg)
    } else {
        arg.to_owned()
    }
}

/// 参数：`exe` 可执行文件路径
/// 核心流程：生成计划任务「操作」里那条命令行，**总是**加一对双引号。路径含空格
/// （`C:\Program Files\…`）时不加引号会被任务计划程序拆成「程序 + 参数」，登录时直接报
/// 「找不到文件」；路径含中文不需要引号，但统一加引号更省心。纯逻辑，可跨平台单测。
pub(crate) fn quote_exe_path(exe: &Path) -> String {
    quote_arg_always(&exe.to_string_lossy())
}

/// 参数：`system_root` `%SystemRoot%` 的值
/// 核心流程：拼出 `schtasks.exe` 的全路径（`<SystemRoot>\System32\schtasks.exe`）。
/// 用全路径而不是裸名字：登录时 PATH 可能不完整，靠 CreateProcessW 的搜索顺序不可靠。
/// 末尾多写的反斜杠先裁掉，避免出现 `C:\Windows\\System32`。纯逻辑，可跨平台单测。
pub(crate) fn schtasks_path(system_root: &str) -> String {
    let root = system_root.trim_end_matches(['\\', '/']);
    format!(r"{root}\System32\schtasks.exe")
}

/// 参数：`exe` 可执行文件路径
/// 核心流程：`schtasks /create` 的参数（不含程序自身）：任务名 `Switcheroo`、`/tr` 指向
/// 带引号的 exe 全路径、`/sc onlogon` 登录时触发、`/rl HIGHEST` 最高权限运行、
/// `/f` 覆盖同名任务（保证重复调用幂等）。纯逻辑，可跨平台单测。
pub(crate) fn create_args(exe: &Path) -> Vec<String> {
    vec![
        "/create".to_owned(),
        "/tn".to_owned(),
        TASK_NAME.to_owned(),
        "/tr".to_owned(),
        quote_exe_path(exe),
        "/sc".to_owned(),
        "onlogon".to_owned(),
        "/rl".to_owned(),
        "HIGHEST".to_owned(),
        "/f".to_owned(),
    ]
}

/// 参数：无
/// 核心流程：`schtasks /delete` 的参数：删掉 `Switcheroo` 任务，`/f` 免确认。纯逻辑。
pub(crate) fn delete_args() -> Vec<String> {
    vec![
        "/delete".to_owned(),
        "/tn".to_owned(),
        TASK_NAME.to_owned(),
        "/f".to_owned(),
    ]
}

/// 参数：无
/// 核心流程：`schtasks /query` 的参数：只查 `Switcheroo` 任务，存在时退出码 0。纯逻辑。
pub(crate) fn query_args() -> Vec<String> {
    vec!["/query".to_owned(), "/tn".to_owned(), TASK_NAME.to_owned()]
}

/// 参数：无
/// 核心流程：`schtasks /query … /xml` 的参数：跟 [`query_args`] 一样只查 `Switcheroo`，
/// 额外的 `/xml` 让 schtasks 把任务定义（含「操作」里的 exe 路径）打成 XML 吐到 stdout，
/// 供 [`reconcile_task_target`] 比对。纯逻辑，可跨平台单测。
pub(crate) fn query_xml_args() -> Vec<String> {
    vec![
        "/query".to_owned(),
        "/tn".to_owned(),
        TASK_NAME.to_owned(),
        "/xml".to_owned(),
    ]
}

/// 参数：`bytes` `schtasks /query … /xml` 的原始 stdout 字节
/// 核心流程：把 XML 解码成 Rust 字符串。**不走控制台的代码页**——我们把 stdout 收成字节
/// 再用 BOM / 内容特征判断编码：`FF FE` 之后按 UTF-16LE 解，`FE FF` 之后按 UTF-16BE 解，
/// `EF BB BF` 之后按 UTF-8 解；没有 BOM 时看「偶数位（0 起算的奇数下标）字节是否全为零」，
/// 是就按 UTF-16LE 解（ASCII 为主的 XML 落在 UTF-16LE 里就是这个特征），否则按 UTF-8 解。
/// 这样无论 schtasks 吐带 BOM 的 UTF-16 还是 UTF-8，中文路径都不会被解成乱码。纯逻辑，
/// 可跨平台单测。
pub(crate) fn decode_task_xml(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16_to_string(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16_to_string(rest, u16::from_be_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }

    let pairs = bytes.len() / 2;
    let high_zero = bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
    if pairs > 0 && high_zero * 2 >= pairs {
        return utf16_to_string(bytes, u16::from_le_bytes);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// 参数：`bytes` UTF-16 的字节序列（已去掉 BOM），`from_bytes` 按字节序把两个字节拼成 `u16`
/// 核心流程：两字节一组拼成 `u16` → `from_utf16_lossy` 容忍坏代理对 → 裁掉结尾的 NUL
/// （重定向到管道/文件时常常带着），避免比较时多出一个 `\0`。
fn utf16_to_string(bytes: &[u8], from_bytes: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| from_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_owned()
}

/// 参数：`text` XML 文本
/// 核心流程：把任务 XML 里会用到的 5 个预定义实体还原成字符。`&amp;` 必须放最后处理，
/// 否则 `&amp;quot;` 会被二次还原成一个引号。纯逻辑，可跨平台单测。
pub(crate) fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// 参数：`path` 任务动作里或本进程的 exe 路径文本
/// 核心流程：比较前统一写法——去掉首尾空白、去掉包裹的双引号（`/tr` 带引号，落进 XML 的
/// 可能没有）、去掉结尾多余的 `\`、再整体小写化。纯逻辑，可跨平台单测。
pub(crate) fn normalize_exe_text(path: &str) -> String {
    path.trim()
        .trim_matches('"')
        .trim()
        .trim_end_matches('\\')
        .to_lowercase()
}

/// 参数：`xml` 任务 XML（`None` 表示没取到），`canonical` 本进程 exe 的长路径
/// 核心流程：判定「任务动作里的 exe 是否已经是当前长路径」——先把 XML 里的实体还原、整体
/// 小写化，再看是否**包含**规范化后的长路径。用小写化包含而不是逐字符相等：路径大小写、
/// 双引号有无、XML 里 `<Command>` 之外多出来的换行都不影响判定；而存储的是 8.3 短名时
/// （`ALWAYS~1` vs `AlwaysOnTop`）文本上就对不上，正好判成「不一致」去覆盖一次。
/// 取不到 XML 一律判成不一致（宁可多覆盖一次也不要漏），`canonical` 为空时同理。
/// 纯逻辑，可跨平台单测。
pub(crate) fn task_target_matches(xml: Option<&str>, canonical: &Path) -> bool {
    let Some(xml) = xml else {
        return false;
    };
    let needle = normalize_exe_text(&canonical.to_string_lossy());
    if needle.is_empty() {
        return false;
    }
    unescape_xml(xml).to_lowercase().contains(&needle)
}

/// 参数：`program` 可执行文件全路径，`args` 它的参数
/// 核心流程：拼成 `CreateProcessW` 的 `lpCommandLine`——程序路径和每个参数各自按
/// [`quote_arg`] 转义，再用单个空格连接。这条命令行走完 CreateProcess 的解析后正好还原成
/// 「程序 + 原样参数」，`/tr` 的值因此能带上嵌套引号。纯逻辑，可跨平台单测。
pub(crate) fn build_command_line(program: &str, args: &[String]) -> String {
    let mut line = quote_arg(program);
    for arg in args {
        line.push(' ');
        line.push_str(&quote_arg(arg));
    }
    line
}

/// 参数：`text` 任意 Rust 字符串
/// 核心流程：转成 Win32 需要的「以 NUL 结尾的 UTF-16」缓冲区。
#[cfg(windows)]
fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 参数：无
/// 核心流程：取 `schtasks.exe` 的全路径——先读环境变量 `SystemRoot`（正常一定有），
/// 读不到或为空时退回 [`FALLBACK_SYSTEM_ROOT`]，再交给 [`schtasks_path`] 拼出全路径。
#[cfg(windows)]
fn schtasks_exe() -> String {
    let root = std::env::var("SystemRoot")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| FALLBACK_SYSTEM_ROOT.to_owned());
    schtasks_path(&root)
}

/// 参数：`program` 要启动的程序全路径，`args` 它的参数
/// 核心流程：`CreateProcessW`（`lpApplicationName` 给全路径，不依赖搜索路径）+
/// `CREATE_NO_WINDOW`（无窗口、不弹窗）启动 → `WaitForSingleObject(INFINITE)` 等它跑完 →
/// `GetExitCodeProcess` 取退出码 → 两个句柄都 `CloseHandle`。关键：不继承句柄、
/// 不用 `ShellExecute`，整个过程不会出现可见窗口。
#[cfg(windows)]
fn run_quiet(program: &str, args: &[String]) -> Result<u32, String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        CreateProcessW, GetExitCodeProcess, WaitForSingleObject, CREATE_NO_WINDOW, INFINITE,
        PROCESS_INFORMATION, STARTUPINFOW,
    };

    let command_line = build_command_line(program, args);
    // lpCommandLine 要求可写缓冲：CreateProcessW 可能就地修改它，所以用 Vec<u16> 而非字面量。
    let mut command_line_wide = to_wide(&command_line);
    let application = to_wide(program);
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();

    // SAFETY: application / command_line_wide 都是以 NUL 结尾、本次调用期间有效的宽字符缓冲；
    // startup 的 cb 已按结构体大小填好；process 是栈上变量，用来接收新进程的句柄。
    // 成功返回的两个句柄由调用方负责关闭，下面统一 CloseHandle。
    let created = unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command_line_wide.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            None,
            &startup,
            &mut process,
        )
    };
    if let Err(err) = created {
        return Err(format!("启动 {program} 失败：{err}"));
    }

    // SAFETY: process.hProcess 是上面刚创建、仍有效的进程句柄；INFINITE 表示一直等到退出。
    let waited = unsafe { WaitForSingleObject(process.hProcess, INFINITE) };
    let mut code = 1u32;
    // SAFETY: 同上的有效进程句柄；code 是栈上变量，成功时被填入退出码。
    let exit_code = unsafe { GetExitCodeProcess(process.hProcess, &mut code) };
    // SAFETY: 两个句柄都只在这里关闭一次，之后不再使用。
    unsafe {
        let _ = CloseHandle(process.hThread);
        let _ = CloseHandle(process.hProcess);
    }

    if waited != WAIT_OBJECT_0 {
        return Err(format!("等待 {program} 退出失败，等待结果 {}", waited.0));
    }
    exit_code.map_err(|err| format!("读取 {program} 退出码失败：{err}"))?;
    log::debug!("{command_line} → 退出码 {code}");
    Ok(code)
}

/// 参数：`program` 要启动的程序全路径，`args` 它的参数
/// 核心流程：跟 [`run_quiet`] 同一套约束（`CreateProcessW` + `CREATE_NO_WINDOW`、不新建
/// 控制台、不弹窗），差别是要把 stdout 收回来读 XML——`CreateProcessW` 帮不上忙，只能自己
/// 接管子进程的 stdout：建一个「本进程专属、名字带 pid」的临时文件，把它的句柄标成可继承后
/// 用 `STARTF_USESTDHANDLES` 塞给子进程当 stdout/stderr，`WaitForSingleObject(INFINITE)`
/// 等它退出 → 按原样读回文件字节 → 删临时文件。转成临时文件而不是管道：`schtasks /xml`
/// 的输出是 UTF-16LE，落在文件里一个字节都不会被控制台代码页转过手，[`decode_task_xml`]
/// 再按 BOM / 内容特征判断编码。退出码非 0 作为 `Err` 返回。
#[cfg(windows)]
fn run_capture(program: &str, args: &[String]) -> Result<Vec<u8>, String> {
    use std::os::windows::io::AsRawHandle;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0,
    };
    use windows::Win32::System::Threading::{
        CreateProcessW, GetExitCodeProcess, WaitForSingleObject, CREATE_NO_WINDOW, INFINITE,
        PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
    };

    let out_path = std::env::temp_dir().join(format!("switcheroo-task-{}.xml", std::process::id()));
    let capture = (|| -> Result<Vec<u8>, String> {
        let out = std::fs::File::create(&out_path)
            .map_err(|err| format!("创建临时文件 {} 失败：{err}", out_path.display()))?;
        let out_handle = HANDLE(out.as_raw_handle());
        // SAFETY: out_handle 是上面刚创建、仍有效的文件句柄；只把「可继承」这一位打开，
        // 子进程才拿得到它。掩码与置位都是 HANDLE_FLAG_INHERIT。
        unsafe { SetHandleInformation(out_handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
            .map_err(|err| format!("设置临时文件句柄可继承失败：{err}"))?;

        let command_line = build_command_line(program, args);
        // lpCommandLine 要求可写缓冲：CreateProcessW 可能就地修改它。
        let mut command_line_wide = to_wide(&command_line);
        let application = to_wide(program);
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            dwFlags: STARTF_USESTDHANDLES,
            // schtasks 不读 stdin；三个句柄给同一个可写文件，既满足「都要有效且可继承」，
            // 又不用额外开 NUL 设备。stderr 也并进同一个文件，出错信息同样能读回来。
            hStdInput: out_handle,
            hStdOutput: out_handle,
            hStdError: out_handle,
            ..Default::default()
        };
        let mut process = PROCESS_INFORMATION::default();

        // SAFETY: application / command_line_wide 都是以 NUL 结尾、本次调用期间有效的宽字符
        // 缓冲；bInheritHandles = true 是为了把上面那个临时文件句柄交给子进程，除此之外不
        // 继承任何句柄；startup 的 cb 已按结构体大小填好；process 是栈上变量接收新进程句柄。
        let created = unsafe {
            CreateProcessW(
                PCWSTR(application.as_ptr()),
                Some(PWSTR(command_line_wide.as_mut_ptr())),
                None,
                None,
                true,
                CREATE_NO_WINDOW,
                None,
                None,
                &startup,
                &mut process,
            )
        };
        if let Err(err) = created {
            return Err(format!("启动 {program} 失败：{err}"));
        }

        // SAFETY: process.hProcess 是上面刚创建、仍有效的进程句柄；INFINITE 表示一直等到退出。
        let waited = unsafe { WaitForSingleObject(process.hProcess, INFINITE) };
        let mut code = 1u32;
        // SAFETY: 同上的有效进程句柄；code 是栈上变量，成功时被填入退出码。
        let exit_code = unsafe { GetExitCodeProcess(process.hProcess, &mut code) };
        // SAFETY: 两个句柄都只在这里关闭一次，之后不再使用。
        unsafe {
            let _ = CloseHandle(process.hThread);
            let _ = CloseHandle(process.hProcess);
        }
        if waited != WAIT_OBJECT_0 {
            return Err(format!("等待 {program} 退出失败，等待结果 {}", waited.0));
        }
        exit_code.map_err(|err| format!("读取 {program} 退出码失败：{err}"))?;

        // 先关掉我们这一侧的写句柄，再读文件：子进程已经退出，数据早就落到文件里了。
        drop(out);
        let bytes = std::fs::read(&out_path)
            .map_err(|err| format!("读取临时文件 {} 失败：{err}", out_path.display()))?;
        if code != 0 {
            return Err(format!("{program} 退出码 {code}"));
        }
        Ok(bytes)
    })();

    // 无论成败都清掉临时文件，%TEMP% 里不留垃圾。
    let _ = std::fs::remove_file(&out_path);
    capture
}

/// 参数：`path` 8.3 短路径或任意路径
/// 核心流程：用 `GetLongPathNameW` 把短名展开成长名——先传 0 长度缓冲区探所需字符数（含
/// 结尾 NUL），再按这个长度分配缓冲取一次，按返回的实际字符数（不含 NUL）截断。卷没启用
/// 8.3、路径不存在、两次调用之间目录结构变化（返回值超过缓冲区）都返回 `None`，由调用方
/// 退回短路径。
#[cfg(windows)]
fn long_path(path: &Path) -> Option<PathBuf> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetLongPathNameW;

    let short = to_wide(&path.to_string_lossy());
    // SAFETY: short 是以 NUL 结尾、本次调用期间有效的宽字符缓冲；缓冲区给 None 时
    // GetLongPathNameW 只回填展开所需的字符数（含结尾 NUL），不写任何内存。
    let needed = unsafe { GetLongPathNameW(PCWSTR(short.as_ptr()), None) };
    if needed == 0 {
        return None;
    }

    let mut buffer = vec![0u16; needed as usize];
    // SAFETY: buffer 的长度正好是上面探到的 needed，作为 lpszlongpath 的可写缓冲；
    // 返回值 written 不含结尾 NUL，可能因路径变长而超过缓冲区（此时按失败处理）。
    let written = unsafe { GetLongPathNameW(PCWSTR(short.as_ptr()), Some(&mut buffer)) };
    if written == 0 || written as usize >= buffer.len() {
        return None;
    }
    let long = String::from_utf16_lossy(&buffer[..written as usize]);
    log::debug!("开机自启：{} → {}", path.display(), long);
    Some(PathBuf::from(long))
}

/// 参数：无
/// 核心流程：取本进程 exe 的**长路径**——`current_exe` 拿到的可能带 8.3 短名（从短名目录
/// 拉起时就会），交给 [`long_path`] 用 `GetLongPathNameW` 展开；取路径失败返回空
/// `PathBuf` 并 `warn`，展开失败退回短路径并 `warn`（宁可沿用旧行为，也不要让自启功能
/// 因为一次 Win32 调用失败而彻底不可用）。
#[cfg(windows)]
pub fn canonical_exe_path() -> PathBuf {
    let short = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            log::warn!("开机自启：取当前可执行文件路径失败：{err}");
            return PathBuf::new();
        }
    };
    match long_path(&short) {
        Some(long) => long,
        None => {
            log::warn!(
                "开机自启：GetLongPathNameW 展开 8.3 短名失败，退回短路径 {}",
                short.display()
            );
            short
        }
    }
}

/// 参数：无
/// 核心流程：跑一次 `schtasks /query /tn Switcheroo /xml`，把 stdout 字节按
/// [`decode_task_xml`] 解码成字符串。任务不存在 / schtasks 启动失败 / 退出码非 0 都作为
/// `Err` 返回，由 [`reconcile_task_target`] 决定怎么兜底。
#[cfg(windows)]
fn query_task_xml() -> Result<String, String> {
    let bytes = run_capture(&schtasks_exe(), &query_xml_args())?;
    Ok(decode_task_xml(&bytes))
}

/// 参数：无
/// 核心流程：跑一次 `schtasks /query /tn Switcheroo`，退出码 0 即「任务存在 = 已启用」。
/// 关键：启动失败（比如 schtasks.exe 缺失）或退出码非 0 都按「未启用」处理——托盘勾选状态
/// 宁可少勾一次，也不要谎报已启用。
#[cfg(windows)]
pub fn is_enabled() -> bool {
    match run_quiet(&schtasks_exe(), &query_args()) {
        Ok(0) => true,
        Ok(code) => {
            log::debug!("开机自启：任务 {TASK_NAME} 不存在，退出码 {code}");
            false
        }
        Err(err) => {
            log::debug!("开机自启：查询任务失败，视为未启用：{err}");
            false
        }
    }
}

/// 参数：无
/// 核心流程：[`canonical_exe_path`] 取本进程 exe 的**长路径**（8.3 短名展开成中文长名）→
/// 拼 `schtasks /create … /rl HIGHEST /f` → 静默执行并等它退出 → 退出码非 0 就把退出码写进
/// `Err` 返回，上层拿去提示用户。`/f` 让重复开启不会因为「任务已存在」报错。
/// 路径取不到（空串）直接返回 `Err`，不拿空路径去建一个必定失败的任务。
#[cfg(windows)]
pub fn enable() -> Result<(), String> {
    let exe = canonical_exe_path();
    if exe.as_os_str().is_empty() {
        return Err("取当前可执行文件路径失败".to_owned());
    }
    let schtasks = schtasks_exe();
    let args = create_args(&exe);
    let code = run_quiet(&schtasks, &args)?;
    if code == 0 {
        log::info!(
            "开机自启已写入计划任务 {TASK_NAME}：{}",
            quote_exe_path(&exe)
        );
        Ok(())
    } else {
        let reason = format!("创建计划任务 {TASK_NAME} 失败，schtasks 退出码 {code}");
        log::warn!("开机自启：{reason}");
        Err(reason)
    }
}

/// 参数：无
/// 核心流程：先 `is_enabled()` 问一下任务在不在 → 不在就直接算成功（「关掉自启」的目标
/// 状态已经达成，重复点击不该报错）→ 在就 `schtasks /delete /tn Switcheroo /f` 删掉，
/// 退出码非 0 作为 `Err` 返回。
#[cfg(windows)]
pub fn disable() -> Result<(), String> {
    if !is_enabled() {
        log::debug!("开机自启：任务 {TASK_NAME} 本来就不存在，视为已关闭");
        return Ok(());
    }

    let schtasks = schtasks_exe();
    let code = run_quiet(&schtasks, &delete_args())?;
    if code == 0 {
        log::info!("开机自启已删除计划任务 {TASK_NAME}");
        Ok(())
    } else {
        let reason = format!("删除计划任务 {TASK_NAME} 失败，schtasks 退出码 {code}");
        log::warn!("开机自启：{reason}");
        Err(reason)
    }
}

/// 参数：无
/// 核心流程：以 `KEY_READ` 打开 HKCU 下的 Run 键，用 `RegQueryValueExW` 只查大小
/// （`lpData` 传空）判断旧版 `Switcheroo` 值是否存在。打不开键（说明这台机器上没有旧版写过
/// 的值或键被策略禁用）和值不存在都返回 `Ok(false)`——迁移只关心「有没有旧值」这一个事实。
#[cfg(windows)]
fn legacy_run_value_exists() -> Result<bool, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        REG_VALUE_TYPE,
    };

    let key_path = to_wide(RUN_KEY);
    let name = to_wide(VALUE_NAME);
    let mut key = HKEY::default();

    // SAFETY: RegOpenKeyExW 打开 HKCU 下一个子键，只读权限；key_path 是以 NUL 结尾的宽字符
    // 缓冲，key 是栈上变量用来接收句柄。成功时必须 RegCloseKey。
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            None,
            KEY_READ,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        log::debug!(
            "开机自启迁移：打不开 Run 键，视为没有旧值，错误码 {}",
            status.0
        );
        return Ok(false);
    }

    let mut kind = REG_VALUE_TYPE::default();
    let mut size = 0u32;
    // SAFETY: lpData 传空、lpcbData 非空时 RegQueryValueExW 只回填数据大小并返回
    // ERROR_SUCCESS——正好用来判断「值存在与否」，不读也不复制值内容。
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };

    // SAFETY: key 是上面成功打开的句柄，这里关闭一次，之后不再使用。
    unsafe {
        let _ = RegCloseKey(key);
    }

    if status == ERROR_SUCCESS {
        Ok(true)
    } else if status == ERROR_FILE_NOT_FOUND {
        Ok(false)
    } else {
        Err(format!("查询旧版 Run 值失败，错误码 {}", status.0))
    }
}

/// 参数：无
/// 核心流程：以 `KEY_SET_VALUE` 打开 Run 键 → `RegDeleteValueW` 删掉旧的 `Switcheroo` 值。
/// 值本来就不存在（`ERROR_FILE_NOT_FOUND`）也算成功——迁移的目标状态已经达成。
#[cfg(windows)]
fn delete_legacy_run_value() -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
    };

    let key_path = to_wide(RUN_KEY);
    let name = to_wide(VALUE_NAME);
    let mut key = HKEY::default();

    // SAFETY: 以 KEY_SET_VALUE 打开 HKCU 下的 Run 键；key_path 是以 NUL 结尾的宽字符缓冲。
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            None,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!("打不开 Run 键，错误码 {}", status.0));
    }

    // SAFETY: 删除已打开键下的一个值；name 是以 NUL 结尾的宽字符值名，不存在的值会返回
    // ERROR_FILE_NOT_FOUND，这不是错误。
    let status = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };

    // SAFETY: key 是上面成功打开的句柄，在这里关闭一次。
    unsafe {
        let _ = RegCloseKey(key);
    }

    if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("删除旧版 Run 值失败，错误码 {}", status.0))
    }
}

/// 参数：无
/// 核心流程：旧 egui 版把自启写在 HKCU Run 键的 `Switcheroo` 值里，迁到计划任务分三步——
/// 查旧值（没有就直接返回，**无副作用**，所以每次启动都能调）→ 建计划任务（[`enable`]，
/// `/f` 保证幂等）→ 删旧值。顺序不能反：先建任务再删旧值，中途失败时旧值还在，自启不会
/// 凭空消失；只有建任务成功才动注册表。建任务失败（比如进程没提权）就返回 `Err` 并保留旧值，
/// 由上层决定只记日志还是提示用户。
#[cfg(windows)]
pub fn migrate_legacy_run_value() -> Result<(), String> {
    if !legacy_run_value_exists()? {
        log::debug!("开机自启迁移：没有旧版 Run 值，跳过");
        return Ok(());
    }

    enable()?;
    delete_legacy_run_value()?;
    log::info!("开机自启已从 HKCU Run 迁移到计划任务 {TASK_NAME}");
    Ok(())
}

/// 参数：无
/// 核心流程：修掉「任务里存着 8.3 短路径」的历史包袱，三步——
/// ① [`is_enabled`] 问任务在不在，不在就返回 `Ok(())`（没开自启就没东西可校正，无副作用）；
/// ② [`query_task_xml`] 读任务 XML（取不到就算不一致，宁可多覆盖一次也别漏）、
/// [`task_target_matches`] 把里面的 exe 路径跟 [`canonical_exe_path`] 比对，一致就返回
/// `Ok(())`（零副作用，所以每次启动都能调）；
/// ③ 不一致就 [`enable`]（`/create … /f`）用当前长路径覆盖一次任务，并 `info` 记下「自启任务
/// 目标已校正」。覆盖失败把 `Err` 交给上层；失败时旧任务还在，自启不会因此消失。
#[cfg(windows)]
pub fn reconcile_task_target() -> Result<(), String> {
    if !is_enabled() {
        log::debug!("自启任务目标校正：任务 {TASK_NAME} 不存在，跳过");
        return Ok(());
    }

    let canonical = canonical_exe_path();
    let xml = match query_task_xml() {
        Ok(xml) => Some(xml),
        Err(err) => {
            log::debug!("自启任务目标校正：读任务 XML 失败（{err}），按不一致处理");
            None
        }
    };
    if task_target_matches(xml.as_deref(), &canonical) {
        log::debug!(
            "自启任务目标校正：任务 {TASK_NAME} 已指向 {}，无需改动",
            canonical.display()
        );
        return Ok(());
    }

    log::warn!(
        "自启任务目标校正：任务 {TASK_NAME} 的目标与当前长路径 {} 不一致，覆盖写回",
        canonical.display()
    );
    enable()?;
    log::info!("自启任务目标已校正：{TASK_NAME} → {}", canonical.display());
    Ok(())
}

/// 参数：无
/// 核心流程：非 Windows 平台没有计划任务，恒返回 `false`。
#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    false
}

/// 参数：无
/// 核心流程：非 Windows 平台不支持开机自启，返回带平台名的错误描述，由上层决定是否提示。
#[cfg(not(windows))]
pub fn enable() -> Result<(), String> {
    Err(format!("{} 平台暂不支持开机自启", std::env::consts::OS))
}

/// 参数：无
/// 核心流程：同 [`enable`]，非 Windows 平台直接返回「不支持」。
#[cfg(not(windows))]
pub fn disable() -> Result<(), String> {
    Err(format!("{} 平台暂不支持开机自启", std::env::consts::OS))
}

/// 参数：无
/// 核心流程：非 Windows 平台既没有 Run 键也没有计划任务，没有可迁移的东西，恒 `Ok(())`
/// ——调用方（启动流程）不必区分平台，重复调用也没有副作用。
#[cfg(not(windows))]
pub fn migrate_legacy_run_value() -> Result<(), String> {
    Ok(())
}

/// 参数：无
/// 核心流程：非 Windows 平台没有 8.3 短名可展开，直接返回 `current_exe()` 的原路径
/// （取不到就空 `PathBuf`），保证上层代码与单测在任意平台都能编译运行。
#[cfg(not(windows))]
pub fn canonical_exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_default()
}

/// 参数：无
/// 核心流程：非 Windows 平台没有计划任务，没有可校正的目标，恒 `Ok(())`——调用方
/// （启动流程）不必区分平台，重复调用也没有副作用。
#[cfg(not(windows))]
pub fn reconcile_task_target() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `%SystemRoot%` 拼接：末尾多写的反斜杠要裁掉，避免 `C:\Windows\\System32`。
    /// 单测只比对字符串，不会真的执行 schtasks。
    #[test]
    fn schtasks_path_is_absolute_and_normalized() {
        assert_eq!(
            schtasks_path(r"C:\Windows"),
            r"C:\Windows\System32\schtasks.exe"
        );
        assert_eq!(
            schtasks_path(r"C:\Windows\"),
            r"C:\Windows\System32\schtasks.exe"
        );
    }

    /// 普通路径（无空格、无引号）：任务动作总是带一对引号，外层 `/tr` 参数里这对引号
    /// 要转义成 `\"`——两层引号少一层，登录时任务就会报「找不到文件」。
    #[test]
    fn create_command_line_with_plain_path() {
        let exe = Path::new(r"C:\Tools\Switcheroo\switcheroo.exe");
        assert_eq!(
            quote_exe_path(exe),
            r#""C:\Tools\Switcheroo\switcheroo.exe""#
        );
        assert_eq!(
            build_command_line(r"C:\Windows\System32\schtasks.exe", &create_args(exe)),
            concat!(
                r"C:\Windows\System32\schtasks.exe",
                r#" /create /tn Switcheroo"#,
                r#" /tr "\"C:\Tools\Switcheroo\switcheroo.exe\"""#,
                r" /sc onlogon /rl HIGHEST /f",
            )
        );
    }

    /// 路径含空格：必须有引号，否则任务计划程序会把 `C:\Program` 当程序、`Files\…` 当参数。
    #[test]
    fn create_command_line_with_spaces() {
        let exe = Path::new(r"C:\Program Files\Switcheroo\switcheroo.exe");
        let args = create_args(exe);
        assert_eq!(
            args,
            vec![
                "/create",
                "/tn",
                "Switcheroo",
                "/tr",
                r#""C:\Program Files\Switcheroo\switcheroo.exe""#,
                "/sc",
                "onlogon",
                "/rl",
                "HIGHEST",
                "/f",
            ]
        );
        assert_eq!(
            build_command_line(r"C:\Windows\System32\schtasks.exe", &args),
            concat!(
                r"C:\Windows\System32\schtasks.exe",
                r#" /create /tn Switcheroo"#,
                r#" /tr "\"C:\Program Files\Switcheroo\switcheroo.exe\"""#,
                r" /sc onlogon /rl HIGHEST /f",
            )
        );
    }

    /// 路径含中文：命令行最终按 UTF-16 传给 CreateProcessW，纯函数这层只保证原样保留、
    /// 不被截断或转码。
    #[test]
    fn create_command_line_with_chinese_path() {
        let exe = Path::new(r"D:\我的工具\切换器\switcheroo.exe");
        assert_eq!(
            build_command_line(r"C:\Windows\System32\schtasks.exe", &create_args(exe)),
            concat!(
                r"C:\Windows\System32\schtasks.exe",
                r#" /create /tn Switcheroo"#,
                r#" /tr "\"D:\我的工具\切换器\switcheroo.exe\"""#,
                r" /sc onlogon /rl HIGHEST /f",
            )
        );
    }

    /// 路径以反斜杠结尾：收尾反斜杠必须翻倍，否则它会转义掉任务的收尾引号
    /// （`"D:\dir\"` 里的 `\"` 会被解析成字面引号，路径反而多一个字符）。
    #[test]
    fn quote_exe_path_doubles_trailing_backslash() {
        let exe = Path::new(r"D:\dir\");
        assert_eq!(quote_exe_path(exe), r#""D:\dir\\""#);
        assert_eq!(
            build_command_line(r"C:\Windows\System32\schtasks.exe", &create_args(exe)),
            concat!(
                r"C:\Windows\System32\schtasks.exe",
                r#" /create /tn Switcheroo"#,
                r#" /tr "\"D:\dir\\\\\"""#,
                r" /sc onlogon /rl HIGHEST /f",
            )
        );
    }

    /// 路径本身含双引号（现实中非法，但拼装逻辑不能因此产出坏命令行）：第一层把路径里的
    /// 引号转义成 `\"`，第二层（作为 `/tr` 参数再转义一次）把 `\` 和 `"` 一起翻倍成 `\\\"`。
    #[test]
    fn quote_arg_escapes_embedded_quote() {
        let path = r#"C:\a"b.exe"#;
        let action = quote_arg_always(path);
        assert_eq!(action, r#""C:\a\"b.exe""#, "第一层：任务动作里的引号要转义");
        assert_eq!(
            quote_arg(&action),
            r#""\"C:\a\\\"b.exe\"""#,
            "第二层：内层已有的反斜杠要跟着引号一起翻倍"
        );
    }

    /// 开关类参数（无空格、无引号）不加引号，可读性更好；空参数则必须加引号。
    #[test]
    fn quote_arg_only_quotes_when_needed() {
        assert_eq!(quote_arg("/create"), "/create");
        assert_eq!(quote_arg("onlogon"), "onlogon");
        assert_eq!(quote_arg(""), r#""""#);
        assert_eq!(quote_arg("a b"), r#""a b""#);
    }

    /// delete / query 的命令行：固定参数，不带 exe 路径。
    #[test]
    fn delete_and_query_args() {
        assert_eq!(
            build_command_line(r"C:\Windows\System32\schtasks.exe", &delete_args()),
            r"C:\Windows\System32\schtasks.exe /delete /tn Switcheroo /f"
        );
        assert_eq!(
            build_command_line(r"C:\Windows\System32\schtasks.exe", &query_args()),
            r"C:\Windows\System32\schtasks.exe /query /tn Switcheroo"
        );
    }

    /// 非 Windows 平台必须给出确定的降级行为，不 panic，也不会去碰注册表/计划任务。
    #[cfg(not(windows))]
    #[test]
    fn autostart_is_unsupported_off_windows() {
        assert!(!is_enabled());
        assert!(enable().is_err());
        assert!(disable().is_err());
        assert!(migrate_legacy_run_value().is_ok());
        assert!(reconcile_task_target().is_ok());
    }

    /// `schtasks /query … /xml` 的命令行：`/xml` 是读任务目标的唯一手段，不能丢。
    #[test]
    fn query_xml_args_ask_for_xml() {
        assert_eq!(
            build_command_line(r"C:\Windows\System32\schtasks.exe", &query_xml_args()),
            r"C:\Windows\System32\schtasks.exe /query /tn Switcheroo /xml"
        );
    }

    /// 已落地的任务存的是展开后的长中文路径（XML 里可能被写成 `&quot;` 包着）：判定为一致，
    /// `reconcile_task_target` 此时必须零副作用，不能每次启动都去覆盖任务。
    #[test]
    fn task_target_matches_long_path_with_quotes_and_chinese() {
        let xml = concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n",
            "<Task version=\"1.2\"><Actions Context=\"Author\"><Exec>\r\n",
            "<Command>&quot;C:\\Users\\chromechen\\AlwaysOnTop\\迷你切换器\\switcheroo-win.exe&quot;",
            "</Command>\r\n<Arguments></Arguments></Exec></Actions></Task>",
        );
        let canonical = Path::new(r"C:\Users\chromechen\AlwaysOnTop\迷你切换器\switcheroo-win.exe");
        assert!(task_target_matches(Some(xml), canonical));
    }

    /// 真机缺陷复现：任务里存的是 8.3 短名（`ALWAYS~1` / `TEMP-~1` / `SWITCH~1`），当前进程
    /// 的长路径跟它在文本上必然不同——必须判成不一致，才会走 `/create … /f` 覆盖。
    #[test]
    fn task_target_matches_rejects_short_name_target() {
        let xml = concat!(
            "<Task><Actions Context=\"Author\"><Exec>",
            r"<Command>C:\Users\chromechen\ALWAYS~1\TEMP-~1\SWITCH~1\switcheroo-win.exe</Command>",
            "</Exec></Actions></Task>"
        );
        let canonical = Path::new(
            r"C:\Users\chromechen\AlwaysOnTop\Temp-Switcher\Switcheroo\switcheroo-win.exe",
        );
        assert!(!task_target_matches(Some(xml), canonical));
    }

    /// 同一个路径的大小写写法不同（任务里是全小写、磁盘上是混合大小写）不算改动：小写化比较
    /// 就是为了避免这种无意义的重建。
    #[test]
    fn task_target_matches_ignores_case_difference() {
        let xml = concat!(
            "<Task><Actions Context=\"Author\"><Exec><Command>",
            r"c:\users\chromechen\alwaysontop\switcheroo\switcheroo-win.exe",
            "</Command></Exec></Actions></Task>"
        );
        let canonical = Path::new(r"C:\Users\Chromechen\AlwaysOnTop\Switcheroo\switcheroo-win.exe");
        assert!(task_target_matches(Some(xml), canonical));
    }

    /// 取不到 XML（`None`）、或路径里带 `&` 被转义成 `&amp;`：前者按不一致处理，后者还原后
    /// 仍判一致。
    #[test]
    fn task_target_matches_handles_missing_xml_and_entities() {
        let canonical = Path::new(r"D:\Tools & Stuff\switcheroo-win.exe");
        assert!(!task_target_matches(None, canonical));
        let xml = concat!(
            "<Task><Actions Context=\"Author\"><Exec><Command>",
            r"D:\Tools &amp; Stuff\switcheroo-win.exe",
            "</Command></Exec></Actions></Task>"
        );
        assert!(task_target_matches(Some(xml), canonical));
    }

    /// XML 解码：带 BOM 的 UTF-16LE、不带 BOM 的 UTF-16LE、UTF-8 三种都要解出同一个字符串
    /// （中文不能变乱码）；这是「不依赖控制台代码页」的落点。
    #[test]
    fn decode_task_xml_handles_utf16le_and_utf8() {
        let text = r"<Task><Exec><Command>C:\我的工具\switcheroo-win.exe</Command></Exec></Task>";
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(text.encode_utf16().flat_map(u16::to_le_bytes));

        assert_eq!(decode_task_xml(&utf16), text, "带 BOM 的 UTF-16LE");
        assert_eq!(decode_task_xml(&utf16[2..]), text, "无 BOM 的 UTF-16LE");
        assert_eq!(decode_task_xml(text.as_bytes()), text, "UTF-8 原样");
    }
}
