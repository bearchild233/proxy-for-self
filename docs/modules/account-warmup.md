# account-warmup（账号预热）

<!-- doc-sync paths: plugins/account-warmup -->

## 职责与边界

可选的管理员插件：页面负责预热设置、账号覆盖、历史和取消；Rust 服务负责持久队列、每日计划、额度重置触发和一次性 `hi` 任务。账号预热不拥有或监控路由分组，也不绕过正常账号选择、养号保护、冷却和共享调度租约。首次执行能力由内核 `accounts.warmup.read/run` 提供，服务只获清单与注册表显式授予的能力。

## 实现逻辑

**页面**（`ui/page/`）：默认自动预热关闭，支持 Asia/Shanghai 等 IANA 时区、每日固定时间、真实五小时额度重置后延迟、0–300 秒抖动、全局模型和逐账号覆盖。全部目标与选中目标分开表达；选中最多 500 个账号，当前页面筛选不会改变“全部账号”的含义。设置用 revision CAS 保存，轮询不会覆盖未提交草稿；历史将 accepted、failed、skipped、unknown、cancelled 分开显示。

**服务队列**（`service/src/`）：SQLite 使用 WAL 与 FULL 同步，保存设置 revision、run/task、自动触发幂等键、抖动后的计划时间、执行者租约和 reset 观测。手动提交按 submissionId 回读同一批次，重复账号去重；同一账号最多一个活动任务。每日任务只补最近一次，重置任务只使用真实观测到的 resetAt，不推算五小时。插件停止后不再认领新任务，已发请求在关停预算内收尾；进程失联或超时写入 unknown，同一计划不自动重发。

**内核调用**：服务先用 `accounts.warmup.read` 读取安全目录，再用 `accounts.warmup.run` 固定发送一次 `hi`。内核只接受 OpenAI OAuth 账号和允许的模型，复用账号出口、额度保护、冷却、模型政策与调度租约；不返回凭据、Cookie、turn state 或任意提示词。`accepted` 只有在 `sendState=sent` 时成立；真实 5 小时窗口确认后才填 `shortTerm.resetAt`。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/account-warmup/plugin.json` | 页面、能力和 service 段清单 |
| `plugins/account-warmup/ui/` | 设置页、账号覆盖、历史和宿主桥调用 |
| `plugins/account-warmup/service/Cargo.toml` | 服务插件 crate 清单 |
| `plugins/account-warmup/service/src/model.rs` | 设置、账号、任务、结果和 RPC 数据结构 |
| `plugins/account-warmup/service/src/schedule.rs` | 时区、DST、抖动和触发校验 |
| `plugins/account-warmup/service/src/store.rs` | SQLite schema、队列、幂等和 lease |
| `plugins/account-warmup/service/src/service.rs` | admin.call、后台认领、内核调用与关停 |
| `plugins/account-warmup/service/src/main.rs` | 服务描述符与信号关停入口 |

## 数据、配置与接口

服务数据位于 updater 为插件准备的 `CPR_PLUGIN_DATA` 目录，主要文件为 `state.sqlite3` 及其 WAL/SHM。服务管理能力为 `account-warmup.read/configure/run/history/cancel`；账号页只声明 `read/run`。服务清单声明 `accounts.warmup.read/run/log`、内核协议最低次版本 7、64 MiB 内存上限和 30 秒关停预算。

## 改动指引

- 修改内核字段先同步 `backend/crates/kernel-protocol`、gateway-host、updater 和 Provider，再更新服务模型与测试。
- 修改调度或 SQLite 事务要保留 submission 幂等、每账号单活动任务、lease fencing、unknown 不重试和停止排空语义。
- 修改页面能力同时同步 `plugins/account-warmup/plugin.json`、`packages/plugin-sdk/policies.json` 与账号插件的显式桥能力。
- 本地验证：`node scripts/check-plugin.mjs account-warmup`、前端预热测试、`cargo test --manifest-path plugins/Cargo.toml --locked -p account-warmup`；服务插件包还需通过 `service_manifest` 的 ELF、能力和内核版本校验。
