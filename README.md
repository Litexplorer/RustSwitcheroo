# RustSwitcheroo

Switcheroo 的 Rust 重写版，UI 基座为 egui（eframe）。需求基线见
[`Switcheroo-功能用户故事.md`](./Switcheroo-功能用户故事.md)，egui 能力验证结论见
[`docs/egui-spike.md`](./docs/egui-spike.md)。

## 目录结构

```
crates/core         纯逻辑内核：窗口模型、查询解析、匹配与排序（不依赖 egui / Win32）
crates/platform-win Windows 平台层：窗口枚举、图标、全局热键、键盘钩子（非 Windows 目标给空实现）
crates/app          egui 浮层界面：eframe 启动、字体注入、视图
```

分层约定：`app` 依赖 `core` 与 `platform-win`；`platform-win` 只依赖 `core`；`core` 不依赖任何一方。

## 常用命令

```bash
cargo check --workspace --all-targets
cargo test -p switcheroo-core
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p switcheroo-app
```

构建与校验默认走 `sccache`（见 `.cargo/config.toml`）。

## 构建 Windows 版

在 Linux 上交叉编译即可，不需要 Windows 机器：

```bash
rustup target add x86_64-pc-windows-gnu
cargo build --release -p switcheroo-app --target x86_64-pc-windows-gnu
```

产物 `target/x86_64-pc-windows-gnu/release/switcheroo.exe` 为 PE32+ GUI 子系统，
静态 CRT，导入表只有系统 DLL。发布时把 `crates/app/assets/` 一起放到 exe 同级。

原生浮层（winui）用 `cargo build --release -p switcheroo-winui --target x86_64-pc-windows-gnu`
构建，产物是 `switcheroo-win.exe`（`crates/winui/Cargo.toml` 的 `[[bin]]` 名）。

### 管理员权限与开机自启（winui）

`crates/winui/switcheroo-win.manifest` 把 `requestedExecutionLevel` 声明为
`requireAdministrator`，所以 `switcheroo-win.exe` 常驻管理员权限：每次启动都会弹一次 UAC
确认（首次运行同样如此）。这不是可选的「管理员模式」开关——低完整性进程会被 UIPI 拦住，
发不出 `PostMessage(WM_CLOSE)`，要关掉「以管理员身份运行的窗口」，自身必须也是高完整性
进程（`crates/platform-win/src/close.rs` 里 `CloseOutcome::Denied` 对应的就是这种情况）。

清单靠 `crates/winui/build.rs` 嵌进 exe：gnu 与 msvc 两个分支都把图标与 `RT_MANIFEST`
清单合进同一份 `.rc` 再编译，避免分成两个资源目标文件后丢清单或丢图标。构建日志里出现
「跳过图标与清单嵌入」的 `cargo:warning`（缺 `windres` / `rc.exe`）时，产出的 exe 就没有
这份提权清单，启动不会弹 UAC，也就关不掉管理员窗口。

开机自启落在计划任务上（任务名 `Switcheroo`，`schtasks /create … /sc onlogon /rl HIGHEST /f`），
不再写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`：manifest 要求管理员权限后，
Run 键拉起的进程拿不到提权，开机时会弹 UAC 甚至直接失效；`/rl HIGHEST` 的计划任务由任务
计划程序在登录时以提权方式拉起，因此登录时不再弹 UAC。`is_enabled` 用 `schtasks /query`
的退出码判定，关闭走 `schtasks /delete … /f`。

旧版（egui 版）留在 Run 键下的 `Switcheroo` 值，由 `crates/platform-win/src/autostart.rs`
的 `migrate_legacy_run_value()` 在启动时迁移：先建计划任务、成功后才删旧值，没有旧值时零
副作用，可重复调用（幂等）。

## 当前进度

- [x] US-032 egui/eframe 能力验证 spike（Windows 真机已点亮：无边框、透明、中文、键盘）
- [x] US-033 最小工程骨架与 CI
- [x] US-034 单实例与后台常驻
- [x] US-035 全局热键唤起/收起空白浮层
- [x] US-036 平台层窗口快照（命令行）
- [x] US-037 浮层载入真实窗口列表
- [x] US-038 键盘导航选中项
- [x] US-039 回车/双击切换窗口

热键/单实例/窗口枚举已实现，运行期行为待真机验证。

M2 主价值闭环（唤起 → 选择 → 切换）代码已完成：真实窗口列表 + 三列行渲染、
↑↓/Alt+J/K/Tab 循环导航、回车（KeyUp）/双击切换并收起浮层。
互动行为（回车切换、双击、自动滚动）待真机验证；首帧耗时只在运行日志与界面上
给出实测值，200 窗口压测基线待真机补录。

- [ ] US-040 起：检索、关闭窗口、图标、配置……

原生浮层（winui）侧本轮落地（代码已实现，待真机验证）：

- [x] 常驻管理员权限：`crates/winui/switcheroo-win.manifest` 声明 `requireAdministrator`，
      理由见上文「管理员权限与开机自启（winui）」（对应 US-055）
- [x] 开机自启改计划任务 `Switcheroo`（`/sc onlogon /rl HIGHEST /f`），并支持把旧 Run 值
      迁移过去（对应 US-056）
- [x] 关闭失败可区分：`CloseOutcome::{Requested, Denied, Failed}`，`Denied` 提示
      「需要管理员权限才能关闭」，不再和普通失败混在一起（对应 US-057）
