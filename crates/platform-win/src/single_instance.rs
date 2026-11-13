//! 单实例互斥量（US-034）。
//!
//! 参数：无
//! 核心流程：进程启动时调用 [`SingleInstance::acquire`] 抢占一个命名互斥量，
//! 抢到即代表当前是唯一实例；返回 [`SingleInstanceError::AlreadyRunning`] 说明已有实例在跑，
//! 调用方应直接退出；返回 [`SingleInstanceError::Failed`] 说明平台调用本身失败，
//! 调用方可以 fail-open 继续启动。互斥量句柄由 [`SingleInstance`] 持有，Drop 时关闭，
//! 进程退出时系统也会回收。

use std::fmt;

/// 互斥量名：`Local\` 前缀表示只在当前登录会话内唯一，符合桌面应用的单实例语义。
#[cfg(windows)]
const MUTEX_NAME: &str = "Local\\Switcheroo-DBDE24E4-4E8B-4C7C-9B37-2A0E1C7E5A11";

/// 单实例抢占失败的原因。
///
/// 参数：`Failed` 携带底层原因（如 Win32 错误码）
/// 核心流程：把「已有实例在跑」与「平台调用失败」分开，前者应静默退出，
/// 后者允许上层 fail-open 继续启动并提示用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SingleInstanceError {
    /// 已有实例在运行。
    AlreadyRunning,
    /// 互斥量创建失败，附带原因。
    Failed(String),
}

/// 参数：`f` 格式化输出器
/// 核心流程：按变体给出可读的中文原因，`Failed` 原样输出底层原因。
impl fmt::Display for SingleInstanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "已有 Switcheroo 实例在运行"),
            Self::Failed(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for SingleInstanceError {}

/// Windows 实现：持有命名互斥量句柄。
///
/// 参数：内部字段为 `CreateMutexW` 返回的句柄，以 `isize` 保存。
/// 核心流程：只负责在 Drop 时把句柄关掉，其他逻辑都在 [`SingleInstance::acquire`]。
#[cfg(windows)]
pub struct SingleInstance(isize);

/// 非 Windows 平台的占位类型。
///
/// 参数：无。
/// 核心流程：不持有任何资源，仅保证上层代码在 Linux/macOS 上可以编译。
#[cfg(not(windows))]
pub struct SingleInstance;

#[cfg(windows)]
impl SingleInstance {
    /// 参数：无。
    /// 核心流程：建一个以 NUL 结尾的 UTF-16 互斥量名 → `CreateMutexW` 创建命名互斥量
    /// → 失败（句柄为空）时读 `GetLastError` 返回 `Failed` → 成功后紧接读取
    /// `GetLastError`，若为 `ERROR_ALREADY_EXISTS` 说明已有实例在跑，关掉刚拿到的
    /// 句柄并返回 `AlreadyRunning` → 否则把句柄存进 `SingleInstance` 返回 `Ok`。
    pub fn acquire() -> Result<SingleInstance, SingleInstanceError> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;

        let name: Vec<u16> = MUTEX_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: 调 kernel32 的 CreateMutexW 创建命名互斥量；不传安全属性（None），
        // 初始不持有；name 是栈上以 NUL 结尾的 UTF-16 缓冲区，调用期间一直有效。
        let handle = match unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) } {
            Ok(handle) => handle,
            Err(_) => {
                // SAFETY: 紧邻 CreateMutexW 失败之后读取 last-error，中间没有任何会覆盖
                // last-error 的 Win32 调用。
                let code = unsafe { GetLastError() }.0;
                log::error!("CreateMutexW 失败，错误码 {code}");
                return Err(SingleInstanceError::Failed(format!(
                    "CreateMutexW 失败，错误码 {code}"
                )));
            }
        };

        // SAFETY: 必须在 CreateMutexW 之后、任何其他可能覆盖 last-error 的调用之前读取，
        // 这里紧邻上一步，中间只有一次比较运算，不涉及 Win32 调用。
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

        if already_exists {
            // SAFETY: handle 是上一步刚由 CreateMutexW 返回的有效句柄，这里关闭一次，
            // 关闭后不再使用，不会重复关闭。
            unsafe {
                let _ = CloseHandle(handle);
            }
            log::info!("已有 Switcheroo 实例持有互斥量，本次不再启动");
            return Err(SingleInstanceError::AlreadyRunning);
        }

        log::debug!("单实例互斥量已获取");
        Ok(SingleInstance(handle.0 as isize))
    }
}

#[cfg(windows)]
impl Drop for SingleInstance {
    /// 参数：无。
    /// 核心流程：把保存的句柄还原成 `HANDLE` 并 `CloseHandle`，进程退出即释放互斥量。
    fn drop(&mut self) {
        use std::ffi::c_void;

        use windows::Win32::Foundation::{CloseHandle, HANDLE};

        // SAFETY: self.0 来自 acquire 中 CreateMutexW 成功返回、且此后从未关闭的句柄，
        // 这里在 Drop 中关闭且仅关闭一次。
        unsafe {
            let _ = CloseHandle(HANDLE(self.0 as *mut c_void));
        }
    }
}

#[cfg(not(windows))]
impl SingleInstance {
    /// 参数：无。
    /// 核心流程：非 Windows 平台没有命名互斥量，直接返回占位实例（视为抢锁成功），
    /// 以满足 Linux 下的编译与调试需求。
    pub fn acquire() -> Result<SingleInstance, SingleInstanceError> {
        log::debug!("单实例互斥量在非 Windows 平台为占位实现");
        Ok(SingleInstance)
    }
}

/// 非 Windows 平台的占位实现无资源可测，这里只验证它的返回值约定。
#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    /// 非 Windows 平台 acquire 一定返回 Ok，保证上层调用方逻辑可测。
    #[test]
    fn acquire_always_succeeds_off_windows() {
        assert!(SingleInstance::acquire().is_ok());
        assert!(SingleInstance::acquire().is_ok());
    }
}

/// 错误类型的文案与 trait 实现在所有平台都能验证。
#[cfg(test)]
mod error_tests {
    use super::*;

    /// 两个变体的 Display 都应带上可读信息。
    #[test]
    fn error_display_is_readable() {
        assert_eq!(
            SingleInstanceError::AlreadyRunning.to_string(),
            "已有 Switcheroo 实例在运行"
        );
        assert_eq!(
            SingleInstanceError::Failed("CreateMutexW 失败，错误码 5".to_string()).to_string(),
            "CreateMutexW 失败，错误码 5"
        );
    }
}
