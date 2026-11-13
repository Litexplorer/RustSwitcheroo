# US-032 egui/eframe 能力验证（Spike）

本文记录 `crates/app` 里 spike 的验证范围、结论与复现方式。文档口径与
`Switcheroo-功能用户故事.md` 的 US-032 一致：**能力结论以本文件为准，实现偏离时先改这里再改代码。**

## 1. 选型结论

| 项目 | 取值 | 理由 |
|---|---|---|
| UI 基座 | `eframe` / `egui` | 立即模式、跨平台、无 Vulkan 1.3 依赖；Windows 走 winit + OpenGL(glow) |
| 版本 | `0.35.0` | 本机 `rustc 1.92.0`，`cargo` 解析到的最高可用版本（`0.37` 要求更高 MSRV）；`0.35` 声明 `rust-version = 1.92` |
| 渲染后端 | `glow`（OpenGL） | 不开 `wgpu`，避开 Vulkan 驱动要求；Intel HD 630 等老核显可用 |
| 平台特性 | `default-features = false` + `default_fonts`/`glow`/`wayland`/`x11` | 不引入 accesskit、web_screen_reader 等无关依赖 |
| 中文字体 | 运行时读取**系统自带**中文字体：`msyh.ttc`（微软雅黑）→ `simhei.ttf`（黑体）→ `simsun.ttc`（宋体），粗体用 `msyhbd.ttc` | 不再随包携带字体文件（`crates/app/assets/` 已删除），发布 zip 从 ~37MB 降到 7.3MB；探测顺序与降级规则见第 10 节 |

### eframe 0.35 的 API 变化（踩坑记录）

- `eframe::App` 的入口方法从 `update(&mut self, ctx, frame)` 变为 `ui(&mut self, ui: &mut egui::Ui, frame)`；
  纯逻辑处理放在新增的可选方法 `logic(&mut self, ctx, frame)`。
- 因此根 UI 直接给到应用，不需要再套一层 `egui::CentralPanel`；`egui::Frame` 用 `show(ui, ..)`。

## 2. 逐项能力结论

| 能力 | 实现方式 | 状态 |
|---|---|---|
| 编译通过 | `cargo check --workspace --all-targets` | 已验证（Linux，本机） |
| 交叉编译出 Windows exe | `--target x86_64-pc-windows-gnu` + `+crt-static` | 已验证：PE32+ GUI 子系统，依赖只剩系统 DLL |
| 无边框 | `ViewportBuilder::with_decorations(false)` | 已验证（Windows 真机截图，无标题栏/无边框） |
| 透明背景 | `with_transparent(true)` + `clear_color` 返回全透明 + 卡片内缩 12px | 已验证（卡片外可见桌面内容） |
| 置顶 | `with_always_on_top()` | 已接入，未做正面验证（需与其他窗口叠放对比） |
| 不占任务栏 | `with_taskbar(false)` | 已接入，未做正面验证（需看任务栏图标） |
| 键盘事件 | `ui.ctx().input(|i| i.events)` 统计 `Event::Key` | 已验证（自检页键盘事件计数递增） |
| 输入框自动聚焦 | `Response::request_focus()` 在未聚焦时调用 | 已验证（唤起后直接可输入） |
| 中文字体 | `FontDefinitions` 把 CJK 字体追加到 Proportional/Monospace 回退链末尾 | 已验证（中文标题无方块） |
| 中文输入法（IME） | 输入框直接使用 egui `TextEdit`，组合输入由 winit 的 IME 事件驱动 | **部分验证**：输入框内中文正常显示；候选框/组合过程需人工确认 |
| DPI 缩放 | winit 自动处理，`egui` 使用逻辑像素 | **未验证**，需 125%/150% 缩放下目测 |
| 多屏定位 | 浮层居中已实现（`SwitcherooApp::center_on_current_monitor`，坐标口径见第 10.7 节）；egui 0.35 只给监视器尺寸、不给原点，按「虚拟桌面按监视器尺寸平铺」估算 | **未验证**（本轮远程桌面锁屏，无法截图确认居中落点） |

> Windows 侧验证方式：把 `switcheroo-win-x64/` 整体放到真机，用计划任务以交互会话启动，再截屏比对。
> 本地容器只有 llvmpipe，不能代表真实渲染效果。

## 3. 复现命令

```bash
cargo check --workspace --all-targets        # 编译校验（当前已绿）
cargo fmt --check                            # 格式
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                       # 内核单测
cargo run -p switcheroo-app                  # 需要桌面环境，容器内不执行
```

### Linux 上直接交叉编译 Windows 版（不依赖 Windows 机器）

```bash
rustup target add x86_64-pc-windows-gnu       # 只需一次
cargo build --release -p switcheroo-app --target x86_64-pc-windows-gnu
# 产物：target/x86_64-pc-windows-gnu/release/switcheroo.exe
```

- `.cargo/config.toml` 给该 target 加了 `-C target-feature=+crt-static`，exe 不再依赖 `libgcc_s_seh-1.dll` /
  `libstdc++-6.dll` / `libwinpthread-1.dll`，导入表只剩 `KERNEL32/USER32/GDI32/OPENGL32/...` 等系统 DLL。
- `[profile.release] strip = true` 去掉符号；再加上字体改由系统提供（不再随包带 20MB 字体文件），
  发布 zip 从 ~37MB 降到 7.3MB（见第 10 节）。
- 发布目录结构（**不再需要 `assets/`**，中文字体取自系统）：

```
switcheroo-win-x64/
├── switcheroo.exe
└── README.txt
```

真机快速验证（无需登录桌面）：用 `schtasks /create ... /it` + `schtasks /run` 在交互会话启动，
再用同样的 `/it` 计划任务跑截屏脚本，即可确认窗口是否真的画出来。

## 4. 能力不足时的备选方案

若 Windows 真机上出现关键能力缺失（例如透明合成失效、IME 丢字），按优先级降级：

1. **原生 Win32 分层窗口 + egui 视图**：用 `WS_EX_LAYERED`/`SetLayeredWindowAttributes` 自建浮层窗口，
   egui 只负责画内容（`app` 层已只依赖 `core` 数据与事件，替换窗口壳不动业务代码）。
2. **保留系统装饰的普通窗口**：牺牲无边框观感，换取输入法稳定。
3. **更换 Rust UI 框架**：仅在 1、2 都无法接受时考虑，`core` 与 `platform-win` 不受影响。

三条路都不影响 US-033 之后的故事编号与依赖表。

## 5. 运行期验证记录（US-034 ~ US-036）

验证环境：Windows 10，交互会话（计划任务 `/it` 启动），本次实测。下表只记录实际看到的结论，
未看到的一律留在下一张表里。

| 验证项 | 结论 | 证据方式 |
|---|---|---|
| 启动后隐藏且进程常驻 | 通过：`Get-Process switcheroo` 能查到进程，`MainWindowTitle` 为空（无常驻窗口） | 进程查询 |
| `Alt+Space` 唤起浮层 | 通过：浮层出现，界面显示「单实例状态：已获取命名互斥量，本进程是唯一实例」「热键注册结果：Alt+Space 已注册（按一次唤起，再按一次收起）」「窗口快照：20 个（真实枚举，唤起时刷新）」 | 截屏 |
| 列表内容为真实窗口 | 通过：列出 msedge、1Remote、WXWork、openidea、datagrip64、rustmobax、Weixin、lx-music-desktop、SystemSettings、dopus；UWP 应用「电影和电视」按降级规则把进程名显示为 `UWP` | 截屏 |
| 再按一次 `Alt+Space` 收起 | 通过：收起前后两次截屏对比，浮层消失 | 截屏 |
| 单实例生效 | 通过：再启动一次 exe 后 `(Get-Process switcheroo).Count` 仍为 1，PID 未变 | 进程查询 |
| `list.exe` 命令行输出 | 通过：实机输出 20 行，每行格式 `序号. HWND=0x… 进程=… 可见=true 标题=…`，HWND/标题/进程名/可见性四要素齐全 | 命令行输出 |

以下项本次未能确认，**保持未验证状态**，需人工在真机补测：

| 待确认项 | 当前状态 | 说明 |
|---|---|---|
| `Esc` 收起浮层 | 待人工确认 | 自动化脚本执行时焦点被计划任务的 PowerShell 窗口抢走，无法确认 |
| 中文输入法（IME）组合输入 | 待人工确认 | 预编辑/组合过程未实测，输入框中文显示正常 |
| 热键被其他程序占用时的提示 | 待人工确认 | 未构造占用场景 |
| `Ctrl+Q` 退出路径 | 待人工确认 | 未实测 |
| 退出时互斥量、热键等资源的释放 | 待人工确认 | 未实测；当前无钩子与托盘实现，暂无对应资源可释放 |

## 6. 运行期验证记录（US-037 ~ US-039）

验证环境：Windows 10，交互会话，本次实测。下表只记录实际看到/查到的结论，
未覆盖的项留在下一张表里，不预先勾选。

| 验证项 | 结论 | 证据方式（截屏 / 运行期日志 / 进程查询） |
|---|---|---|
| 列表渲染与三列布局 | 通过：浮层显示 19 个可切换窗口，左灰方块图标占位、中间标题、右侧右对齐灰色进程名；实测出现 1Remote / openidea / wxwork / msedge / datagrid64 | 截屏 |
| 中文标题显示（CJK 字体） | 通过：列表内「公司开发机」「企业微信」等中文标题显示正常，无方块 | 截屏 |
| 选中态 | 通过：第 1 行有高亮底色（唤起时默认选中第 1 项） | 截屏 |
| 首帧耗时基准 | 记录：20 个窗口 25.8 ms、19 个窗口 112.9 ms；界面右上角同步显示该数值 | 运行期日志 |
| `Enter` 切换窗口 | 通过：日志出现「已切换到窗口「公司开发机 - 1Remote」」，切换后浮层已隐藏（截图中无浮层），再次唤起重新加载为 19 个窗口 | 运行期日志 + 截屏 |
| 空标题脏数据消失 | 同一台机器窗口数由 20 变为 19，上一轮出现的「/msedge」空标题条目本轮不再出现 | 截屏 |

以下项本轮**未覆盖，保持未验证**，需人工在真机补测：

| 待确认项 | 当前状态 | 说明 |
|---|---|---|
| 200 个窗口的压测基线 | 待人工确认 | 本轮只测到 19~20 窗口（25.8 ms / 112.9 ms），200 窗口首帧耗时未采集 |
| 选中项在可视区之外时的自动滚动落点 | 待人工确认 | 仅确认首行默认选中，未验证滚动后的落点位置 |
| `Alt+J` 下移选中项 | 待人工确认 | 本轮未逐一验证该键位 |
| `Alt+K` 上移选中项 | 待人工确认 | 本轮未逐一验证该键位 |
| `Shift+Tab` 上移选中项 | 待人工确认 | 本轮未逐一验证该键位 |
| 鼠标双击切换 | 待人工确认 | 本轮未实测双击路径 |
| 多选批量切换 | 待人工确认 | 当前实现单选，多选语义未落地 |

## 7. 运行期验证记录（US-040 ~ US-043）

验证环境：Windows 10，交互会话，本次实测。下表只记录实际看到的结论；只在代码里成立的结论不写进主表，
一律留在「待人工确认」里，不预先算作通过。

| 验证项 | 结论 | 证据方式（运行期日志 / 截屏 / 基准测试） |
|---|---|---|
| 输入即过滤（US-040） | 通过：浮层内输入 `note` 后列表实时缩小，顶部显示「命中 5 / 共 21」，全程未按任何确认键 | 运行期日志 + 截屏 |
| 过滤后排序（US-040） | 通过：记事本（进程名 `notepad` 命中）排在第一条并高亮选中 | 截屏 |
| 首帧耗时（US-040 同环境参考） | 记录：21 个窗口 29.0 ms，界面右上角同步显示该数值 | 运行期日志 |
| `Ctrl+W` 关闭选中窗口（US-043） | 通过：日志依次出现「已请求关闭窗口「无标题 - 记事本」，每 125 ms 轮询确认」与「窗口已关闭：id=0x2DA04CC，确认耗时 52 ms」 | 运行期日志 |
| 关闭后的进程与列表状态（US-043） | 通过：`notepad` 进程数变为 0；列表移除该项后总数由 21 降为 20 | 运行期日志 + 进程查询 |
| 关闭后浮层保持（US-043） | 通过：关闭期间浮层一直显示、未收起，界面上也没有出现红色错误提示 | 截屏 |
| 同一次按键触发两次关闭请求的竞态（US-043） | 通过：上一版「第二次请求因窗口已消失被误报关闭失败、条目残留」的问题，修复后复测不再复现 | 运行期日志 |
| 1000 窗口过滤基准（US-040） | 通过：`cargo test -p switcheroo-core --release -- --ignored` 实测 1.876 / 1.952 / 2.082 / 2.126 ms，均远低于 16 ms | 基准测试 |

以下项本轮**未覆盖，保持未验证**，需人工在真机补测：

| 待确认项 | 当前状态 | 说明 |
|---|---|---|
| `Alt+X` 关闭路径 | 待人工确认 | 代码已把 `Alt+X` 接到 `close_selected`，本轮只实测了 `Ctrl+W` |
| 关闭中条目置灰的视觉效果 | 待人工确认 | 逻辑已实现（`draw_row` 用 `CLOSING_TEXT` / `CLOSING_ICON`），本轮目标窗口 52 ms 就关掉，没截到置灰帧 |
| 真正关闭失败（窗口存活时拒绝关闭）的提示与条目保留 | 待人工确认 | 未构造「是否保存」对话框场景，红色提示与保留分支只在代码层面成立 |
| 过滤状态下关闭后的选中修正 | 待人工确认 | 本轮关闭时查询为空，选中修正走的是全量列表 |
| 选中项被关掉后「优先下移、否则上移」的落点 | 待人工确认 | 代码已实现（`Selection::clamp` + 按 id 复原锚点）且有单测，但未逐条肉眼确认落点 |
| 快照被关空时自动隐藏浮层 | 待人工确认 | 本轮只关到 21 → 20，未关空 |
| 隐藏 / 退出时停止轮询 | 待人工确认 | 轮询由 `request_repaint_after` 驱动、无常驻线程，未在真机观察退出路径 |
| 中文查询与逐字匹配的界面表现 | 待人工确认 | 仅单测覆盖（`rank_windows_chinese_query_works` 等），未在浮层内实际输入中文 |
| 复合查询 `进程.标题` | 待人工确认 | 属 US-041，本轮未实现（**已被第 8 节取代**：本轮已实现并在真机验证） |

## 8. 运行期验证记录（US-041 ~ US-042）

验证环境：Windows 10，交互会话，本次实测。下表只写实际看到/跑到的结论；仅在代码层面成立、
或本轮没有观察到的项，一律留在「待人工确认」，不预先算作通过。

| 验证项 | 结论 | 证据方式（截屏 / 运行期日志 / 单元测试） |
|---|---|---|
| 复合查询「进程.标题」（US-041） | 通过：浮层内输入 `open.swit` 后列表只剩 1 条，顶部显示「命中 1 / 共 20」，该条为 openidea 进程的「…root@kfc.test.huya.info] – Switcheroo-功能用户故事.md」——进程片段只约束进程名、标题片段只约束标题，两侧同时命中才保留 | 截屏 |
| 命中字符加粗（US-042） | 通过：放大截图可见标题里 `Swit` 四个字符明显加粗，其后的 `cheroo` 为常规字重（命中段用 `FontFamily::Name("bold")`，同一份 MapleMono SemiBold 字形） | 截屏 |
| 首帧耗时（同环境参考） | 记录：20 个窗口 25.1 ms，界面右上角同步显示该数值 | 运行期日志 |
| 单元测试回归（US-041 / US-042） | 通过：`cargo test --workspace` —— app 6 passed、core 104 passed / 1 ignored、platform-win 14 passed；其中复合查询新增 17 条（`composite_query_*` 16 条 + `non_composite_query_is_unaffected_by_foreground_process`）、高亮新增 4 条（分片切分、特殊字符原样、越界/非字符边界/乱序不 panic、真实字体排版不 panic） | 单元测试 |
| 1000 窗口过滤基准（US-041 改动后） | 通过：`cargo test -p switcheroo-core --release -- --ignored` 实测 1.893 ms（真机同批判定）/ 1.862 ms（本地复跑），均远低于 16 ms | 单元测试（基准） |

以下项本轮**未覆盖，保持未验证**，需人工在真机补测：

| 待人工确认 | 当前状态 | 说明 |
|---|---|---|
| 别名 `e` / `f` / `w` / `p` 的实际输入 | 待人工确认 | 代码与单测已覆盖（`PROCESS_ALIASES` / `expand_process_alias`，仅整段单字符时展开），本轮真机只敲了完整进程片段 |
| 空进程片段 `.标题` 回退前台进程名 | 待人工确认 | 代码与单测已覆盖（`resolve_process_filter` 回退 `foreground_process` 并剥 `.exe`），未在浮层内实测 |
| 中文查询的逐字高亮 | 待人工确认 | 仅单测（`chinese_query_matches_character_by_character`、`composite_query_ranges_point_at_each_field`），真机未输入中文 |
| 进程名一侧的加粗观感 | 待人工确认 | `draw_row` 对进程名走同一套 `highlight_job`，本轮截图只确认了标题侧 |
| 超长标题截断下的高亮 | 待人工确认 | `Label::truncate()` 与多分片 `LayoutJob` 的组合未肉眼确认 |
| 关闭中条目置灰且不加粗的视觉 | 待人工确认 | `draw_row` 在 `closing` 时清空命中区间并换 `CLOSING_TEXT`，目标窗口关闭太快，未截到该帧 |
| 原版 `XamlHighlighter` 等价性（快照测试） | 待人工确认 | 现有 4 条高亮单测断言的是分片结构（内联期望值），未引入快照框架，也未逐条对照 `Core.UnitTests/XamlHighlighterTests.cs` |
| 拼音首字母匹配（US-045） | 已实现（**已被第 11 节取代**：真机 `dy` →「电影和电视」） | US-041 的「逐字与拼音首字母」当时只完成逐字部分，拼音首字母后由 US-045 补齐，详见第 11 节 |

## 9. 界面改版与图标（US-044）

验证环境：Windows 10，交互会话，本轮实测。本节记录界面的**最终样式规格**、图标取图链路、
本轮踩到的 `#[repr(C)]` 坑，以及仍未人工确认的项。截图结论与用户给的参考图一致：白卡片、
细边搜索框、右上角 `?`、30px 紧凑行、左侧真实应用图标、右侧灰色进程名、选中行浅蓝底。

### 9.1 界面从「演示页」改为「卡片式列表」

改版前浮层顶部有标题、状态行、命中统计（命中 x / 共 y）、首帧耗时与快捷键说明，属于
演示/调试信息；改版后这些文案**全部删除**，只保留「搜索框 + `?` + 窗口列表」，失败原因仅在
真的出错时以红字出现在搜索框上方。首帧耗时只写日志，不再上界面。模块入口的口径见
`crates/app/src/app.rs` 顶部注释与 `SwitcherooApp::ui`。

| 项目 | 最终规格 | 代码位置 |
|---|---|---|
| 浮层窗口 | 600×640，无边框、透明、置顶、不进任务栏、不可缩放，启动时隐藏 | `crates/app/src/main.rs:36-45` |
| 卡片 | 纯白填充，1px `#E0E0E0` 描边，圆角 6；与窗口边缘留 6px 透明外边距，内边距 10；卡片铺满整窗剩余区域 | `CARD_OUTER_MARGIN` / `CARD_INNER_MARGIN` / `CARD_RADIUS`，`SwitcherooApp::ui` 的 `egui::Frame` |
| 搜索框 | 白底，1px `#E0E0E0` 描边，圆角 4，内边距左右 8 / 上下 5（约 28px 高）；宽度 = 整行宽度 − 22px `?` 按钮 − 一个间距（下限 80）；13 号字，提示文案「搜索窗口，可用「进程.标题」」；唤起即聚焦 | `SwitcherooApp::show_search_row` |
| `?` 帮助按钮 | 搜索行最右侧，22×22，无边框，灰色 `?`，悬停提示「快捷键」，点击翻转帮助面板 | `HELP_BUTTON_SIZE`，`show_search_row` |
| 行高与间距 | 行高 30（`ROW_HEIGHT`），行间 0 间距、无分隔线；行内左右 padding 8 | `ROW_HEIGHT` / `ROW_PADDING_X`，`show_list` / `draw_row` |
| 列宽 | 左侧图标列固定 16×16，图标与标题之间 8px；标题列占剩余宽度、超长截断；进程名列右对齐、按文字自适应（不设固定宽度），11 号灰字 | `ICON_SIZE` / `COLUMN_GAP`，`draw_row` |
| 文字 | 标题 13 号 `#1F1F1F`；进程名 11 号 `#808080`；命中段换 `bold` 族（同一字号、同一字色），仍然加粗 | `draw_row` / `highlight_job`，`crates/app/src/fonts.rs` |
| 选中态 | 浅蓝 `#CCE4F7` 填充 + 1px 淡蓝 `#A8CEEE` 描边，圆角 4；鼠标悬停行底色 `#F5F5F5` | `ROW_SELECTED_BG` / `ROW_SELECTED_STROKE` / `ROW_HOVER_BG`，`draw_row` |
| 真图标 | 等比缩放进 16×16 方框并居中；未取回或该窗口无图标时画 16×16 浅灰 `#DDDDDD` 圆角方块（圆角 3） | `crates/app/src/icons.rs::fit_icon_size`，`draw_row` |
| 关闭中条目 | 标题/进程名换 `#A6A6A6`，真图标 alpha 降到 96，占位图标换 `#EEEEEE`；命中区间清空，不再加粗 | `CLOSING_*`，`draw_row` |
| 帮助面板 | 位于搜索行**下方** 8px、左对齐的前景层 `egui::Area`；宽度 `clamp(280, 460)`；白底 + 1px `#E0E0E0` + 圆角 6 + 内边距 10；默认收起，每次唤起重置为收起；列出输入、`进程.标题`、上下移动、回车/双击、关闭、收起、退出共 8 行 | `show_help_panel`，`load_windows` 里的 `show_help = false` |
| 错误提示 | 仅单实例/热键/切换/关闭失败时，在搜索框上方显示红字 `#B01C1C`；正常时完全不占行 | `WARN_RED`，`show_errors` |
| 已删除的演示/调试文案 | 标题、状态行、命中统计、首帧耗时、常驻快捷键说明均不再渲染 | `crates/app/src/app.rs` 顶部注释、`SwitcherooApp::ui` |

### 9.2 图标取图链路

1. **UI 线程入队**：`IconCache::sync(&snapshot)` 只为「缓存里没有、也没在排队」的窗口投递
   `WindowId`；`hide` 不清缓存，因此二次唤起直接命中（`crates/app/src/icons.rs:51`、`112`，
   `crates/app/src/app.rs` 的 `hide` / `load_windows`）。
2. **后台线程取图**：具名线程 `icon-loader` 循环 `recv` 并调用平台层
   `window_icon(id)`，单次最多被 `WM_GETICON` 的 100ms 超时拖住
   （`crates/app/src/icons.rs:176`）。
3. **平台层降级链**（`crates/platform-win/src/icon.rs`）：`SendMessageTimeoutW(WM_GETICON,
   ICON_BIG/ICON_SMALL)`（100ms，`SMTO_ABORTIFHUNG | SMTO_BLOCK`）→ `GetClassLongPtrW`
   读 `GCLP_HICON` / `GCLP_HICONSM` → `ExtractIconExW` 抽 exe 的第 0 个图标 →
   `SHGetFileInfoW`（`SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES`）兜底。
   任一级解码失败继续降级，全部失败返回 `None`。
4. **HICON 解码**：`GetIconInfo` 复制出颜色位图与 AND 掩码 → `GetObjectW` 读尺寸 →
   `GetDIBits` 读 32 位 BGRA（行序看回写的 `biHeight` 符号）→
   `win32_parse.rs` 的 `bgra_to_rgba` 换 B/R、`bgra_has_alpha` 判断是否自带 alpha，
   没有 alpha 时 `fill_alpha` 先置不透明、再由 `apply_mask_alpha` 按 1 位掩码挖透明。
   单色图标（无颜色位图）与异常尺寸直接放弃。
5. **上传与复用**：UI 线程 `IconCache::poll` 用 `try_recv` 不阻塞地收结果，`upload` 把
   RGBA 转成 `egui::ColorImage` 后 `load_texture` 成 `TextureHandle` 存进
   `IconCache::textures`；失败也记成 `None`，不会反复取图。有新图标时 `request_repaint`，
   占位方块在之后的帧被替换。
6. **诊断工具**：`crates/platform-win/examples/icon_probe.rs` 打印前 10 个窗口的
   `icon=ok WxH 耗时` / `icon=none`，并把平台层 `icon:` 前缀的 debug 日志带到 stderr；
   Windows 上运行 `cargo run -p switcheroo-platform-win --example icon_probe`。

### 9.3 本次踩到的 `#[repr(C)]` 坑

`GetDIBits` 需要一个 `BITMAPINFO`：标准结构体只声明 1 项调色板，而 1 位掩码转换要 2 项，
因此 `icon.rs` 里定义了 `BitmapInfoBuffer { header: BITMAPINFOHEADER, _colors: [RGBQUAD; 256] }`
再把 `self as *mut BITMAPINFO` 交给 GDI。首轮真机**所有窗口都取不到图标**：`icon_probe` 能看到
每一级都拿到了非 0 的 HICON，却稳定出现 `decode fail … reason=color_getdibits`——原因是这个
缓冲结构体当时**没有 `#[repr(C)]`**，Rust 默认布局允许重排字段，GDI 按 C 布局读到的
`BITMAPINFOHEADER` 是垃圾（`biSize`/`biWidth`/`biBitCount` 都对不上），`GetDIBits` 每次都返回 0。
补上 `#[repr(C)]` 后同一台机器立即恢复：前 10 个窗口全部 `icon=ok 32x32`，命中层级
`wm_geticon_big` / `wm_geticon_small` / `class_icon_big`，单窗口 0~4ms。

教训与排查方法：

- 任何交给 Win32 的结构体都必须显式 `#[repr(C)]`，即使字段类型来自 `windows` crate；
  `windows` 的 `BITMAPINFOHEADER` 本身是 `repr(C)`，包一层自定义缓冲不会自动继承 C 布局。
- `icon_probe` 把「拿到句柄」与「解码失败」分开打印，正是这次能一眼定位到
  `color_getdibits` 而不是继续怀疑 `WM_GETICON` 的原因；新增 GDI 结构体时优先用它回归。
- 颜色位图读 32 位、掩码读 1 位，行宽都按 4 字节对齐；行序以 `GetDIBits` 回写的
  `biHeight` 符号为准，不假设固定行序。

### 9.4 真机证据摘要（本轮）

| 项 | 结论 | 证据方式 |
|---|---|---|
| 取图全链路 | 通过：`icon_probe` 前 10 个窗口全部 `icon=ok 32x32`；命中层级 `wm_geticon_big` / `wm_geticon_small` / `class_icon_big`；耗时 0~4ms/窗口 | 命令行输出 |
| 界面观感 | 通过：与参考图一致——白卡片、细边搜索框、右上角 `?`、30px 紧凑行、左侧真实应用图标（msedge / Weixin / WXWork / 1Remote / lx-music-desktop / datagrip64 / openidea）、右侧灰色进程名、选中行浅蓝底 + 淡蓝描边，无演示/调试文案 | 截屏 |
| 命中加粗 | 通过：真实图标版界面里「功能用户故事」「Crush」等命中片段仍然加粗 | 截屏 |
| 失败原因定位 | 通过：`BITMAPINFO` 缓冲缺 `#[repr(C)]` 导致 `GetDIBits` 稳定失败；修复后同一台机器全部成功 | 真机前后对比 + 运行期日志 |
| 单元测试回归 | 通过：`cargo test --workspace`——app 13 passed、core 104 passed / 1 ignored、platform-win 22 passed | 单元测试 |

> 说明：上述 `cargo test` 在 Linux 主机上执行，`crates/platform-win/src/icon.rs` 里
> `#[cfg(windows)]` 的取图/解码分支不会被 Linux 单测执行；本轮另用
> `cargo check` 与 `cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings`
> 对 Windows 目标做了类型检查与 lint，均通过。真机行为仍以 9.4 的 `icon_probe` 与截屏为准。

### 9.5 待人工确认

| 待确认项 | 当前状态 | 说明 |
|---|---|---|
| `?` 帮助面板点击后的实际观感 | 待人工确认 | 代码已实现（面板贴搜索行下方、宽度 280~460），本轮只确认按钮存在与面板布局参数，未截到展开后的界面 |
| 关闭中条目图标置灰变淡的视觉 | 待人工确认 | 代码已实现（真图标 alpha 96、占位图标 `#EEEEEE`），目标窗口关闭太快，未截到该帧 |
| UWP（`ApplicationFrameHost`）窗口的图标 | 待人工确认 | 本轮 `icon_probe` 的样本未覆盖 UWP 窗口，取图与降级表现未实测 |
| 高 DPI（125% / 150%）下的图标与行高观感 | 待人工确认 | 只在 100% 缩放下看过；图标按逻辑像素 16×16 绘制，缩放后的清晰度与行高未目测 |
| `icon_probe` 之外的窗口图标覆盖率 | 待人工确认 | 只探测了前 10 个窗口，未统计全量窗口的成功率 |

## 10. 设置 / 托盘 / 主题（US-046 ~ US-048）

本轮（Windows 10 真机）补齐了配置持久化、选项面板、托盘菜单与开机自启，并把界面扩展成
明 / 暗两套配色、加上「唤起即居中」与系统字体探测。本节记录**最终落地口径**，实现位置以
`crates/core/src/settings.rs`、`crates/app/src/app.rs`、`crates/platform-win/src/{tray,autostart,system_theme,hotkey}.rs`、
`crates/app/src/{main,fonts}.rs` 为准。

> 本轮真机只拿到**启动期日志**（远程桌面处于锁屏 / 空白画面，截图不可用），所以本节多数结论是
> **代码级**的；凡是未目测的项一律汇总在 10.10，不预先算作验证通过。

### 10.1 配置项与默认值

| 键 | 类型 / 取值 | 首启默认 | 含义 | 代码 |
|---|---|---|---|---|
| `enable_hotkey` | `true` / `false` | `true` | 是否注册全局热键 | `crates/core/src/settings.rs:63`、默认值 `:72-83` |
| `hotkey_key` | 十进制 `u32`，Win32 虚拟键码 | `32`（`0x20` = Space） | 组合键的主键 | 同上；常量 `DEFAULT_HOTKEY_KEY`（`crates/core/src/lib.rs:24`） |
| `hotkey_modifiers` | 十进制 `u32`，`MOD_ALT=1`/`MOD_CONTROL=2`/`MOD_SHIFT=4`/`MOD_WIN=8` 位或 | `1`（`MOD_ALT`） | 组合键的修饰键 | `crates/core/src/lib.rs:45-59` |
| `autostart` | `true` / `false` | `false` | 开机自启（与注册表双向对齐） | `settings.rs:67`、对齐逻辑 `crates/app/src/app.rs:439-445` |
| `theme` | `system` / `light` / `dark` | `system` | 主题偏好；`system` 跟随系统 | `settings.rs:21-53`、`:69` |

解析规则：逐行 `key=value`；空行与 `#` / `;` 注释跳过；未知键忽略；单行没有 `=` 忽略；
非法值（含 `enable_hotkey=maybe`、`hotkey_key=not-a-number`、`theme=rainbow`）按**字段**回落默认值，
不会因为某一项写错而丢掉整份配置（`settings.rs:89-129`，单测 `invalid_values_fall_back_to_defaults`）。

### 10.2 配置文件路径与示例

- 路径：`%APPDATA%\Switcheroo\config.ini`（`config_path()`，`settings.rs:160-172`）。
- `APPDATA` 不存在**或为空字符串**时退化为当前工作目录下的 `config.ini`（同上，
  单测 `config_path_uses_appdata_when_present`、`config_path_falls_back_without_appdata`）。
- 读取失败 / 文件不存在直接返回 `Settings::default()`，启动不会因此失败（`settings.rs:176-188`）。
- 保存时先 `create_dir_all` 建父目录再写文件（`settings.rs:193-207`）。

`to_text()` 的输出（键序固定，可直接手改）：

```ini
# Switcheroo 配置，修改后重启生效
# 布尔值取 true / false，theme 取 system / light / dark
enable_hotkey=true
hotkey_key=32
hotkey_modifiers=1
autostart=false
theme=system
```

> 手改文件需要重启才生效（与文件头注释一致）；界面里的改动（选项面板 `确定`、托盘「开机自启」/
> 「切换明暗主题」、`Ctrl+T`）是**即时生效 + 立刻落盘**（`app.rs:977-995`、`:1046-1129`、`:1194-1210`）。
> 例外是选项面板：热键与自启都**先真生效、再落盘**，任何一步失败都不写 `config.ini`（10.5）。

### 10.3 开机自启注册表位置与值名

| 项 | 取值 | 代码 |
|---|---|---|
| 键 | `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run` | `crates/platform-win/src/autostart.rs:17` |
| 值名 | `Switcheroo`（任务管理器「启动」页显示的就是它） | `autostart.rs:20` |
| 类型 / 值 | `REG_SZ`，内容为**带双引号**的 exe 全路径，如 `"C:\Apps\Switcheroo\switcheroo.exe"` | `autostart.rs:26-39`、`:111-168` |
| 查询 | `RegQueryValueExW` 只查数据大小，判断值是否存在；键打不开 / 值不存在一律按「未启用」 | `autostart.rs:53-104` |
| 删除 | `RegDeleteValueW`；`ERROR_FILE_NOT_FOUND` 视为成功（重复点击不报错） | `autostart.rs:175-222` |

三条与验收口径有关的说明：

1. 用 **HKCU Run 键**而不是「用户启动文件夹里的快捷方式」——不需要管理员权限，也只影响当前用户；
   与原版（快捷方式）的差异记在 US-048 的验收注记里。
2. 启动时**以注册表为准**修正配置：`reconcile_autostart` 发现 `settings.autostart` 与
   `is_enabled()` 不一致就改写配置并回写一次文件（`app.rs:697-706`，单测 `autostart_follows_registry`）。
3. 托盘勾选与面板勾选都只是**镜像**：写注册表失败时状态回退到 `is_enabled()`，绝不谎报（`app.rs:948-970`），
   同时把原因写进 `options_error`，面板开着显示在面板内、面板关着显示在列表页（`app.rs:1853-1866`、`:1588-1590`）。

### 10.4 托盘菜单项

`MENU_LAYOUT`（`crates/platform-win/src/tray.rs:53-81`，单测 `menu_layout_matches_contract`）：

| 顺序 | 文案 | 命令 | 菜单 ID | 勾选 |
|---|---|---|---|---|
| 1 | 打开 Switcheroo | `TrayCommand::Show` | 1 | — |
| 2 | （分隔线） | — | — | — |
| 3 | 选项… | `TrayCommand::Options` | 2 | — |
| 4 | 开机自启 | `TrayCommand::ToggleAutostart` | 3 | 按实际状态打勾 |
| 5 | 切换明暗主题 | `TrayCommand::ToggleTheme` | 4 | — |
| 6 | （分隔线） | — | — | — |
| 7 | 退出 | `TrayCommand::Exit` | 5 | — |

- 左键单击 / 双击 = `Show`，双击的去重窗口是 250 ms（`tray.rs:106-118`，单测 `show_is_debounced`）。
- 悬停提示 = `APP_NAME` = `"Switcheroo"`（`app.rs:77` + `crates/core/src/lib.rs:21`）。
- 勾选状态在**弹菜单那一刻**从共享 `AtomicBool` 读取（`tray.rs:675-692`），不会每次弹菜单重读注册表。
- **没有 About / 关于项**（属 US-052），也**没有**「Run on Startup」的英文文案——菜单是中文的。
- 菜单命令只负责投递，真正动作全在 UI 线程：`poll_tray_commands` 用 `try_recv` 非阻塞收割
  （`app.rs:1064-1069`、`:1075-1108`），托盘线程不碰业务状态。

### 10.5 选项面板控件与热键录制规则

入口三个：`Ctrl+,`（面板已开时忽略）、帮助面板底部的「选项…」、托盘「选项…」
（`app.rs:2372-2378`、`:1729-1740`、`:1189-1193`）。面板与搜索框 / 列表互斥，铺满整张卡片。

控件自上而下：

1. `启用全局热键` 复选框；
2. 组合键显示框 + `录制` 按钮（`启用全局热键` 取消勾选时整组置灰不可点，`app.rs:1665-1697`）；
3. `开机自启` 复选框；
4. `主题` 三选一：跟随系统 / 浅色 / 深色（`app.rs:1718-1737`）；
5. 面板内红字：保存失败（`save_error`）+ 热键冲突 / 自启写注册表失败（`options_error`）
   （`app.rs:1853-1866`；面板关着时 `options_error` 回落到列表页，`app.rs:1588-1590`）；
6. 底部 `确定` / `取消`。

热键录制规则（`RecordInput::feed`，`app.rs:495-528`，单测 `record_input_captures_skips_and_cancels`）：

- 只看 `Event::Key` 的**按下**，松开事件不参与；
- 单独按修饰键（Shift/Ctrl/Alt）→ 继续等待；单独按 `Tab` → 只移动焦点、不录制也不结束录制；
- `Esc` → 取消录制，保留原来的组合键；
- 必须**至少带一个修饰键**：光秃秃的字母 / 数字做全局热键会把所有程序打字抢走，宁可不录
  （`hotkey_from_key`，`app.rs:416-427`）；
- Win 键不在 egui 的 `Modifiers` 里，靠 `SuperLeft`/`SuperRight` 的按下 / 松开单独维护一位
  （`app.rs:432-434`、`:396-411`）；
- 可录制的键：字母 A–Z、数字 0–9、F1–F24、Space/Tab/Enter/Esc/Backspace/Insert/Delete/Home/End/
  PageUp/PageDown/四个方向键，以及逗号、句点、斜杠、分号、等号、减号、方括号、反斜杠、引号、反引号等标点（`vk_code`，`app.rs:210-321`）；
- 显示格式 `Ctrl → Shift → Alt → Win → Key`，未识别的键码退化成 `键码 0xNN`（`hotkey_text`，`app.rs:371-391`）；
- 录制态下本帧按键只属于录制，不再当 `Ctrl+W` 等快捷键解释（`logic` 第 0.5 步，`app.rs:2241-2245`）。

保存 / 取消：

- `确定`：**先试注册、成功才落盘**——先按 `plan_hotkey_rebind` 判断要不要用草稿里的组合键试注册
  （组合键没变且旧监听还在 → `Keep`，再注册会撞上自己占着的那一个；组合键变了或旧监听不在 →
  `Register`；关掉热键 → `Release`，`app.rs:497`、枚举 `:472`），试注册结果由 `decide_after_register`
  判成 `Persist` / `Rollback`（`app.rs:511`、枚举 `:486`），单测 `hotkey_conflict_rolls_back_before_persist`；
  通过后依次写注册表自启 → 写配置文件 → 应用主题 → 换监听 → 关面板，任一步失败都停在面板里显示红字、
  运行时不动（`save_options`，`app.rs:1046-1129`）。
- `取消`：丢弃草稿，把主题按 `settings.theme` 重算、托盘勾选对齐 `settings.autostart`，热键不动（`cancel_options`，`app.rs:1021-1036`）。
- **热键冲突的处理时序（本轮修补）**：先试注册 → 成功才落盘 → 失败回滚旧监听与草稿 → 面板内红字。
  试注册期间旧监听一直开着（`HotkeyPlan::Register` 只在 `pending` 里暂存新监听，`app.rs:1053-1068`），
  所以不会出现「旧键已注销、新键没注册上」的空窗；判定为 `Rollback` 时把草稿热键退回 `settings.hotkey`、
  退出录制态，`settings` 与 `config.ini` 都保持旧值，`pending` 随函数返回被丢弃、旧监听继续按旧组合键工作，
  同时在 `options_error` 写「热键注册失败：…，未保存」并**留在面板里**（`app.rs:1071-1081`）。
  旧的「先落盘后重绑」导致「冲突时新组合键已写进配置、旧值不保留」的问题**已修**。
- **开机自启失败的提示与状态回退（本轮修补）**：`apply_autostart` 先写 `settings.autostart`，再调
  `enable()` / `disable()` 落注册表；失败时 `log::warn!`、把 `settings.autostart` 退回 `is_enabled()`
  的真实值、写 `options_error = "设置开机自启失败：…"`、同步托盘勾选与面板草稿，最后返回 false
  （`app.rs:948-970`）。面板路径下 `save_options` 见到 false 直接返回：不进落盘、不关面板，用户改完
  再按「确定」即可重试（`app.rs:1086-1089`）；托盘路径下失败时红字回落到列表页（`app.rs:1588-1590`）。
- **托盘「选项…」不丢草稿（本轮修补）**：`open_options` 在 `options_open` 为真时**只把浮层置前 +
  `ViewportCommand::Focus` 并直接返回**，不重建 `OptionsDraft`（`app.rs:998-1004`），
  `TrayCommand::Options` 就走这条早退（`app.rs:1189-1193`）；只有面板本来没开时才 `OptionsDraft::new`
  复制当前设置。于是「面板开着时再点托盘『选项…』→ 未保存的修改被已保存值覆盖」的旧副作用已消除。

### 10.6 明暗两套配色取值

`palette(dark)`（`app.rs:124-163`），浅色沿用改版前的取值，因此不切深色时观感不变：

| 字段 | 浅色 | 深色 | 用途 |
|---|---|---|---|
| `card_fill` | `#FFFFFF` | `#202020` | 卡片 / 面板底色 |
| `card_stroke` | `#E0E0E0` | `#3A3A3A` | 卡片 1px 描边 |
| `search_fill` | `#FFFFFF` | `#1A1A1A` | 搜索框 / 组合键显示框底色 |
| `search_stroke` | `#E0E0E0` | `#3A3A3A` | 搜索框描边 |
| `text_primary` | `#1F1F1F` | `#E8E8E8` | 正文 / 标题 |
| `text_muted` | `#808080` | `#909090` | 进程名、提示文案 |
| `row_selected_fill` | `#CCE4F7` | `#2C4A63` | 选中行填充 |
| `row_selected_stroke` | `#A8CEEE` | `#3E6B8F` | 选中行描边 |
| `row_hover_fill` | `#F5F5F5` | `#2A2A2A` | 悬停行填充 |
| `icon_placeholder` | `#DDDDDD` | `#3C3C3C` | 占位图标方块 |
| `error_text` | `#B01C1C` | `#FF7B72` | 错误红字 |
| `help_fill` | `#FFFFFF` | `#262626` | `?` 帮助面板底色 |
| `help_stroke` | `#E0E0E0` | `#3A3A3A` | 帮助面板描边 |
| `closing_text` | `#A6A6A6` | `#5E5E5E` | 关闭中条目的标题 / 进程名 |
| `closing_icon` | `#EEEEEE` | `#2E2E2E` | 关闭中条目的占位图标 |

`apply_theme` 以 `egui::Visuals::dark()` / `light()` 为底，用上表覆盖 `panel_fill` / `window_fill` /
`extreme_bg_color` / `faint_bg_color` / `window_stroke` / `selection` / `override_text_color` 等，
再 `set_theme` + `set_visuals_of` 钉住当前主题，所以系统切明暗不会把浮层配色换掉（`app.rs:802-832`）。

主题来源：`dark_for(theme, is_system_dark())`（`app.rs:175-181`，单测 `theme_resolution_and_toggle`）。
`is_system_dark()` 读 `HKCU\...\Themes\Personalize` 下的 DWORD `AppsUseLightTheme`：`0` = 深色、
`1` = 浅色；读不到键 / 值、类型不是 DWORD、或取到 0/1 之外的脏数据，一律按**浅色**降级
（`crates/platform-win/src/system_theme.rs:11-23`、`:37-92`，单测 `missing_or_dirty_value_degrades_to_light`）。

两个「切换」入口都会把偏好从 `system` 拽出来，落成显式的 `light` / `dark`：`Ctrl+T`
（`app.rs:2263-2276`）与托盘「切换明暗主题」（`toggled_theme`，`app.rs:186-192`）——`System` 时先按
系统实际观感落到一侧再翻面，然后写盘。

### 10.7 居中的坐标口径

`center_on_current_monitor` → `centered_outer_position`（`app.rs:1904-1933`、`:2009-2021`，单测
`centered_position_uses_monitor_grid` / `centered_position_degrades_without_monitor_info`）：

1. **窗口尺寸**：优先 `ViewportInfo.outer_rect.size()`——egui-winit 已按 `pixels_per_point` 折算成
   逻辑点，与 `OuterPosition` 的单位一致，**不能再乘一次 DPI**；拿不到或退化成 0 时用 `600×640`
   （`window_size_of`，`app.rs:1977-1982`）。
2. **监视器原点**：egui 0.35 的 `ViewportInfo` 只给 `monitor_size`、不给原点，于是把虚拟桌面当作
   「按监视器尺寸平铺的网格」：用窗口中心的当前坐标算出落在第几格，反推该格左上角
   （`monitor_origin`，`app.rs:1990-2003`）。单显示器与常见左右 / 上下等尺寸拼接都正确；
   不规则拼接可能偏一格，但下一步会把结果夹回范围内。
3. **落点**：`origin + (monitor - window).max(0) / 2`，四舍五入到整逻辑像素，再 `clamp` 到
   `[origin, origin + slack]`——窗口比监视器还大时退化成左上角对齐，绝不会被推到屏幕外。
4. **时序**：`sync_window_visibility` 在「从隐藏变可见」的同一帧里**先居中、后 `Visible(true)` + `Focus`**
   （`app.rs:1940-1969`）；拿不到监视器信息就不设位置（保持系统默认），最多重试 3 帧
   （`CENTER_RETRY_MAX`），超过就放弃并在日志里 `warn`。
5. 每次唤起都会重新加载快照 + 重新居中；收起时清掉待居中标记，下次唤起重新排程。

### 10.8 系统字体探测与「不再随包携带字体」

`crates/app/src/fonts.rs` 在 `SwitcherooApp::new` 第一件事就注入字体（`app.rs:688`）：

| 次序 | 字体 | 说明 |
|---|---|---|
| 1 | `C:\Windows\Fonts\msyh.ttc`（face 0） | 微软雅黑，观感最接近原版，命中即停 |
| 2 | `C:\Windows\Fonts\simhei.ttf` | 黑体，雅黑读不到时兜底 |
| 3 | `C:\Windows\Fonts\simsun.ttc` | 宋体，最后兜底 |
| 粗体 | `C:\Windows\Fonts\msyhbd.ttc` | 雅黑 Bold，注册成 `FontFamily::Name("bold")`；读不到就复用普通中文字体 |

（候选表 `fonts.rs:36-55`，探测 `fonts.rs:87-157`，单测 `load_returns_none_for_missing_file`、
`install_returns_false_without_available_font`、`bold_family_falls_back_to_proportional`。）

- 普通字体是**追加到** `Proportional` / `Monospace` 回退链**末尾**：拉丁字符继续用 egui 自带字体，
  只有缺字（中文）才落到系统字体；
- 系统字体全找不到时**不注册** `bold` 族（epaint 对「没有绑定字体的族」会直接 panic），
  `bold_family()` 回退到默认比例族，命中段只是不加粗，界面照常运行；
- **`crates/app/assets/` 已整目录删除**，仓库里再无字体二进制：包体 zip 由 ~37MB 降到 7.3MB，
  发布目录只剩 `switcheroo.exe` + `README.txt`；
- 代价：字体不在包内，依赖目标机器自带 Windows 中文字体（这三份是 Windows 10/11 的标配，
  精简版 / 非中文语言包系统上可能缺字，见 10.10）。

### 10.9 本轮真机证据（启动期日志）

| 日志 | 说明 |
|---|---|
| `已读取配置：启用热键 true（Alt + Space），自启 false，主题 System` | 启动读配置生效，且与首启默认值一致（`main.rs:24-30`） |
| `使用系统中文字体：C:\Windows\Fonts\msyh.ttc（face 0）` | 系统字体探测命中第 1 候选（`fonts.rs:108-112`） |
| `粗体使用系统中文字体：C:\Windows\Fonts\msyhbd.ttc（face 0）` | 粗体命中雅黑 Bold（`fonts.rs:129-133`） |
| `全局热键 Alt + Space 注册成功` | 配置里的组合键真的注册成功（`app.rs:876`） |
| `窗口加载完成：快照 19 个窗口，选中第 1 项` | 启动即加载 + 默认选中第 1 项（`app.rs:1116-1152`、`:785-786`） |
| `首帧耗时 28.0 ms，窗口数 19` | 19 个窗口的首帧基准（`app.rs:1451-1458`） |
| 发布 zip 7.3MB、目录内只剩 exe + README | 字体不再随包（10.8） |

### 10.10 待人工确认

| 待确认项 | 当前状态 | 说明 |
|---|---|---|
| 浮层是否真的居中 | 待人工确认 | 本轮远程桌面锁屏 / 空白，截图不可用；逻辑与单测覆盖坐标口径（10.7），未目测落点 |
| 多显示器 / 不规则拼接下的居中落点 | 待人工确认 | `monitor_origin` 用网格近似，拼接不规则时可能偏一格（结果仍被夹回监视器范围） |
| 选项面板的实际观感 | 待人工确认 | 控件与布局代码齐备（10.5），未截图 |
| 热键录制的真机交互 | 待人工确认 | 规则与单测齐备（10.5），未在真机按过组合键 / `Esc` |
| 托盘图标与右键菜单 | 待人工确认 | `NIM_ADD` 与菜单布局单测通过（10.4），未目测图标、菜单与勾选态 |
| 暗色配色的实际观感 | 待人工确认 | 取值见 10.6，未截图；对比度与图标在深色底上的可读性未目测 |
| 勾选「开机自启」后注册表是否真的写入 | 待人工确认 | `RegSetValueExW(REG_SZ)` 代码路径完整（10.3），未用 `reg query` 复核 |
| 热键冲突时的提示与回退 | 已修补，待真机验证 | 改为「先试注册 → 成功才落盘」，冲突时不写配置、草稿热键退回旧值、旧监听继续工作，红字「…，未保存」显示在**选项面板内**（10.5，US-047 第 2 条）；未在真机构造被占用组合键 |
| 开机自启写失败的界面提示 | 已修补，待真机验证 | 失败时回退 `settings.autostart` 到 `is_enabled()`、面板内红字「设置开机自启失败：…」、面板保持打开可重试且不落盘；托盘路径下红字在列表页（10.5，US-048 第 3 条）；未在真机构造注册表写失败 |
| 托盘「选项…」不丢草稿 | 已修补，待真机验证 | 面板已开时 `open_options` 只置前 + 聚焦、不重建草稿（10.5）；未在真机操作「编辑中 → 托盘再点选项」 |
| 非中文语言包 / 精简版 Windows 的字体兜底 | 待人工确认 | 三个候选都缺失时界面不 panic，但中文会显示为方块 |
| `About / 关于` 菜单项 | 未实现 | 属 US-052，本轮托盘菜单只有打开 / 选项 / 自启 / 切换明暗 / 退出 |
| Alt+Tab、AutoSwitch、管理员模式相关配置与联动 | 未实现 | 分别属 US-050 / US-051 / US-049，配置里暂无字段（US-046 第 1 条、US-047 第 3 条因此未达标） |

## 11. 拼音首字母检索（US-045）

验证环境：Windows 10，2560×1440，交互会话，本轮实测。本节记录依赖取舍、匹配回退顺序、
区间换算方式、基准数据与真机证据，末尾列出仍需人工确认的项。

### 11.1 依赖与 feature 选择

`crates/core/Cargo.toml:17`：

```toml
pinyin = { version = "0.10", default-features = false, features = ["plain", "heteronym"] }
```

- **为什么用 crate 而不是内置表**：汉字 → 拼音要覆盖四万多个码位并维护异读，内置表等于把一份
  词表搬进仓库还要自己校验；`pinyin` 0.10 是 MIT、无 `unsafe`、**零第三方依赖**（`Cargo.lock`
  里该包没有 `dependencies` 段），只带一份 `pinyin-data/pinyin.txt` 数据表。
- **为什么 `default-features = false`**：默认开的 `default` 会连带 `compat`、`with_tone`、
  `with_tone_num`、`with_tone_num_end`，这些是声调与兼容 API 用的，本项目只要首字母，全关可以
  少编几张表。
- **为什么必须开 `heteronym`**：`ToPinyinMulti` 与整张异读表都由这个 feature 门控
  （crate 内 `src/lib.rs:13-15`、`src/pinyin_multi.rs:1-6`）。`乐` 的数据是 `lè,yuè`，只取首读音
  只能得到 `l`，`yy` 永远命中不了「音乐」；这条追加读音正是 `heteronym` 提供的。
- **为什么还必须同时开 `plain`**：`Pinyin::first_letter()` 由 `plain` 门控
  （crate 内 `src/pinyin.rs:68-72`），而 `heteronym` 并不隐含 `plain`，两者是并列关系——
  只开 `heteronym` 拿得到读音字符串却拿不到首字母。所以最小可用组合就是 `plain` + `heteronym`。

首字母投影实现在 `crates/core/src/pinyin.rs`：

| 入口 | 行号 | 每个汉字贡献的首字母 |
|---|---|---|
| `initials_with_map` | `pinyin.rs:21` | 只取首读音（`文件` → `wj`） |
| `initials_with_all_readings` | `pinyin.rs:31` | 全部读音，同一汉字内去重（`音乐` → `yly`） |
| `project`（共同实现） | `pinyin.rs:44` | 汉字走 `ToPinyinMulti`，非汉字 `to_lowercase` 原样追加 |

### 11.2 三级回退顺序

匹配仍然只挂在**逐字匹配器**（`IndividualCharactersMatcher`，得分 1）上，前缀 4 / 单词首字母 2 /
子串包含 2 三档与排序键完全没动，所以英文文本的分数与收窄行为与改动前一致。
`individual_characters_match`（`crates/core/src/matcher.rs:398-402`）按三级依次尝试：

1. **原文逐字**（`ordered_char_ranges`，`matcher.rs:418`）：`query` 的字符按顺序散落在原文里即可，
   命中区间直接就是原文字节区间。原文能凑出来就到此为止，不再看拼音。
2. **首读音首字母串**（`initials_ranges` + `initials_with_map`，`matcher.rs:447`）：`wj` →「文件」。
   纯 ASCII 文本在进入第 2 级之前就被 `pinyin_ranges` 开头的 `matcher.rs:443-445` 直接跳过——
   ASCII 的拼音投影只是转小写，与第 1 级的 `eq_ignore_case` 等价，省掉一次投影与映射表分配。
3. **全部读音首字母串**（`initials_ranges` + `initials_with_all_readings`，`matcher.rs:452`）：
   只在第 2 级落空时放开异读，`yy` →「音乐」的 `乐`（`yuè`）由这条兜住。

代价是第 3 级会把生僻异读也算进命中面：拼音数据里 `件` 除了 `jiàn` 还有异读 `móu`
（`pinyin-data/pinyin.txt` 的 `U+4EF6: jiàn,móu`），因此 `wm` 也能命中「文件」。这是刻意用
「先严后松」的两级顺序换来的——常见输入仍走第 2 级，只有第 2 级全落空才会放宽。

### 11.3 区间换算（首字母串 → 原文）

`project`（`pinyin.rs:44-79`）在生成首字母串的同时登记映射表 `Vec<(usize, usize)>`，条目是
`(首字母串中的字节起点, 原文中该字符的字节起点)`：

- 逐 `char_indices` 扫原文，每个字符新写入几个首字母就登记几条，因此异读的多个首字母共享同一段
  原文区间（`乐` 的 `l` 与 `y` 都指向 `3..6`）；
- 两侧都只按 `char` 边界写入，区间绝不会切在 UTF-8 中间字节上；
- 拉丁字母、数字、标点原样（转小写）保留，映射指向自己，所以中英混排标题（`文件 - PowerPoint`）
  的命中位置仍然正确。

`original_range`（`pinyin.rs:88-100`）用 `partition_point` 二分定位起点相同的映射条目，再按原文
该字符的 `len_utf8` 补出终点，返回可直接喂给 UI 的 `Range<usize>`；起点不匹配、越界或空表一律
返回 `None`。`initials_ranges`（`matcher.rs:460-478`）把每个命中的首字母换算成原文区间，并把
「同一汉字的多个读音」压成一段，保证 `ranges` 仍然升序、不重叠——这是 US-042 加粗的前置契约，
`crates/app/src/app.rs:1963-1990`（`show_list`）只按 `ranges` 决定加粗哪些字符，`draw_row`（`app.rs:2227`）再把它交给 `highlight_job`，不知道也不关心命中来自哪一级。

界面提示同步更新：搜索框占位文案「搜索窗口，可用「进程.标题」或拼音首字母」
（`crates/app/src/app.rs:1620`），`?` 帮助面板新增一行「wj / yy → 拼音首字母：文件 / 音乐」
（`app.rs:1683`，行表 `HELP_ROWS` 在 `app.rs:1677`）。

### 11.4 单元测试与基准

新增 13 条：`pinyin.rs` 5 条 + `matcher.rs` 8 条。

| 文件 | 用例 | 覆盖点 |
|---|---|---|
| `pinyin.rs:107` | `chinese_text_maps_to_initial_letters` | `文件` → `wj`，映射 `(0,0) (1,3)`，`original_range` → `0..3` / `3..6` |
| `pinyin.rs:120` | `polyphonic_characters_expose_every_reading` | `音乐` 首读音 `yl`、全读音 `yly`，两个首字母共享 `3..6` |
| `pinyin.rs:136` | `mixed_text_keeps_ascii_and_maps_chinese` | `ai-code [hint] 文件` 拉丁段原样、中文段换算正确 |
| `pinyin.rs:153` | `empty_and_ascii_text_are_harmless` | 空串、`PowerPoint` 两个入口都不炸 |
| `pinyin.rs:166` | `unknown_positions_do_not_panic` | 非法起点 / 越界 / 空表返回 `None` |
| `matcher.rs:822` | `individual_characters_prefer_literal_match_over_pinyin` | 原文能命中时不走拼音 |
| `matcher.rs:832` | `individual_characters_fall_back_to_pinyin_initials` | `wj` →「文」「件」 |
| `matcher.rs:843` | `individual_characters_use_polyphonic_readings_when_needed` | `yy` →「音乐」 |
| `matcher.rs:891` | `match_text_hits_chinese_title_by_pinyin_initials` | `match_text("wj", "文件 - PowerPoint")` 得分 1、区间合并成 `0..6` |
| `matcher.rs:901` | `match_text_hits_polyphonic_title_by_pinyin_initials` | `yy` →「音乐播放器」 |
| `matcher.rs:912` | `match_text_keeps_ascii_results_unchanged_by_pinyin` | `pp` → `PowerPoint` 分数与区间不变 |
| `matcher.rs:918` | `match_text_empty_query_is_unaffected_by_pinyin` | 空查询不受投影影响 |
| `matcher.rs:1248` | `composite_query_matches_title_by_pinyin_initials` | readme `p.wj` 场景：进程命中 + 标题按拼音命中 |

基准（`matcher.rs:1366` 的 `bench_filter_1000_windows_under_16ms`，1000 窗口、查询 `chr`）：

| 阶段 | 实测 | 说明 |
|---|---|---|
| US-041 改动后（拼音之前） | 1.862 / 1.893 ms | 第 8 节记录 |
| US-045 改动后 | **3.283 / 3.780 ms**（两次，均为 167 条结果） | 本轮 `cargo test -p switcheroo-core --release -- --ignored --nocapture` |

净增 1.4 ~ 1.9 ms：`chr` 在中文标题上凑不出原文逐字，会落到拼音投影 + 映射表分配，这是最坏路径；
仍远低于 16 ms 的门槛。

### 11.5 本轮真机证据

| 观察项 | 结论 | 证据 |
|---|---|---|
| 拼音首字母检索可用 | 通过：输入 `dy` 后列表命中「电影和电视」（UWP 窗口），截图为深色主题下的卡片 | 截屏 |
| 搜索框提示文案 | 通过：搜索框内显示「搜索窗口，可用「进程.标题」或拼音首字母」 | 截屏 |
| 单测与回归 | 通过：`cargo test --workspace` 全绿——app 30、core 126（+1 ignored）、platform-win 38；core 新增 13 条见 11.4 | 单元测试 |
| 配置 / 热键 / 自启 / 主题（同批真机） | 通过：用户把热键改成 `Win + 反引号键`、开了开机自启、主题设深色，日志「已读取配置：启用热键 true（Win + &#96;），自启 true，主题 System」与「全局热键 Win + &#96; 注册成功」 | 运行期日志 |
| 浮层居中口径（同批真机） | 通过：日志「浮层居中：监视器 Some([2560.0 1440.0])，窗口 [600.0 640.0]，左上角 (980, 400)」；(2560−600)/2 = 980、(1440−640)/2 = 400，与落点一致 | 运行期日志 |
| 首帧耗时（同批真机） | 记录：17 个窗口首帧 133 ms，冷启动首帧 36.6 ms。与第 8 节「20 窗口 25.1 ms」差异很大，属首帧测量本身波动（枚举 + 图标取图），不归因于拼音，也非 US-045 的验收项 | 运行期日志 |

### 11.6 待人工确认

| 待确认项 | 当前状态 | 说明 |
|---|---|---|
| 命中中文字符的加粗视觉是否明显 | 待人工确认 | 区间换算与 `highlight_job` 链路有单测（11.3），但截图中未能肉眼看出「电影和电视」里的命中字更粗——可能被等宽 / 雅黑 Bold 的字重差掩盖 |
| 多音字放宽带来的轻微误命中 | 待人工确认 | 第 3 级会让生僻异读参与匹配：`件` 有异读 `móu`，`wm` 也能命中「文件」；`乐` 有 `lè` / `yuè`，`yl` 与 `yy` 都命中「音乐」。是否收窄需实机观感判断 |
| 进程名为中文的软件 | 待人工确认 | 进程名与标题共用 `match_text`，理论上同样支持；本轮样本里进程名全是英文（msedge / Weixin / WXWork 等），未验证中文进程名 |
| 拼音匹配在复合查询标题侧的界面观感 | 待人工确认 | `p.wj` 有单测（`matcher.rs:1248`），真机只验证了单字段 `dy`，复合查询 + 拼音高亮的实际显示未截图 |
| 英文输入的真机回归 | 待人工确认 | 拼音接入后纯 ASCII 走快路径跳过投影（`matcher.rs:443-445`），单测覆盖 `pp` → `PowerPoint`；本轮真机未重新敲英文用例 |
