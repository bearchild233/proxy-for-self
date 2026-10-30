# gateway-core

<!-- doc-sync paths: backend/crates/gateway-core -->

## 职责与边界

数据面核心：协议无关的 operation、canonical event、运行时快照、路由计划、账号选择、准入与限额、attempt 协调、提交边界、计量。不依赖 HTTP、数据库、Redis 或任何具体 Provider；持久化与上游能力全部以 port（trait）形式声明，由 Store / Provider 实现、组合根注入。

- 依赖：只有通用库与 `kernel-protocol`（插件钩子类型）。
- 调用方：`gateway-api`（通过 `ExecutionService`）、`gateway-admin`（`SnapshotControl`、`AccountProbe`、`ClientKeyVerifier`、账号与计量值对象）、两个 Provider（实现 `Provider`，使用账号/路由/事件/错误类型）、`gateway-store`（实现各 port）、`gateway-host`（实现 `ConnectionLifecycle`、Worker 监督）。
- 总体分层与不变量见 [架构](../architecture.md) 第 3.2、4、6 节，这里只写定位信息。

## 实现逻辑

**账号养号准入**：`account/quota_protection.rs` 的 `AccountQuotaProtection` 保存一个总开关、5小时/7天独立上限和其它窗口legacy上限，均为1–99整数，默认关闭/90。旧对象两项窗口阈值继承legacy；新字段必须成对提供且拒绝null。`AccountQuotaProtectionUsage` 分开承载短期、周及其它有效使用率，向下取整防止临界值提前触发；`reached_usage` 统一用于选号、探测和展示，已分类用量不重复比较legacy。临时 `QuotaExhausted` 阻止新请求，不回写真实额度；停用和认证错误仍优先于额度，额度优先于冷却。固定账号、原生续接与插件选定账号保持既有发送和安全恢复边界。策略关闭或有效窗口重置后按新事实恢复。

**启动**（`src/lib.rs::initialize`）：先按配置恢复 Client Key 准入热状态（`restore_client_admission_startup`，从 `model_requests` 重建 Redis 并发/RPM 计数），再用 `RuntimeSnapshotCompiler` 编译首个 `RuntimeSnapshot`，失败直接拒绝启动；组装 `DefaultExecutionService`，并返回 `CoreBundle`（执行服务、Key 校验器、快照控制、账号探测、健康探针、周期对账 Worker 贡献）。

**一次推理请求**（`src/engine/`）：
1. `execution.rs::DefaultExecutionService::authenticate`：用冻结的快照校验 Client Key（常量时间比较），产出 `AuthenticatedClient`（Key 策略、冻结账号范围、模型映射、计价覆盖、Codex 最低版本）。
2. `start*`：获取 Key 准入（`admission.rs` 的 `ClientAdmissionPort`：RPM 与并发在 Redis 原子判定，金额阈值用 `budget.rs` 的已结算账本判断），`route_context` 编译 `RoutingPlan`（`routing/`：模型 → Provider 候选顺序，账号范围与能力过滤），处理 native continuation（`continuation.rs`：previous-response 固定创建它的 Provider 与账号）、Provider circuit 判定。
3. `coordinator.rs::AttemptCoordinator`：账号重试、发送与下游提交的唯一 owner。每个 attempt 调 `Provider::execute`（一次只选一个 credential，返回冷流）；`not_sent` / `sent` / `ambiguous` 单调推进；`commit_downstream` 之后禁止换号、重试和 fallback；Provider 声明的有界同账号退避、并发等待（`concurrency.rs`）、插件 attempt 预算（`plugin::PluginAttemptBudget`，每请求最多 4 次）都在这里编排。
4. 终态：`ResponseObservation`（`observation.rs`）统一累计用量、费用、时间和响应 ID，重试丢弃的 attempt 不污染最终计量；会话持有的清理 future 负责 `finalize_model_request`、费用幂等结算（`ClientBudgetPort`）和准入释放，被取消后由 detached finalizer 续跑，不重复结算。
5. `probe.rs`：管理端连接测试复用同一执行链（`AccountProbe`），结果带 `gateway/provider/upstream` 来源与发送状态。

**快照与路由**：`routing/snapshot.rs` 把设置、Client Key 策略、账号分组、Provider 账号事实、价格覆盖、全局请求位置、客户端画像编译成不可变 `RuntimeSnapshot`；`runtime/mod.rs` 的 `RuntimeSnapshotPublisher` 负责原子发布（管理提交、Redis 变更通知、周期对账共享一个临界区）、暂停（配置不可确认时新请求 fail closed）和 revision 订阅。目录 generation 变化但编译失败时继续用旧快照。请求读取不等待刷新。

**账号选择**：`account/scope.rs` 的 `FrozenAccountScope` / `RuntimeAccountDirectory` 在认证时冻结 Key 的账号范围（无分组 = 全部账号；有分组 = 启用分组并集；绑定分组为空则空池）；`account/selection.rs` 实现 `RotationStrategy`（`smart`、`quota_reset_priority`、`round_robin`、`sticky`）、权重、请求间隔、会话亲和、失败反馈统计；`account/model_access.rs` 持有管理员模型政策；`account/lifecycle.rs` 的 `effective_weight` 实现临期优先的有效权重。

**选号算法细节**（`src/account/selection.rs::AccountSelectionPolicy::select`）：1) 过滤——只留 `scheduling_blocker` 为空或仅“请求间隔未到”的候选（间隔由原子租约端口等待，不能成为降权或逃离亲和账号的依据）；2) 亲和——若有 `preferred_account`：被阻断则记录阻断原因，权重低于最高权重且未设置 `preferred_account_overrides_weight` 则记为 `LowerWeight`，否则直接命中；3) 在最高有效权重（`effective_weight`，含临期优先）的候选里按策略挑一个：`quota_reset_priority` 先按额度重置时间（无时间的排最后），再按容量占用、最近开始时间、账号 ID；`round_robin` 按账号 ID 排序后用游标取模；`sticky` 取最近开始请求的账号；`smart` 对每个候选打分 `1.0×空闲度 + 0.8×剩余额度 + 1.0×(1−错误率) + 0.5×延迟分`（延迟分 = 10000/(10000+首个有效输出延迟 ms)），取与最高分相差不超过 0.05 的候选，按账号 ID 排序后用游标轮转。错误率与延迟来自进程内按 Provider+账号隔离的 EWMA 反馈（`AccountFeedbackStats`，错误率 15 分钟半衰期），只含真实发送过的 attempt。`wait_candidates` 只把“本地并发 / 调度间隔”视为可等待容量，账号权限、失效、额度和上游冷却立即排除。

**快照编译与发布细节**（`src/routing/snapshot.rs`、`src/runtime/mod.rs`）：`RuntimeSnapshotCompiler::compile` 读取 Store 的一致性事实（设置、分组、Key 策略、账号、价格覆盖、画像、请求位置），向各 Provider 查询模型目录并要求 generation 稳定（最多重试 4 次，否则放弃本次编译），生成 `RuntimeSnapshot`（公开模型与画像、整对象模型映射、每个 Key 的 `ClientPolicy` 与 `FrozenAccountScope`、Codex 最低版本、`inference_limits`）。`RuntimeSnapshotPublisher` 每 5 秒对账一次（失败从 1 秒指数退避到 30 秒），Redis 变更通知只缩短收敛延迟；对账允许持久 revision 回退（不能只比较大小）；`plan` / `plan_provider_endpoint` 在请求线程内根据快照生成 `RoutingPlan`，不访问外部。

**插件**（`src/plugin/`）：`PluginHooks`（五个钩子 + `ownership`）与 `PluginCapabilities`/`PluginAccounts`/`PluginUpstream` 端口；`capabilities.rs::KernelCapabilities` 做能力分发前的参数校验与按账号授权（`require_account` 与 `accounts.read` 过滤都走 `PluginAccess::allows`；`groups.move` 的增删分组仍须在插件声明的分组里）。`accounts.warmup.read/run` 是只给 `account-warmup` 身份的窄维护能力：读取安全账号目录，单账号执行有界文本请求，复用正常资格、额度保护与调度租约，不借用 ownership/watchedGroups。预热目录按 Provider 返回账号 ID 批量查询既有 `PluginAccounts` 端口，精确合并真实分组；缺行、冲突或读取失败报不可用，成功才置 `groupTargetsSupported=true`。每个异步读取后重新校验专属身份与能力，不放宽普通 `accounts.read` 的分组授权。监控规则 `watch_matches`：`watchedGroups` 里 `*` 表示所有不归任何插件所有的账号，`!分组` 表示带该分组的账号不监控（优先于其余条目），其余是具体分组；`PluginHooks::watchers(账号分组)` 按它汇总监控者（默认实现只按逐个分组的 `Watched` 汇总）。实现在 `gateway-host`（连接）与 Provider（路由桥）。

**后台任务契约**（`src/task/mod.rs`）：`WorkerKind` / `WorkerId` / `WorkerSchedule` / `WorkerRunnable`（Scheduled 周期任务或 Daemon）/ leader lease 与 fencing token；各 crate 返回 `WorkerContribution`，由 Host 的 `WorkerSupervisor` 运行。

## 文件表

| 文件或目录 | 职责 |
| --- | --- |
| `src/lib.rs` | `CoreStorePorts`、`CoreBundle`、`initialize`、`CoreError` |
| `src/engine/` | 请求生命周期：`mod.rs`（`ModelRequestId`、`ExecutionStore` 端口、attempt 值对象）、`execution.rs`（执行服务）、`coordinator.rs`（重试/提交边界）、`admission.rs` `budget.rs`（Key 准入与金额端口）、`continuation.rs`、`observation.rs`、`probe.rs`、`provider.rs`（`Provider` trait、`ProviderRegistry`、事件流） |
| `src/routing/` | `mod.rs`（模型 ID、`RoutingPlan`、`ProviderCandidate`）、`catalog.rs`（Provider 目录合同）、`snapshot.rs`（快照事实、编译、发布） |
| `src/runtime/mod.rs` | 快照句柄、发布器、revision 订阅与对账判定 |
| `src/account/` | 账号值对象与端口：`model.rs`（账号、并发、权重、凭据状态）、`scope.rs`、`selection.rs`、`quota_protection.rs`（养号准入政策）、`model_access.rs`、`lifecycle.rs`、`proxy.rs`（出口代理值对象）、`location.rs`、`store.rs` |
| `src/policy/` | Client Key 策略与限额（`mod.rs`）、Codex 最低版本（`client_version.rs`）、入口容量（`inference_limits.rs`） |
| `src/provider_ports/mod.rs` | Provider 运行时需要的中立存储端口：调度状态、租约、冷却、会话亲和、circuit、凭据状态缓存、运行策略、账号选择政策、分组移动等 |
| `src/event.rs` `src/operation/` `src/error.rs` `src/upstream.rs` | canonical event、operation 与能力要求、稳定错误分类、发送状态 |
| `src/metering/` | 十进制金额、币种、用量与费用估算、`pricing.rs` 价格覆盖合并 |
| `src/concurrency.rs` | 中立有界 FIFO 等待位置 |
| `src/diagnostics/` | 有界请求诊断：trace、流分帧、选号证据、统一筛选 |
| `src/plugin/` | 插件钩子与能力端口 |
| `src/task/mod.rs` `src/health.rs` `src/lifecycle.rs` | Worker 契约、健康探针、取消令牌与连接 drain |
| `src/validation.rs` `src/identity.rs` | 纯值校验、`ProviderKind` |

## 数据、配置与接口

无自有存储与配置文件。对外合同：`ExecutionService`（认证、模型目录、启动执行）、`ClientKeyVerifier`、`SnapshotControl`、`AccountProbe`、各 port trait（见 `src/provider_ports/mod.rs`、`src/engine/mod.rs`、`src/account/store.rs`）。运行时参数（轮转策略、并发默认值、全局请求位置、Fast 限制等）来自快照，由 Admin 写入、Store 读取，不在 Core 里配置。

## 改动指引

- 新增 port：在 Core 定义 trait，Store / Provider 实现，`backend/apps/gateway/src/bootstrap.rs` 注入；Core 不得引用具体实现。
- 改发送/提交语义、重试顺序、费用结算：只改 `coordinator.rs` / `execution.rs`，并核对 [架构](../architecture.md) 第 4、6 节不变量；测试在 `tests/engine/`。
- 新增快照字段：同步 `SnapshotSettingsFacts` 等事实类型、`backend/crates/gateway-store/src/postgres/snapshot.rs` 读取、`gateway-admin` 的发布路径；快照编译失败的降级行为不要改。
- 账号选择算法改动影响所有 Provider（`backend/crates/providers/openai/src/credential/selector.rs`、`backend/crates/providers/xai/src/credential/selector.rs`）；先跑 `tests/account/`。
- Core 内部依赖方向由 `backend/apps/gateway/tests/architecture.rs` 冻结（`validation` → `identity` → `account`/`metering`/`upstream` → `event` → `engine`/`routing`），新增模块要放对 owner。
