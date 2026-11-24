# Switcheroo 功能用户故事（反向梳理）

> 分析对象：`/root/job/ai-code/Switcheroo`（C# / WPF / .NET 4.x，Windows 桌面工具）
> 文档目的：以用户故事形式还原现有全部功能，作为后续重写（如 Rust 版 `RustSwitcheroo`）的需求基线。
> 编写方式：先给主干流程与功能全景，再按 Epic 拆分为可独立验收的用户故事。

---

## 一、产品定位

Switcheroo 是面向"键盘优先"用户的 Windows 增量搜索任务切换器。它用"输入窗口标题/进程名的少量字符"替代"连续 Alt+Tab 翻页"，可在悬浮窗内完成窗口搜索、选择、切换与关闭。

- 核心形态：常驻托盘 + 全局热键唤起的无边框白色浮层。
- 与系统关系：可完全接管 `Alt+Tab`，窗口列表口径对齐系统 Alt+Tab。
- 特色能力：进程名与窗口名分段检索（`.` 分隔）、中文标题拼音首字母检索、匹配字符加粗。

---

## 二、主干流程

### 2.1 启动流程

| 步骤 | 函数 | 业务效果（直白描述） |
|---|---|---|
| 1 | `Program.Main` (`Program.cs:35`) | 若配置了"以管理员运行"且当前非管理员，则以 `runas` 重新拉起进程，当前进程退出 |
| 2 | `Program.Main` (`Program.cs:40-53`) | 通过命名互斥量保证单实例，已有实例在跑则新进程静默退出 |
| 3 | `Program.MakePortable` (`Program.cs:97`) | 便携版（`PORTABLE` 编译符号）把配置写到程序目录而非用户配置目录 |
| 4 | `Program.MigrateUserSettings` (`Program.cs:108`) | 首次运行把旧版本用户设置升级迁移，之后不再迁移 |
| 5 | `MainWindow` 构造 (`MainWindow.xaml.cs:65`) | 注册快捷键、托盘图标、全局热键、Alt+Tab 钩子，并异步检查更新；窗口初始透明 |

### 2.2 唤起流程

```
全局热键 Alt+Space ──> hotkey_HotkeyPressed (MainWindow.xaml.cs:439)
Alt+Tab 钩子       ──> AltTabPressed        (MainWindow.xaml.cs:463)
                          │
                          ├─ 记录当前前台窗口 _foregroundWindow
                          ├─ 枚举窗口 LoadData()  (MainWindow.xaml.cs:264)
                          │    ├─ WindowFinder.GetWindows()  (Core/WindowFinder.cs:28)
                          │    │    └─ AppWindow.IsAltTabWindow() 过滤 (Core/AppWindow.cs:122)
                          │    ├─ 前台窗口移到列表底部
                          │    ├─ 全量文本做一次高亮格式化为 XAML
                          │    └─ 选中首项 / 末项，输入框聚焦，窗口居中
                          └─ Opacity=1 显示浮层
```

### 2.3 检索流程

```
输入框 TextChanged (MainWindow.xaml.cs:552)
  └─ 解析查询串：'进程.标题' 冒号前为进程过滤 (Core/WindowFilterer.cs:35-65)
      └─ 对每个窗口的标题、进程名分别跑 4 个匹配器 (WindowFilterer.cs:98-114)
          ├─ StartsWithMatcher           前缀匹配     得分 4
          ├─ SignificantCharactersMatcher 单词首字母  得分 2
          ├─ ContainsMatcher             子串包含     得分 2
          └─ IndividualCharactersMatcher 逐字顺序+拼音首字母 得分 1
      └─ 按"标题得分+进程得分"降序排序输出 (WindowFilterer.cs:86)
  └─ 用最佳匹配结果生成加粗高亮 XAML 绑定到列表项 (XamlHighlighter.cs:30)
```

### 2.4 操作流程

| 用户动作 | 入口 | 效果 |
|---|---|---|
| 回车 / 双击 | `Switch` (`MainWindow.xaml.cs:344`) | 切换到选中窗口（多选则依次切换），随后隐藏浮层 |
| `Ctrl+W` / `Alt+X` | `CloseWindow` (`MainWindow.xaml.cs:603`) | 异步关闭选中窗口，成功则从列表移除，浮层保持打开 |
| `Esc` / 失焦 / 再按热键 | `HideWindow` (`MainWindow.xaml.cs:355`) | 关闭窗口等待器并隐藏浮层 |
| `↑/↓`、`Alt+K/J`、`Tab/Shift+Tab` | `PreviousItem` / `NextItem` (`MainWindow.xaml.cs:646,669`) | 选中项上下移动，越界循环，自动滚动到可视区 |

---

## 三、功能全景

| Epic | 功能 | 关键实现 |
|---|---|---|
| EP-01 常驻与启动 | 单实例、托盘菜单、开机自启、管理员模式、设置迁移、便携模式 | `Program.cs`、`MainWindow.xaml.cs:164-202`、`AutoStart.cs`、`PortableSettingsProvider.cs` |
| EP-02 唤起与热键 | 全局热键、自定义热键、冲突提示、Alt+Tab 接管、AutoSwitch | `HotKey.cs`、`AltTabHook.cs`、`OptionsWindow.xaml.cs` |
| EP-03 窗口采集 | Alt+Tab 等价窗口过滤、UWP 处理、图标获取、前台窗口置底 | `Core/WindowFinder.cs`、`Core/AppWindow.cs`、`Core/WindowIconFinder.cs` |
| EP-04 检索与排序 | 增量搜索、进程/标题复合检索、拼音首字母、结果高亮、评分排序 | `Core/WindowFilterer.cs`、`Core/Matchers/*`、`Core/XamlHighlighter.cs` |
| EP-05 结果操作 | 键盘导航循环、切换窗口、关闭窗口、浮层关闭 | `MainWindow.xaml.cs`、`WindowCloser.cs` |
| EP-06 浮层界面 | 无边框居中浮层、帮助面板、关闭淡出动画、长列表滚动 | `MainWindow.xaml`、`MainWindow.xaml.cs:705-717` |
| EP-07 设置与帮助 | 选项窗口、关于窗口、每日更新检查 | `OptionsWindow.xaml(.cs)`、`AboutWindow.xaml(.cs)`、`MainWindow.xaml.cs:204-259` |
| EP-08 质量保障 | 匹配器与高亮器单元测试、安装包、Chocolatey 分发 | `Core.UnitTests/*`、`Installer/*`、`ChocolateyPackages/*` |

---

## 四、用户故事

### EP-01 常驻与启动

#### US-001 单实例常驻
**描述：** 作为用户，我只想运行一个 Switcheroo 实例，避免重复注册热键、出现多个托盘图标。
**验收标准：**
- [ ] 重复启动时第二个进程在 5 秒内检测到互斥量并静默退出（`Program.cs:40-53`）
- [ ] 已有实例异常退出遗留的互斥量（`AbandonedMutexException`）可被新进程接管
- [ ] 程序启动后不显示主窗口，仅在托盘出现图标（`MainWindow` 初始 `Opacity=0`）

**涉及代码：** `Program.cs:33-73`

#### US-002 托盘右键菜单
**描述：** 作为用户，我希望不打开浮层也能进入设置、开机自启、关于和退出。
**验收标准：**
- [ ] 托盘图标常驻，悬浮提示为 "Switcheroo"（`MainWindow.xaml.cs:164-186`）
- [ ] 菜单包含 `Options`、`Run on Startup`、`About`、`Exit`
- [ ] `Run on Startup` 为复选项，勾选状态实时反映启动文件夹中是否存在快捷方式
- [ ] 选择 `Exit` 会释放托盘图标与全局热键后退出应用（`MainWindow.xaml.cs:418-424`）

**涉及代码：** `MainWindow.xaml.cs:164-202, 418-424`

#### US-003 开机自动启动
**描述：** 作为重度用户，我希望开机后 Switcheroo 已在托盘中待命，不用手动打开。
**验收标准：**
- [ ] 开启时在用户启动文件夹创建指向当前程序的快捷方式（`AutoStart.cs:86-118`）
- [ ] 关闭时删除该快捷方式（`AutoStart.cs:73-84`）
- [ ] 创建失败（权限/路径异常）时弹出错误提示，不崩溃（`MainWindow.xaml.cs:188-202`）

**涉及代码：** `AutoStart.cs:31-119`

#### US-004 以管理员权限运行
**描述：** 作为需要切换管理员窗口的用户，我希望 Switcheroo 以高权限运行时 Alt+Tab 对高权限程序同样有效。
**验收标准：**
- [ ] 选项窗口提供 `Run as Administrator` 开关，设置持久化（`OptionsWindow.xaml.cs:69, 109`）
- [ ] 已开启但当前非管理员时，启动阶段以 `runas` 重新拉起并退出旧进程（`Program.cs:75-90`）
- [ ] 该设置标注"需重启 Switcheroo"生效

**涉及代码：** `Program.cs:75-123`、`OptionsWindow.xaml:32-38`

#### US-005 设置迁移与便携模式
**描述：** 作为升级用户/便携用户，我希望升级不丢配置，且可以把配置放在程序目录随 U 盘带走。
**验收标准：**
- [ ] 首次运行（`FirstRun=true`）时调用 `Settings.Upgrade()` 并清除标记（`Program.cs:108-115`）
- [ ] 便携编译版本通过 `PortableSettingsProvider` 将设置读写到程序目录（`Program.cs:97-106`、`PortableSettingsProvider.cs`）
- [ ] 常规版本设置项包括热键组合、Alt+Tab 开关、AutoSwitch、RunAsAdmin 等（`Properties/Settings.Designer.cs`）

**涉及代码：** `Program.cs:97-115`、`PortableSettingsProvider.cs`

---

### EP-02 唤起与热键

#### US-006 全局热键唤起与隐藏
**描述：** 作为键盘用户，我希望按一个全局快捷键随时呼出搜索浮层，再按一次收起。
**验收标准：**
- [ ] 默认热键为 `Alt + Space`（`Settings.HotKey=32`、`Alt=true`）
- [ ] 浮层不可见时按下热键：显示窗口、置前、聚焦输入框、加载窗口列表并淡入
- [ ] 浮层可见时再次按下热键：隐藏浮层（`MainWindow.xaml.cs:439-461`）
- [ ] 关闭热键功能后按下组合键无任何反应（`Settings.EnableHotKey`）

**涉及代码：** `MainWindow.xaml.cs:136-156, 439-461`、`HotKey.cs:25-42`

#### US-007 自定义唤醒热键
**描述：** 作为用户，我希望把唤醒热键改成不冲突的组合（含 Win 键）。
**验收标准：**
- [ ] 选项窗口的快捷键输入框可捕获组合键并显示为 `Ctrl + Shift + Alt + Win + Key` 形式（`OptionsWindow.xaml.cs:118-156, 166-205`）
- [ ] 单独按 `Tab` 不记为热键，而是把焦点移到下一个控件
- [ ] 仅按修饰键不改变显示值
- [ ] 点击 OK 后新热键立即生效并写入设置（`OptionsWindow.xaml.cs:77-116`）
- [ ] 点击 Cancel 不改变原热键

**涉及代码：** `OptionsWindow.xaml.cs:54-70, 77-116, 118-156`

#### US-008 热键冲突提示
**描述：** 作为用户，如果我选的热键被别的程序占用，我需要被明确告知而不是静默失败。
**验收标准：**
- [ ] 启动时注册失败弹出 "Hotkey already in use" 提示并说明去托盘 Options 修改（`MainWindow.xaml.cs:144-155`）
- [ ] 在选项中设置失败时提示 "Shortcut already in use"，且选项窗口保持打开以便改选（`OptionsWindow.xaml.cs:98-104`）
- [ ] 编辑热键输入框期间临时注销全局热键，失焦后重新注册（`OptionsWindow.xaml.cs:208-224`）

**涉及代码：** `MainWindow.xaml.cs:144-155`、`OptionsWindow.xaml.cs:98-104, 208-224`

#### US-009 接管系统 Alt+Tab
**描述：** 作为用户，我希望 Alt+Tab 直接打开 Switcheroo，而不是 Windows 自带任务切换器。
**验收标准：**
- [ ] 选项窗口提供 `Activate Switcheroo with Alt+Tab` 开关，默认关闭
- [ ] 开关关闭时 Alt+Tab 事件被完全忽略，保持系统行为（`MainWindow.xaml.cs:465-469`）
- [ ] 开关开启时拦截 Alt+Tab，标记事件已处理并显示浮层（`AltTabHook.cs:59-105`、`MainWindow.xaml.cs:479-505`）
- [ ] `Alt+Shift+Tab` 打开浮层并选中列表最后一项（向后切换语义）
- [ ] 检测到系统多任务视图（`MultitaskingViewFrame`）时不重复打开浮层（`MainWindow.xaml.cs:473-477`）
- [ ] 浮层打开时继续按 `Alt+Tab` / `Alt+Shift+Tab` 在列表中前进/后退
- [ ] 名单口径与系统 Alt+Tab 对齐（隐藏、无标题、工具窗口、`NOACTIVATE`、`ITaskList_Deleted`、UWP 幽灵窗口均排除）

**涉及代码：** `MainWindow.xaml.cs:158-162, 463-517`、`AltTabHook.cs`、`Core/AppWindow.cs:122-228`

#### US-010 AutoSwitch：松手即切换
**描述：** 作为习惯系统 Alt+Tab 的用户，我希望按住 Alt 翻到目标窗口后松开 Alt 就直接切过去，像原生一样快。
**验收标准：**
- [ ] 选项窗口提供 `Automatically switch window when releasing Alt + Tab`，仅在 Alt+Tab 开关开启时可勾选（`OptionsWindow.xaml:21-31`、`OptionsWindow.xaml.cs:226-235`）
- [ ] 开启后按下 Alt+Tab 时输入框禁用并显示 "Press Alt + S to search"（`MainWindow.xaml.cs:497-502`）
- [ ] 释放 `Alt` 时自动切换到当前高亮窗口并隐藏浮层（`MainWindow.xaml.cs:125-132`）
- [ ] 按下 `Alt+S` 解除 AutoSwitch，恢复输入框可搜索（`MainWindow.xaml.cs:105-111`）
- [ ] 使用 `Ctrl+Alt+Tab` 呼出时直接进入搜索模式，不自动切换（`MainWindow.xaml.cs:497`）
- [ ] AutoSwitch 模式下 `Ctrl+W` 等关闭操作不再触发自动切换（`AltTabPressed` 与 `KeyUp` 的修饰键判断）

**涉及代码：** `MainWindow.xaml.cs:88-134, 463-517`、`OptionsWindow.xaml.cs:226-235`

---

### EP-03 窗口采集

#### US-011 采集"该出现在 Alt+Tab 里的窗口"
**描述：** 作为用户，我希望列表只包含真正可切换的应用窗口，不要混入工具条、隐藏窗口和幽灵窗口。
**验收标准：**
- [ ] 枚举所有顶层窗口后逐个用 `IsAltTabWindow()` 过滤（`Core/WindowFinder.cs:28-33`）
- [ ] 不可见窗口、无标题窗口直接排除
- [ ] 带 `APPWINDOW` 扩展样式的窗口直接保留
- [ ] 工具窗口、`NOACTIVATE` 窗口、拥有可见 Owner 的窗口、带 `ITaskList_Deleted` 属性的窗口排除
- [ ] Windows 10 UWP 的 `Windows.UI.Core.CoreWindow` 去重排除
- [ ] `ApplicationFrameWindow` 按 `ApplicationViewCloakType` 判断：值 1（程序已退出）排除，0/2 保留

**涉及代码：** `Core/WindowFinder.cs`、`Core/AppWindow.cs:122-228`

#### US-012 UWP 进程名识别
**描述：** 作为用户，我希望能按真实应用名（而不是 `ApplicationFrameHost`）检索和识别 UWP 应用。
**验收标准：**
- [ ] `ApplicationFrameWindow` 窗口的进程名取子窗口中不同 PID 的进程名，取不到时降级为 `UWP`（`Core/AppWindow.cs:38-67`）
- [ ] 进程名解析结果缓存 1 小时，避免重复开进程句柄（`Core/AppWindow.cs:42-64`）
- [ ] 常规窗口的进程名取 `Process.ProcessName`

**涉及代码：** `Core/AppWindow.cs:38-67`

#### US-013 显示窗口图标
**描述：** 作为用户，我希望列表左侧显示每个窗口的图标，便于肉眼快速定位。
**验收标准：**
- [ ] 优先通过 `WM_GETICON`（100ms 超时，挂起即放弃）取大/小图标（`Core/WindowIconFinder.cs:42-45`）
- [ ] 取不到时退回窗口类的 `GCLP_HICON/GCLP_HICONSM`
- [ ] 仍取不到时退回可执行文件图标；进程无权限时不应崩溃
- [ ] 图标异步加载，加载期间使用缓存图标兜底（`MainWindow.xaml:169-180` 的 `PriorityBinding`）
- [ ] 图标获取失败不影响该窗口出现在列表中

**涉及代码：** `Core/WindowIconFinder.cs`、`MainWindow.xaml:169-180`、`WindowHandleToIconConverter.cs`、`WindowHandleToCachedIconConverter.cs`

#### US-014 前台窗口排到最后
**描述：** 作为用户，我希望列表里最上面的就是"下一个窗口"，这样按下 Alt+Tab 再回车即可切换，与系统习惯一致。
**验收标准：**
- [ ] 若枚举结果首项与唤起前的前台窗口同 HWND 或同进程，则移到列表末尾（`MainWindow.xaml.cs:270-278, 301-304`）
- [ ] 正常唤起时默认选中列表第 1 项（`MainWindow.xaml.cs:306-322`）
- [ ] `Alt+Shift+Tab` 唤起时选中列表末项；若前台窗口已被移到底部，则选中倒数第 2 项
- [ ] 前台窗口信息取自唤起瞬间的 `SystemWindow.ForegroundWindow`

**涉及代码：** `MainWindow.xaml.cs:264-322`

---

### EP-04 检索与排序

#### US-015 按标题与进程名增量检索
**描述：** 作为用户，我只需输入窗口标题或进程名里的几个字符，就能实时过滤窗口列表。
**验收标准：**
- [ ] 每次输入变化立即重新过滤，无确认键（`MainWindow.xaml.cs:552-583`）
- [ ] 普通模式下，标题命中**或**进程名命中即保留该窗口（`WindowFilterer.cs:80-83`）
- [ ] 过滤后自动选中第 1 条结果
- [ ] 查询为空时展示全部窗口（4 个匹配器对空串均判为命中或首项直接返回全量）

**涉及代码：** `Core/WindowFilterer.cs:69-96`、`MainWindow.xaml.cs:552-583`

#### US-016 进程 + 标题复合检索
**描述：** 作为用户，我希望能限定"只看某个程序的窗口"，再在这个程序里搜标题。
**验收标准：**
- [ ] 输入 `进程片段.标题片段` 时，`.` 前部分只匹配进程名，`.` 后部分匹配窗口标题（`WindowFilterer.cs:35-41, 64-65`）
- [ ] 复合模式下必须**同时**满足进程命中与标题命中（`WindowFilterer.cs:84`）
- [ ] `.` 前为空（如 `.报告`）时，进程过滤自动取当前前台窗口的进程名（`WindowFilterer.cs:40-43`）
- [ ] 内置进程别名：`e`→`excel`、`f`→`explorer`、`w`→`word`、`p`→`powerpnt`（`WindowFilterer.cs:44-63`）
- [ ] 进程名片段同样支持逐字/拼音首字母匹配（进程名与标题共用同一套 `Score`）

**涉及代码：** `Core/WindowFilterer.cs:30-96`

#### US-017 中文标题按拼音首字母检索
**描述：** 作为中文用户，我希望输入拼音首字母就能找到中文标题的窗口（如 `wj` 命中"文件"）。
**验收标准：**
- [ ] 逐字匹配前先把待匹配串转换为拼音首字母串（`IndividualCharactersMatcher.cs:47-53`）
- [ ] 支持按输入字符顺序非连续命中（正则 `(.*?)(x)([^x]*?)(y)...`）
- [ ] 高亮时能把首字母串的命中位置映射回原中文串的对应字符（`IndividualCharactersMatcher.cs:26-40`）
- [ ] 英文标题不受影响（首字母转换对英文等价于原串）
- [ ] 大小写不敏感

**涉及代码：** `Core/Matchers/IndividualCharactersMatcher.cs`

#### US-018 匹配字符加粗高亮
**描述：** 作为用户，我希望命中的字符在结果里加粗显示，方便确认为什么这条被匹配。
**验收标准：**
- [ ] 标题与进程名分别取"最佳匹配结果"生成高亮（`MainWindow.xaml.cs:585-589`）
- [ ] 高亮结果是一段 XAML 片段，命中部分包 `<Bold>`，未命中部分原样保留（`XamlHighlighter.cs:30-47`）
- [ ] 通过 `FormattedTextAttribute` 附加属性把 XAML 渲染进 `TextBlock`（`FormattedTextAttribute.cs`、`MainWindow.xaml:181-195`）
- [ ] "最佳"的定义：按匹配器顺序取第一个 `Matched=true` 的结果，全部未命中时取第一个结果
- [ ] 特殊字符（`<`、`&` 等）不会破坏 XAML 结构（`XDocument` 自动转义）

**涉及代码：** `Core/XamlHighlighter.cs`、`Core/Matchers/MatchResult.cs`、`Core/Matchers/StringPart.cs`、`MainWindow.xaml.cs:585-589`

#### US-019 结果按匹配质量排序
**描述：** 作为用户，我希望"前缀命中"排在最前，"包含命中"其次，模糊命中最后。
**验收标准：**
- [ ] 每个窗口对标题和进程名各跑 4 个匹配器，取各自分数（`WindowFilterer.cs:98-114`）
- [ ] 分值为：前缀 4、单词首字母 2、子串包含 2、逐字 1，未命中 0
- [ ] 排序键 = 标题匹配得分之和 + 进程名匹配得分之和，降序（`WindowFilterer.cs:86`）
- [ ] 同分时保持原始枚举顺序（`OrderByDescending` 稳定排序）

**涉及代码：** `Core/WindowFilterer.cs:86, 98-114`、`Core/Matchers/*`

---

### EP-05 结果操作

#### US-020 键盘导航与循环
**描述：** 作为键盘用户，我希望全程不碰鼠标就能在结果中移动选择。
**验收标准：**
- [ ] `↓` / `Alt+J` 选中下一项，越界回绕到第一项（`MainWindow.xaml.cs:663-684`）
- [ ] `↑` / `Alt+K` 选中上一项，越界回绕到最后一项（`MainWindow.xaml.cs:640-661`）
- [ ] `Tab` / `Shift+Tab` 在输入框内也能上下切换（`MainWindow.xaml:73-79`）
- [ ] 列表为空时按上下键不崩溃、无副作用
- [ ] 选中项自动滚动到可视区域（`MainWindow.xaml.cs:686-693`）
- [ ] 列表高度超过屏幕时出现滚动条（`Border.MaxHeight = PrimaryScreenHeight`，`MainWindow.xaml.cs:330`）

**涉及代码：** `MainWindow.xaml:12-30, 73-79`、`MainWindow.xaml.cs:640-693`

#### US-021 切换到选中窗口
**描述：** 作为用户，我希望按回车或双击就把目标窗口带到前台。
**验收标准：**
- [ ] `Enter` 键在 **KeyUp** 时执行切换，避免目标窗口收到残留 KeyUp 事件（`MainWindow.xaml.cs:114-133`）
- [ ] 双击列表项同样切换（`MainWindow.xaml.cs:597-601`）
- [ ] 切换使用 `SwitchToThisWindow` 激活目标窗口（`Core/AppWindow.cs:91-101`）
- [ ] 若目标窗口存在最近激活的可见弹窗（如模态对话框），切换的是该弹窗（`Core/AppWindow.cs:97-101, 158-178`）
- [ ] 支持多选：对 `lb.SelectedItems` 逐个切换（`MainWindow.xaml.cs:346-350`）
- [ ] 切换后隐藏浮层

**涉及代码：** `MainWindow.xaml.cs:88-134, 344-353, 591-601`、`Core/AppWindow.cs:91-101, 158-178`

#### US-022 关闭窗口且浮层保持
**描述：** 作为用户，我希望连续关掉多个窗口时不用反复呼出浮层。
**验收标准：**
- [ ] `Ctrl+W` 或 `Alt+X` 关闭当前选中窗口（`MainWindow.xaml:24-25`）
- [ ] 关闭期间该项文字变灰、图标变淡（150ms 动画），表示正在关闭（`AppWindowViewModel.IsBeingClosed`、`MainWindow.xaml:129-158`）
- [ ] 每 125ms 轮询一次窗口是否已关闭/隐藏，确认后才从列表移除（`WindowCloser.cs:12-21`）
- [ ] 关闭失败时该窗口保留在列表中（不误删）
- [ ] 移除后选中位置合理：优先下一条，否则上一条（`MainWindow.xaml.cs:619-638`）
- [ ] 列表被关空时自动隐藏浮层
- [ ] 用户按 `Esc`/回车隐藏浮层时终止轮询等待（`HideWindow` 释放 `WindowCloser`）

**涉及代码：** `WindowCloser.cs`、`MainWindow.xaml.cs:603-638`、`MainWindow.xaml:129-158`

#### US-023 收起浮层
**描述：** 作为用户，我希望按 Esc 或点到别处就关掉浮层，且不影响原本的窗口焦点。
**验收标准：**
- [ ] `Esc` 在 KeyUp 时隐藏浮层（`MainWindow.xaml.cs:121-124`）
- [ ] 浮层失去激活状态（`Deactivated`）时自动隐藏（`MainWindow.xaml:8`、`MainWindow.xaml.cs:695-698`）
- [ ] 隐藏前先释放窗口关闭器，防止后台轮询泄漏
- [ ] 隐藏后再次唤起能重新加载最新窗口列表（不复用旧快照）

**涉及代码：** `MainWindow.xaml.cs:355-366, 695-698`

---

### EP-06 浮层界面

#### US-024 无边框居中浮层
**描述：** 作为用户，我希望搜索框是一个干净的白色浮层，居中出现在屏幕，不进入任务栏。
**验收标准：**
- [ ] 窗口无系统边框、透明背景、`Topmost` 置顶、不显示在任务栏（`MainWindow.xaml:1-10`）
- [ ] 尺寸随内容自适应，最小宽度 542，列表项宽度固定列（图标 22 / 标题 400 / 进程 106）
- [ ] 每次显示都重新计算并居中，分辨率变化后仍居中（`MainWindow.xaml.cs:327-339`）
- [ ] 浮层高度封顶为屏幕高度，避免超出屏幕
- [ ] 禁用系统菜单，避免 `Alt+Space` 误触发窗口菜单（`MainWindow.xaml.cs:705-710`）
- [ ] 从 Alt+Tab 打开时通过模拟 Alt 按下/释放确保浮层真正获得键盘焦点（`MainWindow.xaml.cs:519-550`）

**涉及代码：** `MainWindow.xaml`、`MainWindow.xaml.cs:327-339, 519-550, 705-710`

#### US-025 帮助信息面板
**描述：** 作为新用户，我希望在浮层里直接看到快捷键说明。
**验收标准：**
- [ ] 输入框右侧显示 `?` 图标，鼠标悬停提升不透明度
- [ ] 点击 `?` 展开/收起帮助面板，150ms 高度动画（`MainWindow.xaml.cs:712-717`）
- [ ] 帮助内容包含 "type to search"、`tab/↑↓` 导航、`enter` 打开、`ctrl+w` 关闭、`esc` 关闭
- [ ] 每个帮助项带 tooltip，说明更完整

**涉及代码：** `MainWindow.xaml:39-69, 81-109`、`MainWindow.xaml.cs:712-717`

#### US-026 视觉反馈与主题
**描述：** 作为用户，我希望列表条目清晰展示图标、标题与进程名，并且选中态明显。
**验收标准：**
- [ ] 条目三列布局：图标、加粗高亮的窗口标题、右对齐的进程名（灰色）
- [ ] 选中项高亮，失焦时选中态仍可见（覆盖 `SystemColors.ControlBrushKey`，`MainWindow.xaml:207-210`）
- [ ] 关闭中的条目有颜色/透明度渐变（`BoolToColorConverter`、`BoolToDoubleConverter`）
- [ ] 条目垂直内边距 5，双击触发切换

**涉及代码：** `MainWindow.xaml:111-211`、`BoolToWhateverConverter.cs`

---

### EP-07 设置与帮助

#### US-027 选项窗口
**描述：** 作为用户，我希望一个窗口里集中管理热键、Alt+Tab、AutoSwitch 与管理员模式。
**验收标准：**
- [ ] 打开时展示当前生效的配置值（`OptionsWindow.xaml.cs:43-70`）
- [ ] 选项窗口单实例：已打开时再次点选项只激活原窗口（`MainWindow.xaml.cs:378-393`）
- [ ] 关闭热键复选框会同时禁用热键输入框（`OptionsWindow.xaml.cs:237-245`）
- [ ] 取消 Alt+Tab 时自动取消并禁用 AutoSwitch（`OptionsWindow.xaml.cs:231-235`）
- [ ] `OK` 应用并持久化全部设置；`Cancel` 不落盘
- [ ] 窗口以模态方式显示（`ShowDialog`），不显示在任务栏

**涉及代码：** `OptionsWindow.xaml`、`OptionsWindow.xaml.cs`、`MainWindow.xaml.cs:378-393`

#### US-028 关于窗口
**描述：** 作为用户，我希望查看版本号、许可证与第三方组件归属。
**验收标准：**
- [ ] 从托盘菜单打开，模态居中，单实例
- [ ] 展示程序名、标语、官网链接、版本号、版权、GPL 声明
- [ ] 展示 Managed Windows API、PortableSettingsProvider 等第三方组件与许可证
- [ ] 点击超链接用系统浏览器打开对应网址

**涉及代码：** `AboutWindow.xaml(.cs)`、`MainWindow.xaml.cs:398-413`

#### US-029 版本更新检查
**描述：** 作为用户，我希望有新版本时被告知并可一键前往下载。
**验收标准：**
- [ ] 启动后立即检查一次，未发现更新则改为每 24 小时检查一次（`MainWindow.xaml.cs:204-239`）
- [ ] 版本号从远端 `version.txt` 拉取，解析失败或网络异常时静默跳过（`MainWindow.xaml.cs:241-259`）
- [ ] 开发版（版本号 `0.0.0.0`）不检查更新
- [ ] 有新版本时弹窗提示双方版本号，用户确认后打开 releases 页面
- [ ] 更新检查不阻塞主界面使用（`async/await` + `DispatcherTimer`）

**涉及代码：** `MainWindow.xaml.cs:204-259`

---

### EP-08 质量与分发

#### US-030 匹配逻辑可回归验证
**描述：** 作为维护者，我希望匹配器行为有单元测试固定住，重构（如改写成 Rust）时能对照。
**验收标准：**
- [ ] `StartsWithMatcher`、`ContainsMatcher`、`IndividualCharactersMatcher` 均有空值、命中、分片结构、得分断言（`Core.UnitTests/*.cs`）
- [ ] `XamlHighlighter` 有高亮输出结构测试
- [ ] 测试可在本地一键运行（NUnit）

**涉及代码：** `Core.UnitTests/ContainsMatcherTest.cs`、`StartsWithMatcherTests.cs`、`IndividualCharactersMatcherTests.cs`、`XamlHighlighterTests.cs`

#### US-031 安装与分发
**描述：** 作为用户，我希望通过安装包或包管理器安装，并可选择便携版。
**验收标准：**
- [ ] Inno Setup 安装包可生成标准安装程序（`Installer/Installer.iss`）
- [ ] 提供 Chocolatey 安装/便携包定义（`ChocolateyPackages/*`）
- [ ] 支持 `PORTABLE` 编译符号生成便携版，配置随程序目录

**涉及代码：** `Installer/*`、`ChocolateyPackages/*`、`Switcheroo.csproj`

---

### EP-09 技术基座：使用 egui 搭建 RustSwitcheroo

> **排序目标：** 让每个故事都能单独交付、单独演示、单独回滚，且交付顺序不出现"先做后面再补前面"的返工。
> **排序依据：** 风险前置 → 先走通骨架 → 先闭环后打磨 → 体验逐层叠加 → 高耦合系统集成靠后 → 收尾可分发。
> **框架选择：** 以 egui（eframe）为 UI 基座，立即模式渲染、跨平台，Windows 走 winit + wgpu/glow；窗口装饰、置顶、透明等能力通过 viewport 配置与平台层补齐。

#### 排序原则

| 次序逻辑 | 具体做法 |
|---|---|
| 风险前置 | US-032 先验证 egui/eframe 在 Windows 的窗口、焦点、DPI、中文字体能力，避免全盘返工 |
| 骨架最先 | US-033 ~ US-035 用最少代码打通"进程 → 热键 → 浮层"，形成可演示骨架 |
| 先闭环后打磨 | US-036 ~ US-039 先完成"列表 → 选择 → 切换"主价值闭环，再叠加搜索与视觉 |
| 一层一交付 | US-040 ~ US-045 搜索、复合查询、高亮、关闭、图标、拼音，每层独立可用 |
| 高耦合靠后 | US-046 ~ US-051 设置、托盘、管理员、Alt+Tab 钩子风险高，放在主流程稳定后 |
| 收尾可分发 | US-052 ~ US-054 关于/更新、打包、逐项验收，保证对外可交付 |

#### 交付序列总览

| 序号 | 故事 | 依赖 | 里程碑 | 交付后能演示什么 |
|---|---|---|---|---|
| 1 | US-032 egui/eframe 能力验证（Spike） | 无 | M0 | 透明置顶窗口 + 按键响应 |
| 2 | US-033 最小工程骨架与 CI | US-032 | M1 | `cargo run -p switcheroo-app` 起一个空窗口 |
| 3 | US-034 单实例与后台常驻 | US-033 | M1 | 双击两次只留一个进程 |
| 4 | US-035 全局热键唤起/收起空白浮层 | US-033、US-034 | M1 | 按 `Alt+Space` 弹出/收起浮层 |
| 5 | US-036 平台层窗口快照（命令行） | US-033 | M1 | 命令行列出全部可切换窗口 |
| 6 | US-037 浮层载入真实窗口列表 | US-035、US-036 | M2 | 浮层里看到标题与进程名 |
| 7 | US-038 键盘导航选中项 | US-037 | M2 | 上下键在列表中循环移动 |
| 8 | US-039 回车/双击切换窗口 | US-038 | M2 | 完整"唤起 → 选择 → 切换"闭环 |
| 9 | US-040 输入即时过滤与排序 | US-037 | M3 | 输入几个字符就能缩小列表 |
| 10 | US-041 复合查询 `进程.标题` | US-040 | M3 | `p.wj` 只出 PowerPoint 的"文件"窗口 |
| 11 | US-042 命中字符加粗高亮 | US-040 | M3 | 结果里命中字符可见加粗 |
| 12 | US-043 关闭窗口且浮层保持 | US-039 | M4 | `Ctrl+W` 连关多个窗口 |
| 13 | US-044 窗口图标异步加载 | US-037 | M4 | 列表图标由占位补全为真实图标 |
| 14 | US-045 拼音首字母检索 | US-040、US-042 | M4 | 输入 `wj` 命中"文件" |
| 15 | US-046 设置持久化 | US-034、US-035 | M5 | 重启后热键/开关保持 |
| 16 | US-047 选项面板 | US-046 | M5 | 图形界面改热键与开关 |
| 17 | US-048 托盘菜单与开机自启 | US-047 | M5 | 托盘右键选项/自启/退出 |
| 18 | US-049 管理员模式 | US-046、US-047 | M5 | 高权限窗口下 Alt+Tab 可用 |
| 19 | US-050 Alt+Tab 接管 | US-039、US-046 | M5 | Alt+Tab 打开 Switcheroo |
| 20 | US-051 AutoSwitch 松手切换 | US-050 | M5 | 松 Alt 直接切到高亮窗口 |
| 21 | US-052 关于面板与更新检查 | US-047 | M6 | 查看版本/许可证，收到更新提示 |
| 22 | US-053 便携模式与打包分发 | US-052 | M6 | 安装包与便携 zip 可安装运行 |
| 23 | US-054 逐项对齐验收与差异报告 | US-053 | M6 | 与原版功能对照的验收报告 |

#### 故事明细

#### US-032 egui/eframe 能力验证（Spike）
**描述：** 作为开发者，我需要先确认 egui（eframe）能在目标 Windows 环境编译并显示窗口，避免把整个项目建在不可用基座上。
**依赖：** 无
**验收标准：**
- [x] 独立 spike 工程引入 `eframe`/`egui`，本地与 CI 均能 `cargo check` 通过
- [x] 能显示一个无边框、透明、置顶窗口，并接收到键盘事件
- [x] 逐项给出结论：置顶、无任务栏图标、聚焦输入、DPI 缩放、多屏定位、透明背景
- [ ] 中文字体可加载，中文标题正常显示；输入框支持中文输入法（IME）组合输入，不丢字（字体已确认；IME 组合过程待确认）
- [x] 能力不足时给出备选方案结论（原生 Win32 分层窗口 + egui 视图 / 其他 Rust UI 框架），不阻塞后续故事编号
- [x] 结论写入 `docs/egui-spike.md`，含版本号与复现命令

**小步产物：** 30 行以内的可运行 spike + 一页验证结论。

#### US-033 最小工程骨架与 CI
**描述：** 作为开发者，我需要一个可扩展的 Cargo workspace，让平台、内核、UI 三层能独立测试与替换。
**依赖：** US-032
**验收标准：**
- [x] workspace 含 `core`（模型/过滤/匹配，纯逻辑）、`platform-win`（Win32 封装）、`app`（eframe 启动与视图）
- [x] `core` 不依赖 egui 与 Windows API，`cargo test -p switcheroo-core` 可独立跑
- [ ] `cargo run -p switcheroo-app` 启动空白窗口并以 0 退出（窗口已确认可启动；退出码待确认）
- [x] 统一 rustfmt/clippy 配置，CI 执行 `cargo fmt --check && cargo clippy -- -D warnings && cargo test`
- [x] `platform-win` 仅在 Windows target 编译，其他平台可 build 其余 crate

**小步产物：** 三 crate 骨架 + 绿色 CI，无任何业务功能。

#### US-034 单实例与后台常驻
**描述：** 作为用户，我重复启动时只保留一个实例，且启动后不弹窗打扰。
**依赖：** US-033
**验收标准：**
- [x] 以命名互斥量实现单实例，第二个实例 5 秒内静默退出（对应 US-001）（代码已落地：`platform-win/src/single_instance.rs` 抢 `Local\` 命名互斥量，`app/src/main.rs` 抢不到直接返回、不建窗口不注册热键）（已真机验证）
- [x] 无常驻窗口时进程仍在后台存活，等待热键唤起（代码已落地：`app/src/main.rs` 以 `with_visible(false)` 启动后进入 eframe 事件循环）（已真机验证）
- [ ] 退出路径释放互斥量、钩子与托盘资源，不留孤儿进程（互斥量与热键线程已有 `Drop` 清理，热键线程收到 `WM_QUIT` 后先 `UnregisterHotKey` 再被 join；键盘钩子与托盘资源尚未实现、暂无对象可释放）（退出路径未实测；当前无钩子与托盘实现）

**小步产物：** 双击两次只有一份进程，任务管理器可验证。

#### US-035 全局热键唤起/收起空白浮层
**描述：** 作为用户，我希望按 `Alt+Space` 唤起浮层，再按一次收起。
**依赖：** US-033、US-034
**验收标准：**
- [x] 平台层注册全局热键（`RegisterHotKey` 或等价），默认 `Alt+Space`（对应 US-006）
- [x] 唤起：显示 eframe 窗口（无边框、置顶、跳过任务栏）、置前、聚焦输入框；每次唤起重取真实窗口快照作为列表数据源，快照为空时才用 mock 占位数据并明确标注（无边框/透明/置顶/不进任务栏已在 `app/src/main.rs` 配置，唤起时发 `Visible(true)`+`Focus`，输入框每帧 `request_focus`；原「列表先用 mock 数据」的描述已按实现修正）（已真机验证）
- [ ] 输入框支持中文输入法（IME）组合输入，不丢字（egui `TextEdit` + winit IME 通路存在，未实测组合输入）（需人工确认）
- [ ] 再按一次或按 `Esc`：隐藏浮层（再按热键翻转 `visible`、`Esc` 置 false 后发 `Visible(false)`，逻辑已实现）（Alt+Space 收起已真机验证；`Esc` 未验证）
- [x] 热键被占用时提示且不崩溃（对应 US-008）
- [ ] 热键值从常量/配置读取，为 US-046 预留读写入口（键码 `core::DEFAULT_HOTKEY_KEY`、修饰键 `app::HOTKEY_MODIFIERS` 分散在两个 crate 的常量里，尚无配置读写接口）

**小步产物：** 热键可反复唤起/收起一个空白浮层。

#### US-036 平台层窗口快照（命令行）
**描述：** 作为开发者，我需要平台层产出的窗口快照与系统 Alt+Tab 口径一致，且能脱离 UI 验证。
**依赖：** US-033
**验收标准：**
- [x] `cargo run -p switcheroo-platform-win --example list` 打印 HWND、标题、进程名、可见性（已用交叉编译的 `list.exe` 在 Windows 实机验证，输出 20 行四要素齐全）
- [x] 覆盖原版 9 条过滤规则：可见、有标题、`APPWINDOW`、`TOOLWINDOW`、`NOACTIVATE`、Owner 可见、`ITaskList_Deleted`、`CoreWindow`、`CloakType`（对应 US-011）
- [x] UWP 进程名取子进程名，取不到降级 `UWP`，结果缓存 1 小时（对应 US-012）
- [x] 过滤规则用属性表驱动，可用假数据做单元测试，不依赖真实桌面

**小步产物：** 命令行窗口清单，可与系统 Alt+Tab 逐项人工比对。

#### US-037 浮层载入真实窗口列表
**描述：** 作为用户，我希望唤起后看到一个包含窗口标题与进程名的真实列表。
**依赖：** US-035、US-036
**验收标准：**
- [x] 用 `egui::ScrollArea` 渲染可滚动列表，三列布局（图标占位、标题、右对齐进程名）
- [x] 内置或加载 CJK 字体，中文标题正常显示，不出现方块（已真机验证）
- [x] 唤起时用 US-036 快照替换 mock 数据，并完成此前台窗口置底（对应 US-014）
- [x] 空列表显示占位文案
- [ ] 200 个窗口时打开无明显卡顿，记录首帧耗时基准（实测 19~20 窗口首帧 25.8 ms / 112.9 ms；200 窗口基线待补）

**小步产物：** 可演示的静态真实列表（还不能搜索）。

#### US-038 键盘导航选中项
**描述：** 作为用户，我希望全程不碰鼠标就能在结果间移动选择。
**依赖：** US-037
**验收标准（对应 US-020）：**
- [x] `↓/Alt+J/Tab` 与 `↑/Alt+K/Shift+Tab` 移动选中项，越界循环
- [ ] 选中项自动滚动到可视区（自动滚动落点待真机验证）
- [x] 列表为空时按键无副作用

**小步产物：** 键盘可在列表内循环移动高亮项。

#### US-039 回车/双击切换窗口
**描述：** 作为用户，我希望按回车或双击就把目标窗口带到前台，这是产品的核心价值闭环。
**依赖：** US-038
**验收标准（对应 US-021）：**
- [x] `Enter` 在 KeyUp 时切换，避免目标窗口收到残留 KeyUp（已真机验证：日志记录切换成功且浮层收起）
- [x] 双击列表项同样切换
- [x] 目标存在最近可见弹窗时切换到该弹窗
- [ ] 支持多选时逐个切换（多选语义按产品决策实现并注明取舍）
  （当前实现单选，取舍：先保证单窗口切换正确，多选留待 US-054 差异表记录）
- [x] 切换后隐藏浮层

**小步产物：** 完整"唤起 → 选择 → 切换"可用，达到 MVP。

#### US-040 输入即时过滤与排序
**描述：** 作为用户，我输入几个字符就能实时过滤窗口，且前缀命中排在前面。
**依赖：** US-037
**验收标准（对应 US-015、US-019）：**
- [x] `core` 实现 4 个匹配器与分值：前缀 4、单词首字母 2、子串包含 2、逐字 1（`crates/core/src/matcher.rs` 的 `SCORE_PREFIX` / `SCORE_SIGNIFICANT` / `SCORE_CONTAINS` / `SCORE_INDIVIDUAL`；已真机验证：输入 `note` 命中 5 条）
- [x] 排序键为标题分 + 进程分降序，同分保持枚举顺序（`rank_windows` 用稳定 `sort_by` 按 `标题分 + 进程分` 降序；单测 `rank_windows_sorts_descending_and_keeps_ties_in_order`；已真机验证：进程名 `notepad` 命中的记事本排第一条）
- [x] 输入即过滤，无需确认键；过滤后自动选中第一条（`crates/app/src/app.rs` 的 `edit.changed()` → `refresh_ranked(true)` → `Selection::reset(len, false)`；已真机验证：输入后列表实时缩小、未按任何确认键，首条被高亮选中）
- [x] 空查询展示全部窗口（`rank_windows` 空查询分支原样返回全部快照，分数 0；已真机验证：输入前浮层显示「共 21」条全部窗口）
- [x] 1000 个窗口单词过滤 < 16ms（附基准测试）（`matcher.rs` 的 `#[ignore]` 基准 `bench_filter_1000_windows_under_16ms`；已真机验证：1.876 / 1.952 / 2.082 / 2.126 ms，均远低于 16ms）
- [x] 原版单测用例逐条移植到 Rust（对应 US-030）（`StartsWithMatcher` 10 条、`ContainsMatcher` 12 条、`IndividualCharactersMatcher` 9 条共 31 条一对一移植，`matcher.rs` 内以 `// 对应 Evaluate_*` 注释逐条标注；原版 `XamlHighlighterTests` 的 2 条属 US-042 范围，仍在 US-042 保持未勾选）

**小步产物：** 输入即缩小列表，命令行 demo 也能跑同一套匹配逻辑。

#### US-041 复合查询 `进程.标题`
**描述：** 作为用户，我希望先限定程序，再在该程序内搜标题。
**依赖：** US-040
**验收标准（对应 US-016）：**
- [x] `.` 前片段只匹配进程名，`.` 后片段只匹配标题，且两者必须同时命中（已真机验证：输入 `open.swit` 后列表只剩 1 条、顶部显示「命中 1 / 共 20」，该条为 openidea 进程的「…root@kfc.test.huya.info] – Switcheroo-功能用户故事.md」；`crates/core/src/matcher.rs` 的 `rank_windows_with_foreground` 在第一个 `.` 处 `split_once`，两侧分别 `match_text` 且要求 `title.matched && process.matched`，单测 `composite_query_requires_process_and_title_both_to_match` / `composite_query_excludes_title_only_and_process_only_rows`）
- [x] `.` 前为空（如 `.报告`）时取唤起瞬间的前台进程名（代码已实现，待真机验证：`resolve_process_filter` 回退 `foreground_process` 并剥 `.exe` 后缀；`crates/app/src/app.rs` 的 `load_windows` 在唤起时记录 `foreground_process` 并传给 `refresh_ranked`；单测 `composite_query_empty_process_fragment_uses_foreground_process` / `composite_query_foreground_process_drops_exe_suffix` / `composite_query_without_foreground_process_only_filters_title`）
- [x] 内置别名：`e`→`excel`、`f`→`explorer`、`w`→`word`、`p`→`powerpnt`（代码已实现，待真机验证：`PROCESS_ALIASES` + `expand_process_alias` 仅在进程片段整段等于单个字符时展开，大小写不敏感；单测 `composite_query_process_aliases_cover_all_four_letters` / `composite_query_process_alias_e_equals_excel` / `composite_query_alias_only_applies_to_a_lone_letter`）
- [ ] 进程名片段同样支持逐字与拼音首字母匹配
  （部分达标：逐字已达标——进程名与标题共用 `match_text` 的 4 个匹配器，单测 `composite_query_ranges_point_at_each_field` 断言进程侧区间；拼音首字母属 US-045 范围、尚未实现，故本条保持未勾选）

**小步产物：** 输入 `p.wj` 只出现 PowerPoint 中含"文件"的窗口。

#### US-042 命中字符加粗高亮
**描述：** 作为用户，我希望命中的字符在结果里加粗显示，确认为什么这条被匹配。
**依赖：** US-040
**验收标准（对应 US-018）：**
- [x] 匹配结果转换为 `egui::text::LayoutJob` + `TextFormat`，命中段加粗、其余原样（已真机验证：放大截图可见标题里 `Swit` 四个字符明显加粗、其后 `cheroo` 为常规字重；`crates/app/src/app.rs` 的 `highlight_job` 用 `TextFormat::simple` 逐段 `job.append`，命中段换 `FontFamily::Name("bold")`，`crates/app/src/fonts.rs` 注册该族、字体缺失时回退 Proportional 不 panic）
- [x] 标题与进程名各自取"第一个命中结果"，全未命中时用第一个结果（代码已实现，待真机验证：`match_text` 取最高分匹配器的区间、同分取匹配器顺序靠前者；四档得分 4/2/2/1 随匹配器顺序非递增，因此与「第一个 `Matched=true`」等价，全未命中时 `ranges` 为空即整段常规字重；标题与进程名各自取 `RankedWindow.title.ranges` / `RankedWindow.process.ranges`；单测 `match_text_keeps_ranges_of_highest_scoring_matcher`；进程名一侧的加粗观感待真机确认）
- [x] 特殊字符（`<`、`&`）不破坏渲染（代码已实现，待真机验证：`LayoutJob` 只保存纯文本、不解析标记，`<`/`&` 原样显示；单测 `highlight_job_keeps_special_characters_literal` 断言 `a<b>&c</b>` 的原文与分片不被改写）
- [ ] 快照测试验证分片结构与原版 `XamlHighlighter` 等价
  （部分达标：已有 4 条高亮单测断言分片结构——`highlight_job_splits_matched_range`、`highlight_job_keeps_special_characters_literal`、`highlight_job_ignores_invalid_ranges`（越界/非字符边界/乱序/空文本不 panic）、`highlight_job_layouts_with_real_fonts`（真实字体排版不 panic）；但用的是内联期望值、未引入快照框架，也未逐条对照原版 `Core.UnitTests/XamlHighlighterTests.cs` 的唯一用例，故保持未勾选）

**小步产物：** 搜索结果中命中字符可见加粗。

#### US-043 关闭窗口且浮层保持
**描述：** 作为用户，我希望 `Ctrl+W` 连续关闭多个窗口时浮层不消失。
**依赖：** US-039
**验收标准（对应 US-022）：**
- [x] `Ctrl+W` / `Alt+X` 关闭选中窗口，关闭中条目置灰（`handle_list_keys` 在输入框之前消费两个组合键 → `close_selected`；`draw_row` 对关闭中条目改用 `CLOSING_TEXT` / `CLOSING_ICON` 降饱和。Ctrl+W 已真机验证；Alt+X 与置灰效果代码已实现，待真机验证）
- [x] 以固定间隔（约 125ms）轮询确认窗口已关闭/隐藏，失败则保留条目（`CLOSE_POLL_MS = 125`，`poll_closing` 仍存活就保留条目并续排下一次；已真机验证：日志「已请求关闭窗口「无标题 - 记事本」，每 125 ms 轮询确认」→「窗口已关闭：id=0x2DA04CC，确认耗时 52 ms」。真正关闭失败（窗口存活时拒绝关闭）的提示代码已实现，待真机验证）
- [x] 移除后选中位置优先下移、否则上移；列表空时自动隐藏浮层（`poll_closing` 移除条目后 `refresh_ranked(false)` + `Selection::clamp`，锚点按窗口 id 复原；单测 `closing_selected_row_picks_next_then_previous`；关空即 `hide`。已真机验证：关闭后条目移除、总数 21→20；过滤状态下的选中修正待真机验证）
- [x] 隐藏或退出时停止轮询，不泄漏任务（轮询由 `ctx.request_repaint_after` 驱动、没有常驻线程，`hide` 与 `Drop` 均清空关闭中集合；代码已实现，退出路径待真机验证）

**小步产物：** 连续关闭 3 个窗口，浮层始终不闪退。

#### US-044 窗口图标异步加载
**描述：** 作为用户，我希望列表显示真实应用图标，且不拖慢浮层打开速度。
**依赖：** US-037
**验收标准（对应 US-013）：**
- [x] 先渲染占位/缓存图标，后台线程取图后刷新（代码已实现，真机已显示真图标；占位→真图标的过渡帧未单独截图：`crates/app/src/icons.rs` 的 `IconCache` 起具名后台线程 `icon-loader`，`sync` 只入队、`poll` 用 `try_recv` 把结果上传为纹理并请求重绘；`crates/app/src/app.rs` 每行先查 `icons.texture`，缺失时 `draw_row` 画 16×16 浅灰圆角占位，拿到纹理后按等比缩放绘制）
- [x] 取图顺序：`WM_GETICON` → 窗口类图标 → 可执行文件图标，单次超时 100ms（已真机验证：`icon_probe` 前 10 个窗口全部 `icon=ok 32x32`，命中层级为 `wm_geticon_big` / `wm_geticon_small` / `class_icon_big`，单窗口 0~4ms；`crates/platform-win/src/icon.rs` 的 `window_icon` 依次尝试 `SendMessageTimeoutW(WM_GETICON, ICON_BIG/ICON_SMALL)`、`GetClassLongPtrW(GCLP_HICON/GCLP_HICONSM)`、`ExtractIconExW`，最后用 `SHGetFileInfoW` 兜底，`ICON_TIMEOUT_MS = 100`；本轮样本未命中可执行文件图标这一级）
- [x] 无权限或取图失败时降级为占位图，不 panic、不阻塞列表（代码已实现，待真机验证：任一级失败都返回 `None`，由 `draw_row` 画占位方块；`WM_GETICON` 限时 100ms，单色图标与异常尺寸直接放弃；`icons.rs` 的 `color_image` 先校验缓冲长度，避免 egui 断言 panic；失败结果也以 `None` 写进纹理表，不会反复请求。无权限 / 取图失败场景本轮未在真机构造）
- [x] HICON 转成 RGBA 后通过 `egui::TextureHandle` 渲染，纹理随缓存复用（已真机验证：真机列表左侧显示 msedge / Weixin / WXWork / 1Remote / lx-music-desktop / datagrip64 / openidea 的真实图标；`icon.rs` 用 `GetIconInfo` + `GetDIBits` 取 32 位 BGRA，`win32_parse.rs` 的 `bgra_to_rgba` / `bgra_has_alpha` / `apply_mask_alpha` 处理换色、alpha 与掩码，`icons.rs` 的 `upload` 转 `ColorImage` 后 `load_texture`，结果存 `IconCache::textures` 复用）
- [x] 二次唤起命中缓存，不重复取图（代码已实现，待真机验证：`hide` 明确不清空图标缓存，二次唤起的 `load_windows` 调 `icons.sync` 时已有纹理或仍在排队的窗口直接跳过；单测 `sync_only_queues_each_window_once` / `sync_drops_cache_of_gone_windows` 覆盖入队去重与快照收缩；本轮未对二次唤起的取图次数单独打点）

**小步产物：** 图标从占位逐步补全的动画可被肉眼观察。（本轮真机只确认了最终显示真图标，未单独截取占位→真图标的补全过程）

#### US-045 拼音首字母检索
**描述：** 作为中文用户，我希望输入 `wj` 就能命中标题为"文件"的窗口。
**依赖：** US-040、US-042
**验收标准（对应 US-017）：**
- [x] 接入拼音首字母转换（第三方 crate 或内置表），英文输入行为不变（已真机验证：真机输入 `dy` 命中「电影和电视」；接入用 `pinyin = { version = "0.10", default-features = false, features = ["plain", "heteronym"] }`（`crates/core/Cargo.toml:17`，MIT、无第三方依赖），转换入口为 `crates/core/src/pinyin.rs:21` 的 `initials_with_map` 与 `:31` 的 `initials_with_all_readings`，`crates/core/src/matcher.rs:398-402` 在原文逐字落空后才回退拼音。英文行为不变：纯 ASCII 文本直接跳过投影（`matcher.rs:443-445`），原文能逐字命中时绝不走拼音（`matcher.rs:822-830`，单测 `individual_characters_prefer_literal_match_over_pinyin`），单测 `match_text_keeps_ascii_results_unchanged_by_pinyin`（`matcher.rs:912`）断言 `pp` → `PowerPoint` 的分数与区间不变；本轮真机未重新敲英文，该半边为代码 + 单测结论）
- [x] 首字母命中位置能映射回原中文字符，用于高亮（代码已实现，待真机验证：`crates/core/src/pinyin.rs:88-100` 的 `original_range` 用映射表把首字母串的字节起点二分定位回原文字符区间（终点按 `len_utf8` 补齐），`crates/core/src/matcher.rs:460-478` 的 `initials_ranges` 逐个命中换算、同一汉字的多读音只保留一段以维持「升序不重叠」，`crates/app/src/app.rs:1963-1990` 的 `show_list` 按行取 `title.ranges` / `process.ranges`，再经 `draw_row`（`app.rs:2227`）交给 `highlight_job`（`app.rs:2180`）；单测 `individual_characters_fall_back_to_pinyin_initials`（`matcher.rs:832`）断言 `wj` 的区间正好落在「文」「件」，`chinese_text_maps_to_initial_letters` 断言 `0..3` / `3..6`。真机截图中命中中文的**加粗视觉**肉眼不明显，见 `docs/egui-spike.md` 第 11 节「待人工确认」）
- [x] 与原版中文样例（含 readme 中 `p.wj` 场景）测试用例对齐（已真机验证 readme 场景语义可用：真机 `dy` 命中中文标题；单测 `composite_query_matches_title_by_pinyin_initials`（`matcher.rs:1248`）直接复刻 `readme.md:14` 的 `p.wj` → PowerPoint 标题含「文件」场景，`match_text_hits_chinese_title_by_pinyin_initials`（`matcher.rs:891`）、`match_text_hits_polyphonic_title_by_pinyin_initials`（`matcher.rs:901`）、`individual_characters_use_polyphonic_readings_when_needed`（`matcher.rs:843`）覆盖单段中文与多音字；原版 `Core.UnitTests/IndividualCharactersMatcherTests.cs` 本身没有中文用例（只有 chrome 的 9 条），故「对齐」= 该 9 条原样保留 + readme 中文场景补齐）

**小步产物：** 中文窗口可用首字母搜到且高亮正确。（前半已真机验证：`dy` →「电影和电视」；后半部分达标：区间换算有单测，命中中文的加粗观感待人工确认）

#### US-046 设置持久化
**描述：** 作为用户，我希望热键与各类开关在重启后仍然保持。
**依赖：** US-034、US-035
**验收标准（对应 US-005）：**
- [ ] 设置读写到用户配置目录，字段覆盖热键、Alt+Tab、AutoSwitch、管理员模式、EnableHotKey（部分达标：配置读写落在 `%APPDATA%\Switcheroo\config.ini`，字段为 `enable_hotkey` / `hotkey_key` / `hotkey_modifiers` / `autostart` / `theme`，见 `crates/core/src/settings.rs:61-70`、`:160-207`，单测 `save_then_load_roundtrips`、`config_path_uses_appdata_when_present`；未覆盖：Alt+Tab、AutoSwitch、管理员模式三项没有配置字段——它们分别属 US-050 / US-051 / US-049，本轮未实现，故整条不勾选）
- [x] 首启默认值与原版一致（热键 `Alt+Space`、Alt+Tab 关闭等）（代码已实现，待真机验证：`Settings::default` = 启用热键 / `Alt+Space` / 不开机自启 / 跟随系统主题，见 `crates/core/src/settings.rs:72-83` 与 `crates/core/src/lib.rs:50-59`，单测 `default_settings_match_first_run_values`、`default_hotkey_setting_is_alt_space`；真机启动日志「已读取配置：启用热键 true（Alt + Space），自启 false，主题 System」与该默认值一致。Alt+Tab 尚未实现，其「默认关闭」暂无对应代码可验）
- [ ] 首次运行迁移旧配置并清除标记（未实现：`load_settings` 只读 `config.ini`，文件缺失或读取失败一律回落默认值，没有任何旧配置迁移或迁移标记逻辑，见 `crates/core/src/settings.rs:176-188`）
- [x] 热键的读取/保存接口被 US-035 使用，替换原有常量来源（已真机验证：`crates/app/src/main.rs:23` 启动先 `load_settings()`，`crates/app/src/app.rs:711-716` 按 `settings.hotkey` 决定是否注册，真机日志「已读取配置：启用热键 true（Alt + Space）」「全局热键 Alt + Space 注册成功」；读写入口 `crates/core/src/settings.rs:186-207`）

**小步产物：** 手工改配置文件后重启，行为随配置变化。

#### US-047 选项面板
**描述：** 作为用户，我希望在图形界面里管理热键与开关，而不是改配置文件。
**依赖：** US-046
**验收标准（对应 US-007、US-008）：**
- [x] 快捷键输入框捕获组合键并显示 `Ctrl/Shift/Alt/Win + Key` 形式，单独按 `Tab` 只移动焦点（代码已实现，待真机验证：面板里是「组合键显示框 + `录制` 按钮」，录制由 `RecordInput::feed` 消费本帧按键（`crates/app/src/app.rs:495-528`），单独修饰键与单独 `Tab` 只算「继续等待」、`Esc` 取消并保留原组合、必须至少带一个修饰键；显示格式由 `hotkey_text` 按 `Ctrl → Shift → Alt → Win → Key` 拼接（`app.rs:371-391`），单测 `record_input_captures_skips_and_cancels`、`hotkey_text_orders_modifiers_and_falls_back`）
- [ ] 修改热键立即生效，冲突时提示 "Shortcut already in use" 并保留原值（部分达标：冲突时已改为不落盘 + 保留原组合键 + 面板内红字提示（代码已实现，待真机验证）。`save_options` 现在是**先试注册、成功才落盘**（`crates/app/src/app.rs:1046-1129`），处置由两个纯函数判定：`plan_hotkey_rebind` 决定「试注册 / 沿用旧监听 / 释放」（`HotkeyPlan` `app.rs:472`、`:497`），`decide_after_register` 决定「落盘 / 回滚」（`SaveDecision` `app.rs:486`、`:511`），单测 `hotkey_conflict_rolls_back_before_persist`（`app.rs:2984-3017`）。注册失败即回滚：草稿热键退回已保存值、旧监听全程没被动过继续按旧组合键工作、**不写 `config.ini`**，并在**选项面板内**显示红字「热键注册失败：…，未保存」（`app.rs:1071-1081`，面板渲染见 `app.rs:1853-1866`）。**旧的「先落盘后重绑」问题已修**：此前冲突时新组合键会先写进 `settings` 与 `config.ini`、旧值不保留；现在 `self.settings = next` 与监听替换都挪到落盘成功之后（`app.rs:1096-1114`）。「面板内提示」这一原缺口也已支持——`options_error` 面板开着时显示在面板里、面板关着（例如托盘改自启）时才回落到列表页（`app.rs:1588-1590`）。仍差真机验收：未在真机构造被别的程序占用的组合键；文案仍是中文，不是原版英文 "Shortcut already in use"）
- [ ] 关闭热键复选框会禁用输入框；关闭 Alt+Tab 会连带关闭并禁用 AutoSwitch（部分达标：「启用全局热键」取消勾选时整组录制控件置灰不可点（`crates/app/src/app.rs:1665-1697` 的 `add_enabled_ui(enabled, …)`）；Alt+Tab / AutoSwitch 尚未实现，无对应联动可验）
- [x] OK 落盘、Cancel 不落盘；选项窗口单实例（代码已实现，待真机验证：`确定` → `save_options`（`crates/app/src/app.rs:1046-1129`）→ 热键试注册 → 注册表自启 → `persist_settings` 写 `config.ini`（`app.rs:1091-1094`、`persist_settings` `:977-995`）；`取消` → `cancel_options` 只丢草稿并把主题 / 托盘勾选回滚到已保存值、不写盘（`app.rs:1021-1036`）；面板是窗口内唯一一块区域，`Ctrl+,` 在已打开时被 `!self.options_open` 拦掉（`app.rs:2372-2378`）。原「面板打开时再从托盘点『选项…』会用已保存值重建草稿、未保存的修改被丢弃」的副作用已修：`open_options` 在面板已开时只置前 + 抢焦点并直接返回，不重建草稿（`app.rs:998-1004`、调用点 `:1189-1193`））

**小步产物：** 所有开关与热键都能从界面修改并持久化。

#### US-048 托盘菜单与开机自启
**描述：** 作为用户，我希望不打开浮层也能进入选项、开机自启、关于和退出。
**依赖：** US-047
**验收标准（对应 US-002、US-003）：**
- [x] 托盘图标常驻，悬浮提示为 "Switcheroo"（代码已实现，待真机验证：`Tray::spawn` 在专用线程 `Shell_NotifyIconW(NIM_ADD)` 常驻图标（`crates/platform-win/src/tray.rs:198-243`、`:419-440`），提示文案取 `TRAY_TOOLTIP = APP_NAME = "Switcheroo"`（`crates/app/src/app.rs:77`、`crates/core/src/lib.rs:21`），单测 `tip_text_is_written_and_terminated`；本轮锁屏无法截图，托盘观感未目测）
- [ ] 菜单含 Options、Run on Startup、About、Exit，自启项勾选状态实时反映快捷方式存在与否（部分达标：菜单为「打开 Switcheroo / 选项… / 开机自启 / 切换明暗主题 / 退出」（`crates/platform-win/src/tray.rs:53-81`，单测 `menu_layout_matches_contract`），Options / Run on Startup / Exit 齐备，多出「切换明暗主题」，**缺 About（关于）**——关于面板属 US-052，本轮未实现；自启勾选按内存原子布尔值打勾（`tray.rs:675-692`），启动时由 `is_enabled()` 对齐、每次切换后刷新（`crates/app/src/app.rs:697-706`、`:920-937`），但弹菜单时不会重读注册表：用户在任务管理器里改过之后要重启或再切一次才同步）
- [ ] 开启自启在用户启动文件夹创建快捷方式，关闭时删除，失败弹错误不崩溃（机制变更 + 部分达标：失败时面板内红字提示并回退复选框（机制为 HKCU Run 键而非启动文件夹快捷方式），待真机验证。具体：自启不建启动文件夹快捷方式，而是手动写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下值名 `Switcheroo` 的 `REG_SZ`（带双引号的 exe 全路径）（`crates/platform-win/src/autostart.rs:17-20`、`:111-168`；关闭走 `RegDeleteValueW`，值本来不存在也算成功，`:175-222`）；失败时 `apply_autostart`（`crates/app/src/app.rs:948-970`）`log::warn!` 后把 `settings.autostart` 回退成 `is_enabled()` 的真实值、写 `options_error = "设置开机自启失败：…"`、同步托盘勾选与面板草稿，返回 false，界面红字显示在面板内（`app.rs:1853-1866`）；面板路径下 `save_options` 立即返回、面板保持打开可重试、`config.ini` 不落盘（`app.rs:1086-1089`），托盘路径下同一红字回落到列表页（`app.rs:1588-1590`）。仍是红字提示而非弹窗，「失败弹错误」按字面只算部分达标；未在真机构造注册表写失败）
- [x] Exit 释放托盘图标与全局热键后退出（代码已实现，待真机验证：`TrayCommand::Exit` 下发 `ViewportCommand::Close`（`crates/app/src/app.rs:1103-1106`），`SwitcherooApp::drop` 显式 `take()` 托盘与热键监听（`app.rs:2388-2396`），`Tray::drop` 投递 `WM_QUIT` 后线程内按 `NIM_DELETE → DestroyWindow → UnregisterClassW` 收尾（`crates/platform-win/src/tray.rs:261-274`、`:471-478`），`HotkeyListener::drop` 让热键线程先 `UnregisterHotKey` 再 join（`crates/platform-win/src/hotkey.rs:165-183`））

**小步产物：** 托盘全流程可用，无需任何调试手段。

#### US-049 管理员模式
**描述：** 作为需要切换高权限窗口的用户，我希望 Switcheroo 以管理员运行后，Alt+Tab 对高权限程序同样有效。
**依赖：** US-046、US-047
**验收标准（对应 US-004）：**
- [ ] 选项窗口提供 `Run as Administrator` 开关并标注"需重启生效"
- [ ] 已开启但当前非管理员时，启动以 `runas` 重新拉起并退出旧进程
- [ ] 非管理员下功能可用，仅高权限窗口受限，不崩溃

**小步产物：** 管理员权限下可切换到高权限窗口。

> **现状（Rust 重写版）：** 「选项里开 Run as Administrator、启动时按需 `runas` 重新拉起」这个模型没有采用。原生浮层改为在 manifest 里常驻声明 `requireAdministrator`，启动即提权、不再有「非管理员运行」的分支，详见 EP-10 的 US-055。上列三条按可选开关 / 按需提权 / 非管理员降级来验收的口径对本版不再适用。

#### US-050 Alt+Tab 接管
**描述：** 作为高级用户，我希望 Alt+Tab 直接打开 Switcheroo，而不是系统任务切换器。
**依赖：** US-039、US-046
**验收标准（对应 US-009）：**
- [ ] 低级键盘钩子拦截 `Alt+Tab`，受设置开关控制，关闭时完全透传系统
- [ ] `Alt+Shift+Tab` 打开浮层并选中末项；浮层内继续按 Tab 前进/后退
- [ ] 系统多任务视图（`MultitaskingViewFrame`）存在时不重复弹出
- [ ] Alt 长按期间焦点留在浮层，按键不泄漏到目标窗口

**小步产物：** 系统 Alt+Tab 被 Switcheroo 替换，行为接近原生。

#### US-051 AutoSwitch 松手切换
**描述：** 作为习惯系统 Alt+Tab 的用户，我希望松开 Alt 就切换到高亮窗口。
**依赖：** US-050
**验收标准（对应 US-010）：**
- [ ] 开启后按 Alt+Tab 时输入框禁用并提示 "Press Alt + S to search"
- [ ] 释放 `Alt` 自动切换并隐藏；`Alt+S` 恢复搜索；`Ctrl+Alt+Tab` 直接进入搜索
- [ ] AutoSwitch 模式下 `Ctrl+W` 不触发自动切换
- [ ] 关闭该选项后行为回退到 US-050 的纯搜索模式

**小步产物：** 松手即切换的手感与系统一致。

#### US-052 关于面板与更新检查
**描述：** 作为用户，我希望查看版本与许可证，并在有新版本时收到提示。
**依赖：** US-047
**验收标准（对应 US-028、US-029）：**
- [ ] 关于面板展示版本号、版权、许可证与第三方组件，超链接用系统浏览器打开
- [ ] 启动检查一次，未发现更新则每 24 小时一次；开发版本跳过
- [ ] 版本源可配置，默认指向本项目发布地址；网络异常静默跳过
- [ ] 有新版本时提示双方版本号，确认后打开下载页

**小步产物：** 关于与更新提示均可复现。

#### US-053 便携模式与打包分发
**描述：** 作为用户，我希望通过安装包或便携包使用，便携模式下配置跟随程序目录。
**依赖：** US-052
**验收标准（对应 US-031）：**
- [ ] 便携构建把设置写到程序目录，常规构建写用户配置目录
- [ ] 产出安装包与便携 zip，带版本信息，安装后可从托盘启动
- [ ] 安装包卸载不留残渣，卸载后启动文件夹快捷方式被清理

**小步产物：** 可安装/可拷走的发布产物。

#### US-054 逐项对齐验收与差异报告
**描述：** 作为维护者，我希望按原版故事逐项验收，并记录有意偏离之处。
**依赖：** US-053
**验收标准：**
- [ ] 冒烟清单全绿：启动、单实例、热键、Alt+Tab、搜索、切换、关闭、设置、自启
- [ ] 输出功能差异表：已对齐 / 有意偏离 / 暂不支持，逐条关联 US-001 ~ US-031
- [ ] CI 可产出 release 构建，版本号随 tag 递增

**小步产物：** 一份可安装的 RustSwitcheroo 与一份验收对照报告。

#### 里程碑切片

| 里程碑 | 目标 | 包含故事 | 可演示结果 |
|---|---|---|---|
| M0 可行性 | 确认基座可用 | US-032 | 透明置顶窗口 + 按键响应 |
| M1 骨架 | 打通进程-热键-浮层，平台能力可验证 | US-033 ~ US-036 | 热键弹浮层 + 命令行窗口清单 |
| M2 MVP | 完成切换主价值闭环 | US-037 ~ US-039 | 唤起、选择、切换可用 |
| M3 检索 | 让用户能"打几个字就找到" | US-040 ~ US-042 | 实时过滤、复合查询、高亮 |
| M4 体验 | 补齐高频操作与视觉 | US-043 ~ US-045 | 连关窗口、图标、拼音检索 |
| M5 系统集成 | 接管系统行为与权限 | US-046 ~ US-051 | 设置、托盘、管理员、Alt+Tab |
| M6 分发 | 可对外交付 | US-052 ~ US-054 | 安装包 + 验收报告 |

#### 小步交付约束

1. 一个故事对应一条可单独回滚的 PR，未完成不合并。
2. 每个故事结束必须"可运行 + 可演示"，半成品用功能开关隔离。
3. 依赖表是唯一顺序约束：无依赖故事可并行，编号只表示建议次序。
4. 平台能力（窗口/图标/热键/钩子）先用命令行或最小窗口验证，再接完整 UI。
5. egui 不提供的能力（托盘、全局热键、Alt+Tab 钩子）一律下沉到 `platform-win`，UI 层只消费事件与数据。
6. 每个里程碑结束时打 tag，M2 之后随时可对外发预览版。

#### 本版重排对照（旧编号 → 新编号）

| 旧编号 | 新编号 | 说明 |
|---|---|---|
| US-032 | US-032 | Spike 保持首位 |
| US-033 | US-033 | 工程骨架保持第二 |
| US-034 | US-034 | 单实例，位置不变 |
| US-035 | US-035 | 热键+空白浮层 |
| US-036 | US-036 | 窗口快照，仍先命令行后 UI |
| US-037 | US-037 | 列表渲染 |
| US-038 | US-044 | 图标从 M3 前移到体验层（不阻塞主闭环） |
| US-039 | US-040 + US-041 | 拆成基础过滤与复合查询 |
| US-040 | US-042 | 高亮依赖过滤，后置 |
| US-041 | US-038 + US-039 | 拆成"导航"与"切换"两个故事 |
| US-042 | US-043 | 关闭窗口依赖切换，后置 |
| US-043 | US-050 + US-051 | 拆成"Alt+Tab 接管"与"AutoSwitch" |
| US-044 | US-046 + US-047 + US-048 + US-049 | 拆成设置存储、选项面板、托盘自启、管理员模式 |
| US-045 | US-045 | 拼音，位置不变 |
| US-046 | US-052 | 关于与更新，归入分发前 |
| US-047 | US-053 + US-054 | 拆成打包分发与逐项验收 |

#### 技术约束与风险

- egui/eframe 的窗口能力（透明、置顶、跳过任务栏、IME）以 US-032 结论为准；若关键能力缺失，`app` 层保持可替换（视图仅依赖 `core` 的数据结构与事件）。
- egui 是立即模式 UI，每次输入都会重建界面；US-037 需建立 200 窗口的帧时间基准，必要时用 `ScrollArea` 虚拟化并缓存 `LayoutJob`。
- 全局钩子与高权限窗口切换涉及权限模型，US-049 需单独验证。
- 图标、拼音、UWP 判定均为"容错优先"路径，任何失败都必须降级而非中断检索。
- 主价值闭环（M2）先于任何视觉与系统集成，确保每个里程碑都能独立发布。
- 发布构建不需要 Windows 构建机：Linux 上 `x86_64-pc-windows-gnu` + 静态 CRT 交叉编译即可产出 PE32+ GUI 程序，US-053 的打包沿用该路径。

---

### EP-10 常驻提权与开机自启迁移

> **背景：** 原生浮层（`crates/winui`）必须常驻管理员权限才能关掉高权限窗口，权限模型一变，
> 原来的 HKCU Run 键自启就不再可用（Run 键拉起的进程拿不到提权）。本 EP 记录随之落地的三项
> 改动：manifest 常驻提权、自启改计划任务、关闭失败原因可区分。
> **与 EP-01 的关系：** EP-01 的 US-003（开机自启）、US-004（以管理员权限运行）描述的是原版
> WPF 的「启动文件夹快捷方式 + 可选 runas」模型；两者冲突时以本 EP 描述的 Rust 重写版现状为准。

#### US-055 常驻管理员权限
**描述：** 作为需要关闭管理员窗口的用户，我希望 Switcheroo 自身就以管理员身份运行，这样向高完整性窗口投递的关闭请求才不会被系统拦下来。
**依赖：** US-043、US-049
**验收标准：**
- [x] `crates/winui/switcheroo-win.manifest` 的 `requestedExecutionLevel` 为 `requireAdministrator`，进程启动即拿到高完整性（high IL）令牌
- [x] 图标与 `RT_MANIFEST` 清单合进同一份 `.rc` 再编译（`crates/winui/build.rs` 的 `embed_gnu` 与 `embed_icon_msvc` 两个分支同样做法），避免分成两个资源目标文件后丢清单或丢图标
- [x] 资源编译器缺失或编译失败时只打 `cargo:warning`、不让构建失败（Linux 上的 `cargo check` / `cargo test` 依赖这一点）
- [ ] 真机：启动 `switcheroo-win.exe` 弹一次 UAC 确认，确认后能在浮层里关掉「以管理员身份运行」的窗口（待真机验证）
- [ ] 构建日志出现「跳过图标与清单嵌入」警告时，能确认该 exe 没有提权清单、关不掉管理员窗口（待真机验证）
**备注：**
- 为什么必须常驻提权：实现关闭的方式是向目标窗口 `PostMessage(WM_CLOSE)`，而 UIPI 会拦截低完整性进程向高完整性窗口投递消息，且是静默失败——所以「按需 asInvoker」的方案不成立。
- UAC：`requireAdministrator` 的 exe 每次启动都会弹一次确认（不是只在首次启动弹），这是常驻提权的直接代价。
- 清单不声明 `dpiAware` / `dpiAwareness`，进程保持 DPI 不感知（虚拟化坐标），浮层的 DPI 换算依赖这一点。
- 失败退路：清单没有嵌进 exe 时程序仍能启动，但关闭高权限窗口会落到 `CloseOutcome::Denied`。

#### US-056 开机自启改用计划任务
**描述：** 作为开了开机自启的用户，我希望登录后 Switcheroo 自己以管理员权限起来，且登录时不再被 UAC 拦一下；从旧版升级上来的用户，历史写在注册表 Run 键里的自启也要自动搬过来。
**依赖：** US-055、US-048
**验收标准：**
- [x] 开启自启执行 `schtasks /create /tn Switcheroo /tr "<exe 全路径>" /sc onlogon /rl HIGHEST /f`（`crates/platform-win/src/autostart.rs` 的 `create_args`），不再写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
- [x] 状态判定用 `schtasks /query /tn Switcheroo` 的退出码（`is_enabled`；查询失败或退出码非 0 一律按未启用处理）
- [x] 关闭自启用 `schtasks /delete /tn Switcheroo /f`（`disable`；任务本来就不存在也算成功，不报错）
- [x] 调用 `schtasks.exe` 走 `CreateProcessW` + `CREATE_NO_WINDOW`（`run_quiet`），不弹窗、不闪黑框，并用全路径而不是裸名字
- [x] 任务动作里的 exe 全路径总是加一对引号（`quote_exe_path`），路径含空格、中文或结尾反斜杠时不会被任务计划程序拆错
- [x] （迁移）启动时调用 `autostart::migrate_legacy_run_value()`，把 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下的 `Switcheroo` 旧值迁到计划任务
- [x] （迁移）顺序是「先建计划任务、成功后才删旧值」（内部依次为查旧值 → `enable()` → `delete_legacy_run_value()`），中途失败时旧值还在，自启不会凭空消失
- [x] （迁移）没有旧值时直接返回、零副作用，重复调用结果一致（幂等），因此每次启动都可以调
- [x] （迁移）迁移失败只 `log::warn!` 并保留旧值、不拦启动；非 Windows 平台上 `migrate_legacy_run_value()` 恒 `Ok(())`
- [ ] 真机：勾选自启 → 注销重登 → 托盘出现，且任务计划程序里能看到 `Switcheroo` 任务、触发器为「登录时」、以最高权限运行（待真机验证）
- [ ] 真机：`enable` / `disable` 失败时把 schtasks 退出码写进 `Err`，界面回退勾选并给出提示（待真机验证）
- [ ] 真机：（迁移）先手工写一条旧 Run 值再启动，确认计划任务被创建、旧值被删除、`is_enabled()` 为真（待真机验证）
**备注：**
- 计划任务名固定为 `Switcheroo`；`/f` 让重复开启幂等，不会因为「任务已存在」报错。
- 为什么换掉 Run 键：manifest 要求管理员权限，Run 键拉起的进程拿不到提权，开机时会弹 UAC 或直接失效；`/rl HIGHEST` 的计划任务由任务计划程序以提权方式拉起，登录时不再弹 UAC。
- 建 `/rl HIGHEST` 的任务本身要在已提权的进程里做——本程序常驻管理员权限，所以 `enable()` 不需要额外提权。
- 状态以计划任务为准：启动时用 `is_enabled()` 的真值修正配置并回写，避免用户在任务管理器里改过之后界面还在谎报已启用。
- 迁移的旧值名与任务名同为 `Switcheroo`，旧值由 egui 版写在 HKCU Run 下；迁移本身是幂等的，查不到旧值就直接返回，既不建任务也不碰注册表。
- 迁移排在读自启真值之前，否则旧值会被当成「已启用」而计划任务其实并不存在。

#### US-057 关闭失败原因可区分
**描述：** 作为用户，我希望关窗口失败时能分清是「权限不够」还是「窗口本来就没了」，而不是只有一句笼统的失败。
**依赖：** US-043
**验收标准：**
- [x] `request_close` 返回 `CloseOutcome::{Requested, Denied, Failed}`（`crates/platform-win/src/close.rs`），不再用裸 `bool`
- [x] `GetLastError() == ERROR_ACCESS_DENIED(5)` 归为 `Denied`，0 与其它错误码一律归为 `Failed`（`outcome_from_error`，有跨平台单测）
- [x] `Denied` 时浮层提示「需要管理员权限才能关闭：<标题>」，既不把该行标成关闭中，也不从列表移除（`crates/winui/src/app.rs` 的 `close_selected`）
- [x] `Failed` 时若窗口仍存活提示「关闭失败：<标题>」并保留该行；窗口已经不在则当场从列表移除
- [ ] 真机：对管理员权限的窗口按 `Ctrl+W` 能看到权限提示，对普通窗口能正常关掉（待真机验证）
**备注：**
- `Requested` 只表示 `WM_CLOSE` 已投递进目标窗口的消息队列，是否真的关掉由 `is_window_alive` 的 125ms 轮询确认，三种结果不混用。
- 用 `GetLastError()` 取原始错误码，而不是 `windows` crate 折出来的 `HRESULT_FROM_WIN32(5)`（`0x80070005`），省掉反解编码细节。
- 提示复用卡片底部那一行红字（`notice`），不弹模态框，浮层保持打开、条目保留，用户可以直接切过去。

---

## 五、非功能需求

| 编号 | 类别 | 要求 | 现状依据 |
|---|---|---|---|
| NFR-1 | 平台 | 仅 Windows，依赖 WPF 与 Win32 API | `Switcheroo.csproj`、`Core/WinApi.cs` |
| NFR-2 | 响应性 | 输入过滤需在交互延迟内完成；图标异步加载不阻塞列表 | `PriorityBinding`、`MemoryCache` |
| NFR-3 | 稳定性 | 窗口关闭/图标读取等易失败路径不得导致崩溃 | `WindowIconFinder` try/catch、`WindowCloser` 轮询 |
| NFR-4 | 权限 | 原版口径：非管理员可运行，管理员模式为可选增强；Rust 重写版改为常驻管理员权限（`requireAdministrator`），见 EP-10 的 US-055 | `Program.cs:75-90`、`crates/winui/switcheroo-win.manifest` |
| NFR-5 | 兼容性 | 窗口口径需覆盖 Win7~Win10（含 UWP） | `Core/AppWindow.cs:122-228` |
| NFR-6 | 可本地化 | 界面文案当前为英文硬编码 | `MainWindow.xaml`、`OptionsWindow.xaml` |

---

## 六、已知边界与待确认项

1. **仅前台窗口进程名用于 `.` 简写**：`.` 前缀为空时依赖唤起瞬间的前台窗口，若前台窗口已关闭会取不到进程名。
2. **多选切换语义**：`ListBox` 为 `Extended` 选择模式，回车会依次切换所有选中窗口，与"只切一个"的直觉存在差异，重写时需明确取舍。
3. **拼音仅首字母**：不支持全拼或声调，且首字母映射依赖 `NPinyin`，非中文字符按原样处理。
4. **排序未含"最近使用"权重**：仅按匹配得分排序，同分保持 Z 序，未引入 MRU 历史。
5. **更新检查指向原作者仓库**：fork 后的发布地址与版本源是否需要替换，需产品确认。
6. **关闭窗口策略**：`AppWindow.Close()` 直接发送关闭消息，未做"保存提示拦截/强制结束"等策略区分。
7. **DPI/多屏**：居中基于 `SystemParameters.PrimaryScreen*`，多显示器与混合 DPI 下可能偏移。

---

## 七、功能→故事追溯表

| 功能点 | 用户故事 |
|---|---|
| 单实例、托盘、自启、管理员、便携/迁移 | US-001 ~ US-005 |
| 全局热键、Alt+Tab 接管、AutoSwitch | US-006 ~ US-010 |
| 窗口枚举、UWP、图标、前台置底 | US-011 ~ US-014 |
| 增量检索、复合检索、拼音、高亮、排序 | US-015 ~ US-019 |
| 导航、切换、关闭、收起 | US-020 ~ US-023 |
| 浮层外观、帮助、视觉反馈 | US-024 ~ US-026 |
| 选项、关于、更新检查 | US-027 ~ US-029 |
| 单测、安装分发 | US-030 ~ US-031 |
| egui 项目搭建与小步交付 | US-032 ~ US-054 |
| 常驻提权、计划任务自启与旧值迁移、关闭失败区分 | US-055 ~ US-057 |
