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
