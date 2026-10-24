# 开发实例归属（Development instance ownership）

`scripts/dev-restart.sh --whose-instance` 回答一个问题：**此刻正在运行的 Oh-My-Tab 实例，
是不是本 worktree 构建的那个？** 本文说明它为什么存在、承诺什么、不承诺什么，以及多 Agent
同时开发时应当怎样使用它。

## 1. 问题：锁覆盖事务，不覆盖窗口

`scripts/lib/dev-lock.sh` 提供的是**事务互斥**：同一时刻只有一个「停旧实例 → 构建 → 测试 →
启动 → 验证」事务在跑，跨 worktree 生效。这个锁在脚本返回时释放。

因此下面这段时序是**完全合法**的，锁不会、也不应该阻止它：

| 时刻 | Worktree A | Worktree B |
| --- | --- | --- |
| t0 | `dev-restart.sh` 跑完，A 的构建在运行，锁已释放 | — |
| t1 | Agent A 准备去验证 | `dev-restart.sh` 取锁，`bootout` 掉 A 的实例 |
| t2 | — | B 构建、启动、验证通过，释放锁 |
| t3 | **Agent A 看到的窗口是 B 的构建** | — |

在 t3，两个 Agent 都会认为「我验证了自己的构建」。A 的脚本在 t0 打印的 `restart ok` 是真的，
但它描述的是 t0 的状态；t1 之后它就不再成立，而没有任何东西会告诉 A。

这不是锁的缺陷。把锁延长到窗口关闭会让「留一个窗口给人看」的默认交互独占应用身份，违背
「锁只覆盖事务」的既定设计，也会让 `dev-manually-run.sh` 式的日常使用在每次用完脚本后
把整个仓库锁住。所以这里的解法不是加锁，而是**让归属可断言**。

## 2. 契约

```sh
scripts/dev-restart.sh --whose-instance
```

这条命令**不构建、不启动、不停止任何东西，也不取锁**。它只读：

- 进程表（`pgrep -x oh-my-tab` 后逐个用 `ps -o lstart=,comm=` 核对）；
- app 日志里的 `[launch]` 记录（`~/Library/Logs/oh-my-tab/oh-my-tab.log`）；
- 本 worktree 的 `dist/Oh-My-Tab-Dev.app/Contents/Info.plist`。

### 退出码

| 码 | VERDICT | 含义 | 调用者该做什么 |
| --- | --- | --- | --- |
| 0 | `MINE` | 运行中的实例来自本 worktree，且没有竞争实例 | 可以继续，但结论要绑定 `pid` + `started` |
| 1 | `NOT_MINE` | 运行中的 dev 构建来自别的 worktree（或本树的旧构建，见 `STALE_BUILD`） | **此时任何 UI 结论都无效** |
| 2 | — | 参数错误（`--whose-instance` 与 `--check`/app 参数同时给出等） | 修参数；不要重试 |
| 3 | `NO_INSTANCE` | 没有任何 Oh-My-Tab 进程 | 先启动 |
| 4 | `UNVERIFIED` | 有实例，但身份无法建立（日志缺失、记录不可解析、记录早于进程、枚举失败……） | **不是通过**。按「不知道」处理 |
| 5 | `CONTESTED` | 本树的构建在跑，但另有实例共享快捷键，可能抢走输入 | 结论可能描述的是另一个实例 |
| 6 | `NO_LOCAL_BUILD` | 本 worktree 没有可比的产物 | 先构建 |

`STALE_BUILD` 不是独立的退出码：它意味着「运行中的构建是本树做过的一个旧版本」，与
`NOT_MINE` 同属「不是这一次的构建」，因此同为退出码 1，由输出里的 `VERDICT: STALE_BUILD`
区分。

**4 和 6 都不是 0。** AGENTS.md 的原则是「无法判定的 gate 不是通过的 gate」，这条命令遵
循同一条规则。

### 输出示例

```
$ scripts/dev-restart.sh --whose-instance
instance: pid 41234, started Wed Oct 22 15:12:03 2026
binary:   /Users/ceres/RustroverProjects/oh-my-tab-worktree_one/dist/Oh-My-Tab-Dev.app/Contents/MacOS/oh-my-tab
this worktree's build version: 20261022150411
reported version: 20261022151203
VERDICT: NOT_MINE
reason: the running dev build does not come from /Users/.../oh-my-tab/dist/Oh-My-Tab-Dev.app/Contents/MacOS/oh-my-tab
```

`binary:` 是**正在运行**的实例的可执行文件路径（这里是 `oh-my-tab-worktree_one`），`reason:` 里的路径是**本 worktree**
应该产出的那个（`oh-my-tab`）；两者不同正是判成 `NOT_MINE` 的依据。

`MINE` 的输出额外带一条说明，提示这是**快照**，并给出复检方法。

## 3. 承诺与不承诺

### 承诺

- **某一瞬间**的归属：那个 pid 此刻执行的二进制，等于本 worktree 的产物。
- **可复检的身份**：`pid` + `started`（进程启动时间）构成身份，而不是路径。同一路径可以先后
  运行不同构建，pid 会被复用，只有「这个 pid + 这个启动时间」唯一。
- **拒绝不确定**：日志读不到、记录解析不了、记录早于进程启动——一律 `UNVERIFIED`。

### 不承诺

- **不承诺窗口属于该进程。** 这里判定的是进程身份，不是窗口归属。要建立窗口与进程的对应，
  需要记录目标窗口（例如通过 `Computer Use` 的窗口快照），本文档不假装它能给出这个答案。
- **不承诺验证期间不被替换。** `MINE` 是查询那一刻的结论。要证明「我观察期间它没被换掉」，
  必须在观察**之后**用复检（第 4 节），并且接受一个事实：如果它在中途被换掉又换回来，
  仅凭首尾两次查询无法发现。这需要持续协调（未来可能的 hold-lock 会话），不是一条查询能做
  到的。
- **不承诺运行的是最新构建。** 只证明「是本 worktree 的产物、且版本等于本 worktree 当前
  产物里记录的 `CFBundleVersion`」。版本号来自构建时间戳（`date -u +%Y%m%d%H%M%S`），
  跨 worktree 的区分实际由**可执行路径**完成，版本号只用来区分本树的旧构建。
- **不承诺 app 的配置、剪贴板、缓存状态。** `--check` 的文档里同样的限制在这里继续适用。

## 4. 多 Agent 的标准用法

### 4.1 验证前：确认归属

```sh
scripts/dev-restart.sh --whose-instance || {
    echo "not this worktree's build; do not verify UI"
}
```

### 4.2 验证中：绑定身份

把 `pid` 和 `started` 记下来，**每条 UI 结论都标注它属于哪个身份**。

### 4.3 验证后：复检

```sh
scripts/dev-restart.sh --identity-expects=<pid>[,<started>]
```

退出码与 `--whose-instance` 相同。若返回 1（`NOT_MINE`），说明：
- 那个 pid 已经不在了（实例被替换或退出），**这一轮 UI 结论作废**；
- 或那个 pid 还在，但跑的是别的构建。

### 4.4 竞争实例

`CONTESTED`（退出码 5）的含义是：本树的构建确实在跑，但另有实例共享全局快捷键。这种情况下
**你操作的是哪个实例并不确定**——可能是你的，也可能不是。先清掉竞争实例（按身份，逐个 pid，
见 AGENTS.md 的 process hygiene 一节）再验证。

### 4.5 与锁的关系

`--whose-instance` **故意不取锁**。原因：

1. 它要检测的正是「另一个事务正在替换实例」这件事；取锁会让它排队等那个事务完成，从而在
   完成后才回答，恰好错过要检测的窗口。
2. 它不改变任何状态，互斥没有意义。
3. 一条只读诊断在锁被占用时也必须能跑——那正是最需要诊断的时候。

`--check` 和默认模式**照常取锁**，行为不变：它们是真的会构建和启停实例的事务。

## 5. 归属是怎样判定的

判定分三步，每一步失败都会落到非 0：

1. **枚举候选**：`pgrep -x oh-my-tab`，逐个读 `ps -o lstart=,comm=`。
   `comm` 是内核记录的「这个进程在执行什么」，argv 可以撒谎，`comm` 不行。
   `lstart` 与 `comm` 在同一次 `ps` 调用里读，避免 PID 复用时把 A 的启动时间配到 B 的路径上。
2. **分类**：按 `comm` 的规范化路径与 bundle id 分成
   `mine` / `other-dev` / `packaged` / `unbundled` / `unknown`。路径经过符号链接解析
   （`cd -P` + `pwd`），`/var` 与 `/private/var` 这类拼写差异不会造成误判。
3. **归属记录**：读日志里**最后一条** `[launch] pid=<pid>` 行，要求三条独立证据同时成立：
   - 记录的 ISO-8601 时间戳（本地时间 + UTC 偏移 + 毫秒）**不早于**该进程的启动时间，
     **没有任何宽限**。app 在启动之后才写这条记录，所以早于进程的记录只可能属于用过同一
     PID 的旧进程。两边都截断到整秒，且截断方向一致，因此「确实写在启动之后」的记录不会
     比启动时间更早；同一秒内则视为可归属。
   - 记录里的 `executable` 字段**存在且非空**。编码器在无法解析自身路径时会写占位符 `-`，
     这种情况算作没有证据，而不是「跳过检查」。
   - 解码后的 `executable` 等于该 pid 当前的可执行路径。

   任一条件不满足就是 `UNVERIFIED`，不会因为「记录存在」就报 `MINE`。

4. **返回前复核**：读取 plist 与扫描日志期间进程可能已经退出或被替换，所以在输出
   `MINE` 之前**再读一次**该 pid 的 `lstart` 与 `comm`，与枚举时的值比对。变化即返回
   `UNVERIFIED`，因为此时结论描述的是一个已经不存在的进程。

5. **不可读的候选**：`pgrep` 列出了进程但 `ps` 读不回来时，这**不是**「没有进程」。这种情况
   下不会输出 `MINE`——一个读不回来的进程可能就是竞争者，所以结论是 `UNVERIFIED`。

### 为什么时间戳要按偏移解析

`src/logger.rs` 写的是**本地时间 + UTC 偏移 + 毫秒**，例如
`2026-10-22T15:24:47.271+08:00`；`ps -o lstart=` 写的是本地时间，不带偏移。把两者当字符串
比较，在夏令时切换或日志被复制到别的时区时会悄悄排错顺序。所以两边都换算成 epoch 再比：
日志一侧按它自己的偏移换（`date -j -f "%Y-%m-%dT%H:%M:%S%z"`），`ps` 一侧按本机时区换。

`+08:00` 与 `Z` 两种拼写同一个瞬间必须得到同一个 epoch——这条有断言，见
`scripts/dev-instance-ownership-selftest.sh`。

## 6. 测试

```sh
scripts/dev-instance-ownership-selftest.sh
```

离线自测，不构建、不启动真实 app、不取真实锁。用例数用 `scripts/dev-instance-ownership-selftest.sh | grep -c '^ok '` 现场数（当前为 30），覆盖：

- **纯函数**：时间戳解析（偏移、毫秒可选、无法解析必须失败）、记录归属（早 20 秒与早 1 秒
  的记录都 = 过期、与启动同秒 = 可归属、晚于进程 = 可用、无记录 = 无答案、`executable` 为
  空或为 `-` = 无证据、日志不可读 = 拒绝而不是「搜过了没找到」）。
- **`ps` 字段切分**：个位数日期的填充空格（`Thu Oct  1 …`）与含空格的路径必须同时正确，
  否则会把年份切进路径、把本树实例误判成别的树。
- **真实进程**：启动一个私有进程名的合成实例（`/bin/sleep` 副本，**重新 ad-hoc 签名**——在
  Apple silicon 上拷贝已签名二进制会因 CodeDirectory 不再匹配被内核杀掉，实测 `Killed: 9`），
  断言 `MINE` / `NOT_MINE` / `STALE_BUILD` / `UNVERIFIED` / `NO_INSTANCE` / `NO_LOCAL_BUILD`
  与退出码，以及复检在进程消失后返回 `NOT_MINE`。
- **枚举不完整**：一部分候选读不回来、全部读不回来、枚举直接失败，三种情况都必须是
  `UNVERIFIED`，不能落成 `MINE` 或 `NO_INSTANCE`。
- **反例**：同一个候选，真实分类器判 `other-dev`、而「永远 mine」的 shim 判 `mine`——断言要求
  两者不一致；并且 shim 走完整 probe 也不能产出 `MINE`。库不得定义取锁 API。

私有进程名（`OMT_OWNERSHIP_PROCESS_NAME`）只是为了让自测与机器上真实运行的实例隔离；生产
路径不设置它，默认名字就是 `oh-my-tab`。

## 7. 与其它选项的关系

| 选项 | 会构建 | 会启停实例 | 取锁 | 用途 |
| --- | --- | --- | --- | --- |
| （默认） | 是 | 是，且留下窗口 | 是 | 交互开发 |
| `--check` | 是 | 是，结束即停 | 是 | 自动化验证 |
| `--whose-instance` | 否 | 否 | **否** | 归属断言（快照） |
| `--identity-expects` | 否 | 否 | **否** | 归属复检 |

`--whose-instance` / `--identity-expects` 与 `--check` 或任何 app 参数同时给出会被拒绝
（退出码 2）：一个事务与一个查询是两个问题，混在一次运行里会让其中一个名不副实。

## 8. 已知限制与后续

- **验证期间无保护**：这是本设计的核心取舍。真正的「验证期间窗口不被替换」需要一个 hold-lock
  会话（PeachPic 仓库的 `dev-agent-cua.sh` 是这种形态：构建、启动、保持持锁、由调用者写
  done 文件后才释放），代价是忘关的会话会阻塞其他 worktree，需要 ready/done 协议与持有超时。
  在真的需要长时间 UI 会话之前，`--whose-instance` + `--identity-expects` 给的是「事后可发现」，
  不是「事前可阻止」。
- **首尾查询之间有盲区**：若实例在观察期间被替换又换回原构建，首尾两次查询都会通过。
  要关闭这个盲区只能靠持续信号（例如 app 每次启动写一个单调递增的 run-id，观察期间轮询），
  目前没有做。
- **`FSEvents`/日志轮转**：日志被轮转或截断时，`[launch]` 记录可能不可得，结论是 `UNVERIFIED`。
  这是有意为之——读不到就说不确定，不说「大概是我的」。

## 9. 相关文档

- `AGENTS.md` — *Build, run, and verification* 一节（锁边界、验证前断言的要求）、
  *Process hygiene for runtime verification*（按身份停实例）。
- `scripts/lib/dev-lock.sh` — 事务互斥的实现，以及「锁覆盖事务、不覆盖窗口」的原始说明。
- `scripts/lib/dev-instance-ownership.sh` — 本命令的实现，文件头有完整的契约表。
- `src/logger.rs` — `[launch]` 记录与时间戳格式的源头。
