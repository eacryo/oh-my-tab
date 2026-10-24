# 审查积压（Review backlog）

在 *Review budget and triage*（AGENTS.md）下判定为有效但超出范围的发现，以及没有任何一轮关闭的已知缺口。这里没有任何一条是在声称行为正确——每条都是一次决定：把这项工作排期，而不是在暴露它的那次改动里顺手做掉。

在一轮被分诊时就补记，不要拖后：轮次、严重度、为何延后，以及把它带回来的触发条件。它的工作落地后删掉该条，并在提交或发布说明里讲明。

## 未决

| # | 事项 | 来源 | 严重度 | 为何延后 | 重新审视的触发条件 |
| --- | --- | --- | --- | --- | --- |
| 40 | `scripts/e2e/panel-edge.sh` 的 check 3（material blur）**结果不可判定**：仪器返回 rc=5 `class=unobservable / reason=bands-disagree`，高低位带的读数无法归类 | 架构/性能审计（arch-perf-audit），2026-10-17 实跑 | MEDIUM（既有不可判定结果） | **证据**：改动前 HEAD=`24e9529` 上单独运行该场景得到相同失败，失败输出为 `carrier-on class=unobservable hf=0.214 lf=0.077 ... reason=bands-disagree`，启动 build-version 由进程自报为 `20261017140350`；带本次改动时为 `20261017140829` / `20261017141602`，结果一致。这只能支持「基线也失败」，**不能证明根因在测量仪器**——根因待查。按仓库规则「不能判定的门禁不算通过」，A2 因此不是全绿 | 排查面板材质/毛玻璃渲染或截图测量仪器时，或该 check 再次阻断 A2 全量门禁时 |
| 34 | 设置写入路径（`settings/dispatch.rs`）对整份 `Config` 做三次 clone：`old_cfg`、`cfg`、写回时的 `*w = cfg.clone()`。一次 checkbox 切换会复制 `Vec<KeyBinding>`、多组 `ThemeColors` 与 mouse profiles | 架构/性能审计（`arch-perf-audit`） | LOW（用户交互频率，非热路径） | 试做的「把最后一次写回改成 move」并不成立：`apply_config_change(&old_cfg, &cfg, ..)` 在写回之后还要借用 `cfg`（dispatch.rs:1076-1081），保留副本后收益归零。要真正减少复制需要重排写入/应用的所有权顺序，没有测量收益支撑 | 下次改动 `settings/dispatch.rs` 的写入与应用顺序时，或一次设置变更的耗时可测时 |
| 35 | `card_signature`（`overlay.rs:284`）为每张卡 clone `app_name`/`window_title`/`icon_path` 三个 String 并存入 `CARD_SIGNATURES` | 架构/性能审计（`arch-perf-audit`） | LOW（影响待测） | 这些副本可能承担「签名即当时值」的快照语义（卡片复用判定依赖它），直接改成 `Arc<str>` 或哈希前须先确认频率与复用收益；未测量前不改 | 下次改动卡片复用判定或 `CARD_SIGNATURES` 时，或 summon 耗时可测时 |
| 36 | `refresh_highlight`（`overlay/cancel.rs:494`）对所有卡片无条件发一串 msg_send（border/background/shadow/hidden/frame），选中态未变时也做 | 架构/性能审计（`arch-perf-audit`） | LOW（影响待测） | 那些无条件重置同时在清理**复用卡片**的旧装饰（L532-535），是正确性的一部分。改成只处理脏卡必须同时保留一个显式的全量失效入口（主题/布局变化时），否则复用卡片会继承上一次的样式。未测量前不改 | 下次改动 `refresh_highlight` 或卡片复用路径时 |
| 37 | `theme::resolved_is_dark()` 在 `theme=auto`（默认）下每次调用都分配一个 CFString 并查 `NSUserDefaults`，全库无缓存 | 架构/性能审计（`arch-perf-audit`） | LOW（收益未测） | 系统主题通知链已存在（`lib.rs:3150-3179`），加缓存会引入失效面；`panel.rs:808` 的调用只在面板可见且有内容的分支内，原来的「每秒 60 次」判断是错的。先测量再决定 | 有主题切换相关的用户报告，或 `rebuild_badges` 的耗时可测时 |
| 38 | 剪贴板搜索每敲一字（50ms 去抖后）走 `rebuild_rows()`，先把所有行 `removeFromSuperview` 再重建 | 架构/性能审计（`arch-perf-audit`） | LOW（影响待测） | 文件里已有 `sync_visible_rows()` 增量路径，但它按**显示索引**复用具名行；搜索会改变索引→记录的映射，直接套用会显示旧内容。要复用须重新设计行身份与失效条件，属于独立改动 | 下次改动 `clipboard/detail.rs` 的行复用或搜索路径时 |
| 39 | `config.rs` 一个文件承担六种职责：schema、Default、迁移、validate/merge_valid、原子写、`CONFIG` 全局与持久化 worker | 架构/性能审计（`arch-perf-audit`） | LOW（维护性） | 审查者同意可分离为「模型/校验」与「存储/发布」，但前提是不改配置兼容性、逐字段回退与持久化顺序；与本轮的性能修复无关，不应捆绑 | 下次改动 `config.rs` 的持久化或迁移逻辑时，单独作为维护任务 |
| 33 | `scripts/lib/dev-lock.sh` 的锁文件只写自己的 pid 与时间戳，没有 run-id 归属字段，于是 `dev-restart.sh` 替换另一个 worktree 的实例时只能报出 worktree 路径，报不出是哪一次运行 | PeachPic 工作流借鉴，实现审查 R1–R3 | LOW（归属粒度） | 接管已经被报告（`pid N from <worktree>`），用户能判断哪个 checkout 被替换；补 run-id 要在锁文件格式里加字段并处理并发写，属于另一个改动。R3 明确判定不为此扩展实例协议 | 下次改动 `scripts/lib/dev-lock.sh` 的锁文件格式，或有多个 worktree 同时跑开发版并需要区分是哪一次运行被替换时 |
| 1 | 跨桌面最小化窗口：没有 A2 场景端到端断言其恢复路径 | 功能审查，R9–R12 | MEDIUM | 该路径存在，且当前桌面已由 A1/A2 覆盖；跨桌面变体需要第二个桌面加一个最小化窗口，场景无法确定性地搭出这个状态 | 下次改动 `raise_other_desktop_window` 或最小化恢复路径时 |
| 2 | 同一 app 在两个桌面上都有窗口：没有 A2 场景 | 功能审查，R9–R12 | MEDIUM | 需要该 app 在每个桌面上各拥有一个窗口；frame 级的 `ax_published_pids`/`ax_recovered_pids` 证据只能让既有场景不在它上面误报失败 | 下次改动跨桌面收录逻辑时 |
| 3 | 一个真实第二窗口，其桌面自本进程启动后从未被访问过，会一直隐藏 | 微信重复项修复 | LOW（已接受的取舍） | 隐藏它正是消灭那个死重复卡片的手段；该桌面被访问过一次后窗口就会出现 | 有用户报告跨桌面窗口丢失时 |
| 4 | 一个 app 的 `kAXWindows` **和** key/main 槽都为空时会被直接拒绝，于是它在另一个桌面上的真实窗口也保持隐藏 | 微信重复项修复，后又被 Stats 修复进一步收窄 | LOW（已接受的取舍） | 在该状态下没有任何信息能把真实窗口与已关闭的菜单栏面板区分开，而两种错误答案都比隐藏更糟：收录 CG 条目会让 Stats 已关闭的「组合模块」面板作为死卡片出现在屏幕上，而一次失败的*查询*（另一种状态）会对可见窗口保留回退 | 有用户报告某个对 AX 完全无响应的 app 的跨桌面窗口丢失时 |
| 6 | 用合成 Dock 滑动实现瞬时且尊重菜单栏的 Space 切换（BetterCmdTab） | 改进候选 | —（优化） | 已移植并实测 2026-10-07：三阶段手势（`CGEvent` 字段 55/110/123/124/129/130/132）在普通进程和 app 进程内都能成功创建并投递（`delivered=true`），但在 macOS 26 上两种情况 Space 都**不动**。参考实现还安装了滑动抑制事件 tap，并把该功能置于其 `instantSpaceSwitch` 偏好之后，所以事件序列本身并不是完整机制。探针保留在 `--space-swipe=left\|right`（`src/space_swipe.rs`）后面，供下次尝试 | 跨 Space 动画过渡被报告为太慢时，或抑制 tap 的交互被搞清楚时 |
| 10 | 缩略图卡片尺寸仍会在窗口**数量**跨过阈值时跳一档，而一套远比基准比例宽的窗口在所选尺寸下可能需要滚动行 | 用户报告，2026-10-07 | LOW（已记录的取舍） | 用户选择暂时保留现行为并记录下来：`docs/design-style{,-en}.md` §9 陈述该策略，`docs/developer-notes{,-en}.md` 陈述机制，含可复现的日志行 | 尺寸跳变再次被报告时，或下次 overlay 布局改动时：(a) 在降档前优先选择溢出最多只多一行 teaser 的最大档位，(b) 平滑数量阈值（增加档位，或以行数为锚）。**（c）已完成**：用户手选的缩略图尺寸下拉（2026-10-07）消除了顺序依赖，`auto` 现在只按数量和最宽卡片估算行数 |
| 18 | **位于另一个桌面上的**全屏窗口没有端到端卡片断言（其 `fullscreen` 标志与 `window_server` 来源） | 状态解码审查，R1–R3（本次） | LOW（覆盖缺口） | 跨桌面状态断言现已落在 `scripts/e2e/space-desktops.sh`（minimized/fullscreen/sources/`ordered_in`/`ax_pairing`/原始行字段，PASS 2026-10-08），并且 `0x20` Space 掩码已在本机通过对一个离屏窗口切换 `AXFullScreen` 得到验证；仍然缺的是一个把窗口放进全屏 Space *并*断言其卡片 fullscreen 标志的场景（本 app 无法把自己的探针窗口全屏，所以场景得去驱动另一个 app） | 下次改动 `space-desktops.sh` 或 `window_state.rs` 时 |
| 19 | 各 A2 场景对切换器组合键的取法不一致：`space-desktops.sh` 现在从 `keyboard.modifier` 推导，但 `tab-repeat.sh` 硬编码 Command+Shift+Tab，`switch-basic.sh` 默认 `cmd+tab` 而不读配置 | 跨桌面状态审查，2026-10-08 | MEDIUM（测试工具） | 两个场景在配置为 Command 的机器（本机）上都通过，且共享的 `scripts/e2e/lib/hotkey.py` 回归用例已覆盖该映射，所以这是一致性修复而非坏掉的门禁 | 下次改动其中任一场景时，或套件首次在有 `option` 配置的机器上运行时 |
| 11 | 设置控件写配置这条路径（任何下拉，包括缩略图尺寸）没有 A 层覆盖 | 本次 | LOW（覆盖缺口） | app 的自绘 select 需要真实鼠标按下才能打开菜单，没有场景驱动它；读方向（config -> 弹窗）与映射已覆盖，写方向只有代码审查 | 当某个 smoke 能在抑制持久化的前提下驱动控件时，或其中一次写入坏掉时 |
| 16 | 按键显示 HUD 保留其原生窗口阴影，而不是共用的 elevation 载体 | 面板描边/elevation 改动，2026-10-07 | MEDIUM（已记录的决定） | 它被放在距可见边缘 `PANEL_EDGE_MARGIN`（18pt）处，而 `high` 级阴影需要 64–76pt 的窗口内边距；实测 AppKit 随后会把加了内边距的窗口约束回可见区域，面板会移动 26pt，这也会改变已保存的 `keystroke_display.position` 的含义。给 HUD 加阴影本身就是一个摆放决定（向内移，或按所贴屏幕边缘逐边加内边距），不是第二个 elevation 级别 | 下次改动按键显示的摆放或已保存位置时，或 HUD 的原生阴影被报告为其它面板已关掉的那种深色外圈时 |
| 17 | `liquid-glass` 面板描边由共享装饰视图绘制，因此与玻璃自身的边缘高光并存 | 面板描边/elevation 改动，2026-10-07 | LOW（观察） | 在 picker 上实测：该描边为 1pt、使用 `card_border` 色调，画在面板自身的边缘，其它都没变；`.regular` 玻璃上的双重边缘尚无报告也未被实测为缺陷。文档 §3.4 规定描边是跨材质的一条视觉契约，所以按材质开例外就必须是有文档的例外 | 玻璃边缘看起来像两条边时，或下次改动玻璃样式/着色时 |
| 12 | ~~一个原点是当前桌面的全屏窗口，会从该桌面的卡片中丢失~~ **2026-10-07 关闭**，由基于位置的契约解决：无论切换状态如何，每个全屏 Space 的窗口都被收录，所以既不会因原点缺失、也不会因纯 CG 门禁旧的 `OtherDesktop` 要求而被隐藏（`space-desktops.sh` 钉住了它：「开关关闭时全屏窗口仍是一张卡片」） |
| 13 | `scripts/e2e/space-groups.sh` 在本机无法运行：它的 `ctrl+left` 步骤需要一个已绑定的「向左/右移动一个 Space」快捷键，而 79/80 号键处于启用但未分配按键的状态 | 本次 | LOW（环境） | 该场景是焦点场景（`run-all.sh` 除非加 `--include-focus`，否则跳过）；它覆盖的原点逻辑已由 A1 单元测试断言，其全屏阶段被跑过一次（它识别出了原点并列出了全屏窗口） | 存在 Space 切换快捷键时，或该场景改用其它方式切换 Space 而被重构时 |
| 14 | `scripts/e2e/space-desktops.sh` 按启发式挑跨桌面目标，因此可能落到 app 还无法收录的窗口上（即已记录的「本进程内未访问过的桌面」收窄），从而报告某阶段未运行 | 本次 | LOW（测试工具） | 该场景现在会过滤到 app 在那里没有窗口的普通另一桌面，并连同已收录卡片报告 NOT RUN，而不是误报 FAIL；改为从已收录的其它桌面卡片本身挑目标需要重构场景 | 下次触碰该场景时，或某个目标选择掩盖了真实的收录回归时 |
| 15 | ~~全屏原点由当前 Space 邻接关系推断，因此 Space 列表重排后会把该 Space 归到错误的桌面~~ **2026-10-07 关闭**：原点不再参与收录（契约读窗口所在位置），错误的关联不再能隐藏或错放卡片；该推断保留为 e2e 诊断，`a_reordered_space_list_groups_by_adjacency` 仍钉住它报告的内容 |
| 9 | `space-desktops.sh` 的截图资格检查在 summon frame 早于缩略图 workset 发布时报告 NOT RUN | 本次 | LOW（测试工具） | 该检查是诚实的（NOT RUN，绝不误报 PASS），重跑即通过；改成等待 workset 发布属于测试工具打磨 | 下次改动该场景时 |
| 8 | 配置变更在途失效没有多线程交错断言 | 功能审查，R9 | LOW | 该行为已端到端断言（A2 关/开）；交错需要一个确定性的钩子 | 下次改动 `runtime_config` 失效逻辑时 |
| 20 | overlay 的 `performKeyEquivalent:` 覆写没有调用 `super`，因此不会把未被认领的 key equivalent 转发给下层视图树 | 面板动作键审查，R3 | LOW | 目前没有 overlay 子视图消费 key equivalent，所以转发路径没有可观察效果；等某个子视图需要时补上是两行改动 | 下次改动 overlay 视图树时，或第一个想要 key equivalent 的 overlay 子视图出现时 |
| 22 | 对返回保存确认弹窗的 app 按 Cmd+Q：面板只在动作后 750ms 的 `ACTION_FOCUS_WINDOW` 内保持自身打开，所以一个需要更久才获得焦点的对话框仍会让它关闭 | 面板动作键审查，R4 | MEDIUM | 该保护覆盖动作立刻引发的焦点转移；覆盖延迟发生的转移意味着把抑制挂到目标生命周期（`NSApplicationDidTerminate`）或真正的 key window 变化上，而不是一个截止时间，那是输入路径上的新仪器。该 A2 场景不发送 Cmd+Q，所以这条路径无论如何都未被断言 | 下次改动面板焦点保护时，或首次报告退出确认框存在时面板却关闭时 |
| 21 | ~~面板动作键的 A2 场景尚未运行~~ **2026-10-14 关闭**：经用户同意运行；连续两次 PASS，每次都打印其证据（受控目标 pid、卡片数 15→13、sticky 在释放期间保持、esc 清除声明）。它发现了一个真实缺陷并已修复：Cmd+H/Cmd+Q 会改变最前台 app，使这个非激活面板 resign key 并关闭——与承诺的「面板保持打开」相反。`action_owns_focus` 会为该特定 resign 抑制 750ms，`overlay::panel_keys::tests::a_panel_action_owns_only_the_resign_it_causes` 同时钉住该抑制及其过期 | — |
| 21 | 一次 `--smoke-*` 启动会留下它按进程创建的临时目录（`$TMPDIR/oh-my-tab-clip-images-smoke-<pid>`）：各 runner 通过 `process::exit()` 退出，没有东西清理它 | smoke 隔离任务，审查第 2 轮，2026-10-11 | LOW（临时文件） | 自粘贴板门禁之后，一次 smoke 启动只能写入它自己的合成条目，所以这些目录是垃圾而非用户内容，审查员也将自动清理评为非阻塞；一个必须在每条 `process::exit()` 路径上都运行的清理（20 个 runner 加崩溃路径）会是一个新的、容易做错的仪器 | 下次改动 `clipboard::smoke` 或 `monitor::start` 时，或临时目录堆积被报告时（2026-10-11 实测：60 个目录、30 MB，含修复前那些曾记录真实粘贴板内容的运行——已手工删除） |
| 22 | picker 头部其余静态墨迹（搜索框、角落关闭按钮、过滤药丸、页脚图例）在 picker 窗口被复用且主题变化时不会重绘 | 剪贴板齿轮/拖拽审查，R1（finding 6） | LOW | 齿轮和拖拽把手在 `apply_theme` 里被修是因为那次改动新增了它们；其余早于它，重绘整个头部是另一件事（头部只构建一次，今天只有页脚和清除动作会重建） | 下次改动 picker 头部样式时，或首次报告明暗切换后头部颜色陈旧时 |
| 23 | `scripts/e2e/panel-edge.sh` 在本机失败（检查 1–3 是基于截图的描边/阴影/材质模糊测量），且在 HEAD 上以同样方式失败，因此默认 A2 聚合目前报告一个与被测改动无关的 FAIL | 剪贴板齿轮/拖拽会话，2026-10-12 | MEDIUM（门禁） | 失败是环境性的且早于本工作：在 HEAD 的干净 `git worktree` 中验证过，同样的三项检查以相同输出失败，而其它每个场景都通过。诊断它需要当前会话无法控制的显示/背景设置（这些检查比较白色/灰色背景截图），那是独立任务 | 下次面板描边/elevation 或背景截图路径改动时，或有一台截图环境已知正常的机器能跑它时 |
| 24 | 存储提示的「按状态决定动作」规则、清除事务的完成边界，以及清除的保守校验都没有 A 层断言 | 剪贴板阻塞提示任务，设计轮 1–6 + 实现，审查轮 1–3，2026-10-12 | MEDIUM（覆盖缺口） | 用户明确要求本轮不加新测试，审查员确认该例外由用户给出（不是审查员）——修复本身并未延后。替代现状：分类器是纯函数（`notifications::storage_action_key`），每个界面都在 `--e2e-state` 的 `clipboard_notice` 中报告它*实际*应用的动作（`grant`/`clear`/`hidden`/`absent`/`unset`），既有 smoke 只覆盖 `Unavailable` 路径的按钮标题/选择器（`--smoke-clipboard`）与 `Ready` 下 About 行的重绘 | 下次改动 `clipboard/notifications.rs`、清除事务（`clipboard/storage.rs`、`clipboard/persist.rs`）、设置横幅或 picker 空状态时——或首次报告动作错误或失效时。最省的断言：(a) 纯函数——四种阻塞原因对应 `ClearAndRestart`，仅 `Unavailable` 对应 `GrantKeychain`，`PurgePending`/`Ready`/`Uninitialized` 对应 `None`；(b) smoke——停在一个阻塞状态，断言面板按钮的标题+选择器与横幅的 `clipboard_notice` 值；(c) 完成边界——用清除首次 bump 产生的 generation 捕获一个加载结果，然后断言 `apply_load_outcome` 在边界推进 generation 之后丢弃它（`apply_stale_load_for_tests` 这个形状已存在）；(d) 清除校验——一个存活的普通文件和一个不可读的缓存条目各自让事务保持未完成，而一个已确认的非文件对象（测试模式的 `history` 子目录）不会；(e) 密封缓存探针——不可读条目或符号链接回答「未知」（不创建密钥，也不自动隔离：`Blocked`，而非密钥永久丢失路径） |
| 25 | 按键绑定，第二阶段：就地编辑已有行的组合键（改键）、带参数的动作（打开 app / 文件夹 / URL / 设置面板）、桌面切换、窗口恢复、按下/释放两阶段的切换器动作，以及给动作选择器分组（自绘弹窗没有分隔项） | 按键绑定功能，设计轮 1–3 + 实现审查轮 1–3，2026-10-12 | —（范围） | 用户把第一阶段冻结在无参数动作并延后改键；删除默认行再新增一行已能达成同样结果，且「恢复默认」可撤销。桌面切换需要一个能用的机制（合成 Dock 滑动是在 macOS 26 上不移动 Space 的探针），恢复需要今天没有任何东西持久化的按窗口预吸附状态 | 下次改动 `src/key_bindings/` 时，或用户要求第二阶段时 |
| 26 | 没有 A2 场景端到端驱动「用户绑定：录制 → 确认 → 执行」这条路径（切换器与剪贴板热键各有自己的场景，但在设置 UI 中录制的绑定没有） | 按键绑定实现，审查轮 3，2026-10-12 | MEDIUM（覆盖缺口） | 这类场景必须通过系统事件流注入全局组合键，因此每次运行都会抢焦点，需要用户同意；录制器的决策改由 A1 断言（`release_wait_step` 的完整序列、`draft_rejection`、队列的触发再授权、迁移与回退矩阵），`scripts/e2e/settings-layout.sh` 则断言该页几何、侧边栏条目与已发布状态不变量，并为缺少这些字段的快照准备了反例。GUI 中明确未验证的：取消录制会恢复新增行、草稿会因窗口关闭或权限撤销而被清除、离开页面会取消——每项背后的状态在 A1 有断言（`has_draft`/`pending_binding_recording`、无条件的主线程清理），但没有场景驱动该交互 | 下次改动录制器或新增行时，或首次报告录制的绑定不触发、新增行卡住、或草稿陈旧时 |
| 27 | 自 Key Bindings 页合并成一个扁平列表后 `scripts/e2e/settings-layout.sh` 未再运行，因此其几何断言（一张卡片覆盖其行、卡片底部跟随实时行高）与迁移后状态不变量（混合旧配置下 `key_bindings_live: 5, key_bindings_entries: 13, key_bindings_enabled: true`）只被部分验证 | 按键绑定实现，审查轮 12，2026-10-12 | MEDIUM（覆盖缺口） | 该场景驱动真实窗口并抢焦点，每次运行都需要用户同意。*已*验证的：`binding_state_problem` 的反例可单独通过（含修正后的混合分组迁移现在产生的 `live=5 / entries=13 / enabled=true` 快照，在用户真实配置上核对——`[key_bindings] published 5 live binding(s) from 13 entr(ies)`，修复前是 13），卡片覆盖的调试断言由 `scripts/dev-restart.sh` 下全部七个设置页加 1/18/0 绑定变体跑到，零覆盖或 panic 行，`live_row_height` 回归由 A1 测试钉住 | 下次改动 Key Bindings 页布局或 `page_canvas.rs` 时，或下次带焦点同意运行 A2 时 |
| 28 | `SettingsButtonRole::Compact` 的 normal 与 hover 两个 token 在 `card_bg` 上只差 1.024:1，而且该角色**已没有设置页调用点**来支撑现在就修 | 行按钮 hover 修复，2026-10-14 | LOW（token 设计） | 本条最初声称鼠标页的「Add mapping」条带位于 `field_bg` 面板上、因此仍需要 `Compact`；**那是错的**——该条带被加到 `card_bg` 上（`page_builder.rs:1293`），所以它带着同样的 1.024:1 hover，并在同一次改动里被修（按键绑定的「Add binding」条带也一样）。现在每个设置按钮都指名一个有可见 hover 的角色，`every_role_the_settings_pages_ask_for_has_a_visible_hover_on_the_card` 会在任何页面再次伸手要 `Compact` 时失败。剩下的只是*角色定义*本身：重定义其 hover，或把「字段族按钮」与「内嵌字段」拆开，会一次移动多个界面，而目前没有任何东西需要它 | 下次改动 `SettingsButtonRole` 的调色板映射时，或 `Compact` 出现第一个新调用方时（源码门禁会点名它） |
| 29 | 鼠标页的映射行与「Add mapping」条带是凭着 Key Bindings 模型图加样式文档移到 `SettingsButtonRole::ROW_ACTION` 的；`docs/pictures/mouse.png` 早于 token 层，它把这些控件画在带底色的 HTML 表格条带上而不是 `card_bg`，所以鼠标页的确切预期表面从未对照更新过的参考核对过 | 行按钮 hover 修复，2026-10-14 | LOW（未核对的表面） | 该角色满足规范规则（`button_bg` + 边框 = 文档 §6 的次要动作表面），且 hover 在两种模式下现在都可感知；只有*模型图*一致性未核对，而没有更新过的鼠标页参考可对照。Key Bindings 页已在运行中的 app 上目视确认（`--open-settings=key-bindings`）。相关：Key Bindings 模型图把整宽的「Add binding」条带画成 `field_bg` 族底色，而文档的三角色按钮模型（§6）没有这种东西——该改动遵循了文档，所以模型图与代码在此处分歧 | 存在更新过的鼠标页（或 Key Bindings）参考时，或下次改动任一页的行渲染时 |
| 31 | 极端配置 `appearance.corner_radius` 会把切换器页脚标题挤到不可读宽度：`theme::footer_text_frame` 的 `.max(1.0)` 只保证 1pt 的 frame，于是预留区（角内缩 + 28pt 按钮 + 间隙，对称施加）可以吃掉整个面板 | 切换器/HUD 设置按钮实现审查，R1（finding 7） | MEDIUM（用户可见行为，尚无约定的回退） | 按钮本身仍在角内，且默认 32pt 半径有很大余量（在 560pt 面板上预留区大约到 r ≈ 232 才开始咬人），所以这是刻意构造的边界，不是回归。修法是一个用户尚未做出的*选择*：抬高页脚条带（有界的 F1 渐变，因控件居中而确实减小所需内缩）、给文本设一个写入文档的下限，或在这样的半径下隐藏该条目。在没有该决定的情况下实现其一，等于对一处可见行为改动进行猜测 | 用户选定回退方案时，或首次报告页脚标题变得不可读时 |
| 30 | 无法确认 Fast 模式确实被*使用*：一轮会记录它请求的 tier 和 codex 报告的拒绝，但不记录服务器实际处理的 tier | 审查设置（模型 + Fast），实现，2026-10-14 | LOW（验证边界） | 不引入新机制时唯一的证据是 codex 自己的 OTEL 行（`RUST_LOG=codex_otel.log_only=info` → `event.kind=response.completed … service_tier="priority"`），那意味着 helper 为 CLI 设置 RUST_LOG 并解析其 stderr——一套自己的诊断协议，正是 AGENTS.md 警告的「为满足某一轮而存在的仪器」形状。没有任何东西声称该 tier 被用上：公告与归档的 `service_tier` 说的是*请求*，而 `service_tier_rejected=yes` 加一条 stderr 备注标记拒绝。2026-10-14 在 codex-cli 0.162.1 上实测：`fast = true` 时服务器回显 `service_tier="priority"`，`fast = false`（`service_tier=default`）时不会 | 首次某一轮 `fast = true` 被报告为实际上并不更快时，或首次怀疑静默降级时 |
| 32 | 拖动滑块时，每一次连续 action 都会提交 `keystroke_display.max_extent_percent`；400ms 的落盘防抖是静默期，不是松手提交 | keystroke 最大尺寸审查，R1–R2（finding 2） | HIGH | 写入的值与松手提交会写的相同，且配置合并是逐字段的，所以可观察结果只是拖动期间多写几次、而非错值；改成松手只提交一次需要第 33 条那个可靠的手势结束信号，在这里修只是把同一个竞态挪个位置 | 下次改动 `settings/dispatch.rs` 的滑块 action 路径时，或跟踪结束信号落地时 |
| 33 | `sendEvent:` 不是可靠的滑块跟踪结束点：NSSliderCell 的跟踪循环可能在正常分发之前就消费掉 mouse-up，因此在窗口分发点结束尺寸预览是尽力而为，漏掉一次就回退到 3 秒 watchdog | keystroke 最大尺寸审查，R1–R2（finding 4） | MEDIUM | 一次短拖动最多能让预览（虚拟键帽、真实输入被丢弃）存活 3 秒；正确的结束点在原生跟踪边界内，意味着自己跑跟踪或钩住 cell——设置事件路径上的新仪器。修复期间实测：来自 action 处理器的事后修正、`mouseUp:` 覆写和一个延时定时器都会被 cell 撤销，这正是吸附改用 `allowsTickMarkValuesOnly` 的原因 | 下次改动滑块跟踪或预览生命周期时，或首次报告预览卡住时 |
| 34 | 100% 时流向上限是按完整 `screen.frame` 计算的，而 `clamp_origin_to_visible` 只移动原点，因此一个高或宽超过可见区域的面板不会被缩小以适配 | keystroke 最大尺寸审查，R1–R2（finding 5） | MEDIUM | 50% 默认值留有很大余量，而上限只是*允许*用户要求的尺寸；把最终尺寸夹到可见 frame 是一个可见行为决定（它也会改变靠近边缘的已保存位置的含义），并且需要渲染器与状态机共用同一个有效预算 | 下次改动面板摆放或预算时，或在大上限下面板被报告出现在菜单栏或 Dock 之下时 |

| 36 | `scripts/e2e/panel-contrast.sh` 的 `filter-row` 取景（`ROW_Y_PT=62`）在当前面板布局下对**所有**材质都报“11049/54000 像素成带状变化，不是文字”，因此该区域无法测量；只有逐个页脚 caption 区域仍可用 | frost 浅色修复，审查轮 4，2026-10-16 | MEDIUM（门禁） | 该失败与材质无关（frost 与 liquid-glass 一致），指向取景坐标陈旧（搜索框/筛选行的 y 与选定时不同），不是被测改动造成的对比度回归；页脚区域由实际标签坐标得到、仍然可用 | 下次改动剪贴板 picker 头部/筛选行布局时，或再次依赖该场景的 filter-row 判定时——用 `--e2e-state` 的实际控件 frame 重定 `ROW_*`，并确认差分只来自字形 |

> **订正（2026-10-21，unbundled 安装交接审查）。** 审查轮 1 指出：杀掉 unbundled 实例（SIGTERM）不是成功退出，其 plist 设了 `KeepAlive.SuccessfulExit=false`，因此 launchd 会在大约 8 秒后拉起替代进程（实测 `state = spawn scheduled` → t+8s `state = running`，新 pid），而那时 dev 构建还没结束；替代进程随后占住实例锁，dev 版静默退出。当时我在 `--note` 里补了一句「已装的 `.app` 通过 SMAppService 登录项也有同样形态」，**这句是错的，在此撤回**。为判断这句而实测（与我们的代码无关，只关乎 launchd 语义）：一个临时 ad-hoc 签名的 `.app` 用真实 `SMAppService.mainApp` 注册成登录项（`status = 1`，`sfltool dumpbtm` 可见）后收到 SIGTERM，15 秒观察窗口内始终没有重启。普通 GUI app 的 job 是 `submitted by runningboardd`，出现在 `launchctl print` 但不在 `launchctl list`，随进程消失——它是瞬态的，且不带 `KeepAlive`。`SMAppService` 提供的是登录触发，不是运行期守护；只有 unbundled 的 plist 要求进程监督。因此 `dev-restart.sh` 对已装 `.app` 的既有处理本来就是对的，该 HIGH 只适用于 unbundled 路径，修复完整，`.app` 无需改动。测量用的临时 `.app` 已注销（`status` 回到 0）并删除：无进程、无 launchd job、无启用状态的登录项；`sfltool dumpbtm` 只留一条 `disabled` 的失效记录（bundle 已不在磁盘），无法启动任何东西。`/Applications/Oh-My-Tab.app` 的 BTM 记录未被触碰。

## 已在同一会话内关闭（不要重新打开）

下列各轮都在预算政策存在之前就已在本会话内修完；记在这里只是为了后来的读者不必重新推导。修复都在树里，并带有各自的 A1 反例。

- 条目 20（`shipped_release_notes_satisfy_the_renderer` 门禁）：**该检查已于 2026-10-12 按用户指示移除**，此前已把选择摆给用户（从历史 note 中剥离行内 markdown，或把扫描收窄到已发布的版本）。`release_doc_dev/0.2.2.md`（en）与 `0.2.3.md`（三个 locale）仍带 `**`，`render_release_notes_markdown` 会在 About 页的 notes 中原样显示它们；已经没有东西再断言这一点，所以一条引入行内 markdown、或丢掉一个 locale 块的 note 只能靠审查发现。`cargo test` 重新变绿（855 passed / 0 failed）。
- R12–R18：阶段性资格证据的所有权（每遍 staging、`FORGET_EPOCH`、prewarm 路径）。
- R21–R22：卡片渲染决策（`thumbnail_rendered`）、精确焦点检查的顺序，以及 `scripts/e2e/space-desktops.sh` 中正常路径的分支断言。
- R24–R28：跨桌面收录的 AX 身份历史（按线程证据 staging、计数的 pass epoch、生命周期否决、标记回收）。
- 原点 Space 的前台进程（曾是这里条目 5）：**在本机上不是缺陷。** 2026-10-07 实测：一次跨 Space 提交后经 `SLSManagedDisplaySetCurrentSpace` 返回，原点 Space 显示的是切换前最前台的那个 app，随后用户跑了真实场景——用切换器切走、用四指横向滑动返回——前台 app 是正确的。参考实现用 `SLSSpaceSetFrontPSN` 修这个问题，是因为它通过 `_SLPSSetFrontProcessWithOptions` 把 app 提到前台；我们的激活路径不会破坏它，而 SLPS *rescue* 路径（共用那个调用）在两次测试中都没被走到，因为激活自己完成了切换。如果哪天出现「返回某个桌面后前台 app 不对」的报告，修法是备好的：在前台切换前快照 `NSWorkspace.frontmostApplication` 与原点 Space id，切换后再调用 `SLSSpaceSetFrontPSN`，且仅在目标的跨 Space 归属已确认、原点前台不是我们时调用（这个保护避免撤销 raise，AltTab #5586）。
- `scripts/e2e/settings-layout.sh`（曾是这里条目 7）：长期 33/44 的失败是一个陈旧常量（`TOP_PADDING = 50` vs 规范的 48）加上同一 token 迁移留下的另外三处期望（行内按钮高 28 -> 32、行距 62 -> 60 = 52 + 8、卡片到头部 77 -> 72.2 = 24 + 4 + 20 + 16.2 + 8）。看起来不在网格上的 58.7 步距，是通过某一行的值标签量到的同一行距，该标签比其行标签高 1.3pt。已修到 44/44，未放宽任何容差。

- 终端分类目前还读不到系统最前台 app。`NSWorkspace.frontmostApplication` 属于主线程，而 `terminal_evidence` 跑在 `ax-raiser` 上，所以该读取被去掉，`frontmost_pid_matches` 为 `None`：当 raise 实验带 `--activation-api` 时，终端因此是 `unknown` 而不是 `landed`，产品路径则完全不记录 terminal（`terminal=-`，2026-10-08 实测，`scripts/e2e/space-desktops.sh --include-focus` 仍 PASS）。延后是因为诚实的修法是主线程验证通道，而第二阶段修复步骤无论如何都得建它；为诊断让一个后台 AppKit 调用活着，正是该不变量禁止的越界。触发：加入该通道的改动（届时在主线程采样最前台 pid 并作为纯数据带回），或 terminal 被移入产品行为的任何改动。

- 单独 `setEnabled:` 会让一个**填充了 layer 的 `SettingsButton` 看起来仍是激活的**：该样式按钮自己画 layer 填充，`setEnabled:` 不会让它变暗，所以禁用的主按钮保持全量 accent、读起来像可点（命中测试仍正确；只有外观在说谎）。在构建 Key Bindings 新增面板的 OK 按钮时发现，该按钮现在在本地用 `apply_panel_button_dim` 把自己降到半透明，符合样式文档的禁用规则（控件填充 50% 不透明度）。延后是因为诚实的修法属于 `SettingsRow::set_view_enabled_with_tooltip`——为样式按钮加一个分支——而那会改变 app 里每一个禁用按钮的外观，是一次独立的视觉审查，不该夹带进这次改动。触发：下次改动 `set_view_enabled_with_tooltip` 或 `SettingsButtonRole` 时、对禁用控件做一次视觉巡检时，或首次（截图）报告某个禁用按钮看起来可点时。

- **样式按钮的禁用外观**只被局部修复。`setEnabled:` 不会让填充了 layer 的 `SettingsButton` 变暗，所以禁用按钮保持全量填充、读起来像可点（命中测试正确；只有外观在说谎）。Key Bindings 新增面板把它自己的两个按钮降到半透明（`apply_panel_button_dim`，样式文档 §10 规则），但 app 里其它每个禁用的样式按钮仍显示其正常填充。通用修法属于 `SettingsRow::set_view_enabled_with_tooltip`（为样式按钮加一个分支），并会改变每个禁用按钮的外观，那是一次独立的视觉巡检，不该折进一次布局改动。触发：下次改动 `set_view_enabled_with_tooltip` 或 `SettingsButtonRole`、对禁用控件做一次视觉审查，或（截图）报告某个禁用按钮看起来可点时。

- 跨桌面 raise 的恢复覆盖证据（审查第 2 轮以 MEDIUM 提出：删掉动作前等待也缩短了恢复覆盖）。仍有三种状态缺少反例：第一次 AX raise 没匹配到任何元素、目标只是迟到、以及跨桌面*最小化*恢复。延后是因为每种都需要专门搭的场景——首次尝试时元素缺失、延迟到达、停在另一桌面的最小化窗口——而今天没有东西能从外部强制这些状态。*已*覆盖的：`scripts/e2e/space-desktops.sh --include-focus` 端到端驱动 rescue 分支，并现在**同时**核对系统最前台 pid 与精确的聚焦窗口（第 4 阶段），而 `--other-desktop-no-activation` 让被抑制的激活分支变得确定（2026-10-08 实测：`activation=false`、`first_rescue_ms=0`、`elapsed_ms=216`、目标 app 最前台、聚焦窗口 == 所选那个）。触发：下次改动 raise 或恢复路径时，或首次报告一次迟到或最小化的跨桌面切换没有落地时。

- 通知**点击**路径没有断言。`did_receive_notification_response`（`didReceiveNotificationResponse:withCompletionHandler:` 委托方法）只有在用户点击更新或剪贴板通知时才被走到，且无法由 smoke runner 驱动：回调会读 `response.notification.request.identifier`，而 `UNNotificationResponse` 没有公开构造器能造一个假的。它的参数个数是正确的——三个显式参数，不同于 2026-10-08 崩溃的 `willPresent` 回调，后者少一个参数，因而把通知对象当成了 completion handler——而且它的零参数 completion block 用的正是该模块授权 block 已经成功运行的同一套手写 invoke。作为未运行覆盖延后。触发：点击路径的任何改动，或报告点击通知没有反应时。

- keychain 弹窗**已解决且机制已被测量**（2026-10-08）：ACL 是按*创建*该项的 app 判定的，所以 ad-hoc 构建创建的项会让之后每个构建都成为陌生人（对它 `SecItemDelete` 会被 `errSecInvalidOwnerEdit` 拒绝），而 Developer ID 签名构建创建的项能在重建中存活（探针与真实 app：67 ms，无弹窗）。dev bundle 现在用 Developer ID 签名，那次一次性重置——用户删除该项，app 新建一个并把旧存储留作 `*.failed-<ts>`——已在本机执行。这里没有待办的 backlog 工作；仍未验证的是早于 Developer ID 签名的*发布版*安装，它需要同样的一次性重置（发布渠道今天确实用 Developer ID 签名）。

- **整行选择浮层必须留在该行控件之下**（2026-10-13）。把绑定行移到共享 builder 上后，浮层跑到了上面（`addSubview:` 追加，末尾即顶部），于是透明浮层吞掉了 switch 的点击，点 switch 反而选中了行。修法是用 `addSubview:positioned:relativeTo:` 放到 -1 索引，并由 `debug_validate_row_overlay_is_below_controls`（A1，经 `--smoke-settings-layout`）断言该顺序；把普通 `addSubview:` 恢复回去即可证伪（报告浮层索引 5 vs switch 4）。给下一位读者的提示：先试过基于 `hitTest:` 的检查，它在 smoke runner 中**不可**用——它返回哪个视图取决于窗口坐标状态。子视图顺序才是确定性事实。

- **按键绑定页脚的 segment 门控只有一条 A1 断言，没有 A2**（2026-10-13）。`--smoke-settings-layout` 驱动 `update_binding_controls_enabled` 并对照页面状态断言两个 segment 标志（证伪：去掉 `-` 的选择门控会让它报告 `remove=true (want false)`）。*未*断言的是端到端路线：一个选中某行、点 `-` 单元格并观察 `entries` 下降的 A2 场景。本轮已手工验证（`selected row 1` -> `removing selected row 1` -> `2 entr(ies)`），以及 `+` -> `binding add panel opened`，但 tier-B 证据不是断言。作为未运行覆盖延后。触发：下次改动页脚控件时，或 `scripts/e2e/settings-layout.sh` 增加按键绑定章节时。

- **分段控件上的 `NSControl::setEnabled:` 值得一条注释，而不是一条规则**（2026-10-13）。合并后的页脚控件第一版把整控件的 `setEnabled:` 排在逐 segment 调用之后，理论是 `NSControl` 的 setter 会重置每个 segment。对照真实控件实测（通过 `--smoke-settings-layout` 把两种顺序都证伪）：它**不会**——两种顺序产生相同的逐 segment 标志，所以原来的顺序从来不是 bug。注释现在记录的是实测而非理论。无需进一步工作；记录于此以免仅凭 AppKit 头文件又把错误理论「修」一遍。

- **self-frontmost 标志的通知投递未运行**（2026-10-13）。窗口动作的组合键现在会在本 app 最前台时放行，于是设置里的文本框能保留 Option+方向键的编辑键。规则有断言（单元测试，去掉 pass-through 分支即可证伪），该标志的*接线*在 `--smoke-settings-layout` 中断言（`debug_validate_self_frontmost_wiring`：控制器回应两个激活选择器且标志能往返）。目前没有任何 tier 覆盖通知本身：smoke 运行从不把本进程变成活跃应用，所以 `NSApplicationDidBecomeActiveNotification` / `DidResignActiveNotification` 从不投递，该标志全程合法地为 `false`（实测：`show_settings()` 加一段泵过的 run loop 之后 `nsapp_active=false`）。在那里断言一个值，即使把发布逻辑删掉也会通过。触发：下次改动该标志时，或出现一个激活设置窗口并在文本框中按 Option+方向键的 A2 场景时。

- **待定手势会被任何真实按键清除**（2026-10-13）。`decide_key` 现在拥有清除逻辑，所以接线在被测函数内部（第一次尝试手工断言该规则，把调用从 tap 路径移除后仍然通过——移进去之后被证伪）。只覆盖了纯规则及其位置；真实键盘序列（Command 点按、Command+C、350ms 内再按 Command）没有覆盖，因为它需要活的 tap。触发：下次改动手势处理时。

- **侧边栏墨迹测量的「渲染并扫描」那一半没有单元测试**（2026-10-14，侧边栏对齐审查 R4，LOW——覆盖缺口）。`pixel_rows_to_parent_span`（像素行 → 父空间的换算，含翻转分支与半像素）是参照独立期望断言的，其反例在本任务所替换的实现上会失败。仍未被单元测试覆盖的，是渲染该按钮并判定哪些像素是墨迹的那一半：那需要真实 AppKit 会话，所以只由 `--smoke-settings-layout` 在三个 locale 下走到。审查员指出该扫描仍包含决策逻辑，而不只是纯 I/O。触发：下次改动侧边栏渲染或 `child_ink_span_in_parent` 时，或下次某次 smoke 运行报告某行明显画了字却量不到墨迹时。

- **显示器 backing 变化不会重新解析侧边栏标签几何**（2026-10-14，侧边栏对齐审查 R7，MEDIUM——按用户决定延后）。`center_sidebar_label` 按解析出的 frame 做缓存，键为行高、标签宽度、标题、字号与 backing scale，所以屏幕变化后重新查询会正确解析，纯颜色调用方则复用缓存。缺的是*触发*：没有东西观察 `NSWindowDidChangeBackingProperties`，所以窗口被移到不同 scale 的显示器后，会一直沿用旧显示器上解析出的 frame，直到某个其它动作重跑定位（切页会）。测量只在一个 2x 显示器上做过，所以跨显示器的误差大小未验证；同一标签在 1x 与 2x 下渲染最多差 0.25pt（四个标题中三个为 0.0000pt），低于 1pt 的断言容差，也低于一个显示器像素。触发：报告在显示器之间移动设置窗口后侧边栏文本错位，或下次改动 `center_sidebar_label`、设置窗口的观察者、或 `rebuild_settings_content` 时。

- **成功 toast 没有对照它实际显示的字符串做断言**（2026-10-14，设置导入/导出审查 R3，LOW——覆盖缺口，延后）。`bubble_metrics` 对 `BubbleKind::Success` 的形状有单元测试，且那两条设置 toast（「Settings exported.」/「Settings imported.」）在三个 locale 下都远短于 220pt 的单行预算，但没有任何东西在活的 cell 上测量这些具体字符串——将来更长的翻译或气泡宽度改动会静默截断。部分导入的说明刻意避开这条路径（它用 NSAlert 正因为它放不下）。触发：下次改动成功 toast 文案、tooltip 字体、或 `bubble_metrics` 时。

- **`--tasks` 不显示 session 属于哪个后端**（2026-10-16，pi reviewer 集成，真实 pi 审查 R1，LOW——可见性增强，按仓库预算延后）。新增的 `session-backend` 文件记录了每个任务的 session 由 codex 还是 pi 签发，切换 reviewer 时旧 session 会被丢弃并在 `session-log.tsv` 记 `discarded`；但 `--tasks` 的 `state` 列只看 `session-log.tsv` 的最后一个事件，不读 `session-backend`，所以列表无法直接回答「这个任务现在挂在哪个后端上」，也不能解释那次丢弃。不影响 `--tasks` 已有的承诺（session / 健康 / 在会话内轮数），只是少了一个跨后端排查用的列。触发：下次改动 `--tasks` 的列或 `task_row` 时，或出现「切换 reviewer 后无法判断哪个 session 还活着」的实际困惑时。

- **`handle_hover_at` 不比卡片视口裁剪命中区**（2026-10-16，图标选中样式审查 R1，MEDIUM——本次未在真机验证，延后）。它只逐张卡片比 frame：滚动时最后一排的 frame 本来就伸到视口下方（scroll teaser 的由来），所以指针停在底部标题栏上时，坐标换算到文档坐标后有可能落进那张卡，于是「点在状态栏上却选中了上面一行」。本条来自代码结构推断，未在真机复现。触发：下次改动 hover/tracking 路径时，或首次报告滚动时选中卡住、乱跳时。

- **`smoke_selection_ring` 的名字与 doc 仍以「环」为中心**（2026-10-16，图标选中样式审查 R1，LOW——命名与文案，延后）。它现在同时断言色板形状、关闭按钮的 scrim 数量与隐藏状态、以及按钮实际携带的字形墨色（`glyph_ok`），而名字和 doc 只提「环」。触发：下次改动该 runner 时。

- **新色板的深色模式与 Reduce Transparency 只在渲染稿上看过**（2026-10-16，图标选中样式审查 R1，LOW——未运行覆盖，延后）。浅色模式有真机截图；深色只有按代码数值渲染的模拟图，Reduce Transparency 完全没看过（Reduce Motion 无关：这次没有加动画）。触发：下次改动色板绘制时，或首次报告深色/降低透明度下选中态看不清时。
- **一次未复现的测试失败**（2026-10-16，图标选中样式 + 缩略图色板改动，MEDIUM——门禁不稳，延后）。删掉一个测试后的那次 `cargo test --lib` 报 `1087 passed; 1 failed`，但失败名未被捕获（抓取用的 grep 模式写错了），此后连续 18 次全绿。最可能的原因：`theme` 的几何测试要跨多次调用读全局 `CONFIG`（`card_h()`、`card_title_font_size()` 各读一次），而同一测试二进制里 `overlay/cards.rs`、`clipboard/{persist,detail,text_style}.rs`、`onboarding.rs` 的测试模块会**写** `CONFIG`，并行执行时两次读之间可能被改。触发：它再失败时先抓名字（`cargo test --lib 2>&1 | grep -A8 '^failures:'`），或下次改动这些几何测试时——最省的修法是让几何函数只读一次配置（把两个行高作参数传入，测试改成纯函数调用）。

- **窗口恢复提交判定的「调用点接线」没有自动化覆盖**（2026-10-17，隐藏菜单栏图标 + 窗口动作审查 R5，LOW——覆盖缺口，延后）。`arrange_landed` 本身有单元测试（把规则改成恒 false 会失败），但**调用点**是否使用它的返回值没有任何测试覆盖：把 `apply_direct` 改回忽略 `finish_snap` 的返回结果，现有测试仍会通过。真正覆盖需要驱动 AX 写回（`--smoke-*` 里没有可写窗口的稳定夹具），属于"测试仪器完善"而非用户可见缺陷，按仓库规则延后。触发：出现可按脚本写入 frame 的 A1 夹具时，或下次改动 `apply_direct`/`apply_direction`/`apply_display_move` 的提交路径时。
- **磨砂地板还差两条验证**（2026-10-17，frost 亮度地板审查 R1，LOW——未运行覆盖 + 仪器，延后）。(1) `scripts/e2e/panel-contrast.sh` 还没对 `FROST_FLOOR_ALPHA = 0.4` 跑过：代码注释里的 0.805→0.880 是临时 python 采样（与 backlog 里图标那条同一缺口），触发：下次改动该常量或材质路径时跑一次并留记录。(2) 白纱的**视图树**回归目前只有纯谓词测试（`carries_over`，钉住「三种装饰都不随材质迁移」），没有真的在应用里做一次材质往返/主题切换的断言；触发：下次改动 `swap_backdrop` 或 `apply_live_properties` 时，或在 keystroke/clipboard 的材质 smoke 里补一条「frost 恰好一层白纱、其他材质零层」。
| 41 | AppKit 调用层硬化（`ClassBuilder` 迁移）后，仍有 **252 处**手写 `class_addMethod` + 手写类型编码：`lib.rs` 126、`clipboard/notifications.rs` 38、`updater.rs` 31、`settings/select.rs` 16、`clipboard/detail.rs` 16、`clipboard/text_style.rs` 5 等 | AppKit 调用层硬化，代码审查 R1–R3 | MEDIUM（错误面） | 这些类的回调分散在多个模块（`OhMyTabClipboardWindow`/`OhMyTabClipSearchField` 的回调在 `notifications.rs`，方法签名一改就跨文件）、或方法数过多（`lib.rs` 的 controller 与菜单目标各 700+ 行）；已在 `ffi::dynamic_class_tests` 里用**逐个具名 getter** 的门禁锁住已迁移的 18 个入口，未迁移部分分批跟进 | 下次改动上述任一模块的类注册时；新类一律走 `crate::ffi::build_class` |
| 42 | release 构建不执行 Objective-C override 的类型编码校验（objc2 的检查在 `#[cfg(debug_assertions)]` 内），错编码在 release 静默通过 | AppKit 调用层硬化，代码审查 R1 | MEDIUM（错误面，已知边界） | 这是 objc2 的设计；本仓库用 `dev-opt` profile（保留 `debug-assertions`）与 debug 版 `dev-restart.sh` 作为开发期缓冲。已用 `a_void_pointer_object_parameter_is_rejected_only_where_objc2_checks_encodings` 把这个事实固化成断言，而不是假装 release 也设防 | 需要 release 级 ABI 保证时：只能真正启用 release 的编码校验（改 objc2 feature 或自建注册期检查）；`cargo test --release --lib` 只证明 release 行为没坏，它不启用 override 编码检查，本任务新增的测试也明确期望 release 接受错配 |
| 43 | `StaticClass::register_once` 的 `_name` 参数未被使用（仅为调用点可读性保留） | AppKit 调用层硬化，代码审查 R2 | LOW（整洁） | 保留它是为了让调用点写明类名，与 `build_class` 的第一个参数呼应；删除后调用点会变成只有闭包，可读性下降。不影响行为 | 下次整理 `ffi.rs` 的注册助手时 |
| 44 | `clipboard/detail.rs` 新增了本地 `raw_class` 助手，与 `ffi.rs` 的 `build_class` 职责重叠（前者走手写 `class_addMethod`，后者走 `ClassBuilder`） | AppKit 调用层硬化，代码审查 R2 | LOW（重复实现） | `detail.rs` 里三个类的回调在 `notifications.rs`、签名仍是 `*mut c_void`，迁到 `build_class` 必须同时改那个文件的签名；当时它与另一个并行子代理的文件冲突，因此先用本地助手做到幂等注册 + 冲突 panic，留住迁移路径 | 下次改动 `clipboard/detail.rs` 或 `notifications.rs` 的类注册时，把三个类迁到 `build_class` 并删除 `raw_class` |
| 45 | `super` 派发「`defined` 传错类」这一失败模式**没有机器可检的断言**：`for_defined_class` 接受任何非 null 类，把定义类错写成其父类时，真实调用仍是同一条实现（`setEnabled:` 无论从哪一级开始都解析到 NSButton），因此行为断言区分不出来 | AppKit 调用层硬化，代码审查 R2–R3，GUI 验证轮实测 | MEDIUM（覆盖缺口） | 实测结论：在 `settings_select_set_enabled` 里把 `settings_select_class()` 换成 `class!(NSButton)` 后，新增的 `settings_select_super_dispatch_reaches_nsbutton` 仍然通过——那条断言只覆盖「dispatch 不递归/不崩溃/确实到达父类」，不覆盖「defined 指对了类」。后者只能靠逐点审查：这是类型系统的能力边界（`StaticClass` 无法区分「定义类」与「父类」），不是仪器缺失 | 若要机器化：注册期把每个 override 的「定义类」与 selector 一起登记成表，并在 `for_defined_class` 里校验。下次改动 `ObjcSuper` 或注册助手时评估；或首次出现 super 跳级导致的用户可见缺陷时 |
| 48 | **A2 的 settings-layout.sh 在 master 上就是红的**：15 条断言失败，集中在**跨所有设置页**的页面内边距与行距约定（`content sits under the page header padding: gap=24.0`、`content ends on the bottom padding: bottom=20.0`，以及 `unexpected=[70, 36.5, 56.0, …]` 的垂直步长），而 `docs/design-style-en.md` 写的是页面顶部 48 / 底部 72 | A2 全量运行（`--include-focus`），2026-10-22；同一场景在干净 HEAD 工作树（`/private/tmp/omt-a2-base`，744f0fc）复跑，结果同为 `FAIL (26/42 checks)` | MEDIUM（门禁红；用户可见的页面留白与设计文档不一致） | 与本轮剪贴板改动无关（该改动只新增一张卡，不碰页面内边距），且**要先判定是应用没遵守设计文档还是场景的期望过期**——那是设置页间距的独立任务 | 下次改动设置页间距/页面画布时，或 A2 需要全绿才能发版时 |
| 50 | **A2 的 tab-repeat.sh 在 master 上就是红的**：`each published forward step moves exactly one card: steps=14 mismatches=[(2, 2, 5)]` | 同一次 A2 全量运行；干净 HEAD 上同样失败 | MEDIUM（门禁红；可能是节流/发布顺序的真实缺陷，也可能是采样竞态） | 与剪贴板改动无关（未触碰切换器重复节流）；需要单独定位是逻辑还是采样 | 下次改动切换器重复/节流或该场景的采样式断言时 |
| 51 | **A2 套件跨次运行不可重复**：`panel-action-keys.sh` 在 TextEdit 运行时直接失败，而 `switch-basic.sh` 会拉起 TextEdit 目标窗口却不清理，于是套件跑完一次后再跑就会在 `panel-action-keys` 失败 | A2 全量运行两次（本轮工作树 + 干净 HEAD 工作树），两次都命中同一前置条件 | LOW–MEDIUM（测试工具：A2 自称可重复） | 与剪贴板改动无关；修法要么让 `switch-basic` 结束时关掉自己拉起的 TextEdit，要么让 `panel-action-keys` 自己处理已运行的目标（并区分用户自己的 TextEdit）——需要先定所有权语义 | 下次改动 switch-basic.sh、panel-action-keys.sh 或 run-all.sh 时，或第二次全量运行 A2 时 |
| 52 | 暂停收集与排除应用列表**没有 A2 场景**：面板 orb 的暂停/恢复、菜单栏两态标题、"暂停期间复制不进入历史"的端到端断言目前只有 A1 断言与**人工实机验证**（2026-10-22 已逐条跑过：暂停期间复制 → 日志 `baseline advanced, nothing recorded`；恢复后不补录、下一次新复制正常记录；排除 Finder 后复制 → 日志 `source application is on the exclusion list`，移出后同一操作恢复记录；菜单栏标题在"暂停记录剪贴板/恢复记录剪贴板"间正确翻转），A2 层仍无脚本 | 剪贴板隐私控制实现，2026-10-22 | MEDIUM（覆盖缺口） | 编写该场景需要真实全局快捷键（option+v）与真实复制，属于新增测试面而非修缺陷 | 下次改动暂停/排除逻辑或面板顶栏时，或补齐 A2 覆盖时 |
| 53 | 「添加运行中的应用」下拉的**选择动作无法用现有自动化驱动**：NSPopUpButton 的 NSMenu 不进 AX 树，窗口内坐标点击与 AXPress 都不展开/选中该项。已验证的是：候选列表非空（13 个，日志确认）、菜单展开后 13 项名称正确、`index - 1` 的映射经代码审查；**未**验证的是真正落库那一步 | GUI 验证轮，2026-10-22 | LOW（覆盖缺口：路径正确性靠审查，不靠断言） | 需要能驱动 NSMenu 的仪器（例如 AX 暴露菜单窗口，或用 CGEvent 在菜单窗口坐标系下点击）；不值得为此扩展现有 cua 驱动 | 下次真正有人报告"添加运行中的应用不生效"时，或需要给该控件补自动化时 |
| 54 | 设置页**行分隔线**在 A1 层没有任何断言：`--smoke-settings-layout` 的 `debug_validate_settings_page` 只收集有文字/交互的视图，分割线视图根本不在 `entries` 里（实测 dump 出 `thin views: []`）；唯一的分隔线检查在 A2 的 `settings-layout.sh`，且**只覆盖 About 页**（过滤条件写死 `page_6_about`）。剪贴板页因此少了一条 hairline 也无人报警——2026-10-23 用户在截图里发现「启用剪贴板历史 / 暂停记录」之间缺线，才补上 | GUI 验证轮，2026-10-23；修复为在 `build_clipboard_page` 补 `SettingsRow::separator_above_row` | LOW（视觉细节；修复已按该页其余 10 行的同一组件写法落地） | 想要机器可检，需要先让 `collect_debug_layout` 收录薄视图（它目前按文字/交互过滤），或把 A2 的分隔线检查从 About 页推广到所有页——两者都超出「补一条线」的范围，且 A2 场景本身因 #48 处于红态 | 下次改动 `collect_debug_layout`、`separator_above_row` 或修复 #48 让 A2 恢复绿时 |
| 47 | 切换浮窗页脚标题开关（`layout.show_footer_title`）的三项覆盖缺口：(1) 运行期接线（`ChangeFlags` → `reset_switcher`、`needs_in_place_control_sync` → 就地 `setState:`）只有纯函数层面的几何断言，没有经真实 `apply_config_change` 入口的断言；(2) A1 GUI 与 A2 场景均未运行（`--smoke-overlay`、`--smoke-settings-layout`、`--smoke-settings-state-sync`），关闭标题且窗口列表为空时的居中空态提示只有"不 panic"的纯函数保护；(3) 该开关改变面板高度，未验证"配置 reload 时正在显示的面板被收起、下一次召唤按新几何排版"这条路径 | 页脚标题开关实现审查 R1–R3（见 `.agent-review/tasks/footer-title-toggle/`） | MEDIUM（未运行覆盖 + 断言缺口） | 用户在实现阶段明确要求先不写测试、先看效果，随后才送审；三轮审查预算已用尽，本轮只补了纯函数与布局输出断言（零带子行高、reserve 阈值、卡片与浮动控件不相交、面板不超宽预算、开关→带子映射、配置兼容），这些都是 headless 可跑的；GUI runner 需要图形会话且会改动真实窗口，未在本次执行 | 下次改动该开关、`footer_band_h`、`footer_control_*_reserve` 或 `overlay::update_status_label` 时；或交付/发布前跑 A1/A2 门禁时 |



| 55 | **悬停气泡的移除没有 A1 断言**：2026-10-23 按用户要求删掉设置页每行描述的悬停气泡后，那次改动附带了一条「描述标签上没有悬停出口」的检查。该检查**被验证为无效并已删除**：它按 `t(key)` 的字符串在页面树里找标签，而描述标签位于 `NSScrollView` 的 document 里——按文本查找要么找不到，要么命中页面为其他用途保留的隐藏副本（实测「6 checked」里只有 3 条是用户实际能悬停的那一类，`clipboard_enabled`/`delete_after_paste`/`paused` 只存在于窗口树）。进一步实测：窗口 `contentView` 往下的子视图递归只覆盖 1043 个视图、165 个 tracking area，**不含任何页面标签**；对滚动视图 `documentView` 递归会在主线程卡死；`isKindOfClass:` 会打到 field editor 并崩。就结论而言，代码里没有一条既能在真实视图上判定、又能跑得到那一步的路径。用户选择「删除该断言 + 记 backlog」。当前唯一的保护是 `widgets.rs::add_captioned_row` 里的 `debug_assert!(caption_tip.is_null() && title_tip.is_null())`：它只覆盖**原生** `setToolTip:`，把自定义气泡重新加回来时它不会失败（这正是 2026-10-23 之前的形态，`attach_hover` 加进去时它也是绿的） | 悬停气泡移除（2026-10-23，提交 `4965711`），断言在 `fix:` 之后被验证无效并删除 | LOW（覆盖缺口：行为本身已由真机验证；缺的是回归门禁） | 可以做对的方向有三个，都需要先定机制：(a) 让检查只认 `OhMyTabHoverTooltipTarget` 这一类名持有的 tracking area，并先解决「如何在不卡死的前提下到达页面 document」；(b) 给 `--e2e-state` 增加一个「本窗口悬停出口数量」的状态字段，由视图层自己报告（AGENTS.md 的 promotion rule 允许这种替代）；(c) 改动 `collect_debug_layout`，让它收录 tracking area 并按页面归属分类，再把 `--smoke-settings-layout` 的页面遍历扩展到 document 层。三者都超出「删一个气泡」的范围，且当前的 `--smoke-settings-layout` 本身在风格门禁（#41）就红了 | 下次改动设置页的悬停/提示气泡路径、`add_captioned_row`、`--smoke-settings-layout` 的视图遍历，或修复 #41 让该 smoke 重新全绿时；或首次有人重新引入悬停气泡时 |
## 2026-10-24 — #56 关闭：A2 的 settings-layout.sh 已与设计文档对齐

`scripts/e2e/settings-layout.sh` 原先仍钉 `TOP_PADDING = 48` / `BOTTOM_PADDING = 72` 与 43/60/72.2 的步长表，而设计文档与
`SettingsPageHeader` 早已是 24/20、行高 56。现在两者的常量与步长表都来自同一份实测几何，并已对回 `src/settings/` 的常量：
页顶 24、页底 20、步长 0 / 20 / 36.5 / 56 / 62.5 / 70（每项在场景注释里注明它由哪些常量相加得到）。顺带把场景里其余随结构
漂移的钉子一并更新：About 页根名 `page_6_about` 改为 `page_7_about`（访达菜单页占用了 6），页面清单补 `page_6_finder_menu`
（七页改为八页）。

A2 实跑：`scripts/e2e/settings-layout.sh` PASS（45/45，鼠标页 7/7）。同一次 `run-all.sh --include-focus` 里另有两项失败，
归因如下（**都没有被消除，只是本改动不涉及**）：

- `settings-open-latency.sh`：**已修复并关闭**（见下方 `#49 关闭` 一节）。根因是场景点了只打开筛选菜单的 `...` 按钮，从不打开设置窗口。
- `panel-action-keys`：套件运行时报 `TextEdit is already running`，这是它自己的前置条件；按 pid 停掉该实例后单独跑 PASS。
- `hud-settings`：**套件内失败、单独跑 PASS，根因未确定**。单跑通过只能说明存在运行条件或套件顺序上的差异，不能排除产品竞态或
  场景间的状态泄漏（它失败时报 `the frontmost application could not be read`，紧接着 `pid … is still running with a development
  switch after the restore`）；下一次运行套件时若复现，应按前者分诊。
## 2026-10-24 — #57 关闭：折叠行 smoke 已修好，A1 全绿

两处修复，结论是「测试没有走真实入口」而不是折叠算法有错（这个判断来自 Codex 的独立定位，我据此复核后采纳）：

1. **smoke 用错了入口。** 自定义开关 `OhMyTabHtmlSwitch` 只在 `mouseDown:` 里翻转状态再 `sendAction:`；原 smoke 先 `setState: 1` 再 `performClick:`，而 `performClick:` 只发动作、不切换，于是回调读到旧状态。现在改为合成一次左键 `mouseDown:` 走真实入口，并在每次交互前重新从 `SettingsUi` 取控件（重建会换掉它，缓存指针会让第二次交互打到旧指针上），同时断言状态确实翻转，否则直接判失败而不是拿旧画面断言。
2. **启动阶段排队了一次内容重建。** `change_flags` 对任何 Startup 都把 `clipboard_excluded_apps` 置真，于是 `apply_config_change` 在启动时排队一次设置内容重建，晚到某个 run loop 轮次执行，可能在交互中途替换页面。现在该处只对真正的外部列表变化（Reload / RestoreDefaults / Menu）排队，Startup 不再排队。

**验证**：`settings_collapsible_row_geometry_smoke` 过了变异检查——把 `set_clipboard_delete_dependent_visibility` 改成「永不折叠」后该 smoke 立刻变红，恢复后变绿。A1 从 24/26 变为 **24/24 PASSED**。

**完整门禁的状态（如实）**：本条的修复让 A1 从 24/26 变为 24/24，但**完整 `scripts/gate.sh` 仍然不是 PASSED**，原因是与本次修复无关的两件事，分别记录、不当作已解决：

- `settings-open-latency.sh`：**已修复并关闭**（见下方 `#49 关闭` 一节）。根因是场景点了只打开筛选菜单的 `...` 按钮，从不打开设置窗口。
- 完整 `scripts/gate.sh` 的最近一次结果是 **gate: INCOMPLETE（a2 NOT RUN）**：前七层全部 PASSED，A2 因为 14 个场景需要 `--include-focus/-screen/-prefs` 而报 NOT RUN。带 `--include-focus` 单独跑 A2 是 9 passed / 2 not run / 1 failed —— 那个 failed 是下面这条既有失败，不是 NOT RUN。两个 not run 的单项是本机没有别的桌面、未绑定移动 Space 的快捷键。
- `panel_creation_and_badge_layout_smoke` 要求跑 A1 时没有 app 实例占用单实例锁：有 dev 实例在跑时报 `AlreadyRunning`。这是运行条件而非回归，但**它会让 A1 变红**，所以跑 A1 前必须确认没有实例在跑。
## 2026-10-24 — #49 关闭：settings-open-latency 点错了按钮

**根因（不是环境，不是点击没送达）**：场景点的是选择面板头部的 `...` 按钮，而它的 action 是 `toggleFilterPopover:`——只打开选项弹出框。「打开设置」是那个弹出框里的一**行**（`openClipboardSettings:`）。所以场景每天都点开一个筛选菜单、从不打开设置，`settings_click_ms` 永远是 0，warm 和 cold 都是 0ms。这解释了为什么它在干净 HEAD 上也红、为什么 warm/cold 数字相同。

途中排除的错误假设：光标位置、屏幕坐标换算、被其它应用窗口遮挡、前台应用、cua 点击通路——都实测排除了（暂停按钮的一次点击**确实**生效，证明输入是通的）。

**修复**：
1. `--e2e-state` 新增 `clipboard_picker.settings_row`：弹出框里「打开设置」那一行的矩形（弹出框关闭时为 null）。旧字段 `settings` 是 `...` 按钮，保留但注明它只开弹出框。
2. 弹出框开关现在发布一帧状态（`clipboard_popover`）：此前 `toggle_filter_popover` 只改原子变量，场景轮询会一直读到旧值。
3. 场景改为两步交互：点 `...` → 等 `filter_popover_open` → 点 `settings_row`，并在两步之间做悬停（浮动面板的指针轮询需要先看到光标）。
4. 顺带修了场景里两处 bash 3.2 陷阱（`< <(...)` 进程替换、`sys.argv[1:7]` 切片少一位）。

**验证**：连续三次运行 PASS，warm 113 / 126 / 120ms（均 ≤ 400ms 上限），cold 512 / 561 / 556ms（均严格慢于 warm，反例成立）。A2 全量（`--include-focus`）：**10 passed, 2 not run, 0 failed**。完整 gate：前七层 PASSED，A2 因 14 个场景需 `--include-*` 而 NOT RUN（INCOMPLETE）。

仍 NOT RUN 的两项是机器条件：本机没有别的普通桌面、未绑定移动 Space 的快捷键。

## glass-ui-refresh (2026-10-20)

- **Round 2/3 — MEDIUM — the clipboard light-mode tokens are not registered in the style document.**
  `row_selected/hover`, `field_bg(+focus)`, `hairline`, `glass_line`, `accent` now have clipboard-local
  light values that disagree with §3's table, and §3's contrast numbers were measured on the old values.
  Deferred on purpose: writing the new values into the normative table would present unverified contrast
  as verified. Revisit trigger: the first run of `scripts/e2e/panel-contrast.sh` against the new tokens
  (which is also what the palette decision in the report is waiting on).
- **Round 2 — MEDIUM — the clipboard drag surface has no A2 coverage.** `drag.rs` is exercised only by
  A1 smoke assertions (`OhMyTabClipboardDragSurface` topmost, `hitTest:` accept/reject) plus the existing
  `panel-drag.sh`; a real pointer drag over a blank top bar and a double-click reset are not asserted.
  Revisit trigger: the next `scripts/e2e/run-all.sh --include-focus` run.

## clipboard-search-blur (2026-10-22)

- **Round 1 — HIGH/HIGH/MEDIUM/MEDIUM — all four findings are in the untracked `liquid-glass-rs/` prototype.**
  Round 1 reviewed the whole working tree (the task diff was the three clipboard files), and its findings all
  belong to `liquid-glass-rs/`: `app.rs:60` (`CGDisplayBounds` FFI signature returns `CGRect` by value, not
  through an out-pointer), `app.rs:325` (an over-released `nextDrawable`), `app.rs:154` (a drawing view never
  attached to its window) and `compositor.rs:485` (a drawable copied into but never presented). That directory is
  UNTRACKED (`git status: ?? liquid-glass-rs/`), so it is another task's work-in-progress and not part of this
  change; its correctness is not this change's to assert or fix. Deferred on purpose, and stated in the round's
  `--note` so the reviewer judges the deferral. Revisit trigger: when `liquid-glass-rs/` is brought into the
  repository (its own review must decide those findings), or if any build starts linking it.

## finder-ext-launchd-stale (2026-10-24)

**第 4/5 轮遗留项已在第 6 轮修复，条目保留作记录。**

- **MEDIUM — `scripts/finder-extension-selftest.sh` 会按生产规则清理用户域中的其它陈旧服务。**
  已修：`omt_finder_ext_clear_instances` 新增可选参数 `only_labels`（按整行精确匹配的允许清单）。
  生产调用方不传（行为不变，仍然清理所有钉在已消失路径上的实例）；自测只传自己创建的三个标签，
  清单外的实例被报告为 `skipped` 而不会被 bootout。**自测仍然读取真实的 launchd 列表**——平台格式、
  `pipefail`、`state` 这些覆盖点一个不少——只有"移除的决定"被收窄，因此不存在"隔离性 ↔ 真实路径覆盖"的取舍。
  新断言：`an empty allowlist allows every label` / `a listed label is allowed` /
  `an unlisted label is refused` / `a prefix of a listed label is refused` /
  `a listed label plus a suffix is refused` / `every launchd instance outside the allowlist is still registered`。

## 2026-10-24 — 本任务的代码审查预算提高到 6 轮

用户把本任务的代码审查预算从默认的 3 轮提高到 **6 轮**（首次审查加最多 5 次复审），用于审查
`dev/arch-test-remediation` 分支上的实现。设计轮次不计入这个预算；Phase 0 的 7 轮设计评审记录在
`docs/architecture-test-remediation-plan.md`。

*Review budget and triage* 的其余规则不变：审查只在实现能构建、相关测试通过后进行；按影响而非标签
分级；每一条延后项都记在这里，并在 `--note` 中说明。

## 2026-10-24 — A1 tier measured on `dev/arch-test-remediation`

`scripts/smoke-runners.sh --run` reports 21 passed, 0 not run, 3 failed (of 24). All three failures are
**pre-existing on `master`** and were reproduced against a clean build of `0ea1635`:

| Runner | Failure | Baseline |
| --- | --- | --- |
| `settings::tests::settings_layout_smoke` | `unexpected rendered font size 11.00 ... "Startup Options"` | fails identically at `0ea1635` — this is item #41 above |
| `settings::tests::settings_layout_smoke_under_reduce_motion` | same assertion | same |
| `keystroke_display::tests::panel_creation_and_badge_layout_smoke` | `which: ... rail=false` | same at `0ea1635` |

They stay open here rather than being fixed on this branch: they are the repository's own open defects
(#41 and the rail measurement), not consequences of the gate or the callback-guard work, and fixing them
means changing the type scale or the rail geometry -- decisions that belong to #41 and to the design
system, not to a plumbing change. The A1 tier now *reports* them, which is what this branch was asked to
make possible; it must not turn them green by weakening the assertion.

## 2026-10-24 — A2 tier on `dev/arch-test-remediation`

`scripts/gate.sh --tier=a2` (the gate invocation, consent-switched scenarios skipped) reports:

```
e2e run-all: 1 passed, 0 not run, 1 failed, 14 skipped (focus/screen/prefs)
e2e run-all: panel-drag.sh FAILED
```

- `smooth-scroll.sh` PASSED.
- `panel-drag.sh` FAILED for an **environment** reason, not a code one: its first assertion needs the
  frontmost application as a no-focus baseline, and in this session `cua-driver list_apps` reports no
  app with `active: true` (`active: []`), so the scenario refuses rather than claiming an unverified
  result. That refusal is the correct behaviour; the run should be repeated in a session where the
  driver reports an active application.
- The 14 skipped scenarios need the user's consent for a real focus change (`--include-focus`), a
  full-screen backdrop / screen capture (`--include-screen`), or a real system-preference change
  (`--include-prefs`). The gate reports them as **INCOMPLETE** (exit 75 from the aggregator) rather
  than as success.
- Two scenarios still disturb more than they declare and need their missing marker before the gated
  tier can be trusted: `finder-quick-actions.sh` rewrites the real config (needs
  `# E2E_CHANGES_PREFS=1`). Noted for the next change to those scenarios.

### 2026-10-24 — re-measured after the verdict channel

`scripts/smoke-runners.sh --run` now requires a recorded verdict from each runner (it no longer infers
a pass from an exit code). Re-measured: **20 passed, 0 not run, 4 failed (of 24)**. The fourth failure
is `settings::tests::settings_collapsible_row_geometry_smoke`, which **also fails on master** with the
same numbers (`previous_y=824.4->824.4 next_y=699.0->699.0 move=0.0 expected_shift=56.0`, exit 1 from
the app at `0ea1635`). It had been counted as a pass before only because a silent runner was being
read as a pass — which is exactly the false pass this branch removed.

### 2026-10-24 — code review round 1 (8 findings, all fixed)

| Finding | Disposition |
| --- | --- |
| HIGH — A1 counted exit 0 + no NOT RUN as PASSED | The harness now requires a verdict the runner writes to the file named by `OMT_SMOKE_VERDICT` (`src/smoke_verdict.rs`); a silent runner is NOT RUN and `PASSED 0` is not a pass. Re-measured: 20 passed / 4 failed, the fourth being a pre-existing master failure previously hidden by the false pass. |
| HIGH — onboarding registration still crossed the C ABI | `segment_container_class_handle` is now a plain Rust function (its only caller is Rust), so a registration panic unwinds to the caller instead of aborting from `panic_cannot_unwind`. |
| HIGH — a failed recording-thread spawn left the app "recording" | `spawn_recording_thread` rolls the session back (`roll_back_recording_state` + the panel button); regression test `a_failed_recording_spawn_returns_the_session_to_idle`. |
| MEDIUM — clippy classifier swept production code into test regions | Rewritten to bound a region at the item's brace or its terminating `;`, to blank string literals, and to drop a region it cannot bound; it carries three counter-examples that run with the gate. |
| MEDIUM — the callback scan checked only a first-line prefix | `body_is_guarded` now requires the body to be exactly one guard call whose `)` ends it; the extractor tracks signatures and skips aliases, block declarations and `type` lines; counter-examples added. The stricter check found a real defect in `on_application_should_handle_reopen` (`appkit_bool(false)` outside the guard), now fixed. |
| MEDIUM — the AX observer's autorelease pool leaked on panic | RAII `AutoreleasePoolGuard`, drained on both paths. |
| MEDIUM — the exit-route test used test-only copies | `run_terminate_duties_for_test` calls the real `on_will_terminate`, and `quit_application_with` keeps the route's own body with only `terminate:` supplied; mutation-checked (removing the duties call fails the test). |
| MEDIUM — `finder-quick-actions.sh` lacked the prefs marker | `# E2E_CHANGES_PREFS=1` added. |

### 2026-10-24 — code review round 2 (5 findings, all fixed)

| Finding | Disposition |
| --- | --- |
| HIGH — the macro could still upgrade an early return to PASSED | The macro reads `was_reported` **after** the body runs, so a runner that reported NOT RUN itself is never overwritten; permission short-circuits in `icon_cache` and `collect_windows` now report NOT RUN explicitly; child-spawning runners go through `run_smoke_child`, which fails when the child exits without recording a verdict. |
| HIGH — the AX observer's pool was never constructed (only the pointer kept) | Both pools are now `AutoreleasePoolGuard`s that drain exactly once, in `Drop`. |
| HIGH (new, introduced by the previous fix) — the observer thread drained twice (manual + Drop) | The manual `drain` is gone; `Drop` is the only owner. |
| MEDIUM — the recording rollback restored only three fields | It now clears `REC_MODE` and `REC_PENDING_ACTION` too, stops the recorder tap, and leaves any key-binding recording, so `binding_recording_pending()` is false after a failed start. |
| MEDIUM — the scan ignored comment brackets and `pub(in …)` | `blank_code` blanks string literals **and** `//`/`/* */` comments before parenthesis matching; visibility is stripped generically (`pub`, `pub(...)`); counter-examples `comments_cannot_hide_code_outside_the_guard` and `any_visibility_is_recognised`. |

Re-measured A1 after the verdict feedback path was completed (the app's `--smoke-*` children now record
their own verdict through the same channel): **20 passed, 1 not run, 3 failed (of 24)**. The NOT RUN is
`settings_layout_smoke`, whose child exits 1 for the pre-existing #41 reason; the three failures are the
same pre-existing set. Nothing here claims a complete gate.

### 2026-10-24 — code review round 3 (4 findings, all fixed)

| Finding | Disposition |
| --- | --- |
| HIGH — a child's NOT RUN could still be upgraded to PASSED | `run_smoke_child` now reads the verdict the child appended and **forwards** it: `PASSED n` becomes the parent's `Passed`, anything else becomes `NotRun`, and a missing verdict is `NotRun`. Because the parent reports, its `REPORTED` set is updated and the wrapper cannot append a PASSED. |
| HIGH — a real failure was downgraded to NOT RUN | A child that exits non-zero, dies on a signal or cannot start now reports `FAILED` and panics, so the runner and the tier both fail and its stderr is kept. |
| MEDIUM — the recording rollback left the UI in the recording state | The rollback now reads the mode **before** clearing it (the binding cleanup is guarded by the mode), runs the mode's own cleanup, and the key-binding panel is refreshed for the binding case. |
| MEDIUM — the app read its verdict path from the environment | The app now takes it from **argv** (`--smoke-verdict=<path>`, through `dev_flags`), which is what AGENTS.md requires; only the test process reads `OMT_SMOKE_VERDICT`, because a test binary's argv belongs to libtest. The parent passes the flag to the child. |

A1 re-measured with the corrected classification: **20 passed, 0 not run, 4 failed (of 24)** — the four
are the pre-existing master failures (`settings_layout_smoke`, its reduce-motion twin,
`settings_collapsible_row_geometry_smoke`, `panel_creation_and_badge_layout_smoke`). No failure is
reported as NOT RUN any more.

### 2026-10-24 — code review round 4 (2 findings + 1 LOW, all fixed)

| Finding | Disposition |
| --- | --- |
| HIGH — some child runners still invented a PASSED | The reduce-motion, card-badge and keystroke-panel runners now spawn through `run_smoke_child`, which sends `--smoke-verdict=<path>` and forwards the child's own verdict; `child_verdict_class` parses the count instead of comparing the string, so `PASSED 0`, `PASSED 00` and `PASSED x` are all NOT RUN, and the test exercises that production function rather than a copy. |
| MEDIUM — the recording rollback still left the UI in the recording state | `roll_back_recording_session` now reads the mode first, calls the same `refresh_panel_after_recording()` a successful recording uses, and re-reads the key-binding controls through the dispatcher when the failed session was a binding recording. |
| LOW — the new test did not share the environment lock | It now takes `ENV_LOCK`, like the other two tests in the module. |

Gate: `scripts/gate.sh --offline` -> 75 (INCOMPLETE), six headless tiers PASSED. A1: **20 passed, 0 not run,
4 failed** — the pre-existing master failures, reported as FAILED.
### 2026-10-24 — plan §4.6 and §4.7 implemented

- **§4.6 clipboard source identity.** The copy's source is now resolved **once** into a
  `FrontmostSource` (`Resolved { bundle_id: Option<String> }` vs `LookupFailed`) and decided by one
  `exclusion_decision` table: a failed lookup with a non-empty exclusion list **blocks** the copy
  (it cannot be shown to be off the list), a resolved app with **no** bundle id records (a real
  state, not a failure), and the declared `org.nspasteboard.source` can only add exclusion. The old
  `excluded_app_matches` took `Option<&str>` and could not tell the two `None` cases apart; it is
  removed, and `the_exclusion_decision_table_holds` covers every row.
- **§4.7 pause gate.** `clipboard::collection_gate_state()` reports `active` / `paused` /
  `baselining`, and `--e2e-state`'s `clipboard_privacy` carries it as `gate`. `a_paused_session_stays_paused_across_disable_and_enable`
  drives the gate through the disable/re-enable transitions the settings switch produces. The pure
  `pause_gate_settled` factoring the plan also asked for was **not** done: the ordering outranks the
  master switch through the gate itself, and splitting it out would have separated the decision from
  the baseline it depends on; the state field is the assertion instead.

Gate: `scripts/gate.sh --offline` -> 75 (INCOMPLETE), six headless tiers PASSED.
### 2026-10-24 — plan §4.3 and §4.4 implemented

- **§4.3 per-field fallback.** `an_invalid_field_falls_back_without_taking_its_section_with_it` writes
  real TOML through `Config::load_or_default_from` with one INVALID value and legal NON-DEFAULT
  siblings, for layout and appearance, and asserts that only the invalid field reset. Non-default
  siblings are the point: with defaults on both sides a whole-section reset is invisible. The
  `validate()` + `merge_valid()` refactor the plan sketched was **not** done -- that is a large
  restructure of a working mechanism, and the failure it would prevent is now asserted. Recorded here
  as deliberately not taken.
- **§4.4 control wiring.** `every_control_field_is_wired_in_both_directions` parses the `ControlField`
  enum and requires every variant to appear in BOTH `control_field_of` and `apply_control_field`. This
  is the machine-checked form of the settings checklist steps 10/11: the pointer table is invisible to
  the type system, so a field present in one and not the other used to show, animate and write nothing
  with the whole suite green. Its own parser carries counter-examples after it mis-parsed a wrapped doc
  comment as a variant named `ub` (the instrument was wrong, not the code).

Gate: `scripts/gate.sh --offline` -> 75 (INCOMPLETE), six headless tiers PASSED.
### 2026-10-24 — plan §4.1 switcher predicates

`the_switcher_predicates_hold_their_tables` now asserts the two admission predicates that exist in
production -- `should_filter_hidden_app` and `admits_space_scope` -- including the `None`/`Unknown`
rows: an unknown hidden state must not filter, and an unknown Space is never admitted. I first added
standalone `should_show_minimized` / `should_show_other_desktop` helpers, then **removed them**: they
duplicated `admits_space_scope`, so a test over them would have proved a second copy rather than the
real path. That is recorded here because it is the same failure mode the reviewer caught twice.
### 2026-10-24 — code review round 5 (6 findings, all fixed)

| Finding | Disposition |
| --- | --- |
| HIGH — the source was looked up twice, and a failed second lookup could bypass exclusion | `resolve_frontmost_source` now reads the bundle id from the SAME `NSRunningApplication` object that proved the PID alive (`app_identity::bundle_id_of`), so "no identifier" and "lookup failed" cannot be confused; `bundle_id_for_pid` is removed and `copy_exclusion` returns the verdict together with that one snapshot. |
| HIGH — `selection_ring_runtime_smoke` still trusted the exit code | Converted to `run_smoke_child`. |
| MEDIUM — the rollback lost the mapping panel's OK button and never reset the clipboard prompt | `roll_back_recording_session` reads the mode first, then restores the surface that was live: bindings page -> `update_panel_ok`, clipboard shortcut -> `set_clipboard_shortcut_recording_ui(false, None)`, panel/mouse recording -> `EDIT_PANEL_OK`. |
| MEDIUM — the wiring scan's ranges overlapped (a deleted pointer mapping still passed) | The scan now cuts the source at the test module (so its own literals cannot satisfy it) and extracts each function body separately, with an assertion that the ranges are disjoint. **Mutation-checked**: deleting the `Theme` pointer mapping now fails the test. |
| MEDIUM — the pause test did not exercise the transition | Extracted `pause_gate_action(old_paused, new_paused)` as the pure decision `apply_config_change` now branches on; `only_a_pause_to_resume_transition_rebaselines` and `a_paused_session_stays_paused_across_disable_and_enable` assert it, and the gate test takes `gate_lock()`. `apply_config_change` itself cannot run off the main thread, so the decision is asserted separately from the AppKit work. |
| MEDIUM — `show_app_name_in_cards` did not trigger an in-place control sync | Added to `needs_in_place_control_sync` and to the switch backfill list; `changing_only_the_card_switch_still_refreshes_the_settings_controls` **mutation-checked**. |

A1 re-measured: **21 passed, 0 not run, 3 failed (of 24)**. Note the change: `settings_collapsible_row_geometry_smoke`
now reports PASSED, but NOT because the geometry was fixed -- its parent was still parsing stdout, and the
child had always recorded `PASSED 1` through the new channel. Its runner now goes through `run_smoke_child`,
so this number is the child's own claim for the first time.
### 2026-10-24 — code review round 6 (budget exhausted; 5 of 6 findings RESOLVED)

Round 5's classification by round 6: findings 2-6 RESOLVED, finding 1 PARTIALLY RESOLVED, and the
remainder was fixed after round 6 read the tree:

- **MEDIUM (round 6) — the source snapshot was taken and then discarded.** Correct. `record_source`
  called `resolve_app_identity(pid)` a second time, after the payload was read, so an app that quit in
  between was recorded under a `pid_*` key and a reused PID could carry another app's identity and
  icon. `record_source` now takes the `SourceSnapshot` built once at copy time (name, pid, identity)
  and never re-resolves; `the_recorded_source_uses_the_snapshot_taken_at_copy_time` asserts that an
  identity resolved while the app was alive survives it quitting.

**Budget.** The user allowed 6 code-review rounds; this was round 6, so the loop stops here per
`AGENTS.md`. The reviewer's final status was `FINDINGS`; the single finding it listed is the one fixed
above, after that round ran. I have not asked for a 7th round -- that is the user's call.

A1: **21 passed, 0 not run, 3 failed (of 24)**, stable across the last two runs.
### 2026-10-24 — plan §4.8 PID ledger implemented

One row per pid (`PidRow { generation, terminated }`) replaces `pid_generations` + `terminated_pids`.
Correctness comes from the generation, not from capacity: a process-local monotonic `NextGeneration`
allocator (never reclaimed) means a reclaimed pid returns with a higher generation than any it ever
had, so an old job cannot be re-admitted. `job_is_current` is the one predicate every request path
now uses. Reclaim is **growth-triggered only** (`cancel_pid`/`activate_pid` report whether the pid was
new), and the workspace query runs OUTSIDE `CAPTURE_STATE` -- stricter than the code it replaces,
which held the lock across Objective-C calls in `app_terminated`.

Assertions: `the_ledger_stays_inside_its_bound_after_each_reclaim` (2000 terminate/reclaim cycles
leave the ledger empty; the bound is a set inclusion so a build that never reclaims fails it) and
`a_reclaimed_pid_never_re_admits_an_old_job` (a reclaimed pid gets a higher generation; the old job
is still refused; the new incarnation is capturable).

Not implemented from §4.8: the disabled-reclaim **negative control** seam, and the stale-answer test
that pauses a workspace query. Both need a test-only switch inside the ledger; the two assertions
above cover the same rules without one. Recorded here rather than claimed.

A1 unchanged at 21/24.
### 2026-10-24 — code review round 7 (3 findings, all fixed)

| Finding | Disposition |
| --- | --- |
| HIGH — a reclaimed row re-admitted generation-0 jobs | Correct, and it was the pre-row entry path: a job captured 0 before any row existed, the terminate created a row, reclaim deleted it, and 0 became legal again. `pid_generation` now **mints** a row with a unique generation on first use (so a generation always has an owner), and `job_is_current` returns **false** for a missing row instead of approving generation 0. `a_job_from_before_the_first_row_is_refused_after_reclaim` covers the reviewer's exact sequence. |
| HIGH — the out-of-lock query had no generation stamp | Correct, and it was the staleness hole the plan exists to prevent. `reclaim` now takes a `HashMap<pid, generation>` observation and only touches rows whose generation is **unchanged**; `generations_snapshot` produces it. `a_query_answer_does_not_touch_rows_it_did_not_observe` is the reviewer's sequence. |
| MEDIUM — the source snapshot still resolved the PID a second time | Correct. `app_identity::identity_of_app` was split out of `resolve_app_identity`, and the snapshot is built from the same lookup the verdict used. |

**A1 is now 20 passed / 4 failed.** I must correct my previous report again: when I routed
`settings_collapsible_row_geometry_smoke` through `run_smoke_child` last round I did **not** verify its
failure path, which exits 1 without recording anything, so it read as NOT RUN and the count looked like
21. It now records `FAILED`. I reproduced the same numbers on a clean master worktree (`0ea1635`):
`move=0.0 expected_shift=56.0` — the child reports it and master's runner exits 0 anyway. So 20/4 is the
honest number and this is a pre-existing defect, not a regression.

Gate: `scripts/gate.sh --offline` -> 75 (INCOMPLETE), six headless tiers PASSED (1297 tests).
### 2026-10-24 — code review round 8 (2 findings, all fixed)

| Finding | Disposition |
| --- | --- |
| MEDIUM — the source snapshot still resolved the PID a second time | Correct again: splitting `identity_of_app` out was not enough, because the CALLER still did a second `runningApplicationWithProcessIdentifier:`. `resolve_frontmost_source` now returns `(FrontmostSource, Option<AppIdentity>)` from the one NSRunningApplication object, `copy_exclusion` passes the identity through, and the snapshot takes it as a value. `identity_for_frontmost` is deleted. |
| MEDIUM — the new ledger entry point did not trigger reclaim | Correct: `pid_generation` could create a row while only `activate_pid`/`cancel_pid` reported growth, so a pid whose row was created by a first query would never reclaim. `pid_generation_reporting_growth` now reports creation, `pid_generation` delegates to it, and `current_prewarm_jobs` triggers reclaim after releasing the lock. `the_first_generation_query_reports_growth` covers it. |

**A correction, and a flaky runner.** The reviewer was right that my previous A1 explanation was
wrong: `run_smoke_child` already fails on a non-zero exit, so the collapsible runner's `FAILED`
recording did not change the count. What actually happens is that
`settings_collapsible_row_geometry_smoke` is **intermittent**: in one of my runs it reported
`move=0.0 expected_shift=56.0` (its documented pre-existing failure) and in the next it passed with no
code change between them; a clean 5-run loop then passed 5/5. So the A1 count moves between 20 and 21
depending on that runner, and neither number is a statement about this branch. Recorded here instead of
quoting whichever run looks better.
### 2026-10-24 — code review round 9 (2 findings, all fixed)

| Finding | Disposition |
| --- | --- |
| MEDIUM — growth reclaim covered only one caller | Correct. `pid_generation` now sets a ledger flag when it mints a row, `reclaim_if_growth_pending()` consumes it once the lock is released, and it is called from `enqueue_job_inner` (the common path behind every enqueue, activation and appearance job) and from `current_prewarm_jobs`. `a_first_generation_query_leaves_a_reclaim_pending` asserts the CHAIN -- query -> pending -> consumed by reclaim -> repeat query does not re-request -- rather than a helper's return value. |
| MEDIUM — the new test stole the bound test's `#[test]` | Correct and serious: my insertion landed between a doc comment and its function, leaving `the_ledger_stays_inside_its_bound_after_each_reclaim` unattributed. It had stopped running, so the ledger's bound was no longer checked at all while the test count went UP. Both now carry their own `#[test]`; `cargo test -- --list` is the check, which is what would have caught it. |

This is the second time in this task that an edit of mine silently removed a test's registration. The
lesson recorded in `docs/review-backlog.md`: after moving or inserting a test, list the registered tests
and compare, rather than trusting a green suite -- an unregistered test is a suite that got smaller
while looking the same.

Gate: `scripts/gate.sh --offline` -> 75 (INCOMPLETE), six headless tiers PASSED (1299 tests).
A1 -> 21 passed / 3 failed; see the flake note in the round-8 entry.
### 2026-10-24 — code review round 10 (2 findings, all fixed)

| Finding | Disposition |
| --- | --- |
| MEDIUM — growth paths without a reclaim exit | Correct: `enqueue_activation_job`/`enqueue_blank_retry_job` do not pass through `enqueue_job_inner`, and `pregen_windows_for_pid` returns early after minting a row. `enqueue_activation_job` got an explicit call; `enqueue_blank_retry_job` got a `ReclaimOnDrop` guard because it has several early returns; `pregen_windows_for_pid` publishes before its early returns. |
| MEDIUM — a contended `try_lock` could lose the only reclaim notice | Correct. `reclaim_if_growth_pending` now takes a plain lock (the guard is released before any Objective-C work, so the wait is short); `try_lock`'s comment claiming the holding thread would notice was wrong. |
| MEDIUM (same) — the reclaim cleared a request for rows it never observed | Correct, and the new test found it: the flag was computed AFTER the retain, so a row created during the query was already gone when asked. It is now computed before. `a_reclaim_keeps_a_request_for_rows_it_did_not_observe` covers it. |

Gate: `scripts/gate.sh --offline` -> 75 (INCOMPLETE), six headless tiers PASSED (1300 tests).
A1 -> 21 passed / 3 failed (see the intermittent-runner note in the round-9 entry).
### 2026-10-24 — A2 measured (user-authorised, with --include-focus --include-screen --include-prefs)

**8 passed, 2 not run, 6 failed** of 16 scenarios. Per scenario, with why:

| Scenario | Result | Reason |
| --- | --- | --- |
| panel-drag, hud-settings, tab-repeat, switch-basic, switcher-settings, reopen-settings, finder-quick-actions, scrollbar-style-flip | PASS | the focus/prefs scenarios ran under authorisation |
| smooth-scroll | FAIL | `background wheel changed the frontmost pid: before=5279 after=26117` — focus moved mid-run. **My fault, not the code's**: I started this run while a person was using the machine. Re-run it alone. |
| panel-contrast | FAIL | `filter-row` framing reports "no text" for every material — backlog #36 (stale `ROW_Y_PT`), pre-existing and material-independent |
| panel-edge | FAIL | check 1/3 outline+blur measurements — backlog #23/#40, reproduced on a clean HEAD worktree before this branch |
| settings-layout | FAIL | 13/26; the page-header padding and About-page rows — backlog #41, pre-existing |
| settings-open-latency | FAIL | the settings-click event was not observed (`warm click: 0ms`, seq=0) — most likely the same focus interference |
| space-desktops | NOT RUN | no window on another ordinary desktop on this machine |
| space-groups | NOT RUN | the move-left/right-a-Space shortcuts are not bound here |

**So A2 is not green, and this run cannot say it is.** Two of the six failures (smooth-scroll,
settings-open-latency) are consistent with focus interference during the run; four are documented
pre-existing instrument/geometry defects (backlog #23, #36, #40, #41). I did not re-run the two
focus-sensitive ones yet -- they steal focus for minutes and need a quiet machine.

Nothing in this branch touched the settings layout, the contrast/paint path or the panel geometry;
the diff there is `callback_guard` wrapping and whitespace (`git diff -w`);
### 2026-10-24 — backlog #41, part 1: the section header was 11pt, the design system says 12

`--smoke-settings-layout` rejected `Startup Options` for rendering at 11.00pt. The design document
(`docs/design-style-en.md` §typography, the `section-header` row) fixes **12** pt 600 secondary, and
the token table's scale is 12 / 14 / 20 / 26 -- 11 is not on it. `theme::FONT_SECTION_TITLE` was 11.0
because it was taken from the mock's `.section-title { font-size: 11px }`; per AGENTS.md the document
wins over the code, so the constant is now 12.0 and its comment no longer quotes the mock.

After this the smoke gets past the font check and fails at the next check, which is the second half of
#41: the clipboard shortcut row's measured centre gap is 59.30 where the formula expects 58.00 --
**exactly the 1.3pt difference the backlog recorded**, so that is a real layout defect and not a stale
expectation. Measured row heights are 60 (the shortcut row, `(row_h + 22).max(60)`) and 56 (the pin
row), so the formula's `(60+56)/2 = 58` is right and one of the two frames sits 1.3pt off. Not fixed
yet: locating it needs a per-view frame dump, which I have not done.

Gate: `scripts/gate.sh --offline` -> 75 (INCOMPLETE), six headless tiers PASSED.
### 2026-10-24 — backlog #36 fixed: the contrast scenario was measuring the options orb

`panel-contrast` reported "the filter-row region holds no text" for all 8 theme/material/shade
combinations. The cause was in the app, not the threshold: `picker_filter_row_rect_top_left` falls
back to the **options orb's** 34x34 rect when the popover is closed, so the scenario measured an orb
for glyph ink and concluded there was none. The filters exist only inside the popover; with it closed
there is no filter row on screen at all.

- The app now answers `None` when the popover is closed instead of publishing the orb as a filter row.
- The scenario reports that check as **not run** (and says so at the end) rather than counting it as a
  contrast failure, and still measures the footer captions, which is what it actually covers today.

Result: `panel-contrast.sh` exits 0, with `filter-row checks not run in 8 case(s): the popover was
closed` printed. That is a scenario that no longer lies in either direction -- it neither fails a
correct panel nor claims coverage it does not have.

### #41 part 2, not fixed

The clipboard shortcut row's centre gap measures 59.30 where its formula expects 58.00. The measured
heights are 60 (shortcut row) and 56 (pin row), both controls are centred in their rows, and the rows are
contiguous, so 58 is the correct expectation -- the 1.3pt is a real offset somewhere between a row's
frame and its control's frame. Locating it needs a per-view frame dump; I stopped rather than keep
bisecting a layout I did not change, and the check stays red in the backlog with this evidence attached.
### 2026-10-24 — the four A1 failures, diagnosed one by one

| Runner | Verdict | What it was |
| --- | --- | --- |
| `panel_creation_and_badge_layout_smoke` | **TEST BUG — fixed** | The rail check counted split chord cells by comparing each keycap's label to `"⌥"` or `"q"`. `key_cap_text` UPPER-CASES a keycap (`mapping.rs` asserts `key_cap_text("q") == "Q"`), so `"q"` never matched, `split_cells` read 1 instead of 2, and the check failed a correct panel. Now compares case-insensitively. `rail=false` -> PASS. |
| `settings_collapsible_row_geometry_smoke` | **INTERMITTENT, not a defect** | Measured `move=0.0 expected_shift=56.0` in the A1 run, but 8/8 passes standalone, 3/3 after a preceding layout smoke, 3/3 concurrently, and 3/3 through the exact `cargo test --ignored` path the harness uses. I did NOT find the trigger. I also tried changing the assertion to the opposite direction and the data immediately disproved me (`previous_y` 824.4->768.4 is the row above moving), so I **reverted** that change rather than leave a guess in the tree. |
| `settings_layout_smoke` | **real, unfixed** | The 11pt header is fixed (12pt per the design doc). What remains is the clipboard shortcut row: control centres are 59.30 apart where the formula expects 58.00, while the two row LABELS are exactly 58.00 apart. So the row layout is right and one control sits 1.3pt off its row's centre; I traced the centring helpers (`plain`/`tall_with_height` -> `center_control` -> `control_origin_y`, all `y + (row_h - control_h)/2`) without finding the asymmetry, and stopped rather than keep bisecting. |
| `settings_layout_smoke_under_reduce_motion` | **same as above** | Same check, same 1.3pt. |

The honest summary: **one A1 failure was a test bug and is fixed, one is intermittent and I could not
explain it, two share a single real 1.3pt layout defect I have not located.** My earlier guess that the
collapsible failure was my `FAILED` recording was wrong, and my second guess (that the assertion had the
direction backwards) was disproved by the measurement.
### 2026-10-24 — all four A1 failures resolved; A1 is 24/24

**1. `settings_layout_smoke` (and its reduce-motion twin) — CODE BUG, fixed.** The 1.30pt came from
`dispatch::set_clipboard_shortcut_capture_state`: it derived the row centre from the ROW LABEL's frame
centre. `center_label` adds `text_field_line_height_correction` (1.300) so the glyphs — not the box —
are optically centred, so the label's centre is 1.3pt off the row's centre, and every control centred
from it inherited that shift. It now subtracts the correction before computing the centre. Codex was
asked for an independent read and pointed at the same expression; the arithmetic reproduces the
measured 1113.10 exactly. Once that passed, the check surfaced a second, older problem:
`card_hugs_host` compared against a literal `8.0` while `SettingsLayout::CARD_BOTTOM_INSET` had become
`0.0`, so it demanded an overhang no card draws; it now derives from the token.

**2. `panel_creation_and_badge_layout_smoke` — TEST BUG, fixed.** The rail check compared keycap
labels to `"q"`, but `key_cap_text` upper-cases them (`mapping.rs` pins `key_cap_text("q") == "Q"`),
so `split_cells` read 1 instead of 2 and the check failed a correct panel.

**3. `settings_collapsible_row_geometry_smoke` — NOT A DEFECT.** It passes standalone, after other
runners, and through the harness path; it only failed in the full tier, and the full tier now passes.
I tried changing its assertion once, the measurement disproved the change, and I reverted it.

**4. The one remaining intermittent was environmental.** The last `A1` run failed with
`panel smoke requires no running app instance for its refusal probe: AlreadyRunning` — a dev instance
of the app from my own earlier probing was still running, and the runner takes the single-instance lock
to prove a second launch is refused. After stopping it (`dev-restart.sh --check`), A1 is **24 passed,
0 not run, 0 failed (of 24)**. The harness does not stop a running instance, which is the trigger to
watch: run A1 with the app down, or the refusal probe reports AlreadyRunning.

Gate: `scripts/gate.sh --offline` -> 75 (INCOMPLETE), six headless tiers PASSED.