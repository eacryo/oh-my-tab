# UI 改造计划（UI Refresh Plan）

本文说明如何把现有界面改造成 `docs/design-style.md` 规定的样子。英文版见 `docs/ui-refresh-plan-en.md`，
规范本体见 `docs/design-style.md` / `docs/design-style-en.md`。本文是**计划**不是规范：规范规定界面必须
长成什么样，本文规定怎么改、按什么顺序改。

每个阶段都可以独立提交、独立评审。本计划不改变任何用户会称之为「功能」的行为，只改变应用的观感和可读性。

## 1. 基本规则

- 每个阶段交付前跑完整门禁（见 `AGENTS.md`）：
  `cargo fmt` → `cargo check` → `cargo clippy` → `cargo test`，再跑 `scripts/dev-restart.sh --no-onboarding`。
- 涉及布局的阶段另跑 `--smoke-settings-layout`；涉及浮窗的另跑 `--smoke-overlay`；涉及剪贴板的另跑
  `--smoke-clipboard`。
- **提升规则**：本次评审发现的每个问题，要么在 A 层补上断言，要么补一个让它可被断言的状态字段。没有断言
  的修复不算完成。
- 如果某阶段要改动布局校验器或几何测试里写死的值，**同时**更新常量和测试；绝不放宽断言来让它通过。
- 任何阶段都不得让代码树停留在「渲染出原始翻译 key、原始标识符，或不属于该维度尺度的取值」的状态。

## 2. 实测基线

当前的实际取值及字面量位置，即计划中的「改造前」一列。

| 维度 | 当前 | 位置 |
| --- | --- | --- |
| 字号 | `12, 13, 13.5, 14, 20, 24, 30` | `settings/widgets.rs:826,2099,2132`、`settings/select.rs:130`、`settings/page_builder.rs:1131,1784,1807`、`settings/sidebar.rs:145` |
| 圆角 | `1, 4, 5, 6, 7, 8, 9, 10, 12, 14, 16, 18, 26` | `settings/*.rs`、`overlay/cards.rs:295,329`、`clipboard/*.rs`、`keystroke_display/panel.rs:1183` |
| 不在 4px 网格的间距 | `3, 6, 10, 14, 17, 18, 28, 34, 46, 50, 54` | `settings/components.rs:19,56,60,87,88,103,156,1827,1828`、`settings.rs:341`、`settings/sidebar.rs:178,184,199` |
| 浅色 `text_primary` | `#2C2C30` → 对 `window_bg` 12.97:1 —— 已达标，**不改** | `theme.rs:121` |
| 浅色 `text_secondary` | `#73737A` → **4.39:1** | `theme.rs:122` |
| 浅色 `text_muted` | `#9B9BA2` → **2.76:1**（卡片上） | `theme.rs:124` |
| 浅色 `text_disabled` | `#AEAEB5` → **2.21:1** | `theme.rs:125` |
| 卡片层次 | 7% 描边 + 36pt 柔光投影 | `theme.rs:118`、`settings/widgets.rs:2196` |
| 预设标签 | 原始 key `mouse_smooth_preset_ease_in` | `mouse/smooth/presets.rs:95–105`、`settings/page_builder.rs:1014` |
| 硬编码英文 | `Regular`、`Clear`、`Debug`、`Info` | `settings/page_builder.rs:558,574,658` |
| 下拉框溢出 | 折成两行 | `settings/select.rs:26` |
| 滑杆 | 整数滑杆绘制刻度线 | `settings/widgets.rs:1536` |
| 动效 | 0.16–0.58s，四套手调弹簧 | `settings/components.rs:807–816`、`overlay.rs:71` |
| Reduce Motion | 仅 4 个表面处理 | `settings/tooltip.rs:342`、`settings/components.rs:1431`、`clipboard/picker.rs:277`、`updater.rs:689` |

## 3. 阶段 0 —— 修正确性

改动小、收益大、风险低。这些是评审发现的缺陷，不是审美偏好。

### P0-1 · 浅色模式下实时预览不可见 —— 以删除收尾

- **现状**：通用页不再绘制应用切换浮窗与剪贴板浮窗的实时预览，因此没有可辨性问题。那两个模拟块曾是
  App 里唯一"不是面板、却在模拟面板"的内容；它们的填充是白底 0x48–0x90 alpha 叠在白色卡片上，
  这正是它们在浅色模式下不可见的原因。
- **已达成**：无需再定样式。一个错误呈现真实表面的预览比没有预览更糟，而它本要预览的材质现在直接
  由面板本身来评判。
- **文件**：`settings/glass_preview.rs`（删除）、`settings/page_builder.rs`（区块与度量移除）、
  `settings.rs`/`settings/window.rs`（句柄、初始化与预览检查移除）、`theme.rs`（`PREVIEW_TILE_*`、
  舞台色与对比度助手移除）、`e2e_state.rs`（`settings_preview` 帧字段及其读者一并移除）。
- **验收**：表面已不存在，所以检查是结构性的而非视觉的：`src/settings/` 下不再残留 `preview` 符号，
  且 `--smoke-settings-layout` 在三种语言、缩短后的"外观"卡片下依然通过。浮窗本身的观感由各自的
  检查覆盖（`--smoke-keystroke-display-panel` 与浮窗 smoke）。

### P0-2 · 半透明面板材质的渲染像素对比度

- **现状**：`frost` 与 `liquid-glass` 的文字直接用动态系统标签色画在材质上，因此其对比度取决于 AppKit 如何把
  这些颜色合成到半透明表面上。tier-A 的 smoke 只能**记录**数值（"颜色 vs 常量"看不见合成），所以设计文档把
  它们的档位写成**目标值**而非已执行的下限。**已完成（限于其范围）。**该 A2 场景在渲染像素上测表头筛选行与**逐个**页脚 caption，采用**同一次启动内**的差分帧（两张
  截图之间由 app 就地把面板文字隐藏），低于 3:1 即判失败：`{frost, liquid-glass, opaque} × {浅色, 深色} × {黑, 白}背景` 十二种
  组合全部达标（最低 3.58:1）。这两个角色之外（行标题、详情正文、键帽符号）仍是**目标值**；自身贡献低于 3 个色调单位的墨也超出
  差分可视范围。此前的说法作废：本测量的"两次启动"形态已撤回。
- **目标**：新增一个 A2 场景——从 `--e2e-state` 的 `clipboard_picker` 读面板 frame，截取该区域，在**渲染像素**上
  断言档位（`text_primary` ≥ 4.5:1、caption ≥ 3:1），覆盖 `{frost, liquid-glass} × {浅色, 深色}`。其中**深色模式的
  玻璃**（表面 `#6E6E6E`）要优先测，因为 `NSGlassEffectView` 不提供保对比合成，三种动态标签色是它仅有的墨。
- **文件**：新增 `scripts/e2e/*.sh` 场景及它需要的测量助手；`glass::panel_ink` 的分派；有数字之后再改
  `docs/design-style{,-en}.md` 的档位文字。
- **验收**：当某材质的实测对比度低于档位时该场景失败；设计文档引用实测数字而不是目标值。

### P0-2 · 预设下拉显示原始翻译 key

- **现状**：`SmoothPreset::label_key()` 返回的 key 不带 `settings.` 前缀，而 locale 文件把这些键放在
  `[settings]` 段下，于是 `t()` 逐级回退，最终把 key 本身渲染出来。
- **目标**：下拉显示「缓入 / Ease In」，任何情况下都不显示 key。
- **文件**：`mouse/smooth/presets.rs`、`settings/page_builder.rs:903–907`。
- **验收**：单元测试断言每个 `label_key()` 在三个 locale 中都能解析（针对整个 `label_key` 家族回归，
  而不只是这一个 preset）；再加一个通用 i18n 测试，防止调用点传入没有 `settings.` 前缀对应项的 key。
  可考虑加 debug 断言：`t()` 对含 `_` 的 key 不得返回其入参本身。

### P0-3 · 本地化界面里的硬编码英文

- **现状**：玻璃样式选项与日志级别选项是字面量 `["Regular","Clear"]` / `["Debug","Info"]`
  （`settings/page_builder.rs:558,574,658`）；另有一些控件值直接是后端 token（`Auto`、设备 VID/PID、
  preset key）。
- **目标**：每个选项标签都是 `t()` key 且三个 locale 齐全；枚举值映射到显示字符串；原始值下沉到 caption 行。
- **文件**：`settings/page_builder.rs`、`locales/*.toml`。
- **验收**：i18n 测试断言控件可显示的每个值都能通过 `t()` 解析；伪本地化运行下设置窗口不出现未翻译的 ASCII。

### P0-4 · 控件折成两行

- **现状**：旧函数 `settings_select_needs_wrap` 已从当前代码删除；现有
  `settings_select_centers_single_line_inside_the_control` 只断言文本 frame 在控件内垂直居中，尚无断言覆盖单行
  高度、三个 locale 下最长设备名／preset 名及完整值 tooltip。此项**待复核，未完成**。
- **目标**：单行 + 省略号，完整值放 tooltip。
- **文件**：`settings/select.rs`（移除折行分支，保留标签截断分支）。
- **验收**：布局检查断言控件的文字 frame 高度不超过一行；覆盖三个 locale 下最长的设备名与 preset 名。

### P0-5 · 滑杆绘制刻度线

- **现状**：整数滑杆设置刻度线（`widgets.rs:1536`）。
- **目标**：连续滑杆、无刻度、右对齐等宽数字读数。
- **文件**：`settings/widgets.rs` 及 `settings/page_builder.rs` 中的调用点。
- **验收**：所有滑杆的 `numberOfTickMarks` 均为 0；现有读数测试继续通过。

## 4. 阶段 1 —— 建立 token 层

这是结构性阶段：把规范定义的尺度建立起来，再将现有调用点映射过去。可见结果是界面不再「差一点点就对齐」。

### P1-1 · 字号收敛为 12 / 14 / 20 / 26

- 替换 13 / 13.5 / 14 / 24 / 30 这套取值。`13.5` 消失；页面标题 30 → 26；行标签与控件值 13.5 → 14。
- **文件**：`settings/select.rs:130,786,1552,1601`、`settings/widgets.rs:661,826,1775,2099,2132`、
  `settings/page_builder.rs:1131,1784,1807`、`settings/sidebar.rs:145,166`、`settings/tooltip.rs:538`。
  有些字号是先算进局部 `font` 再 `setFont:` 的；这类调用点请用 `systemFontOfSize|boldSystemFontOfSize`
  搜索确认，不要只信这份清单。
- **风险**：14pt 行标签配上固定的 200pt 控件列，会比 13.5pt 更容易截断。合并前用 `zh-Hans` 和
  `--pseudo-locale` 逐个检查控件值；必要时加宽控件列或缩短标签，**绝不把字号缩回去**。
- **验收**：测试枚举设置视图使用的字号集合，断言恰好是 `{12,14,20,26}`。

### P1-2 · 圆角收敛为 8 / 12 / 16 + 同心推导

- 把 13 种圆角合并。选中环改为 `卡片圆角 + 环内缩`；浮窗的缩略图块、关闭按钮圆角由
  `appearance.corner_radius` 计算得出。
- **文件**：`settings/components.rs:926,955,1065,1104`、`settings/widgets.rs:116,1223,1954,2001,2385,2625`、
  `settings/select.rs:345,858,1059`、`settings/sidebar.rs:188`、`settings/tooltip.rs:438,496`、
  `settings/page_builder.rs:1112,1124`、`overlay/cards.rs:129,150,295,329`、
  `overlay/cancel.rs:722`、`overlay/card_close.rs:751`、`clipboard/*.rs`。
- **验收**：测试断言设置界面的每个 `setCornerRadius:` 取值都来自具名常量；环半径等于卡片半径加上文档规定的内缩。

### P1-3 · 间距吸附到 4px

- `SEPARATOR_ABOVE_ROW_GAP` 3 → 4、`SLIDER_READOUT_GAP` 6 → 8、`SETTINGS_CONTROL_TRAILING_INSET`
  17 → 16、行的 `label_x` 12 → 16、侧边栏条目内缩 14 → **12**（`btn_w = card_w - 28` →
  `card_w - 24`；同时把 `sidebar.rs:184,199` 的重复字面量提成一个具名常量）、侧边栏 `LABEL_X` 46 → 48、
  `TOP_PADDING` 50 → 48、`card_bottom_inset` 10 → 8、行高 54 → 52、控件高 34 → 32、
  行内操作按钮 28 → 32、滑杆读数宽度 40 → 44。
- **文件**：`settings/components.rs`、`settings.rs`、`settings/widgets.rs`。
- **风险**：页面文档高度会变化；布局校验器和测试里写死的几何值必须在同一次改动里更新。做成一个自包含的
  **改动**（self-contained change），使 diff 可以被当作「同一套布局，吸附到网格」来评审。
- **验收**：`--smoke-settings-layout` 八个页面全部通过；测试断言具名度量为 4 的整数倍。

### P1-4 · 调色板对比度

浅色（`theme.rs:121–125`）：

| 角色 | 现状 | 目标 | 改后实测（对 window / card） |
| --- | --- | --- | --- |
| `text_primary` | `#2C2C30` | **不变** | 12.97:1 / 13.91:1 —— 已高于 12:1 下限，不要为了「更好看」而加深 |
| `text_secondary` | `#73737A` | `#4A4A52` | 8.19:1 / 8.78:1 |
| `text_muted` | `#9B9BA2` | `#68686F` | 5.16:1 / 5.53:1 |
| `text_disabled` | `#AEAEB5` | `#9B9BA2` | 2.58:1 / 2.76:1 |

`#9B9BA2` 从 `text_muted` 移到 `text_disabled`：它过不了 4.5:1 的文字下限，但高于 2.5:1 的禁用下限且有
余量，所以旧值被复用而不是丢弃。

深色：复核 `secondary` / `muted` / `disabled` 是否同样满足这些下限（实测值见规范表），不足的提上去。

- **文件**：`theme.rs`（`ui_palette`）以及共用调色板 helper 的表面。
- **风险**：`hex_to_ns_color` 是共用的，改一个角色会同时影响所有面板。改完要重新检查引导页、tooltip 和 HUD。
- **验收（文字）**：单元测试对每个文字角色、相对它可能所在的**两个** surface（`window_bg` 与 `card_bg`）
  分别计算对比度并断言规范下限。只在其中一个 surface 达标就算失败——muted/disabled 这一对正是最容易
  藏错的地方。
- **验收（非文字）**：测试断言 accent 在两个 surface 上都 ≥ 3:1；再加一条回归测试钉住结构性边界的取值
  （`card_border` 1.25:1；开关关闭态轨道浅色 1.68:1、深色 2.33:1）。它们低于 WCAG 1.4.11 的 3:1 是
  **有意为之**（规范 §3.3 第 2 条），因此要钉在文档写明的 1.2:1 下限上：跌破下限必须失败，而**未经说明
  理由就「修好」这个偏离**也不应通过评审。

### P1-5 · 卡片用描边定义，不用阴影

- 设置页卡片降为 elevation `none`：描边 10%，移除阴影；`SETTINGS_CARD_SHADOW_INSET = 36`
  消失。
- **文件**：`settings/widgets.rs:2196`、`theme.rs`（`shadow`）。
- **验收**：测试断言设置页卡片没有阴影；两种调色板目视确认。

## 5. 阶段 2 —— 动效

### P2-1 · 两档时长、一条曲线

- 把四套弹簧和散落的时长收敛为 `duration-fast = 175ms`、`duration-medium = 380ms`、
  `ease-standard = cubic-bezier(0.24,1,0.4,1)`；开关滑块与卡片关闭保留一条统一的具名弹簧。
  开关既有的 220 ms 按压反馈与弹簧稳定时长作为文档说明的组件专属例外保留。
- **文件**：`settings/components.rs:807–816`、`overlay.rs:71`、`settings/widgets.rs:1239–1242`、
  `clipboard/picker.rs`（详情面板时长）。
- **验收**：测试断言动画时长使用具名 token 或文档说明的组件例外，且 hover / 选中变化完全没有时长。

### P2-2 · Reduce Motion 全覆盖

- 把检查扩展到浮窗、设置页切换、展开/收起和 HUD；在动画发生时读取，而不是启动时缓存。
- **文件**：现有四处实现，加上 `overlay/*`、`settings/components.rs` 的调用方。
- **验收**：一个强制 reduce-motion 的 dev flag（或复用现有 smoke 钩子），让测试能断言每个动画表面都走瞬时路径。

## 6. 阶段 3 —— 打磨

| ID | 改动 | 文件 |
| --- | --- | --- |
| P3-1 | 侧边栏「恢复默认设置」改成像按钮（填充 + 描边），并说明全局与单页作用域的差异 | `settings/sidebar.rs`、`settings/page_builder.rs` |
| P3-2 | About 页标题使用产品名称；版本号只出现一次，作为页面副标题 | `settings/page_builder.rs:1784–1808` |
| P3-3 | 句子型行标签的括号内容下沉到 caption 行（如剪贴板「粘贴后删除条目」一行） | `settings/page_builder.rs`、`locales/*.toml` |
| P3-4 | 设备下拉显示短名；VID/PID 下沉到 caption | `settings/page_builder.rs`、`mouse/device.rs` |
| P3-5 | 浮窗文字角色：卡片标题和应用名随 `layout.card_text_size` 缩放；页脚直接使用 `fonts.status_bar_size`；卡片字号在缩放后钳制为 13…20pt；任一 caption 行的行高 ≤ 卡片高度 1/3；派生圆角跟随有效卡片圆角 | `theme.rs`、`overlay/cards.rs` |
| P3-6 | 空态与 About 副标题使用新对比度下的 `text_muted` | `settings/page_builder.rs` |

每个 P3 条目互相独立，可以单独发布。

## 7. 每阶段的验证

```sh
cargo fmt && cargo check && cargo clippy && cargo test
scripts/dev-restart.sh --no-onboarding            # 功能交接
scripts/dev-restart.sh --no-onboarding --pseudo-locale   # 长文本布局
scripts/dev-restart.sh --opt --no-onboarding      # 阶段 2 的手感/性能检查
# GUI smoke（失败时以非零码退出）
./dist/Oh-My-Tab-Dev.app/Contents/MacOS/oh-my-tab --smoke-settings-layout
./dist/Oh-My-Tab-Dev.app/Contents/MacOS/oh-my-tab --smoke-overlay
./dist/Oh-My-Tab-Dev.app/Contents/MacOS/oh-my-tab --smoke-clipboard
# A2 端到端（抢焦点，需先征得同意）
scripts/e2e/run-all.sh
```

阶段 1 会改动页面几何，主门禁是 `--smoke-settings-layout`。阶段 2 关乎手感，应当用 `--opt` 配合真实的
滚动/呼出验证，而不是靠读常量。

## 8. 风险

| 风险 | 应对 |
| --- | --- |
| 吸附间距会改变文档高度、打断布局断言 | P1-3 作为一个自包含改动；校验器与 fixture 同步更新 |
| 14pt 控件值会让现有标签截断 | 合并 P1-1 前检查 `zh-Hans`、`zh-Hant`、`--pseudo-locale`；宁可加宽控件列也不缩小字号 |
| 共用的调色板 helper 会把改动泄漏到引导页/HUD/浮窗 | P1-4 之后在两种调色板下重新验证这些表面 |
| 重构之后派生圆角再次漂移 | P1-2 的断言就是防这个的，不能跳过 |
| 动效收敛后手感变差 | 用 `--opt` 做前后对比；目标是减少不同时长，不是变慢 |
| 浮窗改动与缩略图/权限路径相互影响 | 浮窗只做 P3-5，并跑 `--smoke-overlay` 加一次人工呼出 |

## 9. 完成标准

- §2 的每一行要么已解决，要么在代码里记录了「接受偏离」的理由。
- `docs/design-style.md` §13 的清单对每个表面都成立。
- A 层为每个 P0 条目、以及 P1 的调色板与尺度不变量，都存在断言。
- 没有任何用户可见字符串会渲染出原始 key，没有任何控件会折成两行。
