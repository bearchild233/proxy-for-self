# account-warmup（账号预热）

<!-- doc-sync paths: plugins/account-warmup -->

## 职责与边界

可选的管理员插件：页面只负责自动化设置、账号覆盖、自动历史和取消；账号管理页负责状态及单账号/全部/选中手动执行。Rust 服务负责持久队列、每日计划、额度重置触发和一次文本任务。账号预热不拥有或监控路由分组，也不绕过正常账号选择、养号保护、冷却和共享调度租约。执行能力由内核 `accounts.warmup.read/run` 提供，服务只获清单与注册表显式授予的能力。

## 实现逻辑

**页面**（`ui/page/`）：默认自动预热关闭，支持 Asia/Shanghai 等 IANA 时区、每日固定时间、真实五小时额度重置后延迟、0–300 秒抖动、全局提示词/模型/推理强度和逐账号时间/模型/强度/延迟覆盖。说明统一放在控件后方，模型三列下拉占满列宽、沿同一基线排列；页面和侧栏以 FlameKindling 标识。设置用 revision CAS 保存，轮询不会覆盖未提交草稿；历史在服务查询 LIMIT 前按自动批次来源过滤，下次任务使用 `nextAutomaticRunAt`。明细必须属于当前自动批次，其中合并复用的既有手动任务仍完整展示；独立手动批次不在本页展示。

**范围与批量管理**：`WarmupTargets.vue` 支持全部、指定账号、指定分组三种目标；`accountIds`/`groupIds` 各最多 500 项，切换模式保留选择。分组 ID 持久化，执行按真实当前成员取并集并去重，成员加入/退出自动跟随；分组路由开关不取代账号资格。组名通过独立只读管理目录分页读取，错误不覆盖已有选择。账号勾选跨页保留，支持选本页/筛选结果/分组成员；指定账号模式勾选定义目标，全部/分组模式仅定义本次批量调整。`WarmupBatchModal.vue` 的时间/模型/强度/延迟/自动参与只应用显式字段，跟随全局写 null，保持不变保留原值；写入草稿后统一保存，超过独立配置 500 项时原子拒绝。分组批量设置只应用于当前成员，出组账号不修改，新成员继承全局默认。`groupTargetsSupported` 未确认时禁用分组计划，旧服务缺 `groupIds` 时保留草稿并提示升级。

**服务队列**（`service/src/`）：SQLite 使用 WAL 与 FULL 同步，保存设置 revision、run/task、自动触发幂等键、抖动后的计划时间、执行者租约和 reset 观测。prompt/model/reasoningEffort 在入队冻结；手动 run 的可选三项只覆盖本次快照，不写自动设置。按 submissionId 回读同一批次，重复账号去重；同一账号最多一个活动任务，已有活动任务合并时沿用原参数。每日任务只补最近一次，重置任务只使用真实观测到的 resetAt，不推算五小时。插件停止后不再认领新任务，已发请求在关停预算内收尾；进程失联或超时写入 unknown，同一计划不自动重发。

分组目标的计划发现沿用 30 秒目录缓存；只有到期纯自动任务可认领、发送间隔允许且无在途任务时才强刷可信完整目录。出组未发任务标记 `group_membership_changed`；目录不可用时保留 queued，继续寻找显式手动/有手动引用的任务，不能误发或队头阻塞。有效手动引用继续使用原任务参数，并豁免原自动过期时间。read 的成员、能力和预测必须使用同一目录快照；设置切换期间没有对应可信完整目录时标记未就绪，不产生分组预测。

**账号状态**：read 返回 `accountStates`，按账号索引查询活动/最近任务摘要，不读取或返回任务提示词。queued/running 优先；当前有效的真实 5 小时窗口为 warmed，业务请求已启动的窗口也有效，不表示某次 unknown 请求成功。其它结果区分 pending/unconfirmed/unknown/failed/skipped/unsupported，最新窗口到期后不复活更老观测。老版本已经结束但没有完整观测记录的任务可保守显示计时待确认。

**内核调用**：服务先用 `accounts.warmup.read` 读取安全目录，再用 `accounts.warmup.run` 发送一次文本。目录包含真实分组 ID，Core 按 Provider 返回账号 ID 合并内部 `PluginAccounts` 目录，缺失/冲突/失败报不可用，成功才声明 `groupTargetsSupported=true`；不借用 ownership/watch 或普通账号目录的插件分组授权。prompt 默认 `hi`，原文非空、最多 4096 UTF-8 字节，允许 LF/CR/tab、拒绝其它控制符；强度默认 `auto`，显式强度必须得到模型目录支持，否则发送前跳过。目录 `reasoningEfforts` 原样透传到页面。内核只接受 OpenAI OAuth 账号和允许的模型，复用账号出口、额度保护、冷却、模型政策与调度租约；不返回凭据、Cookie 或 turn state。`accepted` 只有在 `sendState=sent` 时成立；真实 5 小时窗口确认后才填 `shortTerm.resetAt`。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/account-warmup/plugin.json` | 页面、能力和 service 段清单 |
| `plugins/account-warmup/ui/` | 设置页、账号覆盖、历史和宿主桥调用 |
| `plugins/account-warmup/ui/page/components/WarmupTargets.vue` | 目标模式、账号/分组多选、跨页勾选和批量草稿 |
| `plugins/account-warmup/ui/page/components/WarmupBatchModal.vue` | 显式字段的批量覆盖与继承 |
| `plugins/account-warmup/ui/page/useWarmupGroups.ts` | 分组元数据分页、异步竞态和失败保留 |
| `plugins/account-warmup/service/Cargo.toml` | 服务插件 crate 清单 |
| `plugins/account-warmup/service/src/model.rs` | 设置、账号、任务、结果和 RPC 数据结构 |
| `plugins/account-warmup/service/src/schedule.rs` | 时区、DST、抖动和触发校验 |
| `plugins/account-warmup/service/src/store.rs` | SQLite schema、队列、幂等和 lease |
| `plugins/account-warmup/service/src/service.rs` | admin.call、后台认领、内核调用与关停 |
| `plugins/account-warmup/service/src/main.rs` | 服务描述符与信号关停入口 |

## 数据、配置与接口

服务数据位于 updater 为插件准备的 `CPR_PLUGIN_DATA` 目录，主要文件为 `state.sqlite3` 及其 WAL/SHM。服务管理能力为 `account-warmup.read/configure/run/history/cancel`；账号页只声明 `read/run`。界面额外声明只读 `account-groups.get.admin.account-groups`，用于显示组名。history 可传 `source: automatic|manual`，省略则保持全部来源。0.2.1 服务清单声明 `accounts.warmup.read/run/log`、内核协议最低次版本 8、64 MiB 内存上限和 30 秒关停预算；分组模式必须同时有 `groupTargetsSupported=true`。旧 Settings/TaskItem JSON 缺请求字段时默认 `hi/auto`，缺 groupIds 时为空；写入新设置后，旧 0.1/0.2.0 服务不支持直接读取，恢复需配套备份配置及 SQLite。

## 改动指引

- 修改内核字段先同步 `backend/crates/kernel-protocol`、gateway-host、updater 和 Provider，再更新服务模型与测试。
- 修改调度或 SQLite 事务要保留 submission 幂等、每账号单活动任务、lease fencing、unknown 不重试和停止排空语义。
- 修改页面能力同时同步 `plugins/account-warmup/plugin.json`、`packages/plugin-sdk/policies.json` 与账号插件的显式桥能力。
- 本地验证：`node scripts/check-plugin.mjs account-warmup`、前端预热测试、`cargo test --manifest-path plugins/Cargo.toml --locked -p account-warmup`；服务插件包还需通过 `service_manifest` 的 ELF、能力和内核版本校验。
