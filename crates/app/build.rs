//! 构建脚本：把 Windows 图标资源编进 exe。
//!
//! 资源编译器按目标 ABI 分两套（见 `CARGO_CFG_TARGET_ENV`）：
//! - **gnu** 目标（`x86_64-pc-windows-gnu` 等）：gnu 用 mingw 的 `windres`，
//!   把 `assets/app.rc`（内容就一行 `1 ICON "app.ico"`）编成 COFF 目标文件
//!   `$OUT_DIR/app.res.o`，再用 `cargo:rustc-link-arg` 交给链接器；
//! - **msvc** 目标（`x86_64-pc-windows-msvc` 等）：msvc 用 `rc.exe`（Windows SDK，
//!   VS 开发者环境里在 PATH），找不到时退到 LLVM 的 `llvm-rc`，产出 `.res` 资源文件
//!   `$OUT_DIR/app.res`；MSVC 的 link.exe 直接接受 `.res` 文件，同样走
//!   `cargo:rustc-link-arg`。
//!
//! 目标平台一律读 Cargo 注入的环境变量（`CARGO_CFG_TARGET_OS`、`CARGO_CFG_TARGET_ENV`、
//! `CARGO_CFG_TARGET_ARCH`、`CARGO_MANIFEST_DIR`、`OUT_DIR`），绝不用 `cfg!(windows)`——
//! 构建脚本自己是编译到宿主平台上的，交叉编译时会判错。
//!
//! 两个环境变量可覆盖自动探测：`WINDRES`（gnu 用）与 `RC`（msvc 用，`WINDRES` 也接受）。
//! 任何"找不到编译器 / 执行失败"都只打 `cargo:warning` 并跳过，绝不 panic——
//! Linux 上的 `cargo check` / `cargo test` / CI 依赖这一点。
//! 图标嵌入成功后 exe 才有 `.rsrc` 资源节，资源管理器/任务栏才能显示正式图标。

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 参数：`target_arch` 目标 CPU 架构（来自 `CARGO_CFG_TARGET_ARCH`）
/// 核心流程：按架构推出 mingw 工具链前缀（`x86_64-w64-mingw32` 等）拼出 windres 可执行文件
/// 名；`WINDRES` 环境变量优先（方便换工具链）。架构不认识时返回 `None`，由调用方降级跳过。
/// 仅用于 gnu 目标。
fn windres_program(target_arch: &str) -> Option<String> {
    if let Ok(exe) = env::var("WINDRES") {
        let exe = exe.trim();
        if !exe.is_empty() {
            return Some(exe.to_string());
        }
    }
    let prefix = match target_arch {
        "x86_64" => "x86_64-w64-mingw32",
        "x86" | "i686" => "i686-w64-mingw32",
        "aarch64" => "aarch64-w64-mingw32",
        _ => return None,
    };
    Some(format!("{prefix}-windres"))
}

/// 参数：`program` windres 可执行文件；`assets_dir` 放着 `app.rc` 与 `app.ico` 的目录；
/// `obj_path` 要写出的 COFF 目标文件
/// 核心流程：把工作目录切到 `assets/`（这样 `.rc` 里的相对文件名 `"app.ico"` 一定找得到，
/// 不依赖 windres 版本对 include 路径的处理差异），用 `-O coff` 编出目标文件。返回
/// `Ok(())` 表示目标文件已就绪，任何失败都返回带原因的 `Err`，由 `main` 打 warning 后跳过。
fn compile_resource(program: &str, assets_dir: &Path, obj_path: &Path) -> Result<(), String> {
    let output = Command::new(program)
        .current_dir(assets_dir)
        .arg("app.rc")
        .arg("-O")
        .arg("coff")
        .arg("-o")
        .arg(obj_path)
        .output()
        .map_err(|err| format!("无法执行 {program}：{err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{program} 编译 app.rc 失败（{}）：{}",
            output.status,
            stderr.trim()
        ));
    }
    if !obj_path.is_file() {
        return Err(format!("{program} 没有产出 {}", obj_path.display()));
    }
    Ok(())
}

/// 返回 msvc 目标的资源编译器候选，按优先级从高到低：
/// 1. 环境变量 `WINDRES`（与 gnu 共用同一个"指定资源编译器"的开关）；
/// 2. 环境变量 `RC`（Windows 上更自然的名字）；
/// 3. `rc.exe`（Windows SDK / VS 开发者环境里在 PATH）；
/// 4. `llvm-rc`（LLVM 工具链自带的 rc 兼容实现）。
///
/// 空字符串的环境变量视为未设置，避免 `RC=` 这种写法把探测搞挂。
fn msvc_resource_compilers() -> Vec<String> {
    let mut candidates = Vec::new();
    for key in ["WINDRES", "RC"] {
        if let Ok(value) = env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                candidates.push(value.to_string());
            }
        }
    }
    candidates.push("rc.exe".to_string());
    candidates.push("llvm-rc".to_string());
    candidates
}

/// 参数：`program` msvc 资源编译器（`rc.exe` 或 `llvm-rc`）；`assets_dir` 放着 `app.rc`
/// 与 `app.ico` 的目录；`res_path` 要写出的 `.res` 资源文件
/// 核心流程：工作目录切到 `assets/`（保证 `.rc` 里的相对文件名 `"app.ico"` 可解析），
/// 用 `rc /nologo /fo <out.res> app.rc` 编译；`llvm-rc` 不接受 `/nologo`，单独走
/// `llvm-rc /fo <out.res> app.rc`。返回 `Ok(())` 表示 `.res` 已就绪，失败返回带原因的 `Err`。
fn compile_resource_msvc(program: &str, assets_dir: &Path, res_path: &Path) -> Result<(), String> {
    let is_llvm_rc = program.to_ascii_lowercase().contains("llvm-rc");
    let mut command = Command::new(program);
    command.current_dir(assets_dir);
    if !is_llvm_rc {
        // Windows SDK 的 rc.exe 会打一行版权横幅，/nologo 关掉它。
        command.arg("/nologo");
    }
    command.arg("/fo").arg(res_path).arg("app.rc");

    let output = command
        .output()
        .map_err(|err| format!("无法执行 {program}：{err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{program} 编译 app.rc 失败（{}）：{}",
            output.status,
            stderr.trim()
        ));
    }
    if !res_path.is_file() {
        return Err(format!("{program} 没有产出 {}", res_path.display()));
    }
    Ok(())
}

fn main() {
    // 目标平台必须读 Cargo 注入的环境变量：构建脚本自己是编译到宿主平台上的，
    // `cfg!(windows)` 在交叉编译时判的是宿主，会判错。
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        // 非 Windows 目标没有 PE 资源节，什么都不做。
        return;
    }

    // 资源脚本、图标或编译器选择变了都要重跑本脚本，否则改图标不会重新嵌入。
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-env-changed=WINDRES");
    println!("cargo:rerun-if-env-changed=RC");

    let (Ok(manifest_dir), Ok(out_dir)) = (env::var("CARGO_MANIFEST_DIR"), env::var("OUT_DIR"))
    else {
        println!(
            "cargo:warning=缺少 CARGO_MANIFEST_DIR/OUT_DIR，跳过图标资源嵌入，exe 将没有自定义图标"
        );
        return;
    };

    let assets_dir = PathBuf::from(&manifest_dir).join("assets");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    // ---- msvc 分支：用 rc.exe，找不到退到 llvm-rc，产出 .res ----
    if target_env == "msvc" {
        let res_path = PathBuf::from(&out_dir).join("app.res");
        let mut last_error = String::from("没有可用的 rc.exe / llvm-rc");
        for program in msvc_resource_compilers() {
            match compile_resource_msvc(&program, &assets_dir, &res_path) {
                // link.exe 直接接受 .res 文件；`rustc-link-arg` 对包内所有链接目标（bin/test/bench）
                // 生效，而这个资源文件没有符号，重复链接没有副作用。
                Ok(()) => {
                    println!("cargo:rustc-link-arg={}", res_path.display());
                    return;
                }
                Err(reason) => last_error = reason,
            }
        }
        println!(
            "cargo:warning=msvc 图标资源嵌入失败，已跳过（exe 将使用默认图标）；\
             可设置 RC 或 WINDRES 指向 rc.exe：{last_error}"
        );
        return;
    }

    // ---- gnu 分支（及其它 ABI）：用 mingw 的 windres，产出 .res.o ----
    let obj_path = PathBuf::from(&out_dir).join("app.res.o");

    let Some(program) = windres_program(&target_arch) else {
        println!(
            "cargo:warning=不认识的目标架构 {target_arch:?}，跳过图标资源嵌入，exe 将没有自定义图标"
        );
        return;
    };

    match compile_resource(&program, &assets_dir, &obj_path) {
        // 把 .o 直接当链接参数交给 linker：`rustc-link-arg` 对包内所有链接目标（bin/test/bench）
        // 生效，而这个目标文件只有一个 `.rsrc` 节、没有符号，重复链接没有副作用。
        Ok(()) => println!("cargo:rustc-link-arg={}", obj_path.display()),
        Err(reason) => {
            println!("cargo:warning=图标资源嵌入失败，已跳过（exe 将使用默认图标）：{reason}")
        }
    }
}
