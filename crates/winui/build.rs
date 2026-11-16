//! 构建脚本：把 `assets/app.ico`（应用图标）与 `switcheroo-win.manifest`（comctl32 v6 +
//! 「以调用者权限运行」）作为 PE 资源嵌进 exe。
//!
//! 参数：无（读取 Cargo 注入的环境变量）
//! 核心流程：只在 `CARGO_CFG_TARGET_OS == "windows"` 时工作 →
//! **gnu 目标**（`x86_64-pc-windows-gnu` 等）用 mingw 的 `windres`：在 `OUT_DIR` 写一个
//! 只含两行的 `.rc`（`#include "app.rc"` + 清单那一行），工作目录切到 `assets/` 后编成
//! COFF 目标文件 `$OUT_DIR/app.res.o`，再 `cargo:rustc-link-arg-bins` 交给最终链接；
//! **msvc 目标**用 `rc.exe`（退到 `llvm-rc`）把 `assets/app.rc` 编成 `$OUT_DIR/app.res`，
//! `link.exe` 直接吃 `.res`，同样走 `cargo:rustc-link-arg-bins`。
//!
//! 为什么要合成一个 `.rc`：PE 的资源表只能有一棵目录树。清单与图标如果分别编成两个目标
//! 文件，链接后是两段同名 `.rsrc`，第二段的目录节点没有任何东西指过去——图标会静默丢失。
//! 所以 gnu 分支只喂一次 windres，让「资源 1 / 类型 24 / 清单」与「资源 1 / 类型 3 /
//! 图标」落在同一段 `.rsrc` 里。（清单本来就只在 gnu 分支嵌入，msvc 分支维持原状，
//! 只嵌图标。）
//!
//! 找不到 windres / rc.exe，或编译失败时只打 `cargo:warning`，绝不 panic：Linux 上的
//! `cargo check` / `cargo test` 依赖这一点。图标嵌进去之后 exe 才有 `.rsrc` 资源节，
//! 任务栏、资源管理器与 `platform-win` 的托盘（`LoadImageW(MAKEINTRESOURCE(1))`）才能
//! 用上正式图标。

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=switcheroo-win.manifest");
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=WINDRES");
    println!("cargo:rerun-if-env-changed=RC");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        // 非 Windows 目标没有 PE 资源节，清单与图标都无从谈起。
        return;
    }

    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "msvc" {
        embed_icon_msvc();
    } else {
        embed_gnu();
    }
}

/// 参数：无（读 `CARGO_MANIFEST_DIR` / `OUT_DIR` / `TARGET`）
/// 核心流程：拼出 `assets/app.rc` 与清单的绝对路径 → 在 `OUT_DIR` 写合成 `.rc`
/// （`#include "app.rc"` 让仓库里那份单行 `.rc` 真正参与编译，再加清单一行）→
/// 按目标三元组推出 `*-w64-mingw32-windres`（或 `$WINDRES`）→ 工作目录切到 `assets/`
/// 编出 `app.res.o` → `cargo:rustc-link-arg-bins`。任何一步失败只打 warning。
fn embed_gnu() {
    let Some((manifest_dir, out_dir)) = dirs() else {
        return;
    };
    let assets_dir = manifest_dir.join("assets");
    let rc_in_assets = assets_dir.join("app.rc");
    if !rc_in_assets.is_file() {
        println!(
            "cargo:warning=缺少 {}，跳过图标与清单嵌入",
            rc_in_assets.display()
        );
        return;
    }

    let manifest = manifest_dir.join("switcheroo-win.manifest");
    // 合成 .rc：第一行把 assets/app.rc（`1 ICON "app.ico"`）整段包含进来，第二行是清单。
    // windres 的预处理器不搜索当前目录，所以还要配合下面的 `-I .`（工作目录已是 assets）。
    // 清单路径统一成正斜杠：windres 在 Windows / Linux 主机上都能解析。
    let combined = out_dir.join("switcheroo-win-res.rc");
    let rc = format!(
        "#include \"app.rc\"\n1 24 \"{}\"\n",
        manifest.display().to_string().replace('\\', "/")
    );
    if let Err(err) = std::fs::write(&combined, rc) {
        println!("cargo:warning=写入合成 .rc 失败，跳过图标与清单嵌入：{err}");
        return;
    }

    let Some(windres) = find_windres() else {
        println!(
            "cargo:warning=未找到 windres，跳过图标与清单嵌入（exe 用默认图标、原生控件退回 v5 视觉样式）"
        );
        return;
    };

    let object = out_dir.join("app.res.o");
    let status = Command::new(&windres)
        // 工作目录必须是 assets/：`.rc` 里的 `"app.ico"` 是相对路径，windres 按工作目录
        // 解析（而不是按 `.rc` 所在目录），切过去才能跨版本一致。
        .current_dir(&assets_dir)
        // 让预处理器能 `#include "app.rc"`。
        .arg("-I")
        .arg(".")
        .arg(&combined)
        .arg("-O")
        .arg("coff")
        .arg("-o")
        .arg(&object)
        .status();
    match status {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg-bins={}", object.display());
        }
        Ok(status) => println!(
            "cargo:warning=windres 退出码 {:?}，跳过图标与清单嵌入（exe 用默认图标）",
            status.code()
        ),
        Err(err) => println!("cargo:warning=调用 windres 失败，跳过图标与清单嵌入：{err}"),
    }
}

/// 参数：无（读 `CARGO_MANIFEST_DIR` / `OUT_DIR`）
/// 核心流程：`rc /nologo /fo $OUT_DIR/app.res app.rc`（`llvm-rc` 不认 `/nologo`，单独分支），
/// 工作目录切到 `assets/` 保证 `"app.ico"` 找得到；候选按 `$RC` → `$WINDRES` → `rc.exe` →
/// `llvm-rc` 顺序试，第一个成功就用。全部失败只打 warning。
fn embed_icon_msvc() {
    let Some((manifest_dir, out_dir)) = dirs() else {
        return;
    };
    let assets_dir = manifest_dir.join("assets");
    if !assets_dir.join("app.rc").is_file() {
        println!(
            "cargo:warning=缺少 {}/app.rc，跳过图标嵌入",
            assets_dir.display()
        );
        return;
    }

    let res_path = out_dir.join("app.res");
    let mut last_error = String::from("没有可用的 rc.exe / llvm-rc");
    for program in msvc_resource_compilers() {
        match compile_msvc(&program, &assets_dir, &res_path) {
            Ok(()) => {
                // `.res` 没有符号，link.exe 直接当输入接受；重复链接对 bin/test 无副作用。
                println!("cargo:rustc-link-arg-bins={}", res_path.display());
                return;
            }
            Err(reason) => last_error = reason,
        }
    }
    println!(
        "cargo:warning=msvc 图标资源嵌入失败，已跳过（exe 将使用默认图标）；\
         可设置 RC 或 WINDRES 指向 rc.exe：{last_error}"
    );
}

/// 参数：无
/// 核心流程：取 `CARGO_MANIFEST_DIR` 与 `OUT_DIR`，缺任何一个就打 warning 返回 `None`，
/// 由调用方跳过资源嵌入（不让构建脚本 panic）。
fn dirs() -> Option<(PathBuf, PathBuf)> {
    let (Ok(manifest_dir), Ok(out_dir)) = (env::var("CARGO_MANIFEST_DIR"), env::var("OUT_DIR"))
    else {
        println!(
            "cargo:warning=缺少 CARGO_MANIFEST_DIR/OUT_DIR，跳过资源嵌入，exe 将没有自定义图标"
        );
        return None;
    };
    Some((PathBuf::from(manifest_dir), PathBuf::from(out_dir)))
}

/// 参数：无
/// 核心流程：按 target 三元组（`TARGET`）推 mingw 工具链前缀（`x86_64-pc-windows-gnu` →
/// `x86_64-w64-mingw32-windres`），`$WINDRES` 优先；返回第一个能跑 `--version` 的候选，
/// 都不行返回 `None`。
fn find_windres() -> Option<String> {
    if let Ok(exe) = env::var("WINDRES") {
        let exe = exe.trim();
        if !exe.is_empty() && runs(Path::new(exe)) {
            return Some(exe.to_string());
        }
    }
    let target = env::var("TARGET").unwrap_or_default();
    let arch = target.split('-').next().unwrap_or_default();
    let candidates = [format!("{arch}-w64-mingw32-windres"), "windres".to_string()];
    candidates
        .into_iter()
        .find(|candidate| runs(Path::new(candidate)))
}

/// 参数：`program` 候选可执行文件
/// 核心流程：跑一次 `--version`，能正常结束就认为可用（找不到会返回 `Err`）。
fn runs(program: &Path) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// 参数：无
/// 核心流程：返回 msvc 目标的资源编译器候选，优先级 `$RC` → `$WINDRES`（与 gnu 共用同一个
/// 开关）→ `rc.exe` → `llvm-rc`；空字符串的环境变量视为未设置。
fn msvc_resource_compilers() -> Vec<String> {
    let mut candidates = Vec::new();
    for key in ["RC", "WINDRES"] {
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

/// 参数：`program` 资源编译器；`assets_dir` 放着 `app.rc` 与 `app.ico`；`res_path` 要写出的
/// `.res`
/// 核心流程：工作目录切到 `assets/`（`"app.ico"` 是相对路径）→ `rc.exe` 加 `/nologo`
/// （`llvm-rc` 不认）→ `/fo <out.res> app.rc` 编译 → 校验 `.res` 落地。失败返回原因字符串。
fn compile_msvc(program: &str, assets_dir: &Path, res_path: &Path) -> Result<(), String> {
    let is_llvm_rc = program.to_ascii_lowercase().contains("llvm-rc");
    let mut command = Command::new(program);
    command.current_dir(assets_dir);
    if !is_llvm_rc {
        command.arg("/nologo");
    }
    command.arg("/fo").arg(res_path).arg("app.rc");

    let output = command
        .output()
        .map_err(|err| format!("无法执行 {program}：{err}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} 编译 app.rc 失败（{}）：{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if !res_path.is_file() {
        return Err(format!("{program} 没有产出 {}", res_path.display()));
    }
    Ok(())
}
