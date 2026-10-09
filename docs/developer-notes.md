# 开发环境说明

只影响源码开发和调试器启动的裸二进制的问题，不影响 Homebrew 安装或打包 `.app` 的用户。开发期间的常规启动方式是 `scripts/dev-restart.sh`；裸 `cargo run` 仅用于底层诊断。README 的开发环境说明指向本文。

## 浮动面板：面板/窗口矩形契约、描边与 elevation 阴影

三个浮动面板（切换浮窗、剪贴板 picker、剪贴板详情）现在画 1pt `card_border` 描边，并在 `high` 档挂 elevation
阴影。这件事有三处很容易被改坏，所以写在这里，而不是只写在代码里。

**两个矩形，各只有一个含义。** 面板的*窗口*比面板大：窗口会裁掉超出自身 frame 的绘制，而阴影需要在面板外侧有
空间，所以窗口 = 面板矩形按 `theme::elevation_insets(level)` 外扩。**面板矩形才是语义矩形**：布局、命中测试、
picker/detail 的组合几何、`PICKER_EDGE_MARGIN`、`clamp_into_visible`、切换浮窗的尺寸预算、按键 HUD 的已保存位置
以及 `--e2e-state` 的几何，全部指面板。只有 `setFrame:` 拿到外扩后的矩形。`glass::set_panel_frame` /
`animate_panel_frame` / `panel_frame_of` 是唯一的换算入口，面板的每次尺寸变化都必须走它们：直接用面板矩形
`setFrame:` 会把窗口缩小，窗口再通过固定的 autoresizing 边距把材质一起缩小——实测按键显示的键帽因此溢出背条 8pt，
因为它们是按面板尺寸排版的。对没有登记过外扩的窗口，`panel_frame_of` 原样返回窗口 frame，所以在从未安装过材质
底板的窗口上使用它是安全的。

**外扩量来自实测，不是算出来的。** `radius + |offset|` 只是图层阴影核心的边界，不是它衰减到零的位置：只按半径
外扩时，窗口最外一圈仍残留 8 个色阶的阴影。`theme::ELEVATION_SHADOW_TAIL_ALLOWANCE` 就是实测要求的余量；外扩量
同样要从放置预算和可见区收边里扣掉（外扩后的窗口一旦越界，AppKit 会*移动面板*——实测在菜单栏处推移 26pt——而不是
裁掉阴影）。复现该判定的命令是 `scripts/e2e/panel-edge.sh`。

**阴影需要载体，而载体不能裁剪。** `masksToBounds` 会裁掉图层自身的阴影，而三种材质都设了它（glass 那个是承重
的），所以阴影不能挂在材质层上。材质因此成为一个载体视图的子视图，阴影挂在载体的一个**裸 `CALayer` 子层**上，
绝不挂在视图的图层上：AppKit 会重置由视图管理的图层，实测 `addSubview:` 会把视图图层的 `shadowOpacity` 清零而
保留半径——阴影就那样静默地不渲染。`CALayer.shadowPath` 会在赋值时复制路径（Apple 头文件原文 "Upon assignment the path is copied"），而 Core
Animation 对图层上的 CF 类型属性按 CF 所有权持有（Apple QA1565），所以路径的创建、赋值、释放都在一处完成
（`ffi::layer_set_rounded_shadow_path`），图层保留自己的副本。同一图层上的颜色助手也因此不需要缓存。载体本身不能裁剪（`masksToBounds = false`）：
glass 的各个变体会把边缘画在 glass bounds 之外，因此按键显示的 smoke 现在直接断言*宿主不裁剪*，取代了原先按类
判断的代理（glass 本身或 `NSVisualEffectView`）——那个代理会否掉任何不裁剪的宿主（包括载体），而当初 bug 真正
关于的属性就是那个 mask。

载体的 `layout` 会按自身 bounds 重算阴影路径，这个钩子是刻意的：面板的尺寸变化是*动画式*的（picker 与详情开合、
切换浮窗在卡片关闭时重排），在每个 `setFrame:` 调用点设一次路径只能覆盖首尾两帧。

**描边是材质之上的一层装饰视图。** 三种材质共用一套实现：材质分支的结构不同（frost 带圆角 mask image，glass 会
裁剪自身），而 `swap_backdrop` 会迁移 `content_parent` 的子视图，装饰若放在那里就会被带进新层级、留下一条旧描边。
它重写了 `hitTest:` 返回 nil，因为它覆盖整个面板，否则会吞掉所有本该给面板内容的点击。

**开发开关。** `--panel-outline=off|after:N` 与 `--panel-shadow=off|med|high|after:N` 是给 A2 测量用的：每个都能
产出断言所需的对照帧，`after:N` 还能在同一次启动内产出（半透明材质跨启动不会渲染得完全一致，所以这一对必须来自
同一次启动）。`--panel-shadow=off` 会完全移除载体，因此它同时是模糊保留门禁的「无载体」基线。这些开关都不允许留在
运行中的实例上：`scripts/e2e/run-all.sh` 会连同 `--panel-backdrop`、`--clipboard-blank-text` 一起检查进程命令行。

## 开发模式下图标可能不正确

用 `cargo run` 跑裸二进制进行诊断时，浮层偶尔会把 oh-my-tab 自己的卡片显示成首字母占位块而不是应用图标，而且可能一直持续到手动清空图标缓存。图标缓存按 bundle id 索引，以可执行文件的 **mtime** 作为失效指纹；开发模式下每次构建都会重新链接二进制、改变 mtime，导致运行中实例的缓存条目失效。打包后的 `.app` 不受影响（安装后二进制 mtime 稳定）。正常开发运行请使用 `scripts/dev-restart.sh`；如果诊断用裸二进制出现此问题，可从菜单 *Clear Icon Cache* 清空，或删除 `~/Library/Caches/oh-my-tab-icons/`。

## 调试器启动时鼠标控制可能失效

通过 RustRover 或其他调试器以 Debug 方式启动应用时，如果在启动阶段频繁操作鼠标，反向滚动和按设备配置等功能可能会失效。此时应用不再收到鼠标事件，滚动方向恢复为系统默认，指针加速设置也会停止生效，直到应用重启。目前主要在调试器启动的未签名开发构建中观察到这一问题，可能与 macOS 26 对调试器进程的 HID 层事件监听限制有关；打包的 `.app` 和从终端启动的二进制尚未出现相同行为。

## 全屏 Space 归属组

**自 2026-10-07 起，准入不再看这段来源关联**（见下一节的新契约）：切换器仍然只有在观察到同一窗口离开普通桌面、加入全屏 Space，并确认目标 Space 类型和显示器后才记录来源，但这条记录**只用于 e2e 诊断**（`space_contexts` 里显示这个全屏 Space 被算作哪个桌面）。应用启动时已经全屏、事件有缺口，或来源／显示器关系不唯一时，诊断显示「无从判断」——不影响任何窗口是否出现。Space 成员通知按顺序处理。归组正确时一次 Space 切换不会改变候选集合，因此 `--e2e-state` 在活跃 Space 上下文变化时额外写出 `refresh_context` 帧；断言只能读已应用候选集的帧（`refresh` / `refresh_context`），其他帧会把新上下文和旧的卡片列表配在一起。只有确知的事件丢失（队列溢出）才清除待确认转换证据；无法解析属主的成员事件只降低诊断连续性标记，因为一次全屏转换会为该应用的辅助窗口发出这类事件，按它们清空会把正在学习的转换本身清掉。配对窗口由 1 秒放宽到 3 秒：1325 加入事件可能早于 SkyLight 把新 Space 标成全屏，配对必须活到后续查询修正类型。旧版成员查询降级只接受明确处于屏幕上的窗口，不再把全屏尺寸当成跨 Space 放行条件。SkyLight 私有事件与查询接口属于尽力而为，未来 macOS 版本可能不再提供。

### 其他桌面的窗口

**2026-10-07 起的契约：准入只看「窗口在哪里」，不看「全屏 Space 来自哪个桌面」。** 当前 Space 的窗口、
以及**任意全屏 Space** 的窗口永远是候选（全屏窗口在别的 Space 上时没有 AX 元素，`kAXWindows` 按当前 Space
过滤，所以它的可达性不能依赖 macOS 从不暴露的「来源」关联）；另一个**普通**桌面的窗口由
`windows.show_other_desktops`（设置页写作「始终显示其他桌面的窗口」）放行，**当前处于全屏 Space 时也放行**
（此时显示所有桌面的窗口）。来源关联（学到或按原生顺序邻接推断）保留为诊断信息，不再参与准入——因此
「来源未知/推断错」不会让窗口消失，代价是有意的跨桌面串窗：开关关闭时其他桌面的全屏窗口也会出现。
被放行的跨 Space 窗口没有 AX 元素，因此以 CG 窗口名作标题、最小化状态不可读，也不会重新抓图。准入仍然遵守「AX 权威」。当某个应用的 `kAXWindows` 为空、而键/主窗口槽位仍然命名了它**某个**窗口时，
AX 已经对该应用的窗口集合作出答复：一个既不在该答复里、也从未被 AX 识别过的 CG 窗口是 AX 有意排除的
次级表面，必须拒绝——微信的 `3018` 主窗口旁有一个 280×380、离屏、同标题的窗口，从别的桌面看时它曾经
变成第二张死卡（没有 AX 元素，选中只会 `ax NO MATCH`）。该判定放在「其他归属组例外」之前，因为这类应用
正好落在 `ax_empty_pids` 里，否则会被例外先吃掉。一份很小的「AX 曾识别过的窗口」记忆（pid + 进程启动
时间 + CGWindowID；销毁时清除并带否决以防旧收集写回，进程结束时清除）保证真实存在的第二窗口在其桌面被
访问过之后仍可放行；自本进程启动以来从未访问过其桌面的真实窗口会先隐藏。键/主槽位也为空时，AX 对这个应用一个窗口都没命名，它没有可切换的
窗口可展示，其 CG 条目是面板，任何桌面上都拒绝。实测即 Stats：一个 280×800、layer 0、无父窗口、离屏的
菜单栏面板（标题「Combined modules」），对 AX 完全不应答（`kAXWindows` 为空，`AXFocusedWindow`/
`AXMainWindow` 均不支持）；它既会在自己桌面上成为一张死卡，也会在跨桌面时出现，选中没有任何反应。
三种状态必须分清：AX 命名了窗口（走「其他归属组例外」，仍受归属与形状门约束）、**AX 查询失败**
（`ax_failed_pids`，完全未知，保留 CG 兜底）、**AX 应答但未命名任何窗口**（拒绝）。若一个窗口元素都没回来、
而三个属性读里有一个失败，这算「查询失败」而不是「空答复」：窗口列表按 Space 过滤，那个窗口可能只有刚失败的
键/主槽位能拿到。查询失败时**只放行当前在屏的窗口**：没有 AX 答复就分不出真实窗口和已关闭的菜单栏面板，
兜底只覆盖用户看得见的东西（把 Stats 冻结让它的 AX 读超时，修复前会同时放行可见的设置窗口和已关闭的
「Combined modules」面板——正是用户报的那一对）。

准入要求成员
列表非空且命中的 Space 属于已接受的显示器拓扑：空列表或未受管的 Space 正是 orderOut／辅助表面的形状，
开关打开时同样拒绝。该开关不会放宽旧版降级路径——它区分不了「另一个桌面」和「离屏窗口」；最小化开关
仍然先生效，且生效的是下面这套补充后的状态：WindowServer 报为「在另一个桌面上最小化」的窗口也会被它过滤。

**其他桌面上的状态来自 WindowServer，而不是来自应用。** 另一桌面的窗口通常不在 `kAXWindows` 里（AppKit
用一个受 Space 限制的 WindowServer 查询构造该列表），所以 AX 答复描述不了它。每趟采集一次批量
`SLSWindowQueryWindows`，为这趟能叫出名字的每个窗口带回卡片所需的事实：`attributes` 的 `0x2` 位（在屏
/ordered-in）、`tags` 的第 60 位（最小化）与第 39 位（应用被隐藏）、`space_type_mask` 的 `0x20` 位
（窗口所在 Space 是全屏 Space）。解码优先级、逐字段证据来源与反例都在
`src/window_collector/window_state.rs`；每个呈现标志的来源与原始位由 `--e2e-state` 逐卡发布
（`minimized_source`、`fullscreen_source`、`ordered_in`、`ax_pairing`、`ws_*`）。这些位是未文档化的
WindowServer 字段。`--space-state-record` 让应用自建的 probe 窗口依次走 order-in、orderOut、最小化、
恢复、应用隐藏与取消隐藏并打印原始字段，`--smoke-space-state-matrix` 断言记录下来的矩阵（有未钉住的格
时它**失败**而不是通过）。本机实测（macOS 27.0.1 / 26A434，2026-10-07）：在屏时 `attributes` 的 `0x2`
位为 1；最小化会置 `tags` 第 60 位（`0x200100482001` → `0x1000200100480001`）并清 `0x2`；程序化的 `deminiaturize:` 会
把 `0x2` 置回，并在同一个 20ms 轮询样本里清掉第 60 位（没有任何样本出现「已回到屏幕但 tag 仍置位」）；
隐藏应用置 `tags` 第 39 位（`0x208100480001`）。同一次运行还给出两个事实：orderOut 的窗口**不是**最小化
（它的 tag 只掉了在屏位 13，即参考实现记录过的 #5714 判别），以及 `kAXWindows` 会漏掉 orderOut 的窗口、
但隐藏应用的窗口仍在列表里。全屏 Space 的 `0x20` 位后来在一扇真实的跨桌面窗口上验证：把它的 `AXFullScreen`
置为 true 后，行读到 `mask=0x20`、窗口位于新建的全屏 Space；退出全屏后回到 `0x1`。`--space-state-record`
自己产生不了这一格——本应用是菜单栏应用，AppKit 拒绝它的 `toggleFullScreen:`。

还有三条关于「窗口在别的桌面上」的本机实测事实，朴素模型都会搞错：
- 未最小化时它的 `attributes` 第 `0x2` 位**仍然置位**（该位描述的是窗口自己所在的 Space，不是「用户正看着的
  屏幕」）；在那边最小化会清 `0x2` 并置最小化位，恢复时 `0x2` 回来而 tag 已在同一个 20ms 样本里清掉。所以呈
  现状态不因哪个桌面是当前而改变。
- 一个所有窗口都在别的桌面上的应用，`kAXWindows` 什么都不列，但 `kAXFocusedWindow`／`kAXMainWindow` 仍会指
  向该窗口——这就是切换器准入它所用的 `RecoveredAx` 路径（跨桌面卡片能存在的原因）。AX 一个窗口都不回答的
  应用仍然被拒，见上文。
- 远程 token 枚举能拿到这种窗口的元素（实测：离屏 Chrome 窗口用了 43 个 id），动作路径将来需要超出 key/main
  时就走它。

跨桌面卡片的端到端断言——`minimized`／`fullscreen` 及其证据来源、`ordered_in`、`ax_pairing`、原始行字段——
都在 `scripts/e2e/space-desktops.sh`（用 `--include-focus` 跑）；场景现在从 `keyboard.modifier` 推导要注入的
组合键，而不是假定某一个。

最小化解码有意让 ordered-in 位压过最小化声明。参考实现实测的是**点 Dock** 的恢复：WindowServer 的 tag 会
在 AX 已经报「已恢复」之后再挂一段时间；本仓库自己的记录用的是程序化的 `deminiaturize:`，看到的是 `0x2` 与
「tag 已清」出现在同一个轮询样本里——它**没有**复现那个延迟，也不能代表 Dock 那条路径。两种情况下采用的规则
都是保守的那一条：只要 WindowServer 说窗口已回到屏幕，就不把它呈现为仍在最小化。当 ordered-in 字段读不到时，由 AX 读数单独决定，只有对
AX 完全不发布的窗口才看 WindowServer 的 tag。

缩略图：这类卡片**不会请求**新的抓图（本应用使用的私有抓图调用 `SLSHWCaptureWindowList` 抓不到不在活跃
桌面上的窗口，参考实现也不会去请求），但会显示缓存里已有的那一张——那是它还在自己桌面上时抓到的。生产端把它过滤掉，渲染端照旧读缓存；A2 快照为此发布
`thumbnail_ready`，让这个行为可断言。连缓存帧一起拒绝是错的：两个桌面都去过的用户，大多数卡片本来就有
一张，参考实现也是保留上一张缩略图。

激活：精确窗口的前台切换（`_SLPSSetFrontProcessWithOptions` + 窗口 id + userGenerated 模式 + 定向点击）
与应用激活（`NSRunningApplication activateWithOptions:`）都能移动 Space，谁先落地取决于状态。AltTab 对
跨 Space 目标依赖前台切换（「它同时会让 macOS 切到显示该窗口的 Space」），BetterCmdTab 把它保留为菜单栏
正确的跨 Space 兜底——他们放弃了 `CGSManagedDisplaySetCurrentSpace`，因为那个调用跳过 Space 切换机制，
从全屏 Space 出来时会让目标 Space 没有菜单栏。本项目现在**立即**发出精确窗口的前台切换与定向点击（不再先等窗口进入活跃桌面），AX 精确抬窗也随即提交；
窗口若尚未到达，AX 那一步只是落空，随后由**一次有界补做**重新抬窗（`OTHER_DESKTOP_LATE_SETTLE_BUDGET`）。
这个顺序是实测选的，但要注意这些数字是**抬窗工作线程内部的阶段耗时**（以工作线程开始为零点，不含此前的激活
调用与排队，也不是"五项成功条件首次成立"的时刻）：等待版本 3,227–3,288ms（最坏一次 4,613ms 且始终没有落地），
立即提交版本 139–291ms（同一台机器，2026-10-08，macOS 27.0.1／26A434；由 `scripts/e2e/space-desktops.sh
--include-focus` 的 `[raise] other-desktop terminal` 记录复现：`first_rescue_ms=0`、`first_ax_attempt_ms<=30`、
`elapsed_ms` 落在上述区间）。参考实现同样不等这个信号：AltTab 与 DockDoor 完全不等待，BetterCmdTab 只在它
自己的合成手势路径上等 Space 变化通知，vorssaint-utils 等的是 Space 可见性而不是窗口的 `kCGWindowIsOnscreen`。
本项目保留的判定信号仍是 CG 的 `kCGWindowIsOnscreen`（option 8）：成员关系查询在过渡期间滞后——实测中
CG 已经报告 `onscreen=true` **之后**紧接着做的成员查询，仍显示目标 Space 与当前 Space 不相交（两次读取是顺序
执行的，不是同一瞬间的两次采样）。`--raise-wait` 只用于对照旧行为，默认不启用。另外：终态里的系统前台判定目前不可用（那次读取属于主线程，通道尚未建立），所以实验开关下的终态在建立通道之前只会是 `unknown`，不会冒充 `landed`。这个文件早先的版本只依赖应用激活、跳过了前台切换；macOS 在某些
状态下会拒绝那个激活（`activateWithOptions=false`，目标始终没有成为前台——这正是用户日志里的情形），
于是这类卡片完全没有反应。此前这里写的「前台切换返回成功但活跃 Space 不动」来自一个没有辅助功能信任的
探针程序，是错的：该调用确实会切换，两个参考实现都依赖它。每次抬窗都会发布是哪条路径移动了 Space，
`scripts/e2e/space-desktops.sh` 还会用「抑制应用激活」的一趟单独验证兜底，而不是靠推断。

## 缩略图卡片尺寸

浮窗的缩略图网格给**所有**卡片一个高度。用户可以用 App Switcher 页的「**缩略图大小**」下拉钉住它
（`layout.thumbnail_size`：`auto` 或基准卡片的百分比）；`auto` 则在 `THUMB_SCALE_STEPS` 里取**第一个按数量
估算放得下**的档（`thumb_scale_for_panel` → `thumb_count_estimate_fits`）。钉住的档位直接跳过搜索，因此
面板只会换行与滚动，不会缩小卡片。

`auto` 的估算从构造上就只依赖数量：用窗口数量、面板预算和一张**按基准比例算出的参考卡**——不看序列、
不看各窗口比例、也不看整组宽度上限。基准比例集合里每张卡正好这么宽，所以任何放得下的一行至少容纳
`floor((max_inner + gap) / (reference_w + gap))` 张，于是 `ceil(count / per_row)` 是排布可能产生的行数的
**上界**——而 `overflowed` 正是 `rows > max_rows`。因此通过判定的档位在基准比例集合下必然不溢出；远宽于
基准比例的集合不在该上界内，可能多出一行并在选定尺寸下滚动，这正是「自动档不受窗口形状影响」的代价。
钉住百分比只固定**大小**——`thumb_widths_with_max_card_w` 仍按各窗口自身比例算宽度并重新排布，因此换行
仍随窗口的数量、比例与顺序变化。

三件事仍使自动档位是窗口集合的粗粒度函数：

- 行数预算 `thumb_max_rows` 由卡片高度与可用面板高度算出，因此随档位变化：卡片越高，可放的行越少。
- 每行能放几张是卡片宽度的阶跃函数，而宽度跟随各窗口的宽高比；跨过「每行多放一张」的门槛会改变整组的行数。
- 整组的宽度上限来自 `thumbnail_max_card_width`：被判定为最大化的窗口（宽 ≥ 屏宽 90% 且高 ≥ 可用高 80%）
  里最宽的那个，决定所有卡片的宽度上限。

排布本身仍然依赖卡片的**顺序**——`pack_rows` 保留 MRU 顺序、再最小化剩余宽度，因此同一组宽高比在不同
顺序下可能一个需要三行、另一个需要四行——这正是 `auto` 不再用真实排布选档的原因。钉住百分比时，顺序只
决定卡片落在哪些行，不影响它们的大小。

实测 2026-10-07（复现命令：`grep "layout mode=thumbnail" ~/Library/Logs/oh-my-tab/oh-my-tab.log`）：
多一个窗口把选中档位从 1.10 移到 0.85、卡片高度从 248 移到 201——因为 1.10 到 0.90 都需要四行，而高度
预算只允许三行，只有 0.85 窄到能每行放五张。同一天、同一批 11 个窗口：从最大化窗口呼出选中 1.10
（248pt），从非最大化窗口呼出选中 1.00（230pt）——1.10 档下同一组宽高比在一种 MRU 顺序里排成三行、
在另一种里排成四行。记录在案的取舍是「优先不滚动，而不是优先尺寸稳定」；另一种
做法——先接受最多一行 teaser 溢出再降档，并且不再让宽度上限跟随单个最大化窗口——登记在
`docs/review-backlog.md`。

## 日志与内存诊断

默认日志路径为 `~/Library/Logs/oh-my-tab/oh-my-tab.log`。活动文件达到 10 MB 后依次滚动为 `oh-my-tab.log.1` 到 `oh-my-tab.log.5`，每次启动写入会话标记；旧版按启动生成的日志和超过 30 天的旧备份会在启动时清理。

启动约 60 秒后先记录一行，之后每 5 分钟记录一行 `[mem]`。日志包含当前功能画像、进程 footprint/RSS、采样期间 footprint 峰值、线程数，以及缩略图、剪贴板和窗口账本估算值。`footprint` 是 macOS physical footprint 指标，对应活动监视器的“内存”列，是判断内存压力的主要数字；`rss` 是当前驻留内存，会随 macOS 压缩或回收页面而下降。`footprint_peak_sampled` 是应用采样得到的峰值，`rss_peak_kernel` 是内核记录的进程生命周期峰值。

剪贴板账本会拆分文本、预览和元数据；磁盘缓存中的图片原图不计入驻留内存。日志不包含剪贴板内容和窗口画面。debug 日志只记录切换器按键 tap 中的 `Tab`、`Command`、`Option` 和召唤组合名，其余按键记为 `Other`，不包含键码和修饰位。

## 设备识别细节

应用通过 Generic Desktop 页的 Pointer(1,1)、Mouse(1,2) 和 Trackpad(1,5) 用途判断鼠标或触控板。它使用公开 API `IOHIDServiceClientConformsTo` 检查完整的 `DeviceUsagePairs`，而不是只读取单个 `PrimaryUsage` 值。

有些鼠标会报告错误的主用途。例如 **ATK A9 SE**（Nearlink/星闪鼠标）的 `PrimaryUsage = 6(Keyboard)`，因此在系统设置中显示为键盘，但它的 `DeviceUsagePairs` 同时声明了 Mouse(1,2)。检查完整用途列表可以将其识别为鼠标；如果只看 `PrimaryUsage`，设备事件会落入“最近使用”的档位。

有些蓝牙键盘的 HID 描述符也会声明指针用途，例如 Kzzi-i75 声明了完整的 Mouse 集合。设备下拉框会进一步检查蓝牙 **GAP Appearance**（0x03C1 = 键盘），数据来自 bluetoothd 写入 NVRAM 的缓存，并通过蓝牙地址与 HID 服务匹配。macOS 蓝牙面板的图标也使用这项数据。不在缓存中的设备（如刚配对的设备）以及非蓝牙设备，会回退到纯 HID 判定。

设备连接或断开时，下拉框会刷新。插拔事件会经过防抖处理，延迟重查用于覆盖较短的 BLE 休眠和唤醒过程。

## 剪贴板历史落盘（加密、状态、迁移）

剪贴板功能开启期间，它落盘的每一个字节都是密封的：历史 TOML、图片原始字节、缩略图与详情图预览都走 `src/clipboard/crypto.rs` 的信封（AES-256-GCM；`magic | kind | key_id | object_id | nonce | ct+tag`，AAD 绑定 magic、kind、key_id、object_id）。信封里的内容格式不变，所以 TOML 结构与 `HISTORY_VERSION` 仍然描述历史本身。读取方按它请求的逻辑名重算 `object_id`，并拒绝头部与之不一致的文件，因此两个完整的同类文件无法互换。

主密钥是登录钥匙串里的一个通用密码项（`com.eacryo.oh-my-tab.clipboard-history` / `master-key`，值 `key_id || key`），用 `SecItemAdd` 语义创建——**从不更新**，所以另一个构建渠道（`.dev` bundle）不会覆盖 release 的密钥。dev 与 release 刻意共用这一项，因为它们共用同一个历史文件；探针显示「身份与 bundle identifier 相同、Info.plist 不变」的重建之间静默可读，换 bundle identifier 会弹框。开发构建曾每次启动都重新签名（时间戳构建号写进 Info.plist）而**每次启动都弹、且「始终允许」不跨重启生效**：**2026-10-08 已定位并修复**——项的 ACL 按「创建该项的应用的签名身份」判定，而这一项是 ad-hoc 构建创建的；开发包改用 Developer ID 签名并一次性重置了该项，此后重建后的开发包 67ms 静默读完（实测）。完整规则与未验证边界见下方「钥匙串授权」。开发运行请优先用 `--clip-no-keychain`。

`src/clipboard/storage.rs` 掌握本次会话的状态，所有写盘、清扫、删除入口都先问它：

- `Ready`：密钥可用、历史已加载；允许读、写与孤儿清扫。
- `Unavailable(reason)`：本次会话拿不到密钥（被拒、被取消、钥匙串被锁，或 `--clip-no-keychain`）。**功能不可用**：不记录、不写盘、不清扫、不删除（2026-10-08 用户决定，取代早先「内存模式」的半可用状态）；进入该状态时，本次会话早先记录的条目连同待写字节一起丢弃。设置页横幅与面板空状态给出同一句提示，并提供「授予钥匙串访问」（重新读取密钥，系统弹一次授权框）。
- `Blocked(reason)`：数据在但读不出来（`damaged`、`foreign-key`、`version`、`migration`）或清理未完成（`purge-pending`）：不写盘、不清扫、不删除，且绝不替换密钥。

值得守住的几条：只有「不存在加密数据」（或用户清理之后）才创建密钥；清扫只在加载成功后执行；失败路径只做隔离（改名），不做删除；密钥缺失而存在加密数据时，把整个存储集合改名隔离（`*.failed-<ts>`）再重新开始。

明文 → 密文的迁移可续做且原子提交：先转换每一个被引用的缓存文件（已密封的文件必须用当前密钥、按预期 kind 与 object id 认证通过才算已转换），再发布加密索引，最后才删除明文索引。任何文件转换失败都会阻止提交（`migration`），明文索引与所有原件原样保留，供下次重试。

删除是一个事务（`purge-pending`）：先写意图（历史文件旁边，失败则退回 `~/Library/Application Support/oh-my-tab/`——绝不用 `~/Library/Caches`，系统会清理它），再失效在飞任务，然后删除并逐项验证，最后撤销意图。只有删除与撤销都确认成功才恢复写盘；陈旧标记会继续禁止写盘，从而不会删掉刚写入的新历史。待清理任务在启动时与每次配置变更时处理，且都早于任何加载或写入。

测试与冒烟从不碰真实钥匙串：`cfg(test)` 与 `--smoke-*` 使用固定测试密钥并配合临时存储目录，`--clip-no-keychain` 强制走 `Unavailable`，让不可用分支保持可达。`--e2e-state` 报告 `clipboard_storage`（`state` / `reason` / `key_provider` / `plaintext_leftover` / `keychain_acl`）；断言「已加密」的场景仍必须自己读磁盘文件——应用自述不算证据。
两条运行观察。

**离线加载与耗时**：加载之所以放在主线程之外，是因为钥匙串读取可能阻塞（开发构建实测：读取本身约 11 秒，其余决策 0.2 秒；期间应用完全可用，这正是拆分的意义所在）；因此 `clipboard_storage.state` 在加载落地之前会读作 `uninitialized`，断言它的场景必须等待并触发一次新的快照。

**钥匙串授权**（已实测、推断与未验证必须分清）：**2026-10-08 定位并修复**。判定规则（三个 C 探针 + 实机）：**钥匙串项的 ACL 按「创建该项的那个应用的签名身份」判定**。自签名/ad-hoc 构建创建的项，二进制一变就不再被承认（探针：重建同一路径、重新签名后读取即弹框）——这是历史现象，已不再适用于本机（2026-10-08 一次性重置后，项由 Developer ID 身份创建，重建后实测 67ms 静默读完）。Developer ID 构建创建的项跨同样的重建仍被承认（探针：静默；实机：重建后的开发包无弹框）。**注意不要把 ACL 的文本形态当成稳定性判据**：macOS 用废弃读取 API 对非 Apple 锚定的应用一律报「路径」，`--e2e-state` 的 `keychain_acl`（`requirement` / `foreign-requirement` / `path` / `none` / `unknown`）只是形态报告。**应用无法认领别人的项**：用 Developer ID 签名去删异签名创建的项返回 `errSecInvalidOwnerEdit`（-25244，实测），所以历史遗留项只能由用户删掉一次、再由应用新建（旧存储集合按设计改名保留为 `*.failed-<ts>`）。**未验证**：正式构建（Developer ID）安装在没有重置过的旧环境时的行为——「最多弹一次后静默」是预期而非保证。纯 UI 验证可用 `--clip-no-keychain` 完全绕开钥匙串。



## 构建与测试

开发期间常规的启动方式是 `scripts/dev-restart.sh`。它构建开发版 `.app`，**只在输入（构建产物、拷入包的内容、签名身份）变化时**重新组装并用 **Apple 签发**的身份签名（`CODESIGN_IDENTITY` 可覆盖；签名不带安全时间戳，所以本地构建不依赖 Apple 的时间戳服务），随后交给用户级 `launchd` 启动。**只接受 Apple 签发与自签名两种身份，ad-hoc 一律拒绝**；Apple 身份签名失败时**构建失败**（除非显式 `--allow-signing-fallback` 要求退回自签名）。签名身份稳定，辅助功能/屏幕录制授权与钥匙串项的授权才能跨重建保留；输入没变就复用现有包，重启不会重置这些授权。

单元测试默认无 GUI 依赖；剪贴板图片/历史测试夹具使用按进程、按线程隔离的系统临时目录。CG/AX **冒烟测试**标记为 `#[ignore]`，需要 GUI 会话和辅助功能权限：

```sh
cargo test -- --ignored
```

在 macOS GUI 会话中，真实的 AppKit 设置页冒烟测试可以遍历所有设置页并执行相同的布局后置检查，无需手工点击：

```sh
cargo build
cargo test settings_layout_smoke -- --ignored
```

它会以 `--smoke-settings-layout` 启动 debug 二进制，在主线程打开真实设置窗口，遍历全部七个页面，校验其子视图 frame 后退出。

浮窗运行路径也有一个被忽略的子进程冒烟测试。先运行 `cargo build`，再执行 `cargo test overlay_runtime_smoke -- --ignored`；`--smoke-overlay` 会跳过单实例锁，避免已有开发版实例导致测试未执行却误判通过。

### 本地化与布局 QA

`scripts/dev-restart.sh` 构建的 Debug 版会在语言下拉框中加入 `[TEST] English x3` 选项。选中后会把每一条英文 UI 文案重复三遍，便于在真实设置窗口中检查超长下拉项及其所在行、卡片。`scripts/release-dev.sh` 构建的优化开发包通过 `dev-long-text` Cargo feature 包含同一夹具；正式发布脚本不启用它。旧的 `--pseudo-locale` 开关仍可用于仅限 debug 的符号膨胀。

在 debug 构建中用 `--layout-debug` 启动可开启运行时设置页断言；同级交互控件与标签/控件相交，或 frame 越出 document 时会立即失败，并给出页面名和出错的 frame。原生控件内部的子视图不会参与同级比较。
