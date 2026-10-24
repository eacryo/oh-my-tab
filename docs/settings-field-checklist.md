# 新增一个设置项：需要动哪些地方

这个应用里的"加一个设置项"从来不是一处改动。它是一条链：配置结构体 → 语言文件 → 设置页构建 →
控件事件分发 → 运行期变更标志 → 渲染点。漏掉任何一环，代码照样编译、控件照样显示，但什么都没发生；
更糟的是控件看起来是活的，而正在运行的界面还停在旧值上。

本文就是这条链的清单。它存在的理由：靠读代码去查"加一个开关要动哪几处"，代价远大于改动本身。
工作量本身不大——完整范例落在 **10 个文件**里（7 个 `src/` 下的 Rust 文件 + 3 个语言文件，
由 `grep -rl focused_thumbnail_prewarm src/ locales/ | wc -l` 得到）——但**是哪几个文件，从类型系统里
推不出来**。Objective-C 的消息派发（`msg_send!` / `sel!`）和控件指针映射表对符号跳转是不可见的，
所以"这个字段被谁读"要靠 `grep`，而不是"跳转到定义"。

全文以 `layout.focused_thumbnail_prewarm` 为例——App Switcher 页上的一个布尔开关，控制浮窗隐藏时是否
低频预热前台窗口的缩略图。选它做范例而不是更常见的 `show_app_name_in_cards`，原因是它**完整**：下表
第 1–15 行的接线它一步不缺（第 16 行是测试）。`show_app_name_in_cards` 曾经恰好漏了第 13、14 步，
教训见"示例"一节。

## 清单

下表按依赖顺序排列：后一行在前一行编译通过之前没法做完。范例字段在各处的具体行号可以直接
`grep -n focused_thumbnail_prewarm src/ locales/*.toml` 得到。

| # | 位置 | 要加什么 | 漏了会怎样 |
|---|---|---|---|
| 1 | `src/config.rs` 对应 section 结构体（如 `Layout`） | 字段，加 `///` 注释写明默认值以及它在界面上改变什么 | 什么都不编译 |
| 2 | `src/config.rs` 的 `impl Default for <Section>` | 默认值 | 布尔会变成 `bool::default()`（false），或结构体构建失败 |
| 3 | `src/config.rs` 的 `merge_valid` 分区 else 分支 | 布尔加一行（布尔无条件采纳） | 同段其它字段有一个非法时，用户这个开关被静默丢弃 |
| 4 | `locales/{en,zh-Hans,zh-Hant}.toml` | `row_*` 以及通常还有 `desc_*` | `t()` 把原始 key 直接显示出来；`all_locales_share_identical_key_sets` 会失败 |
| 5 | `src/settings/page_builder.rs` | 行本身：`SettingsRow::…` + `SettingsControl::…` + `bind_control(target, …)` | 要么没有控件，要么控件收不到事件 |
| 6 | `src/settings.rs` 的 `struct SettingsUi` | `*mut AnyObject` 字段 | — |
| 7 | `src/settings/window.rs` 的 `SettingsUi { … }` 字面量 | `std::ptr::null_mut()` 初始化 | 编译错误（这一环是好的） |
| 8 | `src/settings.rs` 的 `load_settings_from` | 把存下来的值写进控件 | 打开设置页显示的是默认值，不是用户保存的值 |
| 9 | `src/settings.rs` 的 `log_config_changes` | 一条 `changed!(…)` | 这个字段永远不出现在配置变更日志里 |
| 10 | `src/settings/dispatch.rs` 的 `ControlField` + `control_field_of` + `apply_control_field` | 枚举分支、指针映射、回读进 `cfg` | 开关能拨动，但什么也没写入 |
| 11 | `src/settings/dispatch.rs` 的 `update_windows_controls_enabled`（或对应的冻结列表） | 控件指针，当该字段位于某个总开关之下 | 总开关关掉后，这个控件还是可点的 |
| 12 | `src/runtime_config.rs` 的 `ChangeFlags` + `change_flags` | 标志位与 `old != new` 比较 | 运行期毫无反应，只有重启才生效 |
| 13 | `src/runtime_config.rs` 的 `needs_in_place_control_sync` | 该标志位，**如果**已打开的设置窗口需要重读它 | 配置文件重载或恢复默认后，页面还显示旧值 |
| 14 | `src/settings/dispatch.rs` 的 `refresh_switcher_keystroke_and_mouse_controls_from_config` | 实际的 `setState:` / `selectItemAtIndex:` 就地写入 | 第 13 步触发了但没人去重读——最常见的"接了一半"的字段 |
| 15 | 消费点：渲染它，或读它决定行为（本例是 `src/thumbnail/capture.rs`；纯浮窗绘制类字段才在 `src/overlay.rs`、`src/overlay/cards.rs`） | 读这个字段并改变绘制或行为 | 开关纯属装饰 |
| 16 | 测试 | 见下一节 | 下次动这个模块时悄悄回归 |

**第 13、14 步是一对，也是最常见的漏项**：`needs_in_place_control_sync` 只决定*要不要*跑同步，
同步函数本身必须真的把新控件的状态写进去。只加标志位的效果是：页面刷新了除新行以外的一切。
下面就是这条链在这个仓库里真实漏过一次的例子。

改变**布局签名**的字段（卡片标题格式、卡片尺寸）还需要在运行期应用路径里加 `reset_switcher()`，
否则复用已有视图树会保留旧渲染——`show_app_name_in_cards` 就是这么做的（`runtime_config.rs`）。
只隐藏或显示一个已经排好版的元素的字段**不需要**它，但需要渲染函数被调用（走 `visual` 标志与
`ui_coordinator::apply_theme_and_locale_refresh`）。

### 示例：一次"接了一半"的字段和一个完整的字段

第 13、14 步是一对，而且都不会在编译时暴露：漏掉它，页面刷新了除新行以外的一切，只在"设置窗口已开着、
配置被外部改变"这一条路径上才看得出来。`layout.show_app_name_in_cards`（缩略图卡片标题是否带应用名）
就是这样漏过一次的典型——`reset_switcher()` 接对了（标题格式是卡片签名的一部分），控件回填却漏了。

这个缺陷已在 `docs/review-backlog.md` 关闭：该标志已加入 `needs_in_place_control_sync`
（`src/runtime_config.rs` 的成员列表）和 `refresh_switcher_keystroke_and_mouse_controls_from_config`
的回填列表（`src/settings/dispatch.rs` 的 `(u.show_app_name_in_cards, cfg.layout.show_app_name_in_cards)`
一项），并有 A1 断言 `changing_only_the_card_switch_still_refreshes_the_settings_controls` 守着。
新增字段仍以 `focused_thumbnail_prewarm` 为准：它第 1–15 步一步不缺。

## 已经免费的部分：验证，不要重写

字段一旦进了 `Config`，下面这些就自动生效；改动时确认即可，不要重复实现：

- **持久化**。配置用 `toml::to_string_pretty` 写出，新字段不需要额外代码。
- **导出 / 导入 / 恢复默认**。`export_toml` 序列化活的 `Config`，"恢复默认"整段替换值，两者都自动覆盖
  新字段。只有**机器本地**字段（需要被 `restore_local_fields` 保留的）才值得扩展
  `an_export_round_trips_every_portable_field`。
- **校验**。布尔不需要 `validate()` 条目。有范围约束的数字需要，并且要在 `validate_rejects_*` 系列
  补一条测试：越界值必须单独回退，不能连累同段其它设置。
- **语言文件键对齐**。`i18n::tests::all_locales_share_identical_key_sets` 会在任一种语言缺键时让构建失败。
- **旧配置文件**。带 `#[serde(default)]` 的 section 加上 `Default` 里的新字段，意味着字段存在之前写下的
  配置会以默认值载入。这条要断言，因为它决定了结果是"新功能"还是"老用户丢了一个设置"。

## 新设置项应当携带的断言

仓库规则是：用户可见的行为必须落地一条 tier-A 断言，或者一个让它可断言的 state 字段。
对设置项来说至少是：

1. **配置层**（纯函数，`cargo test`）：默认值；不含该键的旧文件载入为默认值；序列化往返；
   同段兄弟字段非法时该值仍被 `merge_valid` 采纳。
2. **渲染层**（A1，真实 view tree）：界面真的变了。对浮窗而言是 `--smoke-overlay` 驱动一次真实召唤并
   读回 frame / 可见性。探针必须能证伪——仓库的惯例是：无法被证伪的探针不算门禁。
3. **接线层**（A1，真实入口）：改动在界面**正在显示时**生效，且经由 `apply_config_change`，
   而不是直接调渲染函数。直接调渲染函数只证明了渲染函数能用，完全没说明开关是否接到了它。
4. **设置页**（A1）：`--smoke-settings-layout` 会在三种语言下走遍每一页，所以新行自动获得几何、
   字号 token 与标签关联的覆盖。"控件跟得上配置重载"属于 `--smoke-settings-state-sync`。
5. **A2，只在行为确实只能靠真实输入触达时才加**。大多数设置项不是：它们从内部状态就能断言。
   不要为了一个 A1 已经能判定的字段，去加一个抢焦点的场景。

## 记两个坑

- **隐藏不等于移除**。`setHidden:` 保留 view 的 frame，所以比较 frame 的几何断言（以及 A2 state 里
  "这两个矩形不相交"）语义不变。当周围布局必须不动时用它。`removeFromSuperview` 会改变视图树，
  让那些假设失效。
- **控件的 `tag` 不是 action id**。`SettingsButton` 的 `tag` 决定它的悬停/常态配色。
  保留组件自己的 tag、把 sender 指针映射到 action id，否则指针离开后颜色会卡住
  （见 `AGENTS.md` 的 "Architecture and invariants"）。

## 各部件在哪

| 概念 | 文件 |
|---|---|
| 配置结构体、默认值、校验、合并、持久化 | `src/config.rs` |
| 运行期变更标志与应用路径 | `src/runtime_config.rs` |
| 主题/语言变更后的跨界面刷新 | `src/ui_coordinator.rs` |
| 设置页构建 | `src/settings/page_builder.rs` |
| 条件行（`RowGroup`）与页面几何 | `src/settings/page_canvas.rs` |
| 共享行/控件组件 | `src/settings/components.rs`、`src/settings/widgets.rs` |
| 控件 → 字段映射、回读、就地同步 | `src/settings/dispatch.rs` |
| 从配置回填控件状态 | `src/settings.rs`（`load_settings_from`） |
| 主题 token 与布局计算 | `src/theme.rs` |
| 浮窗渲染 | `src/overlay.rs`、`src/overlay/*.rs` |