//! Windows 顶层窗口枚举（US-036）。
//!
//! 参数：无，全部数据都是运行期由 Win32 查询出来的
//! 核心流程：`EnumWindows` 遍历所有顶层窗口 → 逐个采集 [`WindowAttributes`] →
//! 交给 core 的 [`is_alt_tab_window`] 判定是否进入切换列表 → 通过的窗口再补上
//! 标题与进程名组装成 [`WindowInfo`]。过滤规则只在 core 里写一份，本模块只负责
//! "如实采集属性"，不重复写任何排除分支。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use switcheroo_core::{is_alt_tab_window, WindowAttributes, WindowId, WindowInfo};
use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetClassNameW, GetPropW, GetWindow, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE,
    GW_OWNER, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::win32_parse::{
    cloak_type_from_prop, has_meaningful_title, process_name_from_exe_path, UNKNOWN_PROCESS_NAME,
};

/// UWP 外壳窗口的类名：它自身的进程是 ApplicationFrameHost.exe，真实应用在子窗口里。
const CLASS_APPLICATION_FRAME: &str = "ApplicationFrameWindow";

/// UWP 内核窗口类名，core 的规则 4 会据此去重。
const CLASS_UI_CORE_WINDOW: &str = "Windows.UI.Core.CoreWindow";

/// "已从任务栏移除"的属性名。
const PROP_TASKLIST_DELETED: &str = "ITaskList_Deleted";

/// cloak 类型属性名，取值 0/1/2 对应 core 规则 6。
const PROP_APP_VIEW_CLOAK_TYPE: &str = "ApplicationViewCloakType";

/// 进程名缓存有效期：同一 PID 一小时内不再重复开进程句柄。
const PROCESS_NAME_TTL: Duration = Duration::from_secs(60 * 60);

/// `QueryFullProcessImageNameW` 的输出缓冲区长度（宽字符个数）。
const EXE_PATH_BUF: usize = 1024;

/// 参数：无
/// 核心流程：枚举 + 按 Alt+Tab 口径过滤后，只保留上层需要的 [`WindowInfo`]。
pub fn snapshot() -> Vec<WindowInfo> {
    snapshot_detailed()
        .into_iter()
        .map(|(info, _attrs)| info)
        .collect()
}

/// 参数：无
/// 核心流程：用 `EnumWindows` 遍历顶层窗口，回调里采集属性并调用
/// [`is_alt_tab_window`] 判定，通过的窗口连同属性一起收集返回。
pub fn snapshot_detailed() -> Vec<(WindowInfo, WindowAttributes)> {
    let mut found: Vec<(WindowInfo, WindowAttributes)> = Vec::new();

    // SAFETY: 调 user32 的 EnumWindows 遍历顶层窗口；lp 指向栈上的 found，
    // 枚举在本次调用内同步完成并返回，回调结束后不会再被访问。
    let result =
        unsafe { EnumWindows(Some(collect_window), LPARAM(&mut found as *mut _ as isize)) };

    if let Err(err) = result {
        // 枚举失败（通常是被系统中断）时不 panic，返回已经收集到的部分结果。
        log::warn!("EnumWindows 枚举中断：{err}");
    }

    log::debug!("Alt+Tab 口径窗口 {} 个", found.len());
    found
}

/// 参数：`hwnd` EnumWindows 逐个给出的顶层窗口句柄；`lparam` 指向 `snapshot_detailed`
/// 栈上的结果 Vec
/// 核心流程：取标题与类名 → 采集属性 → core 判定 → 通过则解析进程名并 push
/// 一条 `(WindowInfo, WindowAttributes)`。始终返回 `BOOL(1)` 以继续枚举。
unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam 是 snapshot_detailed 传入的 &mut Vec，枚举期间该 Vec 一直存活，
    // 回调在枚举线程上同步执行，不存在并发别名。
    let found = unsafe { &mut *(lparam.0 as *mut Vec<(WindowInfo, WindowAttributes)>) };

    // 标题只取一次：has_title 与最终展示共用同一份 GetWindowTextW 结果。
    // has_title 按 trim 之后的文本判定（[`has_meaningful_title`]）：真机上有窗口
    // （如 msedge 的壳）标题只有空白，按原始串判空会把这种“没有可读标题”的窗口
    // 放进切换列表。判定结果交给 core 的规则 1 排除，此处不另写过滤分支。
    let title = window_title(hwnd);
    let has_title = has_meaningful_title(&title);
    let class_name = window_class_name(hwnd);
    let attrs = read_attributes(hwnd, &class_name, has_title);

    if !is_alt_tab_window(&attrs) {
        return BOOL(1);
    }

    let process_name = resolve_process_name(hwnd, &class_name);
    found.push((
        WindowInfo::new(WindowId::new(raw_handle(hwnd)), title, process_name),
        attrs,
    ));
    BOOL(1)
}

/// 参数：`hwnd` 窗口句柄
/// 核心流程：把平台句柄转成上层的 `u64`，避免 `WindowId` 接触到 Win32 类型。
/// 与 [`hwnd_from_id`] 配对，句柄与 `WindowId` 的互转只在本模块写一份。
pub(crate) fn raw_handle(hwnd: HWND) -> u64 {
    hwnd.0 as usize as u64
}

/// 参数：`id` 上层持有的窗口标识
/// 核心流程：把 `WindowId` 里的句柄数值还原成 [`HWND`]，供前台窗口查询与窗口切换
/// 重新调用 Win32；数值无效时得到空指针，由调用方的 `IsWindow` 挡掉。
pub(crate) fn hwnd_from_id(id: WindowId) -> HWND {
    HWND(id.raw() as usize as *mut core::ffi::c_void)
}

/// 参数：`hwnd` 顶层窗口句柄；`class_name` 已取到的类名；`has_title` 标题 trim 后是否非空
/// 核心流程：一次读出可见性、三个扩展样式位、owner 可见性、任务栏删除标记、
/// CoreWindow 判定与 cloak 类型，原样填进 core 的 [`WindowAttributes`]。
fn read_attributes(hwnd: HWND, class_name: &str, has_title: bool) -> WindowAttributes {
    // SAFETY: 下面全部是只读属性查询（user32 的 Get*），句柄由 EnumWindows 提供，
    // 在回调期间始终有效；它们都不分配资源，无需释放。
    unsafe {
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let owner_is_visible = GetWindow(hwnd, GW_OWNER)
            .map(|owner| IsWindowVisible(owner).as_bool())
            .unwrap_or(false);

        WindowAttributes {
            is_visible: IsWindowVisible(hwnd).as_bool(),
            has_title,
            is_appwindow: ex_style & WS_EX_APPWINDOW.0 != 0,
            is_toolwindow: ex_style & WS_EX_TOOLWINDOW.0 != 0,
            is_noactivate: ex_style & WS_EX_NOACTIVATE.0 != 0,
            owner_is_visible,
            is_tasklist_deleted: read_prop(hwnd, PROP_TASKLIST_DELETED).is_some(),
            is_core_window: class_name == CLASS_UI_CORE_WINDOW,
            cloak_type: cloak_type_from_prop(read_prop(hwnd, PROP_APP_VIEW_CLOAK_TYPE)),
        }
    }
}

/// 参数：`hwnd` 窗口句柄
/// 核心流程：先用 `GetWindowTextLengthW` 申请足够大的缓冲区，再 `GetWindowTextW`
/// 取标题；无标题或调用失败都返回空串，是否算“有标题”由 [`has_meaningful_title`]
/// 在 trim 之后判定。
fn window_title(hwnd: HWND) -> String {
    // SAFETY: GetWindowTextLengthW 只读窗口标题长度，参数是有效句柄。
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return String::new();
    }

    let mut buffer = vec![0u16; length as usize + 1];
    // SAFETY: buffer 长度至少为 length + 1，GetWindowTextW 最多写入 len-1 个
    // 宽字符并补 NUL，不会越界。
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if copied <= 0 {
        return String::new();
    }

    String::from_utf16_lossy(&buffer[..copied as usize])
}

/// 参数：`hwnd` 窗口句柄
/// 核心流程：`GetClassNameW` 写进栈上缓冲区后转成字符串，用于 CoreWindow 判定
/// 与 ApplicationFrameWindow 分支；[`crate::foreground`] 复用同一份类名查询。
pub(crate) fn window_class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: GetClassNameW 把类名写入栈上缓冲区，最多 buffer.len() 个宽字符。
    let length = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if length <= 0 {
        return String::new();
    }

    String::from_utf16_lossy(&buffer[..length as usize])
}

/// 参数：`hwnd` 窗口句柄；`key` 属性名（如 `"ITaskList_Deleted"`）
/// 核心流程：把属性名转成以 NUL 结尾的宽字符串交给 `GetPropW`；
/// 返回值为空句柄表示属性不存在，此时返回 `None`。
fn read_prop(hwnd: HWND, key: &str) -> Option<usize> {
    let wide: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: PCWSTR 指向本次调用期间有效、以 NUL 结尾的宽字符串；GetPropW 只读
    // 窗口属性表，不转移所有权。
    let value = unsafe { GetPropW(hwnd, PCWSTR(wide.as_ptr())) };

    if value.is_invalid() {
        None
    } else {
        Some(value.0 as usize)
    }
}

/// 参数：`hwnd` 窗口句柄
/// 核心流程：`GetWindowThreadProcessId` 只写回 PID，取不到时返回 0。
fn process_id(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: 只把 PID 写进栈上变量，句柄在回调期间有效。
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}

/// 参数：`hwnd` 顶层窗口句柄；`class_name` 该窗口的类名
/// 核心流程：普通窗口直接按 PID 取进程名；ApplicationFrameWindow 需要走
/// 子窗口去找真实应用进程，取不到时降级为 `"UWP"`，其余情况取不到都降级为
/// `"unknown"`。[`crate::foreground`] 复用本函数，不另写一份进程名解析。
pub(crate) fn resolve_process_name(hwnd: HWND, class_name: &str) -> String {
    let pid = process_id(hwnd);
    if pid == 0 {
        return UNKNOWN_PROCESS_NAME.to_string();
    }

    if class_name == CLASS_APPLICATION_FRAME {
        return child_process_name(hwnd, pid).unwrap_or_else(|| "UWP".to_string());
    }

    process_name_of(pid)
}

/// 子窗口遍历时在回调之间传递的上下文。
///
/// 参数：`parent_pid` 父窗口 PID，`found` 找到的进程名
/// 核心流程：由 [`child_process_name`] 放在栈上，回调只读 `parent_pid`、写 `found`。
struct ChildSearch {
    parent_pid: u32,
    found: Option<String>,
}

/// 参数：`parent` ApplicationFrameWindow 句柄；`parent_pid` 它的进程 ID
/// 核心流程：`EnumChildWindows` 遍历子窗口，回调里找到第一个 PID 与父窗口不同
/// 且能解析出进程名的子窗口就停下；一个都没有则返回 `None`。
fn child_process_name(parent: HWND, parent_pid: u32) -> Option<String> {
    let mut search = ChildSearch {
        parent_pid,
        found: None,
    };

    // SAFETY: lp 指向栈上的 search，EnumChildWindows 同步完成枚举后返回，
    // 回调不会在返回之后被调用。
    unsafe {
        // 返回值只表示"是否枚举完"，提前停止也是预期行为，因此忽略。
        let _ = EnumChildWindows(
            Some(parent),
            Some(find_child_process),
            LPARAM(&mut search as *mut _ as isize),
        );
    }

    search.found
}

/// 参数：`hwnd` 被遍历到的子窗口句柄；`lparam` 指向 [`ChildSearch`]
/// 核心流程：PID 与父窗口相同的子窗口跳过；能解析出进程名就写入上下文并返回
/// `BOOL(0)` 结束枚举，否则继续。
unsafe extern "system" fn find_child_process(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam 是 child_process_name 传入的 &mut ChildSearch，枚举期间一直存活。
    let search = unsafe { &mut *(lparam.0 as *mut ChildSearch) };

    let pid = process_id(hwnd);
    if pid == 0 || pid == search.parent_pid {
        return BOOL(1);
    }

    let name = process_name_of(pid);
    if name == UNKNOWN_PROCESS_NAME {
        return BOOL(1);
    }

    search.found = Some(name);
    // 返回 0 提前结束子窗口枚举，没必要继续遍历。
    BOOL(0)
}

/// 参数：`pid` 目标进程 ID
/// 核心流程：先查 1 小时有效的 PID 缓存；未命中则 `OpenProcess` +
/// `QueryFullProcessImageNameW` 取路径，交给 [`process_name_from_exe_path`] 削成
/// 进程名，再写回缓存。任何一步失败都降级为 `"unknown"`，不 panic。
fn process_name_of(pid: u32) -> String {
    let cache = process_name_cache();
    // 锁中毒时继续用里面的数据：别的线程 panic 不该让枚举整体失败。
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some((name, resolved_at)) = guard.get(&pid) {
        if resolved_at.elapsed() < PROCESS_NAME_TTL {
            return name.clone();
        }
    }

    let name = query_process_name(pid).unwrap_or_else(|| UNKNOWN_PROCESS_NAME.to_string());
    guard.insert(pid, (name.clone(), Instant::now()));
    name
}

/// 参数：无
/// 核心流程：进程名缓存只在首次调用时创建，之后所有线程共用同一张表。
fn process_name_cache() -> &'static Mutex<HashMap<u32, (String, Instant)>> {
    static CACHE: OnceLock<Mutex<HashMap<u32, (String, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 参数：`pid` 目标进程 ID
/// 核心流程：`OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` 打开进程 →
/// `QueryFullProcessImageNameW` 取完整路径 → 关闭句柄 → 削成进程名。
/// 句柄打开失败或查询失败都返回 `None`，由调用方降级。
fn query_process_name(pid: u32) -> Option<String> {
    query_process_image_path(pid).map(|path| process_name_from_exe_path(&path))
}

/// 参数：`hwnd` 目标窗口句柄
/// 核心流程：`GetWindowThreadProcessId` 拿 PID → 交给 [`query_process_image_path`] 取
/// exe 全路径。[`crate::icon`] 用它去可执行文件里抽图标；取不到（进程已退出、权限不足）
/// 返回 `None`，由调用方降级。
pub(crate) fn process_image_path(hwnd: HWND) -> Option<String> {
    let pid = process_id(hwnd);
    if pid == 0 {
        return None;
    }
    query_process_image_path(pid)
}

/// 参数：`pid` 目标进程 ID
/// 核心流程：`OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` → `QueryFullProcessImageNameW`
/// 取完整路径 → 无论成败都 `CloseHandle`。进程已退出或权限不足时返回 `None`。
fn query_process_image_path(pid: u32) -> Option<String> {
    // SAFETY: 只申请查询权限、不作为父进程继承句柄；失败返回 Err 而不是 UB。
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;

    let mut buffer = [0u16; EXE_PATH_BUF];
    let mut size = buffer.len() as u32;
    // SAFETY: buffer 是栈上的可写缓冲区，size 给出容量上限；调用后 size 被改写为
    // 实际写入的宽字符个数（不含结尾 NUL）。
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };

    // SAFETY: 关闭本函数自己打开、之后不再使用的进程句柄，避免句柄泄漏。
    unsafe {
        let _ = CloseHandle(process);
    }

    result.ok()?;

    // 防御异常返回值：截断到缓冲区长度，绝不让切片越界 panic。
    let length = (size as usize).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}
