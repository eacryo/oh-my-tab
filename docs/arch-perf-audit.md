# 架构与性能审计（2026-10-17）

这是一次**静态审查 + 定点修复**的结果记录。它分两栏，且两栏不混用：

- **静态确认的代码事实**：读代码就能确定，附路径与行号。
- **尚未测量的影响**：推断出的成本，未做性能测量，只作为排期依据。

P0/P1/P2/P3 是**处理顺序**，不是 BLOCKER/HIGH 之类的严重度。审查由 `scripts/agent-review.sh --task arch-perf-audit` 进行（设计轮 8 轮，最终 `PLAN-STATUS: AGREED`）。

## 一、本轮已修复

| 项 | 问题（代码事实） | 修复 | 验证 |
| --- | --- | --- | --- |
| **P0-1** | `mouse/resolve.rs` 的缓存失效竞争：`resolve()` 在 CACHE 锁内未命中就**释放锁** → clone CONFIG → 计算 → **重新加锁插入**。中间任何 `invalidate_cache()`（`runtime_config.rs:293`、`settings/mapping.rs:21`）都会被这次插入撤销，此后每次 resolve 都读到失效前的配置 | 缓存条目带 epoch；resolve 在读 CONFIG **之前**取 epoch，只在 epoch 未变时发布。`invalidate_cache` 在同一把 CACHE 锁内 `fetch_add` + `clear`，两者串行化 | `an_invalidation_during_resolve_is_not_undone_by_the_publish`。**已验证该测试在去掉 epoch 校验的原实现上失败**（panic: "a computation that raced an invalidation must not be published"） |
| **P0-2** | `clipboard/persist.rs:231` 用无界 `mpsc::channel()` 传完整历史快照；worker 的合并发生在**取到任务之后**，写盘停滞期间生产者可无限积累 | 改为**单待写槽 + 容量 1 的唤醒通道**。生产者覆盖槽位、`try_send` 唤醒（Full 即忽略），不等容量也不等磁盘。提交积压上界为 1 | `a_burst_of_saves_leaves_at_most_one_pending_snapshot`、`an_older_snapshot_never_replaces_a_newer_pending_one` |
| **P1-1** | `collect.rs` 在 `show_minimized == false`（**默认值**）时再发一次 `CGWindowListCopyWindowInfo(ALL)`，而主查询本来就是 All 模式 | 直接用本 pass 已有的 `array`；删除第二次枚举与其条件 `CFRelease` | 全量测试通过；该路径无独立断言（见"未覆盖"） |
| **P1-2** | `window_refresh.rs:700` 在**主线程** apply 阶段调 `window_server_candidates()`，即再枚举一次全部窗口——就在后台采集线程刚枚举完之后 | 采集 pass 从**同一份 CG 快照**提取订阅集，随 `WindowRefreshResult` 带回；主线程零枚举 | `the_extracted_rule_admits_exactly_the_layer_zero_rows`、`the_production_fold_deduplicates_by_window_and_pid`、`the_fold_has_no_alpha_notion`。**注意**：CFArray 遍历本身（`subscriptions_from_cg_list`）没有单元测试——单测构造不了真实 CG 数组；被测的是它调用的 `fold_subscriptions` |
| **P2-1** | `resolve()` 命中缓存也 `clone`（`ResolvedMouse` 含 `HashMap`），未命中更 clone 整份 `Config`；在每次离散滚轮事件上执行 | 缓存与返回值改为 `Arc<ResolvedMouse>`；4 处调用点都是取字段即丢 | 全量测试通过 |
| **P2-2** | `cf_dict_get_*` 每次查询都 `CString::new` + `CFStringCreateWithCString` + `CFRelease`（`collect.rs` 40 处调用） | 改为**闭集枚举 `CgKey`**：12 个键各有一次 `LazyLock<RetainedCf>` 静态持有，没有容量上限、没有按地址匹配、没有逐次分配。创建失败时回退到「临时创建 + 释放」的原路径（不可能泄漏）；两者都失败才返回 `None`，绝不把空键交给 CF API | `each_key_resolves_to_one_stable_cfstring`、`repeated_lookups_never_create_a_new_key`、`the_index_mapping_matches_the_table_order`、`every_variant_holds_its_own_literal` |
| **P2-3** | `panel.rs` 的 render tick 里 `ui_palette()` 取两次（L808 与 `rebuild_badges` 内各一次） | 一次取、按引用传入 | 全量测试通过 |

### 语义变化（必须记录，不是"无行为变化"）

- **P1-1**：`live_set` 的前瞻性从"刚枚举的"变为"本 pass 开始时的快照"，即**新鲜度下移**。过滤规则未变（layer / pid / AX main / attached surface）。
- **P1-2**：订阅集与显示列表现在来自同一快照。**二者之间没有包含关系**（订阅只取 layer-0；显示列表允许有正面 AX 证据的非普通层窗口与 AX 回填窗口）——测试只断言同一输入上的规则等价，不建立集合包含。
- **P1-2**：`None` 快照（隐藏应用的提前返回、枚举失败、AX 无答案）先**在 worker 上走一次回退枚举**（只为订阅集），取不到才保留旧 registry；有效空集合正常清空。回退在 worker 上执行，主线程仍然零枚举。

## 二、未修复 / 延后（已记入 `docs/review-backlog.md`）

| 编号 | 事项 | 为何延后 |
| --- | --- | --- |
| 34 | `settings/dispatch.rs` 三次整份 `Config` clone | 「最后一次写回改 move」编译不过：写回后仍要 `&cfg` 调 `apply_config_change`。真正减少复制需重排所有权，无测量收益支撑 |
| 35 | `card_signature` 每卡 clone 三个 String | 副本可能承担快照语义，需先测量 |
| 36 | `refresh_highlight` 全量设置 | 无条件重置同时在清理复用卡片的旧装饰；改增量必须保留全量失效入口 |
| 37 | 暗色模式无缓存 | 已有通知链；"每帧 60 次"的原判断**是错的**（只在面板可见且有内容的分支内） |
| 38 | 搜索走全量 `rebuild_rows` | `sync_visible_rows` 按显示索引复用，搜索会改变索引→记录映射，直接套用会显示旧内容 |
| 39 | `config.rs` 六职责 | 可分离，但不与性能修复捆绑 |

## 三、明确**不做**（附拒绝理由）

- **玻璃管线抽象**：`apply_live_properties` 已共享核心机制，剩下是调用端编排；`BackdropPanel::sync(...) -> &mut PostSync` 的返回引用归属、锁释放、重入约束都说不清，收益不足。
- **一次性拆四个大文件**：审查意见为"先挑一个有充分测试、所有权清晰的文件做保持行为的移动"。本轮不做。
- **smoke 代码加 `#[cfg(test)]` 门控**：会破坏 A1 的 AppKit smoke 入口（独立启动的进程入口）。若整理，只能是**文件层面**集中到 `*_smoke.rs`。

## 四、非测试源码行数估算

口径：按 `#[cfg(test)]` 属性后的花括号深度剥离后的行数，**是估算，不是精确生产代码统计**。仅用于排序。
下表的数字是 2026-10-17 的测量快照；截至 2026-10-24 它们全部已失真（`lib.rs` 与
`window_management.rs` 之间的相对顺序也已反转），引用前按同一口径重算，不要照抄。

```
settings/widgets.rs 3961 · keystroke_display/panel.rs 3891 · settings/window.rs 3651
lib.rs 3641 · clipboard/detail.rs 3290 · updater.rs 3037 · config.rs 2792
window_management.rs 2536 · window_collector/collect.rs 2416
```

## 五、验证状态

| 项 | 结果 |
| --- | --- |
| `cargo fmt --check` | 通过 |
| `cargo check` | 通过（无新增警告） |
| `cargo clippy --all-targets` | 无**本轮引入**的警告；残留的 `window_management.rs:2801` 等为原有 |
| `scripts/cargo-test.sh` | **1120 passed, 0 failed**（22 ignored）——2026-10-17 实测值；2026-10-24 复测为 1297 executed / 24 ignored，引用前重跑 |
| GUI A1 smoke | **22/22 通过**（2026-10-17，Aqua 会话；当日 runner 共 22 个，2026-10-24 为 24 个，见 `scripts/smoke-runners.sh --list`）。复现命令：`cargo test --lib <完整模块路径> -- --ignored --exact`（或 `scripts/cargo-test.sh --lib … -- --ignored --exact`，它在零匹配时失败）。完整名：`clipboard::tests::picker_rebuild_smoke`、`overlay::tests::overlay_runtime_smoke`、`keystroke_display::tests::panel_creation_and_badge_layout_smoke`、`overlay::cards::badge_smoke_tests::card_badge_runtime_smoke`、`overlay::cards::selection_ring_smoke_tests::selection_ring_runtime_smoke`、`settings::tests::settings_layout_smoke` 与 `settings::tests::settings_layout_smoke_under_reduce_motion`、`settings::tests::settings_collapsible_row_geometry_smoke`、`settings::tests::settings_state_sync_after_content_rebuild_smoke`、`settings::tests::settings_mouse_profile_callback_smoke`、`settings::tests::application_reopen_delegate_opens_settings_smoke`、`settings::widgets::tests::slider_double_click_smoke`、`settings::widgets::tests::stepped_slider_rests_on_whole_steps_smoke`、`settings::widgets::tests::switch_hit_test_and_focus_ring_smoke`、`settings::tests::smooth_scroll_synthetic_event_fields_smoke`、`onboarding::tests::onboarding_live_apply_smoke`、`updater::tests::update_prompt_button_layout_smoke`、`window_collector::tests::objc_exception_guard_contains_a_raise`、`icon_cache::tests::icon_cache_roundtrip_smoke`、`thumbnail::cgshwc_capture_smoke`、`window_collector::space_state_probe::tests::space_state_matrix_smoke`、`window_collector::tests::collect_windows_smoke`。其中 `keystroke_display::tests::panel_creation_and_badge_layout_smoke` 第一次失败是**环境冲突**：它需要独占单实例锁做拒绝探测，而另一个 worktree 的 dev 实例正持锁。按身份停掉那个实例（pid 94170，路径 `oh-my-tab/dist/Oh-My-Tab-Dev.app`）后重跑通过。 |
| A2（`scripts/e2e/run-all.sh`） | 不抢焦点：**4 passed / 1 failed / 11 skipped**。抢焦点（`--include-focus`）：**10 passed / 3 failed / 1 not run / 2 skipped**。逐项见下。 |
| A2 失败项归因 | `panel-edge` check 3（material blur）**在 HEAD 上同样失败**，为既有问题，非本轮引入。`smooth-scroll` 在聚合运行中失败一次，独立复跑通过，**原因未确定**——`run-all.sh` 是顺序执行（`for script in ...`，见 scripts/e2e/run-all.sh:89-108），所以"并发争用"这个说法没有证据，撤回。两次结果都保留在原计数里，不用复跑覆盖失败。`space-groups` 失败原因是 `ctrl+left never reached the expected Space context`——这正是 `docs/review-backlog.md` #13 记录的既有环境限制（缺少已绑定的 Space 切换快捷键），不是回归。`space-desktops` 由脚本自身判定 NOT RUN（不计为通过），对应 backlog #14/#18。 |## 六、下一步建议

1. `panel-edge` check 3（material blur）在 HEAD 与本轮都失败，是**既有缺陷**，应单独立项排查（不属于本审计范围）。
2. `space-groups` 需要一条已绑定的「向左/右移动一个 Space」快捷键才能跑（backlog #13）；绑定后可复跑确认。
3. 若要做 P1 的收益测量：在 `collect_windows_with_frontmost_bump` 与 apply 阶段各加一次计时日志，用 `dev-restart.sh --opt` 对比 summon 耗时。
