# gateway-admin

<!-- doc-sync paths: backend/crates/gateway-admin -->

## 职责与边界

管理控制面的语义层：Command / Result 值对象、用例（service）、稳定错误分类、外部能力端口，以及三类 Admin 自有后台任务（备份、定价同步、容量冻结恢复）。不含 HTTP wire、SQL、Redis 或任何具体 Provider 实现。

- 依赖：`gateway-core`（快照控制、账号探测、Client Key 校验、账号与计量值对象、Worker 契约）。Provider 差异经 `ports/provider.rs` 的 `ProviderAdmin` 动态注册表注入。
- 调用方：`gateway-api` 的 `/api/admin/*`、`/api/auth/*`、`/api/key-usage/*`、`/api/plugins`（经 `AdminServices` 的 accessor）；组合根注入 Store 端口与运行能力。
- 管理写入统一流程见 [架构](../architecture.md) 第 7 节：校验 → 用例 → Provider 准备 → Store 事务 + 审计 → Provider 事实失效 → 发布快照。

## 实现逻辑

**初始化**（`src/lib.rs::initialize`）：校验 `AdminConfig`（用户名、会话 TTL、初始密码强度，弱口令/含 `$` 拒绝）与 `ClientConfig`；建立 `ProviderAdminRegistry`（必须含 openai 与 xai）；创建默认管理员（幂等，`ensure_default_admin`）；组装 `AdminServices`（14 个 service 句柄，API 只通过 accessor 取用）；返回 Bundle，附带 Worker 贡献。

**用例划分**（`src/use_case/`，每个文件一个 service trait + `Default*` 实现）：
- `auth.rs`：登录（管理员密码或 Client Key 两种方式）、会话恢复、改密、登出；会话主体 `SessionSubject::{Admin, Key}`；Key 会话每次恢复都重新校验 Key 存在且启用。
- `accounts.rs`：跨 Provider 账号目录（分页、筛选、排序、额度投影、批量编辑、删除、导出、连接测试、模型列表、额度预测）；按账号 Provider 动态分派到 `openai.rs` / `xai.rs`（导入、OAuth start/complete、刷新、凭据轮换等经 `ProviderAdmin` 交给 Provider 准备，再由 Store 事务提交）。
- `import_tasks.rs`：进程内后台导入任务队列（有界、管理员归属、条目状态；完成即释放原始输入），以 `account_import` Daemon 贡献给 Host。
- `account_groups.rs` `client_keys.rs` `proxies.rs` `settings.rs`：分组、Client Key（含绑定、金额重置、reveal）、命名代理（含测试与导入保护）、运行设置与管理员 API Key；写入后调用 `SnapshotControl` 发布。
- `observability.rs`：Dashboard / 用量 / 诊断 / 成本覆盖 / 健康时间线查询规则；`key_usage.rs`：Key 自助查询，范围由会话或 Bearer Key 决定。
- `backup.rs`：备份配置、手动创建、下载链接、删除；`system.rs`：版本、更新、回滚、重启、登录防护、插件请求转发（全部经 Host 提供的 `SystemOperations` 端口）；`client_distribution.rs`：Codex Desktop 下载信息。

**后台任务**：
- `src/backup/`：`policy.rs` 是纯策略（Cron 推进、保留候选）；`task.rs::BackupTask` 是单个可取消 Daemon，承担调度、执行（`pg_dump` → 暂存 → 对象存储上传）、删除收敛、保留清理；插件策略进程（`backup_policy`）可参与准入决策。
- `src/pricing_sync.rs`：定价同步 `ScheduledTask`，按定价插件策略进程给出的计划运行，复用手动同步的校验与审计。
- `src/freeze_recovery.rs`：容量熔断冻结的恢复（到期探测、失败顺延、按冻结代次提交、自适应并发下调，只降不升）。

**错误**：`model/mod.rs::AdminError{kind,message}`，`AdminErrorKind` 与 API 状态码一一映射；Provider 的 `public_message` 可安全展示，其余固定文案。

## 文件表

| 文件或目录 | 职责 |
| --- | --- |
| `src/lib.rs` | 配置、`AdminServices`、`AdminRuntimePorts`、`initialize`、Worker 注册 |
| `src/model/` | Command / Result：`accounts.rs` `account_groups.rs` `client_keys.rs` `auth.rs` `settings.rs` `system.rs` `proxies.rs` `pricing.rs` `observability.rs`（UTC 语义事实）`provider_credentials.rs`（Provider 交换用中立文档）`quota_forecast*.rs` `import_tasks.rs` `key_usage.rs` `client_distribution.rs`，`backup/` 备份状态机与错误码 |
| `src/ports/` | 外部能力端口：`store.rs`（各 Store trait 与 `AdminStorePorts`）、`provider.rs`（`ProviderAdmin`、注册表）、`backup.rs`、`proxy.rs`、`pricing.rs`、`system.rs`、`client_distribution.rs` |
| `src/use_case/` | 各 service 实现（见上） |
| `src/backup/` | 备份纯策略与 Worker |
| `src/freeze_recovery.rs` `src/pricing_sync.rs` | 冻结恢复与定价同步 Worker |

## 数据、配置与接口

- 配置段 `admin`（会话 TTL、默认管理员用户名与初始密码）与 `client`（Key 会话 TTL），见 `deploy/config.example.yaml`。
- 无自有表；全部持久化经 `ports/store.rs` 由 `gateway-store` 实现，审计与 revision 在 Store 事务里写。
- 对 API 暴露 `AdminServices`；对组合根暴露 `AdminRuntimePorts`（定价/备份策略、价目源、Provider 适配、快照控制、探测、系统操作）。

## 改动指引

- 新增管理接口：先在 `model/` 定义 Command/Result，再在对应 service 加方法，Store 端口加方法并由 `gateway-store` 实现，最后 `gateway-api` 加路由与 wire；同步 [管理接口文档](../api.md)。
- 会改变路由快照或安全配置的写入，必须在同一 Store 事务里推进 `config_revision` 并写脱敏审计，然后调用 `SnapshotControl` 发布；运行时观测（额度、冷却、目录 generation）不推进 revision。
- 改共享账号行解析字段时，核对 `gateway-store` 所有 SELECT 调用方（含分组成员）。
- Provider 相关差异放进 `ProviderAdmin`，不要在这里 `match` 具体 Provider。
- 测试在 `tests/`，镜像 `src/` 目录；Store 端口用内存替身。
