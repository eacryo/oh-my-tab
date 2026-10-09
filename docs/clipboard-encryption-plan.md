# 剪贴板历史落盘加密方案（评审稿 · 第 2 版）

- 日期：2026-10-08
- 状态：**设计已定稿，代码未动**（用户要求本轮只完善设计）。已经过十一轮独立设计评审（Codex）：第 1–4、6–8、10 轮为 `CONCERNS`，**第 5、9、11 轮为 `PLAN-STATUS: AGREED`**，第 11 轮另有两处 LOW 文档残留已当场修掉；全部发现与处理见 §10。**§8 的五个决策点已全部确定（2026-10-08）**，实现按 §9 的顺序推进，并逐条核对 §11 的检查清单。
- 项目：Oh My Tab
- 范围：`src/clipboard/` 的持久化与图片缓存；不含内存中的历史、不含 `config.toml`、不含 UI 视觉改动。

本文是方案，不是实施报告。所有「已验证」条目都在 §6 给出可复现的探针命令；所有「待验证」条目在 §7 如实列出，不当作已知事实。

## 0. 摘要

采用**系统钥匙串保存随机主密钥 + AES-256-GCM 整文件加密**的透明加密：用户不需要设密码，密钥由 macOS 登录钥匙串保管，只有本应用能静默读取——授权是否弹框取决于代码签名与 ACL 的匹配情况，其已实测边界与未验证部分见 §6 与 §7（开发构建曾每次重启都换签名、每次启动都弹一次授权框；**2026-10-08 已定位并修复**：判定按「创建该项的应用的签名身份」，而该项由 ad-hoc 构建创建；开发包改用 Developer ID 签名，并按输入指纹复用未变的包，见事实 8）。

- 覆盖三个数据面：历史 TOML、图片原始字节、图片预览（缩略图与详情图）。任何一个漏掉，加密都只是表演。
- **历史的内容格式不变**（仍是 `version` + `entries` 的 TOML），只换外层容器：`serialize_history` / `parse_history` 原样复用。
- **迁移是带提交点的可续做过程**：先逐文件加密图片（幂等、可中断重入），再原子发布加密索引作为提交点，最后才删除明文索引。
- **失败不丢数据**：密钥读不到、解密失败、格式损坏时进入**只读阻塞态**——不写盘、不清扫、不删除、不替换密钥，并明确告知用户。只有「密钥项确实不存在」这一种情形才允许隔离旧文件并重新开始。
- **绝不回退明文**：任何失败路径都不会写出明文历史。
- 与 Windows 内置剪贴板历史（DPAPI → KEK → AES-256-GCM，全程无提示）和 CopyQ 的「外部密钥库」模式是同一思路，§3 有调研。

## 1. 现状：要保护什么

| 数据面 | 路径 | 内容 | 现状 |
| --- | --- | --- | --- |
| 历史文件 | `~/.config/oh-my-tab/clipboard-history.toml` | 全文文本、来源应用名、时间戳、图片 hash、文件路径 | 明文，0600，原子写 |
| 图片原始字节 | `~/Library/Caches/oh-my-tab-clip-images/{hash:016x}` | 复制的原始图片（PNG/JPEG/GIF…） | 明文 |
| 缩略图预览 | 同上 `{hash:016x}.preview` | 降采样 PNG | 明文 |
| 详情图预览 | 同上 `{hash:016x}.detail` | → 详情面板的大图 PNG | 明文 |
| 写入临时文件 | `clipboard-history.toml.tmp<pid>-<gen>`、`{hash:016x}[.preview\|.detail].tmp` | 与主文件同内容 | 明文，崩溃残留 |
| `config.toml` | `~/.config/oh-my-tab/config.toml` | 只有设置项，无剪贴板内容 | **不需要加密** |

已有防线（加密之外，仍然保留）：`org.nspasteboard.ConcealedType` / `TransientType` 标记的内容不记录（`pasteboard.rs:125-131`）、功能默认关闭、`clear_on_quit` 有序退出时清理、文件 0600。

崩溃残留的临时文件不会被遗忘：历史侧的 `sweep_history_temp_files` 在加载时按名字精确清理；图片侧的 `.tmp`（`{hash}.tmp` / `{hash}.preview.tmp` / `{hash}.detail.tmp`）会在下一次加载时被 `sweep_clip_image_cache` 当作「未被引用」删除（`image_cache.rs:788-800`，stem 解析失败即删）。改动后这些临时文件里装的是密文，清理逻辑不变。

## 2. 威胁模型

**保护目标**：让**拿到了历史文件与图片缓存、但没有拿到钥匙串项**的对手读不出内容。具体覆盖：

- 同机其它用户账户、其它进程直接读文件（0600 只挡得住其它用户，挡不住同用户进程）。
- 备份与同步外泄：`~/.config` 会进 Time Machine，也可能被 iCloud Drive / Dropbox 之类同步；图片缓存在 Caches（默认不进 Time Machine）。
- 未开启 FileVault 的机器上，磁盘被拿走。

**不防（必须如实写进 README，不能夸大）：**

- **已经以当前用户身份运行、且用户点了「始终允许」之后的对手**：钥匙串保护的是「读取别的应用创建的项」，不是「同用户进程一律读不到」。
- 已解锁会话下的实时剪贴板本身、进程内存、swap。
- **元数据不被隐藏**：图片文件名仍是内容 hash（泄漏「是否复制过某张已知图片」以及跨会话的重复关系），文件大小与修改时间同样可见。
- **历史明文曾经落过盘**：迁移前写入的明文可能留在 APFS 快照与既有备份里，应用无法回收。
- 删除不等于彻底消失：快照、备份、以及删除失败（见 §4.5）都不在承诺范围内。

## 3. 同行调研：各类剪贴板历史怎么做加密

| 产品 | 做法 | 关键事实 |
| --- | --- | --- |
| **Windows 10/11 内置剪贴板历史** | OS 密钥库透明加密 | `%LOCALAPPDATA%\Microsoft\Windows\Clipboard\`，每项是 CMS `envelopedData`：DPAPI blob → KEK → RFC 3394 unwrap → CEK → **AES-256-GCM**；完全无提示、绑定用户账户（Windows Hello 账户由 PIN 驱动 CredKey 链）。第三方离线取证工具能解，正说明「同用户即可解」——保护的是「别人拿到文件」，不是「拿到账户」 |
| **CopyQ**（v14+，2025） | 用户设密码加密全部 tab | 启动时要求输入；可选「Require password after an interval」定期重问；可选 **「Use external key store」** 把密码交给系统钥匙串（macOS = Keychain）以免每次启动输入；导出默认不加密（GUI 导出可另设密码）。早期 GnuPG 插件已废弃（需外部 `gpg2`，「密码每几分钟就要重输」）。FAQ 专门有一条「为什么加密老是问密码」 |
| **Ditto**（Windows） | 数据库加密 | 早期明文 SQLite（issue #986 至今未关，用户能直接在文本编辑器里看到密码）；PR #666 合并后改用 SQLite3MultipleCiphers（SQLCipher 家族）。社区讨论的两条路就是「每次启动输密码」vs「存 OS keychain」 |
| **Maccy**（macOS 开源） | 未实现 | issue #151 请求加密，被关闭、未实现。社区给出的方案正是本文采用的做法：**首启生成随机密钥存 Keychain、透明加解密、不打扰用户**。作者当时的顾虑是「和 FileVault 比有什么收益」「App Store 对加密软件的流程」 |
| **Alfred**（macOS） | 无加密 | 帮助文档只讲「忽略指定应用」和「忽略 Concealed 数据」，没有任何静态加密承诺 |
| **Raycast** | 无加密承诺 | 手册只说剪贴板历史保存在本机，未见静态加密说明 |

**结论**：主流分三类。(a) 明文 + 忽略敏感来源（Alfred、Maccy 现状）；(b) 用户密码加密（CopyQ 内置、Ditto），安全但每次启动要输入，连 CopyQ 自己都提供「外部密钥库」来绕开；(c) **OS 密钥库透明加密**（Windows 内置、CopyQ 外部密钥库模式、Maccy 社区提案）。本应用是常驻菜单栏、后台持续记录的工具，(b) 的启动提示不可接受，选 (c)。

## 4. 方案

### 4.1 密钥来源与生命周期

- **算法与生成**：32 字节随机密钥（`getrandom`/CSPRNG），首次需要落盘时生成。
- **存放**：登录钥匙串 `kSecClassGenericPassword`，`service = "com.eacryo.oh-my-tab.clipboard-history"`，`account = "master-key"`；值 = `key_id(16B) || key(32B)`（`key_id` 随机，用于区分「密钥不对」和「数据损坏」）。
- **属性**：`kSecAttrSynchronizable = false`（永不进 iCloud 钥匙串，是要求而非默认假设，见 §4.8）；**不设** `kSecAttrAccessible*`（那是 Data Protection 钥匙串的属性，legacy 钥匙串忽略）；不加显式 `SecAccess`，用默认 ACL = 仅创建它的应用可静默读取。
- **创建必须是「仅新增」语义**：`security-framework` 的 `set_generic_password` 是**创建或更新**——直接用它会把另一个构建（dev ↔ release）已经建立的密钥**覆盖掉**，让对方的历史永久无法解密。必须用 `SecItemAdd` 语义：新增成功即持有；返回 `errSecDuplicateItem` 时**转为读取既有项**，绝不更新。该语义在实现时要有反例测试（§5）。
- **进程内缓存与状态**：密钥在进程内缓存一次。**本方案不在关闭开关时删除钥匙串项**（§8 决策点 3 已定），所以缓存跨「关闭 → 开启」仍然有效；但存储状态（`Ready` / `Unavailable` / `Blocked` / `purge_pending`）必须随存储代际重算（§4.7），不得沿用关闭前的旧状态。
- **dev 与 release 共用同一项**：两者共享同一个历史文件路径，若各用一把密钥就会互相读不懂。授权代价按 §6 的边界理解：换 bundle identifier 的构建读取会弹框（事实 7 实测）；**开发构建的弹框因果已于 2026-10-08 定位（事实 8 更新）**：项的 ACL 按「**创建该项的那个应用的签名身份**」判定（不是 ACL 条目的文本形态）。`dev-restart.sh` 原本每次都写新的时间戳 `CFBundleVersion` 再签名，所以每次启动都弹，且「始终允许」不跨重签生效。**修复**：开发包改用 **Developer ID** 签名，脚本按「构建产物 + 全部拷入包的内容 + 签名身份」的指纹复用未变的包；用户一次性重置了那个 ad-hoc 时代创建的项。**应用内无法认领别人的项**（`errSecInvalidOwnerEdit`，-25244 实测），也不该把身份钉到 requirement 上（`SecTrustedApplicationSetData` 拒绝 requirement blob，status 100002 实测）。诊断字段 `--e2e-state` 的 `keychain_acl` 只是 ACL 的**形态**报告（requirement / foreign-requirement / path / none / unknown）。**正式构建安装在没有重置过的旧环境时会如何仍未实测**（§7）。开发运行也可用 `--clip-no-keychain` 完全绕开钥匙串。
- **单写者由既有机制保证**：`single_instance.rs` 用 `~/.config/oh-my-tab/instance.lock` 的 flock 做跨渠道单实例锁（测试 `lock_path_is_shared_by_all_bundle_channels` 明确要求 dev 与 release 共用一个锁文件），因此同一 HOME 下同一时刻只有一个进程在写历史——本方案不需要额外的跨进程协调。要守住的边界是：这个锁按 HOME 而非按应用身份，多用户或不同 HOME 仍可能并发；实现时不得绕过或削弱它。
- **明确拒绝的替代方案**：
  - `kSecUseDataProtectionKeychain`（iOS 式钥匙串）：对 Developer ID 应用需要 provisioning profile + `application-identifier`/`keychain-access-groups` 权限，给非沙盒应用带不来收益，只增加发布签名复杂度。
  - **Secure Enclave**（P-256 + ECIES 包裹 AES 密钥）：密钥不可导出、硬件绑定，但需要 T2/Apple Silicon（Intel 无 T2 不可用）；若加生物识别门禁则每次读取要 Touch ID，与「后台持续记录」直接冲突。列为后续可选强化，不做默认。
  - **用户密码派生（Argon2id）**：见 §3 的 CopyQ 教训；后台常驻工具不可接受。列为后续可选的「锁定历史」功能。

### 4.2 加密算法与文件外壳

- **AES-256-GCM**（`aes-gcm`，纯 Rust，无 C 依赖），每次写盘新随机 96-bit nonce，单密钥下 nonce 空间充足（几千次写盘的碰撞概率可忽略）。是否走 AES 硬件指令与真实吞吐见 §7 待验证项，本方案不写未测过的性能数字。
- 外壳（所有数据面统一，仅 `kind` 与 `object_id` 不同）：

```
magic     [8]  = b"OMTCLIP\x01"     文件类型 + 外壳版本
kind      [1]  = 1 历史 | 2 图片原始字节 | 3 缩略图 | 4 详情图
key_id    [16] 密钥标识（与钥匙串项里的 key_id 比对）
object_id [16] 对象标识 = 该文件逻辑名的 SHA-256 前 16 字节
               （历史 = "history"；图片类 = "{hash:016x}" / "{hash:016x}.preview" / "{hash:016x}.detail"）
nonce     [12] 随机
ct||tag   [..] AES-256-GCM(key, nonce, plaintext, AAD = magic || kind || key_id || object_id)
```

  AAD 绑定头部与**对象标识**：既防止把预览密文塞进原始字节槽位（`kind`），也防止**两个同类图片文件互换**（`object_id`），`key_id` 让「换了一把密钥」成为可诊断的明确结果而不是笼统的解密失败。
  实现补充：头部里携带的那份 `object_id` 不参与 AAD，但 `open()` 会把它与重算值比对，不一致即判 `Damaged`——文件头部与自己的名字不一致属于可疑状态，不能带着这种不一致继续读。
  **`object_id` 由读取方按它请求的逻辑名重新计算并作为 AAD**，绝不信任文件头里携带的那份（头部字段只用于诊断与快速识别）——否则互换两个完整密文文件时，攻击者只需连同头部一起搬运就能通过校验。
- **内容格式不变**：解密后的字节仍是原来的 TOML（`version` + `entries`），`serialize_history` / `parse_history` 不动。
- **文件命名**：历史改用 `clipboard-history.enc`（明文旧文件 `clipboard-history.toml` 迁移后删除）；图片文件**沿用 `{hash:016x}` 系列命名**（残留的元数据泄漏见 §2，决策点 2）。
- **原子写、0600、临时文件清理全部沿用现有实现**：临时文件里现在是密文，`is_history_temp_file_name` / `sweep_history_temp_files` 语义不变。
- 图片缓存的加解密收口在 `image_cache.rs` 的读写助手（`cache_write_image` / `cache_read_image` / `cache_write_preview` / `cache_read_preview` / `cache_write_detail_preview` / `cache_read_detail_preview`），路径与 epoch/generation 逻辑不动。注意 `cache_write_image` 现有「文件已存在即返回成功」的短路**不能**用于迁移转换（§4.4）。

### 4.3 代码落点

| 位置 | 改动 |
| --- | --- |
| 新模块 `src/clipboard/crypto.rs` | 外壳编解码（纯函数、可单测）、密钥类型（`Zeroize` on drop） |
| 新模块 `src/clipboard/keyring.rs` | 密钥状态机：初始化（后台线程）、读取、仅新增创建、删除、缓存失效 |
| 新模块/新状态 `storage_state`（放 `persist.rs` 或 `clipboard.rs`） | 全局存储状态：`Ready` / `Unavailable(reason)` / `Blocked(reason)`；所有写盘、清扫、删除入口先读它 |
| `persist.rs` | 写盘前加密、读盘后解密；迁移提交协议；失败分类；`.failed-<ts>` 隔离 |
| `image_cache.rs` | 6 个读写助手加解密；sweep 加「加载成功」前置条件 |
| `monitor.rs` | `start()` 里的 sweep 改为受状态门控 |
| `dev_flags.rs` | 新增 `--clip-no-keychain`（模拟钥匙串不可用，供降级分支测试） |
| `e2e_state.rs` | 新增 `clipboard.storage = "encrypted" \| "unavailable" \| "blocked"`、`clipboard.key_provider`、`clipboard.keychain_acl` |
| i18n / README / AGENTS.md / `docs/developer-notes{,-en}.md` | 文案与不变量同步（§4.9） |

### 4.4 迁移：带提交点的可续做协议

明文 → 密文必须是**幂等、可中断重入、且在任何中断点都不丢数据**的过程。加载路径同时承担迁移的续做：

1. **先看加密索引，且只看它**：`clipboard-history.enc` **存在**就只走加密路径——能解密就正常加载；解密、认证或解析失败（含 `version` 过新）→ 立即停止：**不改写、不清扫、不迁移**，进入 `Blocked`。
   **只有 `.enc` 不存在时**，才读取明文 `clipboard-history.toml` 并进入迁移。密文失败时旁边可能还躺着上一轮迁移遗留的明文索引，回退到它并用旧内容覆盖新历史，是必须禁止的路径。
2. **逐文件加密图片**（幂等）：对明文索引里每条引用的图片文件（原始字节 / 缩略图 / 详情图），
   - **判定「已转换」必须认证通过**：用当前密钥、按读取方重新计算的 `object_id`、并核对 `kind` 解一次；只有认证通过才跳过。仅凭外壳 magic 相同就跳过不算（可能是上一次失败的残留或别的对象）。
   - 明文文件 → 读入、加密、写临时文件、**读回校验**、原子 rename 覆盖。
   - **失败一律阻止迁移完成，不做「可再生成所以可以放弃」的例外**：现存缓存文件里没有一个是「可丢弃」的——清扫会**保留**幸存条目的 `.preview` / `.detail`，所以遗留的明文预览不会被自动清掉，会在活动缓存里长期留存，而界面还显示「已加密保存」；预览写助手遇到「文件已存在」还会直接返回成功，重新生成也替换不掉它；文件引用条目（`source_path`）不缓存原图，来源文件消失时预览是唯一可用副本，更不允许删。因此：任何被索引引用的现存缓存文件转换失败 → 记日志、**保留原件、保留原索引与全部相关引用**，整个迁移停在 `Blocked`：不发布加密索引、不清扫、不删除、不宣告完成；故障解除后从本阶段续做即可完成。迁移的替换必须走「临时文件 → 读回校验 → 原子 rename」，不得复用预览写助手的「已存在即成功」短路。
   - 失败分类只用于日志与提示（必需原图 / 预览 / 详情图），不影响上面的阻断规则。
3. **提交点：原子发布加密索引**，且只在**索引引用的每个现存缓存文件都已认证通过**之后。写 `clipboard-history.enc`（临时文件 → 读回校验 → rename）。**这一步成功之前，明文索引一律不动。**
   由此得到两个可断言的不变量：**已发布的索引 = 已加密且可认证的集合**；**进入 `Ok` 时缓存目录里不存在该索引引用的明文文件**。任何「部分文件仍是明文却宣告迁移完成、进入 `Ok`」的状态都不存在。
4. **收尾**：`.enc` 存在后，删除明文索引与明文 `.tmp*`。因为阶段 2 的阻断规则，走到 `Ok` 时不可能还有未转换的被引用缓存文件，**正常清扫不承担任何迁移收尾工作**（它只处理真正未被引用的文件）。
5. `config.toml` 不动。

中断与恢复：崩溃在阶段 2 的任意位置，下次从阶段 1 重来（`.enc` 尚不存在 → 明文索引仍是权威 → 已认证的密文被跳过、未转换的继续转换）。崩溃在阶段 3 之后：`.enc` 是权威，明文索引与明文图片在阶段 4 被清理。

约束：任何时刻都不删除唯一的原件；「新副本已可靠写入并认证」永远早于「删除旧副本」；迁移未完成时**不得宣告「已加密保存」**，也不得执行正常清扫。文档必须写明：**迁移前写入的明文可能存在于 APFS 快照与既有备份中，应用无法回收**。

### 4.5 存储状态与失败路径（本版重写）

先定义三个状态，所有写盘、清扫、删除入口都以它为准：

| 状态 | 含义 | 允许写盘 | 允许清扫/删除 | 允许改密钥 |
| --- | --- | --- | --- | --- |
| `Ok` | 密钥可用且历史加载成功 | 是 | 是 | 否（已有密钥） |
| `Unavailable(reason)` | 本次会话拿不到密钥（被拒/被取消/钥匙串被锁）或初始化失败；**功能不可用，什么都不记** | **否** | **否** | 否 |
| `Blocked(reason)` | 数据在，但读不出来（解密/解析失败、版本不支持、`key_id` 不匹配、迁移未完成） | **否** | **否** | 否 |

失败分类与行为（**替换密钥、清扫、删除都只发生在明确授权或明确无救的情形**）：

| 情况 | 分类 | 行为 |
| --- | --- | --- |
| 首次写盘、无钥匙串项、无加密历史 | 正常 | 仅新增地创建密钥（创建不弹框，§6 实测），正常落盘 |
| 密钥项可读、历史可解密 | 正常 | 正常读写 |
| **密钥项不存在，但存在任何加密数据** | 密钥永久丢失 | 数据**确定不可恢复**（换机、钥匙串被清、仅从配置备份恢复、迁移中途密钥丢失）：把**整个存储集合**（历史文件 + 图片缓存目录）一起改名隔离为 `*.failed-<ts>`（名字冲突时加序号，绝不覆盖既有文件），随后以空历史 + 新密钥开始。整体隔离是必需的——只隔离历史文件的话，重新开始后的正常清扫会把尚在缓存里的图片删掉（R2 只挡清扫，不挡「新历史不含旧引用」）。**隔离失败则退回 `Blocked`**，不覆盖 |
| **密钥项存在但读取被拒/被取消/钥匙串被锁** | 访问失败 | `Unavailable`：本次会话不记录、不写盘、不碰任何现有文件；三处提示说明功能不可用，并给出「授予钥匙串访问」按钮（§4.6） |
| **解密失败 / `key_id` 不匹配 / 解析失败** | 数据可能仍完好 | `Blocked`：**不替换密钥、不清扫、不删除**（索引损坏不代表密钥失效；图片与备份可能仍可用），提示「历史无法读取，已原样保留」 |
| **`version` 高于本版支持** | 版本过新 | `Blocked`，且**不做任何改写**（现有 `parse_history` 把「损坏」和「版本过新」都压成 `None`，实现时必须先拆开这两类） |
| **局部清空**（按分类、按搜索结果、保留固定条目） | 用户授权删除部分记录 | 只删被清掉的条目及其不再被引用的文件，**保留密钥**：现有清空路径是按筛选与查询限定范围的（`notifications.rs:1029`），删密钥会让幸存条目（尤其是固定条目）引用的图片永久解不开，超出用户授权范围 |
| **用户关闭剪贴板开关** | 用户授权全量删除 | 删历史文件 + 图片缓存，**保留密钥**（2026-10-08 用户决策，理由见 §8 决策点 3）。走下面的**清理事务**；现有 `discard_history_in` 用 `let _ = std::fs::remove_file(...)` 静默忽略失败（`persist.rs:421-428`），必须改成收集每个删除结果 |
| **清理事务**（关闭开关 / `clear_on_quit` 共用） | 用户授权全量删除 | **① 先持久化意图**：写 `~/.config/oh-my-tab/clipboard-history.purge-pending`（内容标明范围=全量）。写不进去时**依次尝试备用位置** `~/Library/Application Support/oh-my-tab/purge-pending`——**必须是持久目录，不能用 `~/Library/Caches`**（系统可以清理缓存文件，而清理意图无法从剩余数据重新生成，缓存被清就等于丢了清理依据）；备用位置同样在**被清理集合之外**，该目录只放这个标记、按需创建。启动检查两处、撤销时两处都删。**两个位置都写不进去 = 清理请求未能持久受理**：本次进程内停写、**不执行任何删除**（保持存储原样，避免出现「界面说没执行、实际删了一半」的不一致），如实报告，且**不承诺跨启动仍然阻塞**。**② 停止旧写入**：推进存储代际、失效在飞任务（否则清理后旧 worker 可能把数据写回来）。**③ 执行删除**并逐项收集结果。**④ 验证**目标文件确实不存在。**⑤ 撤销意图**（删标记）并确认。**只有 ④ 与 ⑤ 都成功，才允许建立新的可写状态**——标记撤销失败时若放行写盘，下次启动会把这个陈旧标记当成待清理任务，**删掉刚写入的新历史**。任何一步失败：保持阻塞、如实报告、下次启动重试 |
| **待清理任务的处理时机** | — | **每次启动都在加载历史之前处理待清理任务，与当前开关是否开启无关**：`clear_on_quit` 的清理失败正是在开关开启状态下发生的，不能靠「功能关闭」来兜底；不处理就加载，会看到用户要求删掉的历史「复活」 |
| **待清理标记的授权范围** | — | **标记只用于全量清理**（关闭开关 / `clear_on_quit`）。**局部清空失败不得写全量标记**（§4.5 第一行）：那会把「按分类 / 按搜索结果 / 保留固定条目」的失败升级成启动时全目录删除，删掉用户从未授权删除的幸存记录。局部清空失败保持原授权范围：只重试被授权的那部分，或如实报告 |
| **永久权限障碍** | 无法完成 | 不承诺「最终完成」：保持阻塞、如实报告、障碍解除后继续重试（例如文件被 `chflags uchg` 锁定、目录只读） |
| `clear_on_quit` 触发 | 用户授权全量删除 | 同一个清理事务（含先写意图、后验证、再撤销），**保留密钥**（下次启动继续用） |

**硬不变量（实现时的检查清单）：**

- **R1**：只要存在读不出来的加密数据，就不创建、不替换密钥。密钥创建只发生在「无密钥项且无加密历史」或「用户明确清空之后」。
- **R2**：**清扫只在历史加载成功后执行**。`persist.rs:266`、`persist.rs:277`、`monitor.rs:212` 三处 `sweep_*` 调用都必须受状态门控——空历史 + 无条件清扫会把用户全部图片删掉（现网已有该形状的调用，加密后它会从「回收垃圾」变成「销毁可恢复数据」）。
- **R3**：失败路径只做隔离（改名），不做删除；隔离名不覆盖既有文件。
- **R4**：`Unavailable` / `Blocked` 下 `save_history()` 整条跳过（不排队、不写盘、只记一次日志）。加载路径本身在文件缺失或解密失败时已提前 `return`，不会走到末尾的重写（`persist.rs:265-282`）；真正的风险在后续任意一次复制触发的保存。
- **R5**：状态变化要落到日志与 `--e2e-state`，让「本次没写盘」可断言。
- **R6**：**用户授权的删除是一个有意图记录的清理事务，必须被验证、并且只有完整成功才恢复写盘**。顺序固定为「先写意图（主位置失败则试持久目录 `~/Library/Application Support/oh-my-tab/`）→ 停写并失效在飞任务 → 删除 → 验证 → 撤销意图」；**两个位置都写不进去时不执行任何删除、不宣称完成，也不承诺跨启动阻塞**（清理请求未能持久受理，文案如实说明，存储保持原样）；**标记撤销失败仍然禁止写盘**（否则下次启动会删掉新数据）；待清理任务在每次启动加载历史之前处理，与开关状态无关；**该标记只用于全量清理**，局部清空失败不得升级成全量删除。现有 `discard_history_in` 的 `let _ = std::fs::remove_file(...)` 必须改掉——静默失败正是这条规则要消灭的东西。

### 4.6 拿不到密钥时：功能不可用（`Unavailable`）

**2026-10-08 用户决定，取代早先的「内存模式」**：没有授权（读取被拒 / 被取消 / 钥匙串被锁）或初始化失败时，历史剪贴板功能**就是不可用**——不会退回「只在内存里记一份」的半可用状态。理由：那种状态一边显示「当前无法使用历史剪贴板功能」，一边又能搜能粘，「不可用」的承诺与实际行为不一致。

- **什么都不记**：`Unavailable` 下不新增任何条目、不写盘、不清扫、不删除（R1–R6 不变）。进入该状态时，本次会话早先（加载尚未决定时）记录下来的条目连同它们的待写字节一起丢弃——不允许出现「提示说不可用、列表里却有内容」。
- **不可用期间**：设置页（剪贴板页顶部横幅）、面板空状态、以及**关于 → 权限**里的「钥匙串访问」行都反映同一件事，前两处各带一个**「授予钥匙串访问」**动作（面板里提示在上、按钮在下、整体居中；关于页用短文案「授予访问」以适配行内按钮宽度）。动作 = 重新走一次密钥读取：系统会弹一次授权框，授权成功后回到可用状态，不授权则维持不可用。
- **一次授权之后为什么能不再弹**（2026-10-08 实测）：钥匙串 ACL 按应用的**指定要求**判定，Apple 签发身份的要求是「锚定 Apple + 该证书」——跨重编不变；自签名证书虽然 DR 字符串稳定，但实测 macOS 对它是按二进制判定的（重编即失效）。因此 `scripts/dev-restart.sh` 现在优先用 Developer ID 签名并在指纹里包含签名身份（见事实 8）。
- **用户授权删除仍然有效**：关闭开关、主动清空、`clear_on_quit` 在 `Unavailable` 下照常执行（它们只删文件与项，不需要密钥）。
- **不再是「一次提示」**：这句话现在常驻在三处（设置页横幅、打开的面板空状态、必要时的一次系统通知），直到用户授权或关掉功能。

### 4.7 并发与线程

- **密钥初始化与所有钥匙串调用都在后台线程**（持久化 worker 或专用线程）：`SecItemCopyMatching` 是阻塞调用，且授权框可能让调用者一直等待（§6 事实 2/7 实测到阻塞）。**主线程不发起钥匙串调用**，只接收普通结果并更新 UI/状态。
- 初始化期间：**不持有历史锁、IO 锁、缓存写锁等待钥匙串**；保存请求按现有 worker 的「合并到最新快照」语义有界排队（不是无界堆积）。
- **初始化、迁移、清理都绑定「存储代际」（storage generation），而不是布尔开关**：只检查「功能当前是否开启」挡不住这个交错——初始化 A 开始 → 用户关闭并清空 → 再开启触发初始化 B → A 返回时开关又是开启的，A 可能把已经清掉的旧历史与旧密钥重新应用；异步删除同样可能与重新初始化交错。
  规则：关闭立即推进代际并失效所有旧任务；任何结果回传、任何改写动作执行前都核对代际是否仍是发起时那一代；重新开启必须等**清理事务**（含标记撤销）结束，再建立新状态——不是等某个「密钥删除」动作。复用现有机制（`PERSIST_GENERATION`、图片缓存 generation/epoch），不新造通用协议。
  必须有反例测试：**初始化未完成 → 关闭 → 开启 → 旧结果返回**，断言旧结果不被应用。
  实现检查（评审同轮提出）：清理与历史 / 原图 / 预览的写入必须互斥——清理期间不得有任何写盘路径通过代际校验。
- **实现补充（清理与保存序号的水位）**：清理在推进 `PERSIST_GENERATION` 的同时必须把「已排空」水位（`PERSIST_DONE_GENERATION`）一并推进——清理是丢弃排队中的快照而不是执行它们，否则后续的「等 worker 排空」会永远等不到，测试会挂住。
- **存储代际与保存快照序号是两件事，不能复用同一个计数器**：现有 `PERSIST_GENERATION` 表达的是「哪一次保存快照最新」，一次普通保存就会推进它；若把它当作存储生命周期代际，普通复制会误取消正在进行的初始化。生命周期代际独立定义，只有「功能关闭 / 全量重置 / 存储状态重建」才推进。
- 启动加载路径（`load_history` 在 `monitor::start()` 内、主线程调用）改为：主线程发起、worker 完成解密与解析、主线程应用结果；期间 UI 不阻塞。

### 4.8 依赖

新增（前四项均为纯 Rust，无 C 依赖）：

- `aes-gcm` 0.11（RustCrypto）
- `zeroize`（密钥与明文缓冲区清零）
- `getrandom`（密钥/nonce 随机源）
- `sha2`（`object_id` = 逻辑名 SHA-256 前 16 字节）
- `security-framework` 3.7（钥匙串；`item` 模块同时提供仅新增创建与带同步过滤的查询，**不需要** `security-framework-sys`）

钥匙串访问**已定（决策点 4）**：`security-framework` 3.7。需要钉死的三件事，以及对应的具体 API（名称与形态已按 3.7.0 的 docs.rs 核对）：

- **① 仅新增的创建语义**：`ItemAddOptions::new(ItemAddValue::Data { class: ItemClass::generic_password(), data })` + `.set_service(...)` + `.set_account_name(...)` + `.add()`。`add()` 直接包装 `SecItemAdd`，所以**重复项返回 `errSecDuplicateItem` 而不是更新**，正是要的语义。**禁止**用 `passwords::set_generic_password`（创建或更新，会覆盖另一个构建已建立的密钥）。
- **② 读取的查询属性**：`ItemSearchOptions::new()` + `.class(ItemClass::generic_password())` + `.service(...)` + `.account(...)` + `.load_data(true)` + `.limit(1)` + `.search()`（`limit` 走 `From<i64> for Limit`，默认也是 1）。**同步属性的处理方式已确定**：`ItemAddOptions` **没有** `kSecAttrSynchronizable` 字段，而**省略该属性或设为 false 都不会产生同步项**（Apple 文档 + 3.7.0 源码），且 `ItemSearchOptions` 的 `cloud_sync` **不指定时本身就只搜非同步项**——所以**生产创建一律省略该属性，不做任何事后补写**（第一次创建成功后，再补一次 `SecItemAdd` 只会撞上重复项，改不了已有项）。
- **验证分层（必须照做）**：
  - **A1（headless，不碰真实钥匙串）**：断言请求构造正确（`cloud_sync` 未指定/等于 `MatchSyncNo`、`class`/`service`/`account` 正确）与错误码分类映射；测试密钥与临时存储绑定。
  - **真实平台探针（具名命令、独立测试项、用完删除）**：用 `cloud_sync(CloudSync::MatchSyncYes)` 反查，**只有 `errSecItemNotFound` 才算「不是同步项」**；任何其它错误都如实报告「未验证/失败」，**绝不把无法判断当成满足要求**。
  - **验证失败不得触发密钥替换或重新初始化**——它只是诊断，不是密钥生命周期的输入。
- **③ 错误码到失败分类的映射**：`errSecItemNotFound` → 无密钥；`errSecAuthFailed` / `errSecUserCanceled` / `errSecInteractionNotAllowed` → `Unavailable`；`errSecDuplicateItem` → 转读既有项（绝不更新）；其余错误按 `Unavailable` 处理并如实报告。
- 决策 3 之后**没有任何路径会删除钥匙串项**，所以实现**不需要** `delete_generic_password`，也不需要处理 `errSecInvalidOwnerEdit`——后者只在将来加入「全库重置」功能时才需要（§6 事实 3 保留在文档里，用于威胁模型与那条未来路径）。

### 4.9 UI、文案与不变量

- **不新增开关**：功能开启即始终加密（给用户一个「明文」选项等于把默认值交给最不关心它的人）。
- **不加常驻状态行**（2026-10-08 用户决定）。需要用户当场知道的失败由提示语表达，全 locale 的 key 是：`clipboard.unavailable_title` / `unavailable_key`（「获取剪贴板密钥失败，当前无法使用历史剪贴板功能。」）/ `unavailable_history` / `unavailable_purge` / `toast_image_unavailable` / `grant_keychain_access`；其余状态（`Ready` / `blocked` 的具体原因 / 清理未完成或未受理）由日志与 `--e2e-state` 反映。

> **用户决定（2026-10-08）：设置页里那行常驻状态文案不需要，直接移除，不延期。** 状态本身落到 `--e2e-state` 的 `clipboard_storage`（`state` / `reason` / `key_provider` / `plaintext_leftover` / `keychain_acl`）与启动/失败路径的日志。原先为常驻行准备的三语 `status_clipboard_*` key 已删除（无消费者）；后来那批「一次性提示」key（`toast_not_saved` / `toast_history_blocked`）也随内存回退一起删除——不可用时面板与设置页横幅**常驻**显示同一句提示（§4.6），另有一次系统通知。
- 降级与阻塞各给一次提示，走现有通知通道，文案走 `t()`，全 locale。
- 必须同步修改的既有承诺：
  - README §Clipboard history 现在写着「stored without encryption」——改为加密表述，并补 §2 的「不防」边界（内存、快照、备份、元数据、删除失败）。
  - `AGENTS.md` 的剪贴板不变量段（「written to disk in plaintext while the feature is on」）必须改写。
  - `docs/developer-notes-en.md` **与 `docs/developer-notes.md`**（中英双份）的剪贴板持久化小节。
- **日志不变量**：现有日志只记条数/代数，从不记剪贴板内容。本次改动不得把密钥字节、明文或内容片段写进日志；解密失败只记「文件 + 失败分类」。

## 5. 测试与可断言性（A1/A2）

**A1（纯函数与 headless 测试）**

- 外壳往返：四类文件（历史、原始字节、缩略图、详情图）各自「加密 → 落盘 → 读盘 → 用测试密钥认证解密 → **逐字节比对原内容**」，并断言外壳 magic 与 `kind` 正确。仅断言「不含明文子串」不够（压缩/编码/错误数据都可能通过），保留它作为附加断言。
- 反例：任一字节篡改（含 AAD 头部字段）、错误密钥、`key_id` 不匹配、`kind` 互换、**两个同类图片的 `object_id` 互换** → 全部必须拒绝。
- **失败路径反例**（每条对应 §4.5 一行）：密钥项缺失 + 有密文（隔离 + 新密钥，且**断言图片未被清扫**）；读取被拒（`Unavailable`：断言不写盘、文件字节不变、状态字段正确、本次会话不记录）；索引损坏 / `key_id` 不匹配 / 版本过新（`Blocked`：断言不写、不清扫、不删、不换密钥）。
- **迁移续做反例**：在阶段 2 中途（部分图片已加密）、阶段 3 之后（`.enc` 已发布但明文索引仍在）分别造出磁盘状态，断言加载路径能正确恢复且不丢条目；断言迁移期间**不会删除唯一的原件**；断言「部分图片仍是明文」时**不会**进入 `Ok`、不会宣告「已加密保存」、不会执行正常清扫。
- **必需文件转换失败（临时故障）**：原图完整，注入一次临时读取/转换失败 → 断言条目引用与原件都保留、迁移停在 `Blocked`、不清扫、不宣告完成；**随后加载、保存、重启并尝试清扫，条目与原件仍在**；解除故障后重新加载能续做并完成迁移。
- **明文预览转换失败**：原图认证通过、已存在明文 `.preview` 且转换失败 → 断言**不宣告迁移完成、不进入 `Ok`**（明文仍在时不得显示「已加密保存」）；断言后续重启与预览回写不会因「文件已存在」而跳过修复。
- **来源消失的文件引用条目**：`source_path` 指向的文件已不存在、缓存里只剩预览 → 断言预览被正常转换（或被阻断保留），**不得**以「可再生成」为由删除唯一可用预览。
- **坏密文 + 旁边有可读明文**：造一个损坏的 `.enc` 与一个完好的明文 `.toml`，断言两者都不被改写、不清扫、进入 `Blocked`（禁止回退明文重新迁移）。
- **迁移中途密钥缺失**：明文索引仍在、部分图片已加密、密钥项不存在 → 断言整体隔离（历史文件与缓存目录一起改名）且不清扫。
- **完整密文互换**：把两个同类、完整、能通过自身头部校验的密文文件对调 → 必须被拒绝（读取方按请求的逻辑名重新计算 `object_id`，不信任头部字段）。
- **清空范围**：局部清空（按分类 / 按搜索结果 / 保留固定条目）后，断言幸存条目（含固定条目）的图片仍能解密；断言密钥项**未**被删除。关闭开关后的完整重置同样断言**保留**密钥项。
- **清理事务（R6）**：① **主位置写不进**（历史目录只读）→ 断言改用**持久目录** `~/Library/Application Support/oh-my-tab/` 的备用标记并继续事务（断言备用标记**不在** `~/Library/Caches` 下）；② **两个位置都写不进** → 断言本次进程内停写、**存储字节未变（没有部分删除）**、不报告清理完成，且文案如实；随后**退出并重启**，断言行为与文案一致（不承诺阻塞、正常加载）；③ **各阶段中断**（写入意图后崩溃 / 删除中途崩溃 / 撤销标记前崩溃）→ 下次启动在加载历史之前继续清理，且**不加载旧历史**；④ **标记撤销失败** → 断言仍然禁止写盘、状态显示清理收尾未完成；再走「尝试开启 → 重启」，断言**新写入的历史不会被删掉**；⑤ **开启状态下的启动**也处理待清理任务（`clear_on_quit` 路径）；⑥ **局部清空失败** → 重启后固定条目与其他幸存条目仍可读取，**不得**进入全量清理；⑦ 清理完成后断言在飞的旧 worker 不会把数据写回来；⑧ 全程断言**密钥项未被删除**；⑨ 断言「标记检查失败」不会被误当成「标记不存在」。让历史或缓存文件不可删可用只读父目录或不可变标志。
- **生命周期代际**：初始化未完成 → 关闭 → 开启 → 旧结果返回，断言旧历史与旧密钥不被应用（§4.7）。
- **不可用会话不记录**（2026-10-08 取代「内存模式容量」）：断言 `Unavailable` 下不新增任何条目、不写盘、不清扫、不删除；断言进入不可用状态时清掉本次会话早先记录的条目与其待写字节。
- **开关往返**：同一进程内关闭 → 开启，断言重新开启时存储状态按代际重算（不沿用关闭前的旧状态）、清理未完成时不加载旧历史；断言**密钥项保留**（本方案不删除它）。
- **创建语义**：模拟 `errSecDuplicateItem`，断言走「读取既有项」而不是更新密钥。
- 测试/冒烟一律用**注入的测试密钥**（`cfg(test)` / `SMOKE_MODE`，路径本就是临时目录），不碰真实钥匙串 → 自动化不会被系统弹框卡住；测试密钥必须与临时存储绑定（断言它不会作用到真实路径）。

**A2（e2e 与状态字段）**

- `--e2e-state` 增加 `clipboard.storage` / `clipboard.key_provider`；**同时由 shell 脚本直接读磁盘文件**，断言外壳 magic 存在且不含明文子串——应用自述不算证据。
- 新增 dev flag `--clip-no-keychain`：真实进程走 `Unavailable`，断言提示、空面板与「不写盘」，并保证场景结束后应用仍然可用（现有 e2e 的硬要求）。

## 6. 已验证的平台事实（含可复现探针）

以下都是本机实测（macOS 26 / darwin 27，login keychain），探针源码见附录 A：

| # | 事实 | 证据 |
| --- | --- | --- |
| 1 | ad-hoc 签名的裸 CLI 可创建钥匙串项并静默读回 | `SecItemAdd -> 0`，`SecItemCopyMatching -> 0`，32 字节一致 |
| 2 | 裸 CLI 换一个 cdhash（重建）读同一项 → **被阻塞**（弹系统授权框） | 8 秒看门狗杀掉，`exit=142` |
| 3 | 非属主删除该项 → `-25244`（`errSecInvalidOwnerEdit`），**不弹框、不删除** | 删除探针立即返回 -25244 |
| 4 | **.app bundle + 固定签名身份（本机 `oh-my-tab-sign`）、`Info.plist` 不变的两个构建之间读取同一项 → 静默成功** | build1（cdhash 30026d83）创建，build2（cdhash 6f555f0f）读回 `-> 0, bytes=32`（**注意**：cdhash 不同而静默，故不能把弹框简单归因于「签名变了」，见事实 8 与其后的边界说明） |
| 5 | 用户级工具 `/usr/bin/security delete-generic-password` 可删除任意项 | 「password has been deleted」 |
| 6 | 创建项本身不弹框 | 探针全程无 SecurityAgent 进程 |
| 7 | **bundle 身份相同、但 bundle identifier 不同**（`com.eacryo.oh-my-tab` 创建 vs `com.eacryo.oh-my-tab.dev` 读取）→ **被阻塞（弹框）** | 8 秒看门狗杀掉，`exit=142` |
| 8 | **真实开发构建每次启动都弹授权框，「始终允许」不跨重启生效**（用户报告，实测复现；**2026-10-08 已定位并修复**） | 修复前的实测（保留为历史证据）：`scripts/dev-restart.sh` 每次运行都在签名前写入新的时间戳 `CFBundleVersion`（`Info.plist` 在签名范围内）→ 每次启动都是新签名；`codesign -dvvv dist/Oh-My-Tab-Dev.app \| grep CDHash` 连续两次「无代码改动」重启得 `e83e53f2…` 与 `d9a5272c…`；两次启动都被授权框阻塞约 10 秒；弹框文案「"Oh-My-Tab Dev" 想要访问你的钥匙串中的密钥 "com.eacryo.oh-my-tab.clipboard-history"」；两次「始终允许」都只对当次构建生效。**定位（2026-10-08，三个探针 + 实机；取代早先按「ACL 形态」的猜测）**：判定依据是**创建该项的那个应用的签名身份**，而不是 ACL 条目的文本形态（macOS 用废弃 API 读出来一律报「路径」）。自签名/ad-hoc 构建创建的项，一旦二进制变了就不再被承认（探针：重建同一路径、同一自签名身份后读取即弹框）；Developer ID 构建创建的项**跨同样的重建仍被承认**（探针：静默；实机：重建后的开发包 67ms 读完、无弹框）。**应用无法认领别人的项**：用 Developer ID 签名去删一个异签名创建的项会返回 `errSecInvalidOwnerEdit`（-25244，实测）。因此**历史遗留项的修法是一次性重置**：用户删掉该项 → 应用新建一把密钥（此时由 Developer ID 身份创建）→ 旧存储集合按设计**改名保留**为 `*.failed-<ts>`（不删除），从空历史开始。重置后，**本机已实测**开发构建重建后读取静默（67ms、无弹框）；**正式构建安装在没有重置过的旧环境时会如何，仍未实测**（§7）。**修复（两步）**：①`scripts/dev-restart.sh` 按「构建产物 + 全部拷入包的内容 + 签名身份」的指纹复用未变的包（不重签、不重打包）；②开发包改用 **Apple 签发的 Developer ID** 身份（`CODESIGN_IDENTITY` 可覆盖，自签名与 ad-hoc 只作回退）。实测（Developer ID + 一次授权之后）：**改代码重编也静默**（22:10:21.985 启动 → 22:10:22.298 保存完成，约 0.3s，无弹框）；自签名身份下重编必弹（对照探针实测）。**开发期仍需注意**：换回自签名/ad-hoc 就退回「每次重编弹一次」。诊断字段：`--e2e-state` 的 `keychain_acl`（ACL 的**形态**报告，不作稳定性判据） |

**授权行为：已实测、推断与未验证三者的边界必须分清。**

- **已实测**：同一签名身份 + 同一 bundle identifier、`Info.plist` 不变的两个探针构建（cdhash 不同）之间读取静默（事实 4）；换 bundle identifier 弹框（事实 7）；ad-hoc 裸 CLI 换 cdhash 弹框（事实 2）；**修复前**「每次重启都换签名」的开发构建每次启动都弹、且「始终允许」不跨重启生效（事实 8）；**修复后**（Developer ID 身份）签名不变的连续重启静默（~0.3s）、**改代码重编后的首次启动同样静默**（事实 8）；自签名/ad-hoc 回退时才是「每次重编弹一次」（旧行为）；项的 ACL 按**创建该项的应用的签名身份**判定，应用内无法认领异签名创建的项（事实 8）。
- **已定位（2026-10-08，取代早先「仅属推断」的猜测）**：弹框取决于**创建钥匙串项的那个应用的签名身份**——由 ad-hoc/自签名构建创建的项，在其二进制变化后就不再被承认（重签过的包弹框并阻塞，实测）；由 Developer ID 构建创建的项跨同样的重建仍被承认（静默，探针与实机各一次）。**适用条件写清楚**：上面这几条历史观察针对的是本机那个 ad-hoc 时代创建的旧项；本机已于 2026-10-08 一次性重置，重置后的实测见事实 8。
- **未验证**：正式构建（Developer ID、签名随版本稳定）的授权持久性——「最多弹一次后静默」是预期而非实测，仍留在 §7。

实践结论（已实测的那部分，2026-10-08 修复后）：**当前开发包（Developer ID 身份）在授权一次之后，改代码重编也弹零次**（读取静默 ~0.3s）；**回退到自签名或 ad-hoc 时**才回到「每次重编弹一次」（这是旧行为，对照探针实测）；纯 UI 验证仍可用 `--clip-no-keychain` 完全绕开钥匙串；本方案不需要为开发构建单独准备密钥来源（共用同一项是刻意的，见 §4.1）。事实 3 与 5 说明威胁模型里要如实写：钥匙串保护的是「读取」，不是「删除」（别的用户级工具能删，但删掉只会让历史不可恢复，不泄露内容），而删除失败必须如实上报（§4.5）。

## 7. 待验证与风险（诚实清单）

1. **钥匙串授权记忆的持久性**：机制已定位——项的 ACL 按「**创建该项的应用的签名身份**」判定，所以「始终允许」对**由 ad-hoc/自签名构建创建**的项不跨重签生效（历史现象，事实 8）。本机已在 2026-10-08 做过一次性重置，此后开发构建重建后读取静默（实测 67ms）；**正式构建安装在没有重置过的旧环境时会如何**，以及正式构建是否「最多弹一次后静默」，**均未实测**，不得写成保证；`SecTrustedApplicationSetData` 拒绝 requirement blob（status 100002，实测）说明**自签名** dev 构建无法把身份钉到 requirement 上，Developer ID 下是否不同**未实测**。
2. 钥匙串证书缺失、dev 回退 ad-hoc 签名的 bundle，重建后是否每次弹框（按事实 2 推断为「会」，bundle 形态未实测）。
3. `aes-gcm` 在 `aarch64-apple-darwin` 上是否走硬件加速、以及大历史（`max_entries` 上限 + 大文本）加解密耗时（实现时用命令测出真实数字，不臆测）。
4. 初始化期间保存请求的合并队列上界（实现时给出具体数字与断言）。
5. 旧明文的残留：APFS 快照/既有备份中的明文不可回收（既知边界，必须写进文档）。

## 8. 决策记录（2026-10-08 全部已定）

1. **始终加密，不提供「不加密」开关** —— 已定。功能开启即加密；失败路径是**功能不可用**（不记录、不写盘），没有明文回退，也没有内存回退（2026-10-08 用户决定，见 §4.6）。
2. **图片文件名沿用 `{hash:016x}`** —— 已定；代价是 §2 记录的元数据泄漏（存在性、跨会话重复关系、大小/时间）。**本次只做 `object_id` 认证绑定**，不等待随机文件名改造；彻底消除泄漏需要把文件名改成不透明随机 id（`image_cache.rs` 的路径/去重/epoch 多处联动，约 100–200 行），列为**后续独立改动**。
3. **关闭剪贴板开关时不删除钥匙串项** —— 已定；作为交换，关闭时必须**确保删除成功**（§4.5 的清理事务与 R6）。理由按评审意见收紧为**减少生命周期复杂度**：删密钥会多出一条不可逆的失败路径（非属主删除返回 `errSecInvalidOwnerEdit`）与一层顺序约束，正好落在评审反复指出风险最高的生命周期顺序上，收益却不明显。**不得据此作出过强的安全承诺**：用户可以授权其他应用访问钥匙串项，所以「只有本应用能读它」不是保证；清理承诺只覆盖**本应用管理的存储集合**，**不承诺阻止从旧备份中单独恢复历史文件后再次解密**（Time Machine 支持单文件恢复）。
4. **钥匙串封装用 `security-framework` 3.7** —— 已定。三件事与具体 API 见 §4.8：`ItemAddOptions` 提供仅新增语义、读取用 `ItemSearchOptions`（**不指定 `cloud_sync`，只搜非同步项**）、错误码到失败分类的映射；并按要求做验证分层（A1 断言请求构造，真实探针用 `cloud_sync(CloudSync::MatchSyncYes)` 反查，只有 `errSecItemNotFound` 才算「不是同步项」）。
5. **dev 与 release 共用同一密钥项** —— 已定。两者共享同一历史文件路径，各用一把密钥会互相读不懂；代价是不同 bundle identifier 首次互相读取会弹一次系统授权框（§6 事实 7）。**授权记忆的持久性保持为 §7 待验证，不写成保证。**
6. **失败提示走三个表面，不做常驻状态行** —— 已定（2026-10-08 用户决定）。文案与三处位置见 §4.6「提示」。**同一天后续决定（同条）**：不可用时不再回退内存模式，所以面板不会出现「提示与列表并存」的情形——不可用即不记录，面板只显示提示与「授予钥匙串访问」按钮；`--smoke-clipboard` 断言「健康时空状态是普通文案、不可用时是失败文案、恢复后回到普通文案」。

## 9. 落地顺序

1. `crypto.rs`：外壳 + `object_id` 绑定（读取方重算）+ 纯函数反例测试。
2. 存储状态机与失败分类（§4.5 的 R1–R6）+ 存储代际（§4.7）+ `--clip-no-keychain`。
3. 密钥模块：后台初始化、仅新增创建、缓存失效、错误码映射（含反例）。
4. `persist.rs` 接线 + 迁移提交协议（§4.4）+ 清理验证与 `purge-pending` 重试（R6）+ 「盘上无明文」与迁移续做断言。
5. `image_cache.rs` 三个文件类型加解密 + sweep 门控（R2）。
6. 不可用状态完整行为（§4.6）+ 状态文案 + i18n + `--e2e-state` 字段。
7. 文档同步（README / AGENTS.md / developer-notes 中英双份）。
8. 全量 gate（`cargo fmt --check` / `cargo check` / **`cargo check --release`**（本次改动含 `cfg(test)`/`cfg(debug_assertions)` 门控与依赖变更，release 配置必须单独过一遍）/ `cargo clippy` / `cargo test` + 适用的 smoke/e2e）+ `scripts/dev-restart.sh` 实跑 + 独立代码评审；性能类验证（`--opt` / `dev-opt`）与构建门禁分开报告。

## 10. 设计评审的发现与处理

评审结论 `PLAN-STATUS: CONCERNS`，方向（Keychain 随机密钥 + AES-256-GCM + 保留 TOML 内容格式）被认可。逐条处理：

| # | 严重度 | 发现 | 处理 |
| --- | --- | --- | --- |
| 1 | HIGH | 仅保留索引不足以保住数据：索引损坏被误判为密钥失效、空历史触发图片清扫 | §4.5 重写为三状态 + 失败分类；**R1 不替换密钥、R2 清扫门控、R3 只隔离不删除** |
| 2 | HIGH | 迁移没有可恢复的提交边界（中途退出会留下明文/密文混合） | §4.4 改为「逐文件幂等加密 → 原子发布索引作为提交点 → 收尾删除明文」；`cache_write_image` 的「已存在即成功」短路不得用于转换 |
| 3 | HIGH | ~~内存模式缺图片存储方案~~ | 已被 2026-10-08 的决定取代：**去掉内存回退**，不可用即不记录（§4.6） |
| 4 | HIGH | 密钥创建/删除/缓存不一致（`set_generic_password` 会覆盖、删项后缓存仍生效、删除失败被当成功） | §4.1 仅新增语义 + 缓存失效；§4.5 删除失败如实上报；§4.8 钉死三件事 |
| 5 | HIGH | 把主线程阻塞留到实现后验证不符合证据 | §4.7 现在就规定：钥匙串调用全在后台、不持锁等待、初始化结果回传时复核功能开关 |
| 6 | MEDIUM | 「盘上无明文」断言不足、AAD 未绑定对象身份 | §5 改为认证解密逐字节比对 + 四类文件与临时文件覆盖；§4.2 AAD 加入 `object_id`；A2 由脚本直读磁盘文件 |
| 7 | MEDIUM | 部分安全/平台承诺超出证据（进程边界、元数据、互读授权、删除彻底性） | §2 精确化「保护目标 vs 不防」；§2 补元数据泄漏；§7 保留授权持久性为待验证；§4.9 文案只承诺可确认的清理 |

第 2 轮（`PLAN-STATUS: CONCERNS`，4 条 HIGH；第 1 轮的 3、5、6、7 项判定已解决）：

| # | 严重度 | 发现 | 处理 |
| --- | --- | --- | --- |
| 1 | HIGH | §4.4 第 1 步：密文读取失败后仍允许回退明文，会用旧明文覆盖需要保留的新历史 | §4.4 第 1 步改为：**只有 `.enc` 不存在时才进入明文迁移**；密文存在但解密/认证/解析失败一律停止改写、清扫、迁移 → `Blocked`。新增反例「坏密文 + 旁边有可读明文，两者都不得改动」 |
| 2 | HIGH | §4.4 第 2–4 步：迁移未完成也可能被当作已提交（明文缓存仍在却显示「已加密保存」；仅凭 magic 跳过无法确认此前转换成功） | §4.4 第 2 步把「已转换」定义为**按当前密钥 + 读取方重算的 `object_id` + `kind` 认证通过**；第 3 步把提交条件收紧为「索引引用的每个文件都已认证通过」，得到不变量「已发布的索引 = 已加密且可认证的集合」；转换失败的条目从待发布索引剔除并保留原件。新增「部分明文不得进入 `Ok`」断言 |
| 3 | HIGH | §4.7：异步初始化只检查当前开关，挡不住「关闭 → 重新开启」后旧结果返回 | §4.7 改为**存储代际**绑定：初始化、迁移、清理都带代际，结果回传与改写前核对代际；关闭立即失效旧任务；重新开启等待密钥删除顺序结束；复用现有 generation/epoch 机制。新增「初始化未完成 → 关闭 → 开启 → 旧结果返回」反例 |
| 4 | HIGH | §4.1/§4.5：「主动清空」不能一律删除主密钥（现有清空是按筛选/查询限定范围且保留固定条目，删密钥会让幸存图片永久解不开） | §4.5 拆成三类：**局部清空**只删被清条目与其无引用文件、**保留密钥**；**关闭功能**才删文件 + 缓存 + 项；`clear_on_quit` 删文件与缓存、保留密钥。新增「局部清空后幸存（含固定）条目图片仍可解密、密钥项未被删除」断言 |

同轮补充采纳的意见：`object_id` 必须由**读取方按请求的逻辑名重新计算**（§4.2），不信任文件头；单实例依据改为「守住现有跨渠道 `flock`（`lib.rs:2380` 启动即持有）」而不是「明文时代已有同样问题」（§4.1）；验证重点补上「迁移中途密钥缺失」「内存容量被固定图片占满」「完整同类密文互换」三类反例（§5）。

第 3 轮（`PLAN-STATUS: CONCERNS`，1 条 HIGH；第 2 轮的 1、3、4 项判定已解决）：

| # | 严重度 | 发现 | 处理 |
| --- | --- | --- | --- |
| 1 | HIGH | §4.4 第 2–4 步：失败条目被剔除后原件仍会被正常清扫删除（剔除 → 失去引用 → `Ok` 后清扫；且临时读取失败不代表原件损坏），与「失败路径不删除」「不删唯一原件」直接冲突 | §4.4 第 2 步改为**必需 / 可再生成分开**：必需文件（原图原始字节）转换失败 → **保留原索引与全部引用**、迁移停在 `Blocked`、不清扫不删除不宣告完成、故障解除后续做；可再生成文件（预览）失败不阻塞迁移。删除「把失败条目剔除出索引」的写法。§5 增加「注入临时转换失败 → 条目与原件都保留，重启清扫后仍在，解除故障后续做成功」反例 |

同轮补充采纳：**存储生命周期代际与 `PERSIST_GENERATION`（保存快照序号）必须分开**，否则一次普通复制会误取消正在进行的初始化（§4.7）。

第 4 轮（`PLAN-STATUS: CONCERNS`，1 条 HIGH；第 3 轮的必需原图阻断与代际分离判定已解决）：

| # | 严重度 | 发现 | 处理 |
| --- | --- | --- | --- |
| 1 | HIGH | §4.4：允许「预览转换失败不阻塞迁移」会留下明文预览——清扫**保留**幸存条目的 `.preview`/`.detail`，所以遗留明文不会被清掉；预览写助手「已存在即成功」也替换不掉它；文件引用条目的来源消失时预览还是唯一副本 | 删除「可再生成」例外：**任何被索引引用的现存缓存文件转换失败都阻止迁移完成**，保留原件与原索引、停在 `Blocked`、可续做；迁移替换必须走临时文件 + 原子 rename，不得复用预览写助手的短路。提交条件改为「每个现存缓存文件都已认证通过」，并新增不变量「进入 `Ok` 时缓存里不存在该索引引用的明文文件」；明确**正常清扫不承担迁移收尾**。§5 新增两个反例（明文预览转换失败不得宣告完成；来源消失时不得以「可再生成」删除唯一预览） |

第 5 轮：**`PLAN-STATUS: AGREED`**，未发现新的 BLOCKER / HIGH / MEDIUM 设计问题。评审确认 §4.4 与 §5 已统一遵守认证与失败阻断规则、不再以剔除条目或放弃预览绕过提交条件、清扫不承担转换任务，并确认此前认可的边界（密文失败不回退明文、不自动替换密钥、局部清空保留密钥、代际与保存序号分离、后台钥匙串调用）继续有效。评审同时明确：**设计通过不代表性能、容量上限、故障恢复效果与授权持久性已经成立**，这些要在交付阶段用命令测出并附证据。

**用户决策（2026-10-08）**：§8 决策点 3 定为**保留密钥 + 确保删除成功**，取代第 2 轮曾写入的「关闭功能时删除钥匙串项」。相应地：§4.5 新增清理事务与不变量 **R6**，§4.9 的状态文案增加「清理未完成」，§5 增加清理事务的反例，§4.1 的缓存失效规则随之取消（密钥不再被删除）。

第 6 轮（`PLAN-STATUS: CONCERNS`，3 条 HIGH + 1 条 MEDIUM，全部关于新引入的 `purge-pending` 机制）：

| # | 严重度 | 发现 | 处理 |
| --- | --- | --- | --- |
| 1 | HIGH | 清理意图只在失败后才写：删除中途崩溃、或目录不可写导致标记写不出来，下次启动就不知道还有清理任务；`clear_on_quit` 尤其不能靠「功能关闭」兜底 | §4.5 改为**清理事务**：① 先持久化意图 → ② 停写并失效在飞任务 → ③ 删除 → ④ 验证 → ⑤ 撤销意图；意图写不出来就**不执行删除**、不宣称完成；待清理任务在**每次启动加载历史之前**处理，与开关状态无关；§4.7 的等待对象由「密钥删除」改为「清理事务」 |
| 2 | HIGH | 标记删除失败只记日志并恢复写盘，会让下次启动把这个陈旧标记当成待清理任务，**删掉刚写入的新历史** | §4.5：只有「数据清理 + 标记撤销」都确认成功才允许建立新的可写状态；标记撤销失败继续禁写，并以日志与 `--e2e-state` 的 `blocked/purge-pending` 如实反映；§5 增加「标记撤销失败 → 尝试开启 → 重启，断言新数据不被删」反例 |
| 3 | HIGH | 全量标记可能由局部清空失败产生，启动重试会越权删掉用户从未授权删除的幸存记录 | §4.5 明确标记**只用于全量清理**（关闭开关 / `clear_on_quit`）；局部清空失败保持原授权范围（只重试被授权部分或如实报告）；§5 增加「局部清空失败 → 重启后固定与其他幸存条目仍可读」反例 |
| 4 | MEDIUM | 保留密钥的理由含过强断言（「只有本应用能读」忽略用户可授权其他应用访问钥匙串；「备份成对恢复」不覆盖 Time Machine 的单文件恢复） | §8 决策点 3 的理由收紧为「减少生命周期复杂度」，并写明：清理承诺只覆盖**本应用管理的存储集合**，**不承诺阻止从旧备份单独恢复历史文件后再次解密** |

同轮补充采纳：永久权限障碍（不可变标志、只读目录）下不承诺「最终完成」，表述为**保持阻塞、如实报告、障碍解除后继续重试**（§4.5）；验证还要覆盖「清理完成后在飞的旧 worker 不得把数据写回来」。

第 7 轮（`PLAN-STATUS: CONCERNS`，1 条 HIGH；第 6 轮的 2、3、4 项判定已解决，第 1 项部分解决）：

| # | 严重度 | 发现 | 处理 |
| --- | --- | --- | --- |
| 1 | HIGH | 意图写入失败时「阻塞」只存在于本次进程：重启后既没有标记也没有持久化的阻塞状态，旧历史仍会被解密加载，违反「清理完成前不加载旧数据」；`clear_on_quit`（开启状态下触发）尤其如此 | §4.5 ① 增加**备用标记位置** `~/Library/Caches/oh-my-tab-clip-images.purge-pending`（放在缓存目录**之外**，清理与清扫都碰不到它）：主位置写不进就写备用位置，启动检查两处、撤销时两处都删；**两个位置都写不进去 = 清理请求未能持久受理**——本次进程内仍然停写并尽力删除，但**不承诺跨启动仍然阻塞**，文案必须如实说明「本次清理未受理，重启后可能重新看到旧历史」。R6 同步收紧；§5 反例扩到八条，新增「主位置失败 → 用备用位置继续」与「两处都失败 → 退出并重启 → 行为与文案一致」 |

同轮补充采纳：实现时守住「立即失效旧任务」的代际规则，并验证**清理与历史 / 原图 / 预览的写入互斥**（§4.7 已加实现检查）。

第 8 轮（`PLAN-STATUS: CONCERNS`，1 条 HIGH + 1 条 MEDIUM；第 7 轮的跨启动依据判定部分解决）：

| # | 严重度 | 发现 | 处理 |
| --- | --- | --- | --- |
| 1 | HIGH | 备用标记放在 `~/Library/Caches` 不能作为持久依据：系统可以清理缓存文件，而清理意图无法从剩余数据重新生成——缓存被清就等于丢掉清理依据 | 备用位置改为**持久目录** `~/Library/Application Support/oh-my-tab/purge-pending`（跨 dev/release 共用、在被清理集合之外、只放这一个标记、按需创建），并明确**禁止用 `~/Library/Caches` 存放该标记**；§5 反例①断言备用标记不在 Caches 下 |
| 2 | MEDIUM | 「两处标记都失败」时事务描述与测试写「尽力删除」，R6 与文案写「不执行删除」，实现无法同时满足；部分删除时界面声称「未执行」也不真实 | 统一为「**不执行任何删除**」：停写、报告未持久受理、存储保持原样、不承诺跨启动阻塞；§5 反例②断言**存储字节未变（没有部分删除）**之后再验证重启行为 |

同轮补充采纳：断言「标记检查失败」不会被误当成「标记不存在」（§5 反例⑨）。

第 9 轮：**`PLAN-STATUS: AGREED`**。评审确认第 8 轮两条发现（备用标记移至持久目录、两处都失败时语义统一）均已解决，清理事务的边界完整：启动先检查两处标记；已受理的清理完成并确认撤销两处标记后才恢复写盘；局部失败不扩大为全量删除；代际失效与清理/写入互斥能约束旧任务写回。评审未发现新的 BLOCKER / HIGH / MEDIUM，也未发现新的仓库约束冲突；同时重申**设计通过不代表实现或运行验证已经通过**，性能、容量、授权持久性与故障恢复效果仍须在交付阶段验证并附门禁结果。

第 10 轮（`PLAN-STATUS: CONCERNS`，3 条 MEDIUM；方向与五个用户决策无需重议，`ItemAddOptions::add()` 直接调用 `SecItemAdd` 的仅新增语义被确认成立）：

| # | 严重度 | 发现 | 处理 |
| --- | --- | --- | --- |
| 1 | MEDIUM | §4.8 里写的具体 API 名称/参数形态与 3.7.0 不符（`ItemAddValue::Data` 是含 `class` 与 `CFData` 的结构体变体；类别来自 `ItemClass::generic_password()`；限制是 `Limit::Max(1)`/`From<i64>`；非同步查询是 `CloudSync::MatchSyncNo`，我写的 `GenericPassword`/`Limit::One`/`CloudSync::No` 都不存在） | 逐项按 3.7.0 的 docs.rs 核对并改写 §4.8 ①②（并补一条已核实的平台事实：`ItemSearchOptions` 的 `cloud_sync` **不指定时本身就只搜非同步项**）；§11 清单同步更正 |
| 2 | MEDIUM | 非同步验证失败后的「再次新增兜底」没有执行边界：第一次创建成功后再补 `SecItemAdd` 只会撞重复项、改不了已有项；同步查询的权限/参数错误不能被当成「搜不到同步项」；真实查询验证与「headless 不碰钥匙串」需要分层 | §4.8 新增**验证分层**：生产创建一律省略 `kSecAttrSynchronizable` 且**不做任何事后补写**；A1 只断言请求构造与错误码映射；真实探针用具名命令 + 独立测试项，`CloudSync::MatchSyncYes` 反查时**只有 `errSecItemNotFound` 算「不是同步项」**，其它错误如实报告未验证；**验证失败不得触发密钥替换或重新初始化** |
| 3 | MEDIUM | §9/§11 的「全量 gate」漏了 `cargo check --release`，格式验收也没写 `cargo fmt --check` | 两处补齐：`cargo fmt --check` / `cargo check` / **`cargo check --release`**（本次改动含 `cfg(test)`/`cfg(debug_assertions)` 门控与依赖变更）/ `cargo clippy` / `cargo test`；并写明性能验证与构建门禁分开报告 |

第 11 轮：**`PLAN-STATUS: AGREED`**（上一轮 3 条中 2 条 RESOLVED、1 条 PARTIALLY RESOLVED，另有 2 处 LOW）。评审确认：仅新增创建、重复项转读、查询字段与 `limit(1)` 与 3.7.0 源码一致；非同步属性与兜底边界已按「生产创建固定省略 + 不做事后补写 + 只有 `errSecItemNotFound` 才算否定结论」闭合；门禁漏项已补齐。两处 LOW（依赖清单里残留的「显式属性兜底」、决策记录里不存在的 `CloudSync::No`）已当场改为「`item` 模块即可满足，不需要 `security-framework-sys`」与「不指定 `cloud_sync`，只搜非同步项」。评审同时重申：实现交付仍需生产路径反例测试、独立钥匙串探针、故障恢复、容量与性能验证以及完整门禁结果，**平台行为不得视为已验收**。

## 11. 实现检查清单（实现阶段逐条核对）

**不变量（§4.5）**

- [ ] R1 存在读不出来的加密数据时，绝不创建或替换密钥（创建只发生在「无密钥项且无加密历史」或用户明确清空之后）
- [ ] R2 清扫只在历史加载成功后执行——`persist.rs:266`、`persist.rs:277`、`monitor.rs:212` 三处全部受状态门控
- [ ] R3 失败路径只隔离（改名）不删除，隔离名不覆盖既有文件
- [ ] R4 `Unavailable` / `Blocked` 下 `save_history()` 整条跳过
- [ ] R5 状态变化落到日志与 `--e2e-state`
- [ ] R6 清理事务：先写意图（主位置 → 持久目录备用）→ 停写并失效在飞任务 → 删除 → 验证 → 撤销意图；两处标记；**只有删除与撤销都确认才恢复写盘**；标记只用于全量清理；两处都写不进时不删除、不宣称完成、不承诺跨启动阻塞
- [ ] `discard_history_in` 的 `let _ = std::fs::remove_file(...)` 改成逐项收集结果并上报

**平台与依赖（§4.1 / §4.8）**

- [ ] 创建走 `ItemAddOptions::new(ItemAddValue::Data { class: ItemClass::generic_password(), data })`（仅新增），**禁止** `set_generic_password`；`errSecDuplicateItem` 转读既有项
- [ ] 读取走 `ItemSearchOptions::new()` + `.class(ItemClass::generic_password())` + `.service` + `.account` + `.load_data(true)` + `.limit(1)`；**生产创建省略 `kSecAttrSynchronizable` 且不做事后补写**
- [ ] 同步属性验证分层落地：A1 断言请求构造与错误码映射（不碰真实钥匙串）；真实探针用 `cloud_sync(CloudSync::MatchSyncYes)` 反查，**只有 `errSecItemNotFound` 算「不是同步项」**，其它错误如实报告未验证；**验证失败不触发密钥替换或重新初始化**
- [ ] 错误码映射按 §4.8 ③，并各有反例
- [ ] 密钥初始化与所有钥匙串调用都在后台线程；不持历史 / IO / 缓存锁等待；初始化结果回传前复核存储代际

**数据面（§4.2 / §4.4 / §4.6）**

- [ ] 四类文件与写入中的临时文件全部加密；`object_id` 由**读取方按逻辑名重算**，不信任文件头
- [ ] 迁移提交点：所有被引用的现存缓存文件认证通过之后才发布加密索引；进入 `Ok` 时缓存里不存在该索引引用的明文文件
- [ ] 迁移替换走「临时文件 → 读回校验 → 原子 rename」，不复用预览写助手的「已存在即成功」短路
- [ ] 不可用会话不记录、进入不可用时清空本次会话条目（§4.6，2026-10-08 取代内存模式）

**测试（§5）**

- [ ] A1 的九类反例全部落地（外壳与篡改、失败路径、迁移续做、临时故障、明文预览、来源消失、坏密文旁有明文、迁移中途密钥缺失、完整密文互换、清空范围、清理事务九条、生命周期代际、内存容量、开关往返、创建语义）
- [ ] 测试/冒烟使用注入的测试密钥并与临时存储绑定，不触碰真实钥匙串
- [ ] A2：`--e2e-state` 的 `clipboard.storage` / `key_provider` + 脚本直读磁盘断言；`--clip-no-keychain` 场景结束后应用仍可用

**文档与门禁（§4.9 / §9）**

- [ ] README（加密表述 + §2 的「不防」边界）、`AGENTS.md` 不变量段、`docs/developer-notes{,-en}.md` 中英双份
- [ ] 全 locale 文案：`clipboard.unavailable_title` / `_key` / `_history` / `_purge` / `toast_image_unavailable` / 引导页加密说明
- [ ] 日志不出现密钥字节或明文
- [ ] `cargo fmt --check` / `cargo check` / `cargo check --release` / `cargo clippy` / `cargo test` + 适用的 smoke/e2e；`scripts/dev-restart.sh` 实跑并报告 build-version；A2 抢焦点场景需用户同意；性能验证与构建门禁分开报告

## 附录 A：探针源码（复现 §6）

```c
// probe.c —— 建/读一个钥匙串通用密码项；MODE=1 创建，MODE=2 读取。
#include <Security/Security.h>
#include <stdio.h>
#ifndef MODE
#define MODE 1
#endif
int main(void) {
    CFStringRef service = CFSTR("com.eacryo.oh-my-tab.keychain-probe");
    CFStringRef account = CFSTR("probe-master-key");
    if (MODE == 1) {
        unsigned char key[32]; arc4random_buf(key, sizeof(key));
        CFDataRef keyData = CFDataCreate(NULL, key, sizeof(key));
        CFMutableDictionaryRef add = CFDictionaryCreateMutable(NULL, 0,
            &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        CFDictionarySetValue(add, kSecClass, kSecClassGenericPassword);
        CFDictionarySetValue(add, kSecAttrService, service);
        CFDictionarySetValue(add, kSecAttrAccount, account);
        CFDictionarySetValue(add, kSecValueData, keyData);
        OSStatus st = SecItemAdd(add, NULL);
        printf("SecItemAdd -> %d\n", (int)st);
        return st == errSecSuccess ? 0 : 1;
    }
    CFMutableDictionaryRef get = CFDictionaryCreateMutable(NULL, 0,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFDictionarySetValue(get, kSecClass, kSecClassGenericPassword);
    CFDictionarySetValue(get, kSecAttrService, service);
    CFDictionarySetValue(get, kSecAttrAccount, account);
    CFDictionarySetValue(get, kSecReturnData, kCFBooleanTrue);
    CFTypeRef found = NULL;
    OSStatus st = SecItemCopyMatching(get, &found);
    printf("SecItemCopyMatching -> %d, bytes=%ld\n", (int)st,
           (st == errSecSuccess && found) ? (long)CFDataGetLength((CFDataRef)found) : -1L);
    return 0;
}
```

```c
// del.c —— 删除探针项（事实 3：非属主删除返回 -25244）。
#include <Security/Security.h>
#include <stdio.h>
int main(void) {
    CFMutableDictionaryRef del = CFDictionaryCreateMutable(NULL, 0,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFDictionarySetValue(del, kSecClass, kSecClassGenericPassword);
    CFDictionarySetValue(del, kSecAttrService, CFSTR("com.eacryo.oh-my-tab.keychain-probe"));
    CFDictionarySetValue(del, kSecAttrAccount, CFSTR("probe-master-key"));
    printf("SecItemDelete -> %d (0=deleted, -25300=absent, -25244=not owner)\n",
           (int)SecItemDelete(del));
    return 0;
}
```

```sh
# 事实 1/2（裸 CLI，无 bundle）：两个不同 cdhash 的二进制
clang -DMODE=1 -framework Security -framework CoreFoundation -o probe_add probe.c
clang -DMODE=2 -framework Security -framework CoreFoundation -o probe_read probe.c
codesign -s - --force probe_add; codesign -s - --force probe_read   # ad-hoc：cdhash 不同
./probe_add                                                        # -> 0
perl -e 'alarm 8; exec @ARGV' ./probe_read                         # 被阻塞，exit 142

# 事实 3（非属主删除）
clang -framework Security -framework CoreFoundation -o probe_del del.c
codesign -s - --force probe_del
perl -e 'alarm 6; exec @ARGV' ./probe_del                          # -25244

# 事实 4（.app bundle + 固定身份，模拟「重建后再读」）
# probe.c 编译进 ProbeDev.app/Contents/MacOS/probeapp，
# Info.plist 的 CFBundleIdentifier = com.eacryo.oh-my-tab.probe：
mkdir -p ProbeDev.app/Contents/MacOS
clang -DMODE=1 -framework Security -framework CoreFoundation -o ProbeDev.app/Contents/MacOS/probeapp probe.c
codesign --deep --force -s oh-my-tab-sign --identifier com.eacryo.oh-my-tab.probe ProbeDev.app
./ProbeDev.app/Contents/MacOS/probeapp                             # build1: SecItemAdd -> 0
clang -DMODE=2 -framework Security -framework CoreFoundation -o ProbeDev.app/Contents/MacOS/probeapp probe.c
codesign --deep --force -s oh-my-tab-sign --identifier com.eacryo.oh-my-tab.probe ProbeDev.app
./ProbeDev.app/Contents/MacOS/probeapp                             # build2: -> 0, bytes=32（静默）

# 事实 7（同身份、不同 identifier 的 bundle 读取）
# 另建 BundleB.app，CFBundleIdentifier = com.eacryo.oh-my-tab.dev，签名同样用 oh-my-tab-sign，
# 用 MODE=2 的可执行文件：
perl -e 'alarm 8; exec @ARGV' ./BundleB.app/Contents/MacOS/probeapp  # 被阻塞，exit 142

# 清理探针项（事实 5：系统工具可删任意项）：
/usr/bin/security delete-generic-password -s com.eacryo.oh-my-tab.keychain-probe
```

## 附录 B：调研来源

- CopyQ 文档：`copyq.readthedocs.io/en/latest/password-protection.html`（内置加密、外部密钥库、GPG 插件废弃）、`security.html`（数据存储、secret 格式识别）、FAQ「Why does encryption ask for password so often?」；issue #3345/#3443/#3367/#3446/#3529、#2414（加密 tab 删除最后一项后 `.dat` 仍留数据）。
- Ditto：issue #986（明文数据库，未关闭）、#666（改用 SQLite3MultipleCiphers）、#657/#374/#171（加密诉求与方案讨论）。
- Maccy：issue #151（请求加密，已关闭未实现；社区方案 = Keychain 随机密钥 + 透明加密）。
- Windows 剪贴板历史：`github.com/mrfa3i/clipboard-history-decrypt`（artifact 路径、CMS/DPAPI/KEK/CEK/AES-256-GCM 链路）。
- Alfred 帮助：`alfredapp.com/help/features/clipboard/`（只有忽略应用与 Concealed，无加密）。
- Raycast 手册：`manual.raycast.com/clipboard-history`（本地存储，无加密说明）。
- `security-framework` 3.7 `passwords` 模块 API：`docs.rs/security-framework/latest/security_framework/passwords/`；`item` 模块（`ItemAddOptions` / `ItemSearchOptions` / `CloudSync`）：`docs.rs/security-framework/latest/security_framework/item/`。
