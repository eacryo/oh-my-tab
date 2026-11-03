# 数据与渲染解耦方案

- 状态：P0–P4 全部实施完成，八个设置页全部由描述生成（含 Mouse 与 Key Bindings）；页面级钩子（before_page/extra_row/after_page）覆盖描述无法承载的控件。M1 门禁通过；M2 未运行。
- 日期：2026-11-03
- 任务键：`ui-data-decoupling`

## 范围

本方案回答两个问题：**oh-my-tab 的 UI 与数据该在哪里分开**，以及**该不该为此抽一个 crate**。
它只覆盖"设置页、切换器浮窗、剪贴板面板、引导页"这四个用户可见界面的**结构**，不涉及剪贴板加密、
窗口采集、缩略图、输入法等子系统内部的行为。

本方案不改变任何用户可见行为。所有阶段都以"同样的界面、同样的数值"为验收条件；凡是会改变界面的
改动都不在本方案内。

## 结论摘要

1. **耦合点是"控件即状态存储"和"创建即计算"，不是"视图读配置"。** 配置层本身是健康的：一个
   `LazyLock<RwLock<Config>>`、逐字段回退、一套 `ChangeFlags` 增量。问题是配置值被搬进 NSControl
   之后，控件就成了第二份真相，反向再由指针找回字段。
2. **可以分，而且分得动**，但动作是四件有界的小事，不是一次重构：逐页 `RowSpec` + 同一页的控件身份
   表、浮窗主线程布局态收进结构体、`plan_card` 抽纯函数、`theme` 输入参数化。
3. **现在不该抽 UI crate**——理由不是"没有收益"（它确实提供依赖方向约束与可见性边界），而是"现在
   没有值得这份成本的复用需求"：渲染层只有一个消费者，模块边界已经够用，而 `ffi`、`theme` 读全局、
   `ui_coordinator` 的跨表面调用与 gate 的单 crate 假设都要先处理。真正值得成 crate 的是**视图数据**，
   而且是在它稳定之后。
4. **硬前提**：`theme.rs` 的 14 处 `CONFIG.read()` 必须改成参数传入（一份完整快照，一次事务只取一次），
   并在操作 AppKit 之前释放配置锁；这一步单独就能修掉一处已记录的测试并行缺陷。

## 现状与证据

以下是本方案的事实基础，全部可由命令复现。

| 事实 | 数字 | 复现命令 |
| --- | --- | --- |
| `SettingsUi` 的控件指针字段 | 114 个 `*mut AnyObject` | `sed -n '/^pub(super) struct SettingsUi {/,/^}/p' src/settings.rs \| grep -c '\*mut AnyObject'` |
| 控件身份靠 sender 指针线性比对 | 44 条 `m(u.x, Field)` 分支 | `sed -n '/unsafe fn control_field_of/,/^}/p' src/settings/dispatch.rs \| grep -c 'm(u\.'` |
| 控件到语义的映射表 | 65 个 `ControlField` 变体、78 个 apply 分支 | 枚举体 `grep -c '^    [A-Z]'`；`apply_control_field` 内 `grep -c 'ControlField::'` |
| `load_settings_from` 的值搬运 | 42 次 `msg_send` 对应 48 个 `cfg.` 引用 | `sed -n '/fn load_settings_from/,/^}/p' src/settings.rs \| grep -c 'msg_send!'`，同区域 `grep -c 'cfg\.'` |
| `create_card_view` 的混合度 | 433 行内 87 次 `msg_send`、4 处 `CONFIG.read()` | `sed -n '/pub(crate) fn create_card_view/,/^}/p' src/overlay/cards.rs \| grep -c 'msg_send!'` |
| `theme.rs` 自读全局 | 14 处 `CONFIG.read()` | `grep -c 'CONFIG.read()' src/theme.rs` |
| 浮窗的布局结果与视图态静态 | 迁移成员以 P3 的表格为准（该表逐项列出），不在摘要里另给总数 | `grep -n '^static .*Mutex' src/overlay.rs`，与 P3 的表逐行核对 |
| 真实测试模块对这些静态的读取 | 0 处。第 412 行的 `#[cfg(test)]` 是只编译进测试的辅助函数，不是测试本身 | `awk 'NR>=1143' src/overlay.rs \| grep -c 'THUMB_\|CARD_SIGNATURES'` 应为 0 |
| 全树 `CONFIG` 读写 | 212 读 / 78 写 / 48 个文件（`read()` 加 `write()` 共 235 处） | `grep -rn 'CONFIG\.read()\|CONFIG\.write()' src --include=*.rs \| wc -l` |

已经存在的、形状正确的先例（本方案要复用的就是这三处的形状）：

- `src/finder_menu/model.rs`：`build(&Config, …) -> MenuModel`，纯函数，零 AppKit，渲染在另一个进程里。
- `src/key_bindings/table.rs`：`BindingTable::build(...)`，消费方以 `RwLock<Arc<_>>` 整体替换。
- `src/settings/page_canvas.rs`：`PageCanvas` 把一页的行、卡、游标收成结构体，是最近一次成功的
  "布局所有权收拢"。

已经付出的代价（说明问题是真的，不是品味）：

- `docs/settings-field-checklist.md` 把"加一个开关要动 10 个文件、16 步"写成**清单**维护；该文档
  记录的 `show_app_name_in_cards` 漏接第 13、14 步，正是这类缺陷的一个实例。
- `src/runtime_config.rs` 与 `src/settings/dispatch.rs` 里存在 `include_str!`／`read_to_string`
  的**源码扫描测试**。只有在没有可断言的值时，测试才会退化成断言文本。
- `docs/review-backlog.md` 记录了 `theme` 几何测试必须读全局 `CONFIG` 因而不能并行运行。

## 目标与非目标

目标：

1. 让每个界面的"长什么样"成为一个可以构造、可以比较的值，渲染只负责把它应用上去。
2. 让"哪个控件对应哪个字段"成为建树时产出的数据，而不是一份手工维护的指针比对表。
3. 缩小后的目标（第 2 轮再收窄）：**每一个已声明的可交互行都必须携带合法身份**，且描述、控件种类与
   字段身份同源——这样"控件建出来却没有身份"在编译期就不成立。它**不**保证"某个配置字段没有整行
   漏掉"：那需要一份配置字段的完整性清单（字段集合 → 行集合），本方案**不**提供，若将来需要，它是
   独立的一步。
   同样**不**承诺整条配置到界面的链："就地刷新"走的是与初始化不同的函数
   （`refresh_switcher_keystroke_and_mouse_controls_from_config`，`src/settings/dispatch.rs:1946`），
   触发条件是独立的 `needs_in_place_control_sync`。本方案只要求二者**消费同一份纯更新描述**
   （见 P2），并分别断言"字段变化会触发同步"与"更新描述包含该字段"。

非目标（明确不做，避免范围蔓延）：

- 不消除 `CONFIG` 全局。它的读访问不是问题，且改动会波及全部 FFI 签名。
- 不重写 `runtime_config::apply_config_change`。它是 290 行、约 35 个标志、21 个调用点的分发
  函数，但它是**增量层**，在模型出现之前重写它没有依据。
- 不改变任何用户可见的外观、尺寸、文案或交互。

## 阶段

每个阶段都独立可评审、可回滚、可单独发布；后一阶段依赖前一阶段的产出，但任何阶段中途停下，
仓库都处于可发布的完整状态。

### P0 —— `theme` 的几何与色板改成参数传入

做什么：把 `theme.rs` 的 14 处 `CONFIG.read()` 改成由调用方传入一份**完整快照** `ThemeInput`。
快照的字段就是这些函数今天实际读到的全部输入，而不是"挑几个常用项"：深色标记、自定义色板
（`colors.light` / `colors.dark` 两个结构体的全部颜色字段）、圆角半径、文字比例，
`fonts` 段的字段（卡片文字大小、页脚字号），`layout` 段的字段（缩略图开关与比例、
`show_app_name_in_cards`、`show_footer_title`）。实施第一步是把 `theme.rs` 里每一处
`CONFIG.read()` 读到的字段逐个列成清单，写进 `ThemeInput` 的文档注释——**清单不全，P0 就没有完成**。

`colors_from_config(dark)` 今天仍然自己读全局（`src/theme.rs:620`），本阶段把它改成
`colors_from(&ThemeInput, dark)`；它不是现成的先例，而是本阶段要改的对象之一。

一致性：一次布局或刷新事务只取**一次**快照，然后释放配置读锁，再操作 AppKit。这条规则同时修掉
`docs/review-backlog.md` 记录的"几何测试跨多次调用读全局 `CONFIG`"问题——测试改为一、二次调用
传入同一份快照，位置参数化之后不再需要全局。

过渡：`ThemeInput::current()` 只作为**唯一**的过渡入口。退出条件的范围要收窄——**只约束纯主题、
布局与描述层不得读取全局**，不要求"UI 模块都不读配置"：设置的事件处理与刷新入口本来就要读业务
配置，那与"保留 `CONFIG`、逐步解耦"的范围一致。`current()` 最终的替代者是**各入口在事务开始时取
一次快照**（设置页刷新入口、浮窗 `show_overlay`、剪贴板面板打开、引导页渲染），由调用方持有并传下去。

关于用字面量计数守边界：`grep -c 'CONFIG.read()'` **不可作为判据**——现有 `text_scale()`
（`src/theme.rs`）就把 `CONFIG` 与 `.read()` 分行书写，而 `page_builder.rs` 里有四处
`crate::config::CONFIG` 换行接 `.read()`。如果确需自动检查，它必须至少覆盖多行与
`crate::config::CONFIG` 两种写法，并用这两个反例证明它不会误报通过；否则由实施者对
`theme.rs` 逐行核对，测试只断言依赖关系。

怎么验证：**不看两次调用是否相等**——字号与圆角本来就会影响卡片几何。正确的不变量是依赖关系：
改变快照中的某一项，**只有文档列明的消费者输出变化**，其余输出逐位相同。第二条断言是迁移前后等价：
用当前 `CONFIG` 生成的快照，在关键几何函数（`card_w`、`card_h`、`footer_band_h`、
`card_title_font_size`、`status_bar_text_size`）上的返回值与改动前一致——这条在改动前先写，
作为迁移的基准。

### P1 —— 设置页：`RowSpec` 描述，`PageCanvas` 排布

做什么：把 `page_builder.rs` 六个 `build_*_page` 里"读值 + 算 frame + 建控件 + 绑回调 + 写搜索
索引"的顺序代码拆成两段：

```text
page_view(&PageInput) -> Vec<RowSpec>                            // 纯：只描述语义，不含坐标
     ↓
layout_rows(&[RowSpec], &LayoutInput) -> Vec<RowLayout>          // 纯：唯一的布局计算者
     ↓
render_page(&mut PageCanvas, &SettingsUi, &[RowLayout])          // 只应用结果
```

`LayoutInput` 是**完整**的布局输入，而不是只挑字体度量：内容宽度、页面视口高度、页面内边距
（`SettingsLayout` 的 `label_x`/`label_w`/`control_x`/`control_w`/`row_h`）、
`MeasuredText`（各文案在当前语言与字号下的实测尺寸）、以及 `SETTINGS_DETAIL_INSET` 这类页面常量。
这些正是现有 `SettingsPageBuildContext`（`src/settings/page_builder.rs:6`）已经在用的输入，本阶段只是
把它们收进一个类型并交给纯函数。

`RowSpec` 携带：所在卡片（section）、标签文案键与描述键、控件种类、**字段或动作身份**（见 P2）、
初值、可选项列表、依赖关系（哪个总开关关掉时禁用）、搜索索引条目。

**页面输入的完整集合是 `PageInput`**，不是一个泛泛的 `Config`：至少包含 `Config`、`ThemeInput`、
以及**当前选中的鼠标设备**。后者不能省——同一份配置下，切换设备会改变滚动、加速度等控件的值
（现有代码读 `current_selected_device()`，`src/settings.rs:1022`，再由
`mouse::resolve::resolve_from_config` 解析出该设备的有效配置）。所以设备身份（或已解析的设备配置）
是 `page_view` 的显式输入，更新描述也用同一份 `PageInput`，否则函数只能偷读全局或显示错误设备的值。

关于坐标（两轮修正后的结论）：初稿说"坐标不纯"是错的——**先测量、再把测量结果交给纯布局函数**是纯的。
第 1 轮之后初稿改成了"RowSpec 携带确定 frame"，这又改过了头：把语义描述和布局结果塞进同一个类型，会让
`page_view` 既要知道配置又要知道视口，职责重叠。最终形态是**三个类型、一个布局计算者**：
`RowSpec`（语义，无坐标）、`RowLayout`（frame，由 `layout_rows` 产出）、`PageCanvas`（只累加落位）。
这与 `theme::ThumbFlowLayout` 的既有分工一致。

迁移顺序：**Switcher** 页先行，然后 General、Mouse、Clipboard、Key Bindings、About、Finder Menu、
Keystroke Display。

**按页完整迁移、一次一页、不留半接线状态**：每一页在同一个改动里完成 描述 → 建树 → 回调 →
`load_settings_from` 回填 → `needs_in_place_control_sync` 的就地刷新，然后删除该页的旧构建路径。
不存在"先只做描述、回调稍后"的中间版本（这是初稿的缺陷：它把 P1 与 P2 拆成两个阶段，却又在 P2 里
承认 P1 依赖 P2，形成循环）。因此 **P2 不是独立阶段，而是 P1 每一页交付的一部分**；下面单独成节只是
为了把机制说清楚。

怎么验证（M1，纯逻辑）：给定固定的 `PageInput`（含设备）与 `LayoutInput`，`page_view` 的行数与
文案键、控件种类、字段身份、依赖关系被断言，`layout_rows` 产出的每行 frame 也逐项断言（两者分开测，
因为它们回答不同的问题）。Mouse 页另加一条：**同一份 `Config`、两个设备配置不同**时，`page_view`
产出不同的初值。设备切换与拔出后的真实回填归 M2。真实 AppKit 的 target/action 与三语下
的实际几何不在 M1：它们属于该页既有的 **M2** runner（如 `--smoke-settings-layout`）。本方案不新增
M2，但某页改动后若 M2 未获授权运行，交付时必须记为 NOT RUN。

### P2 —— 控件身份在建树时产出（P1 每页的一部分）

机制：控件身份**由描述产生**，而不由"控件种类"推断——多个开关对应不同字段、按钮对应独立动作，所以
`RowSpec` 携带的是显式的 `ControlId`（字段身份或动作身份），不是"这是个开关"。`render_page` 在建出
控件的同时把它与 `ControlId` 成对登记到 `ControlTable`（挂在 `SettingsUi` 上），`control_field_of`
的 44 行手写比对改为一次查表。

生命周期约定（初稿缺失，必须补齐）：

- **登记与注销成对**：每一页重建时先注销该页登记的条目再重新登记；控件销毁时同步注销。
- **重复登记必须中止本次页面构建**：同一 `ControlId` 或同一指针重复登记时 debug 下 panic、release 下
  记日志并**丢弃本次构建**（页面回退到上一次可用状态并保持可见），**不能**只记日志就继续发布控件——
  否则新控件显示正常却无法响应，正是本方案要消除的那种"看起来活着但没接线"的缺陷。
- **三种销毁路径都要先注销**：局部替换（单行重建）、整页重建、整窗销毁，一律在释放旧控件**之前**
  注销；重复清理必须是安全的（幂等）。
- **失败回退与注销顺序不冲突**：身份校验（重复、缺身份）在**破坏旧页面及其登记之前**完成；新构建
  失败时，旧视图与旧映射**一起**保留，二者不会一个消失一个还在。首次构建失败没有"上一次状态"可退，
  它的处理是明确的：页面保持空白并在日志中记录失败原因，控件不发布、不响应——宁可空着，不可
  "看起来活着但没接线"。M1 断言这一条：登记失败之后，旧映射仍能解析出同一批 `ControlId`。
- **未知 sender 保留今天的回退**：`ControlTable` 查不到时维持现有行为，不静默丢弃。
- **动态行不并入本表**：Key Bindings 的绑定行、Mouse 的映射行有自己的重建与清理责任
  （见 `settings/bindings.rs` 的 `ROW_CONTROLS`），本阶段**只覆盖静态配置控件**。这也回答了初稿的
  开放问题 1：`RowSpec` 只描述固定家具。
- **其余按指针索引的全局表不合并**：`select.rs`、`tooltip.rs`、`sidebar.rs` 的表职责不同（浮层状态、
  提示目标、搜索索引），"键都是指针"不是合并的理由。初稿的开放问题 2 据此关闭。
- **控件里的临时无效输入与录制草稿保留**：数字框打到一半的值、快捷键录制的草稿是编辑期状态，不是
  "第二份真相"，继续留在控件与录制状态里。

注意：`SettingsButton.tag` 的语义不变（tag 仍是 hover/正常调色板），不用 tag 携带语义 id。

怎么验证（M1，纯逻辑）：`ControlTable` 用假的指针值做单元测试——登记、注销、重复登记被拒、三种销毁
路径的清理顺序与幂等、未知 sender、页面重建后无残留条目。真实 `SettingsUi` 的指针与 target/action
绑定、以及真实清理路径由 M2 覆盖（未运行则记 NOT RUN）。

**回填与就地刷新消费同一份纯更新描述。** 初稿把"比较 `load_settings_from` 与 `page_view` 的初值"
当作第 13、14 步的覆盖，这不成立：

- `load_settings_from` 会操作 AppKit，**不能**在 M1 里调用；
- 就地刷新走的是另一个函数（`refresh_switcher_keystroke_and_mouse_controls_from_config`，
  `src/settings/dispatch.rs:1946`），触发条件也独立（`needs_in_place_control_sync`）。

因此改为：两处都从同一份**纯更新描述**取"字段 → 值"（`Vec<ControlUpdate>`，由 `page_view` 与
`PageInput` 算出），M1 断言三件事——(a) 更新描述对给定 `Config` 的 (字段, 值) 序列正确；(b) 字段变化会
让 `needs_in_place_control_sync` 为真（纯函数，已有测试可直接扩展）；(c) 每个**需要配置回填的状态**都在
更新描述里出现。动作控件（如"导出日志"这类按钮）没有可回填的配置字段，它们另验身份与 target/action
接线，不强迫它们制造无意义的更新条目。真实的两条写入路径由 M2 断言。这不需要改动
`apply_config_change` 的分发结构。
### P3 —— 浮窗布局态收进 `AppState`

做什么：把 `overlay.rs` 的布局结果静态与滚动/悬停/拖拽静态收进一个 `SwitcherLayout`
（持有 `ThumbFlowLayout`、`ScrollState`、`HoverState`、`RenderCache`），挂在 `AppState` 上。
迁移成员**以下表为准**，本文不另给总数——先前几稿各写一个数字，正是这类文档最容易出错的地方。
`MainThreadSlot` 只保留真正持有 ObjC 指针的部分；`DISPLAY_RECONFIG_*` 两个合并标志与
`CARD_SIGNATURES`、`LAST_APPLIED_THEME_DARK` 三个缓存留在原处，理由见下表。

| 类别 | 静态 | 去向 |
| --- | --- | --- |
| 布局结果 | `THUMB_VISIBLE_RANGE`、`THUMB_ROW_RANGES`、`THUMB_MAX_ROWS`、`THUMB_CONTENT_INSET`、`THUMB_PANEL_MAX_H`、`THUMB_TEASER_FITS`、`THUMB_SCROLL_MAX_OFFSET`、`THUMB_DOCUMENT_HEIGHT`、`THUMB_SCROLL_ROW_PITCH`、`THUMB_CAPTURE_TARGET_PX_H` | `SwitcherLayout.flow` |
| 视图态 | `THUMB_SCROLL_ROW`、`THUMB_SCROLL_OFFSET`、`THUMB_SCROLL_DRAG`、`THUMB_SCROLLER_HOVER`、`THUMB_NAV_ANCHOR_X`、`MOUSE_MOVED`、`PENDING_CARD_CLOSE` | `SwitcherLayout.scroll` / `.hover` / `.close` |
| 保留 | `OVERLAY_WINDOW`、`CONTAINER`、`CARD_DOCUMENT`、`STATUS_LABEL`、`THUMB_SCROLLER`、`OVERLAY_BACKDROP`、`CARD_CLASS` | 仍由 `MainThreadSlot`/类缓存持有 |
| 保留 | `CARD_INDEX_MAP`、`CARD_KEY_MAP` | 视图指针到数据的映射，服务动态 ObjC 类（AGENTS.md 明令禁止靠 `msg_send!` 读 Rust 属性） |
| 保留 | `CARD_SIGNATURES`、`LAST_APPLIED_THEME_DARK`、`DISPLAY_RECONFIG_REFRESH_SCHEDULED`、`DISPLAY_RECONFIG_RELAYOUT_PENDING` | 渲染缓存与合并标志，不是布局决策的输出，搬进 `AppState` 只会让 `AppState` 变胖而不增加可断言性 |
| 保留（跨线程通道） | `CARD_CLOSE_AX_RESULT` | 后台线程写入（`overlay/card_close.rs:415`，`std::thread::spawn` 内），主线程回调取出（同文件 :464）。它是主线程与 AX 关闭工作线程之间的交接槽，不是布局态；搬进仅主线程可用的 `AppState` 会破坏这个线程边界。主线程**消费之后**的关闭动画状态在 `PENDING_CARD_CLOSE` 里，那部分才随 `.close` 搬迁 |

**借用与重入边界（初稿缺失，这是本阶段的关键风险）**：`with_tab_state` 借出的是
`RefCell<Option<AppState>>` 的可变借用（`src/lib.rs:182`），而 `refresh_highlight`、`show_overlay`
等函数**会再次进入** `with_tab_state`（`overlay/cancel.rs:503`、`cards.rs` 多处）。因此挂到
`AppState` 必须遵守一条硬规则：

> 短借用取出计划或更新状态 → **释放借用** → 再执行 AppKit 操作与该操作触发的回调。

任何持有 `state.layout` 借用去调用 `msg_send!`、`refresh_highlight`、`show_overlay` 或
`thumbnail::*` 的写法都是重入 panic。选择 `AppState` 而非另开 `thread_local` 的理由是访问边界
（它已经与主线程约定绑定，且 `with_tab_state` 是唯一合法入口），**不是**结构体大小；另开 TLS 并不能
解决重入，只是把问题挪了一个位置。跨线程的东西（例如关闭结果 `CARD_CLOSE_AX_RESULT` 若经后台"
回传）不随布局态搬迁——它属于跨线程交接槽，保留原处（见上表）。

为什么这块最稳：`overlay.rs` 里真正的测试模块从不读取这些静态（第 412 行那个 `#[cfg(test)]` 块是只
编译进测试的辅助函数，它会读写它们；测试本身不碰）。已有两个访问器（`thumbnail_visible_range`、
`thumbnail_scroller_max_offset`）保持签名不变，改为读结构体。

怎么验证：一条 M1 断言——由同一个 `ThumbFlowLayout` 与同一个待关闭项，生成的回流预算与"关闭前布局
加关闭后项数"一致。

借用边界**没有**类型系统保证，初稿说有是错的：外层闭包的 `RefCell` 借用可以一直存活，内部函数再次
调用 `with_tab_state` 照样编译通过，然后在运行期 panic；返回快照也不会自动结束外层闭包。
`snapshot()` 仍然值得提供（它把"读"变成一次短借用的结果），但它的作用是**让规则容易遵守**，不是
**让违反规则编译不过**。因此本阶段的做法是：保留短借用规则，逐处核查实际调用边界，并用一条回归
断言覆盖"嵌套进入 `with_tab_state`"这条路径会 panic 而不是静默损坏（`lib.rs` 已有的
`assert_main_thread` 同款做法）。注意不要把每一处 `msg_send!` 或 `thumbnail::*` 都当成必然重入——
只有那些会再进入 `with_tab_state` 的调用才是；核查时要按调用列出来，而不是一概而论。

### P4 —— `plan_card` 抽成纯函数，创建与刷新共用同一几何来源

做什么：把 `create_card_view`（433 行）里的内部 frame 计算抽成
`plan_card(card_w, card_h, layout, theme) -> CardPlan`，返回关闭按钮、徽标、标题、预览、选中环的
frame；`create_card_view` 与 `refresh_highlight` **都**消费这一个 `CardPlan`，而不是各自算一遍再比。

为什么这一步不能只做"测试两者相等"：保留两套公式再断言它们相等，等于把漂移冻结成契约。正确做法是让
`refresh_highlight` 从 `CardPlan`（或从建卡时存下的 plan）取几何，删掉它自己重算的那两处
（`overlay/cancel.rs` 里从 `[sv frame]` 反推选中盘与图标原点）。

怎么验证：M1 断言 `plan_card` 在给定的卡片尺寸、布局与主题下产出确定的 frame，并在两种布局
（图标 / 缩略图）下与 `theme::icon_card_layout`、`thumb_caption_h_for_card`、`thumb_preview_h` 一致。
"创建与刷新共用同一来源"由代码结构保证（刷新的几何来自 plan），不靠断言两套公式相等。
### P5（可选，非本方案必做）—— 视图数据成 crate

**这不是本方案的输出，而是重新评估的触发点。** 条件与决策 1 一致，任一成立就重新评估，不要求全部
满足，也不要求"必须有第二个消费者"：

1. 出现必须共享同一份视图数据的第二个消费者（例如 Finder 扩展要渲染同一套设置描述）；
2. 模块边界已经稳定、团队希望用编译器**强制**依赖方向（这本身可以构成独立理由）；
3. 需要在不启动 AppKit 的前提下做视图一致性检查。

评估时还要满足两条硬约束：`RowSpec`/`CardPlan`/`SwitcherLayout` 已经稳定（P1–P4 完成，且之后至少
一次设置项改动没有迫使它们变更），并且 `theme` 已经不再自己读配置（P0 完成）。

届时抽出来的是 `oh-my-tab-view`（纯数据，依赖只有 `serde` 这一类，不含 `objc2`），不是
`oh-my-tab-ui`（含渲染）。仓库里唯一一次成功的抽 crate（`crates/finder-protocol`）抽的正是**数据
约定**。
## 决策与理由

### 决策 1：现在不抽 UI crate（修正初稿的过度断言）

**初稿说"收益为零"、"四个无法回避的成本"，这两句都过头了，撤回。** 准确的版本是：

抽 crate **确实**能买到两样东西——编译期的依赖方向约束，以及一个可见性边界。内部 workspace 成员的
`pub` 也**不是**永久的对外兼容承诺，本仓库本来就有多个成员，根应用继续承载集成 smoke 也不证明拆分
无效。所以正确的理由不是"做不到"或"没有好处"，而是**现在没有值得这份成本的复用需求**：

1. **没有必须共享该模型的第二个消费者。** `crates/finder-protocol` 被抽出来是因为 app 与沙盒化的 appex
   是两个二进制，必须共享同一套类型；渲染层今天只有一个消费者。"需要编译器强制依赖方向"本身也可以是
   独立理由——只是现在连要强制的边界都还在移动，强制一个还在变的东西没有意义。
2. **模块边界已经够用。** 本方案 P1–P4 想要的"纯函数 + 描述"，在同一个 crate 里就能表达并被 M1
   断言，`theme::icon_card_layout`、`key_bindings::table`、`finder_menu::model` 都是先例。crate 边界
   带来的是**强制**，不是**能力**；在模块边界已经稳的时候，强制没有额外收益。
3. **拆分的成本随当前形状而定，但确实存在**：`ffi.rs` 的 `ObjPtr`/`MainThreadSlot` 被 40 个文件使用；
   `theme.rs` 自己读 `CONFIG`（P0 之后消失）；`ui_coordinator.rs` 调各表面的内部刷新函数；gate 的
   `cargo-test.sh --require-match` 与 smoke runner 的推导方式是按单 crate 写的。这些不是不可逾越，
   而是**现在不划算**。
4. **顺序问题依然成立**：先立边界后写纯函数，会让边界内包裹的仍是读全局的实现。先有描述与纯函数，
   才看得清边界应该画在哪里。

触发重新评估的条件（不要求"必须有第二个消费者"）：出现第二个消费者；**或**模块边界已经稳定、团队
希望用编译器强制依赖方向；**或**需要在不启动 AppKit 的前提下做视图一致性检查。届时抽的是**视图数据**
（如 `finder-protocol` 的形状），不是渲染代码。没有依据现在就单独抽一个 `ffi` crate：它的作用只是让
渲染层依赖一个更小的包，而依赖方向的问题用模块规则和 P0 就能解决。
### 决策 2：先设置页，后浮窗

设置页背着最重的耦合（114 个控件字段、44 条指针比对、78 个 apply 分支、一份 16 步清单），浮窗的布局
已经部分收拢在 `theme::ThumbFlowLayout` 里。所以先设置页。这与"先做最容易的一块"相反，理由是可断言性
的收益：设置页每迁一页就多一条今天写不出来的断言，浮窗那块更多是把已有约定收拢。

如果 reviewer 更愿意先做 P3（它 0 测试依赖、最机械），可以交换；方案只依赖 P0 在最前，不依赖 P1 与
P3 的先后。**但 P1 与 P2 不可分离**——见 P1 的说明。

### 决策 3：语义与布局分成两个类型（两轮修正后的结论）

初稿写"`RowSpec` 不携带坐标，因为行高依赖字体度量所以坐标不纯"，这个论证是错的，已改写为：语言与
字号是渲染期输入，把它们作为 `MeasuredText` 参数传入之后，坐标同样可以由纯函数算出。因此
`RowSpec` **携带确定的 frame**，`PageCanvas` 只做累加与落位；这与 `theme::ThumbFlowLayout` 的既有
做法一致，也让"每行在哪"第一次成为可断言的值。
## 修订记录

| 轮次 | 结论 | 主要改动 |
| --- | --- | --- |
| 1 | CONCERNS | 收到 6 条发现，逐条处理见下表 |

### 第 1 轮发现的处理

| 发现 | 级别 | 处理 |
| --- | --- | --- |
| "漏接即编译失败"的承诺没有实现路径 | HIGH | **部分接受**：把目标 3 缩小为"描述、控件种类与字段身份同源，漏字段编译期暴露"；不承诺覆盖 `needs_in_place_control_sync` 与就地回填的整条链，那两步仍靠清单，但由 P2 的"回填序列与初值逐项一致"M1 断言覆盖。 |
| P1 与 P2 循环依赖，违反"每阶段独立可发布" | HIGH | **接受**：P2 不再是独立阶段，而是 P1 每一页交付的一部分；每页在同一个改动里完成描述→建树→回调→回填→就地刷新后，才删除旧路径。开放问题 1、2 据此关闭。 |
| P3 未规定借用与重入边界 | HIGH | **接受**：P3 增加"短借用取出计划或更新状态 → 释放借用 → 再执行 AppKit 操作"的硬规则，并说明 `with_tab_state` 是 `RefCell` 可变借用、`refresh_highlight` 会重入；选择 `AppState` 的依据改为生命周期与访问边界；跨线程通道单独确认。 |
| `ControlTable` 缺生命周期约定 | MEDIUM | **接受**：补齐登记/注销成对、重复登记被拒、未知 sender 保留回退、动态行不并入、其余指针表不合并；并明确控件内的临时无效输入与录制草稿不是"第二份真相"。 |
| P0 快照内容与验证条件不完整 | MEDIUM | **接受**：列出快照必须覆盖的完整输入（含自定义色板与 `fonts`/`layout` 字段）；承认 `colors_from_config(dark)` 今天仍读全局、是改动对象而非先例；验证改为依赖关系断言 + 迁移前后等价断言；`ThemeInput::current()` 的退出条件由 M1 断言守住。 |
| M1 断言不能证明真实接线与界面等价 | MEDIUM | **接受**：明确 M1 只覆盖纯描述、身份表逻辑与布局决策；真实 target/action 与三语几何由既有 M2 覆盖，未运行则记 NOT RUN；P4 改为创建与刷新共用 `CardPlan`，不再"断言两套公式相等"。 |
| 不抽 crate 的论证过度断言 | MEDIUM | **接受**：撤回"收益为零"与"四个无法回避的成本"；改为"现在没有值得这份成本的复用需求"，并承认 crate 确实提供依赖方向约束与可见性边界、内部 `pub` 不是永久承诺。 |
| 定量证据前后不一致、部分复现栏不是完整命令 | LOW | **接受**：清点后统一口径（theme 14 处读取、`SettingsUi` 114 个指针字段、overlay 31 个静态中 18 个迁移）；证据表每行给完整可复制的命令。 |

### 第 2 轮发现的处理

| 发现 | 级别 | 处理 |
| --- | --- | --- |
| 缩小后的编译期保证仍过强；回填测试覆盖范围错 | MEDIUM | **接受**：目标 3 再收窄为"每个已声明的可交互行必须携带合法身份"，并明确不保证"整行漏掉"（那需要独立的字段完整性清单）。回填改为两处消费同一份纯更新描述 `Vec<ControlUpdate>`，M1 测该描述、`needs_in_place_control_sync` 与覆盖性，不调用 `load_settings_from`。 |
| P3 迁移表把跨线程结果通道列为主线程布局态 | HIGH | **接受**：`CARD_CLOSE_AX_RESULT` 移入"保留（跨线程通道）"，理由是 `overlay/card_close.rs:415` 在工作线程写入、主线程 :464 取出；只有消费后的关闭动画状态（`PENDING_CARD_CLOSE`）随 `.close` 搬迁。迁移数与清单同步修正。 |
| `&self` 与快照不能提供重入的编译期保证 | MEDIUM | **接受**：删除"类型上不可能同时存在"的说法，保留短借用规则，改为逐处核查调用边界 + 一条覆盖嵌套进入 `with_tab_state` 的回归断言；并注明不是每处 `msg_send!`/`thumbnail::*` 都必然重入。 |
| P0 退出条件扩大了范围，字面量计数守不住 | MEDIUM | **接受**：退出条件收窄为"纯主题/布局/描述层不得读全局"，允许边界入口取快照，并指明替代者；`grep -c` 降级为辅助，附上 `text_scale()` 与 `page_builder.rs` 四处多行写法的反例。 |
| 把确定 frame 全放进 `RowSpec` 改过头 | MEDIUM | **接受**：改为三个类型一个布局计算者——`RowSpec`（语义）/ `layout_rows(&LayoutInput) -> RowLayout`（frame）/ `PageCanvas`（应用）；`LayoutInput` 列全内容宽度、视口、`SettingsLayout` 与 `MeasuredText`。 |
| 重复登记的 release 行为缺构建失败处理 | MEDIUM | **接受**：重复登记中止本次页面构建并回退到上一次可用状态；补齐局部替换/整页重建/整窗销毁三条清理路径与幂等要求，M1 覆盖表操作序列，真实清理路径归 M2。 |
| "必须有第二消费者"不该作为拆分前提 | MEDIUM | **接受**：改为重新评估的三个触发条件之一，承认"需要编译器强制依赖方向"本身可以是独立理由。 |
### 第 3 轮发现的处理

| 发现 | 级别 | 处理 |
| --- | --- | --- |
| 页面失败回退与"先注销旧条目"冲突 | MEDIUM | **接受**：身份校验在破坏旧页面及其登记**之前**完成；新构建失败时旧视图与旧映射**一起**保留；首次构建失败是明确行为（空白页面、记日志、不发布控件），不承诺回退。M1 断言"登记失败后旧映射仍可解析"。不扩展成通用事务框架。 |
| Mouse 页的纯描述缺选中设备 | MEDIUM | **接受**：页面输入改为 `PageInput { Config, ThemeInput, 选中设备 }`，因为同一配置下切换设备会改变滚动与加速度（`current_selected_device()`，`src/settings.rs:1022`）。更新描述用同一份 `PageInput`；M1 覆盖"同一配置、两个设备配置不同"；设备切换与拔出的真实回填归 M2。 |
| 完整性断言错误地包含动作控件 | MEDIUM | **接受**：限定为"每个需要配置回填的静态值控件"；动作控件（如"导出日志"）另验身份与 target/action 接线。并写明共用来源不等于无条件全量执行——编辑中的值、录制草稿、懒加载页与可见性跳过规则都保留。 |
| LOW：摘要与正文数字不一致；P5 仍保留"全部满足"与第二消费者硬条件 | LOW | **接受**：证据表与 P3 不再给总数，以 P3 的成员表为准；P5 改为三个独立的重新评估触发条件 + 两条硬约束，与决策 1 一致。 |
### 第 4 轮发现的处理

| 发现 | 级别 | 处理 |
| --- | --- | --- |
| P1 示意签名仍写 `page_view(&Config, &ThemeInput)`，P3 开头仍写"10 个 + 8 个"，与 `PageInput` 和成员表不一致 | LOW | **接受**：中英两份都已修正——示意图改为 `page_view(&PageInput)`，P3 开头不再给数量、以成员表为准。reviewer 明确这一条不需要再开设计审查轮。 |
## 开放问题（给 reviewer）

- 本方案是否漏掉了一处比 P1–P4 更划算的解耦点？
- P2 的 `ControlTable` 生命周期约定是否覆盖了真实控件重建路径（尤其是 `PageCanvas` 逐行重建与
  `SettingsUi` 整窗重建两种情况）？
- P3 的借用规则是否足够——是否还有 `with_tab_state` 之外的路径会在持有布局借用时触发 AppKit 回调？
- `ThemeInput` 的输入清单是否完整（实施第一步会按 `theme.rs` 的每一处 `CONFIG.read()` 逐行列出）？
- 现在不抽 crate 的判据（三个触发条件任一 + 两条硬约束）是否清晰到可以在将来被独立判定？
- `ThemeInput` 字段清单与 P3 的调用边界清单按 reviewer 的意见留到各阶段实施第一步，不在设计文档里
  穷举；嵌套借用 panic 的测试只能证明违规被拒绝，不能证明生产路径不会重入，借用范围仍需在实施回审时
  逐处检查。