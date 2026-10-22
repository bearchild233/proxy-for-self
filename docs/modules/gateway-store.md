# gateway-store

<!-- doc-sync paths: backend/crates/gateway-store backend/migrations -->

## 职责与边界

PostgreSQL、Redis、S3/R2 与 `pg_dump` 的适配层：把 Core / Admin / Provider 声明的 port 实现为具体存储。不拥有业务策略；业务规则、校验和流程归 `gateway-core`、`gateway-admin` 与各 Provider。

- 依赖：`gateway-core`（port 与值对象）、`gateway-admin`（Admin 端口与模型）、`gateway-protocol`（用量/限流文档解析）。
- 调用方：只有组合根 `backend/apps/gateway/src/bootstrap.rs`（`gateway_store::initialize`），其他 crate 通过 `StoreBundle` 取出的 port 间接使用。
- 状态所有权见 [架构](../architecture.md) 第 8 节：业务事实在 PostgreSQL，可丢失/可重建协调状态在 Redis，日志与更新状态在 `.runtime/`。

## 实现逻辑

**养号配置持久化**：迁移 0025 为 `provider_accounts` 追加独立的 `quota_protection_json`，默认关闭、阈值 90，并校验开关、整数类型与 1–99 范围。Core 与 Admin 的列表、详情、可调度目录、分组成员共享解析都读取该列；单个、批量和 API Key 凭据轮换与原设置、revision、审计处于同一事务。省略字段以及重新导入保留既有配置，不写入上游额度。槽启动仍严格比较全部迁移，0025 后不能用只含 0024 的旧二进制冷启动；新增列不是可直接滚动迁移的保证。

**启动**（`src/bundle.rs::initialize`）：
1. 校验配置；非槽模式 `connect_and_migrate`（sqlx 迁移，已应用迁移的字节必须与 `_sqlx_migrations` 一致），槽模式（`deployment_slot: a|b`）只 `connect_for_slot` 校验 schema、不运行迁移，并获取每槽独立锁和共享存活锁（`PgSlotGuard`）。非槽模式持有会话级排他实例锁，冷启动执行中断恢复（`recover_after_restart`）。
2. 初始化 vault（`store.vault_key_file` 为可选项，未配置则按明文读写，已加密的行缺少密钥时拒绝读取）：读取 `store.vault_key_file`（32 字节十六进制主密钥，绝对路径、文件大小受限），`src/vault.rs` 用 HMAC 派生加密密钥与查找密钥，AES-256-GCM 加密 Provider credential JSON 与 Client Key 明文（前缀 `hubenc1:`，AAD 绑定行 ID）；冷启动时 `migrate_existing` 把遗留明文行加密。
3. 建 Redis 连接管理器，构造各 Redis 仓库（命名空间 `codex-proxy-rs`）；冷启动清理被中断的 credential lease。
4. 组装 `AdminStorePorts`、`CoreStorePorts`、`ProviderStorePorts`，注册 Worker 贡献与健康探针。`complete_startup` 在 Core 恢复完成、监听前调用（槽模式完成启动屏障）。

**PostgreSQL 仓库**（`src/postgres/`，每个文件是对应表的唯一 owner）：
- `provider_accounts/`：账号与 credential。`repository.rs` 实现 Core/Admin 端口和事务（`upsert`、`rotate_provider_account_in_transaction` 带 credential revision CAS、批量调度更新、删除、归档恢复、分组移动——插件经 `groups.move` 发起的移动审计操作者记 `plugin:<id>`，其他来源记 `gateway_pool`），`rows.rs` 行解码与校验，`admin_queries.rs` 管理列表分页 read model，`admin_adapter.rs` 是 Admin 账号端口适配，`core_adapter.rs` 是 Core `ProviderAccountStore` 适配，`runtime.rs` 把持久事实与 Redis 冷却合成账号状态，`quota_forecast.rs` 只读采样，`mapping.rs` 领域映射。修改共享行解析字段（`ACCOUNT_SELECT` 等）要核对所有 SELECT 调用方，包括分组成员。
- `client_keys.rs`（Key 与绑定、reveal、金额窗口）、`client_budgets.rs`（按 Key 串行检查并幂等累计 USD 费用）、`key_bindings.rs`（固定账号绑定和 Key 级插件设置）、`account_groups.rs`、`proxies.rs`（命名代理，账号保留解析后的 URL）。
- `execution.rs` 与 `execution_buffer.rs`：`model_requests` 生命周期与最终用量/费用；写入经有界队列异步落库，队列满/不可用丢弃可恢复观测并计数。`ops_events.rs`、`retention.rs`（终态请求、事件、审计的保留清理）。
- `observability/`：Dashboard / Usage / Ops / 账号用量查询族，`query_budget.rs` 限制管理观测占用的连接槽位，`usage_facts.rs` 是“完整交付”用量口径的唯一谓词（排除预热记录）。
- `runtime_settings.rs`（`runtime_settings` 单例与 `config_revision`）、`snapshot.rs`（终态配置表的一致性快照读取）、`pricing.rs`（价格覆盖与费用快照）、`backup.rs`（`backup_settings` / `backup_records`）、`admin_security_audit.rs`（`admin_users` / `admin_audit_events`）、`admission_recovery.rs`（Redis 丢失后由 `model_requests` 恢复准入热状态；槽守卫）。

**Redis 仓库**（`src/redis/`，全部可丢失、可重建）：`credential_leases`（账号/OAuth refresh lease 与 fencing）、`credential_cooldown`（冷却与容量冻结）、`credential_state`（credential state 与模型目录 cache fence）、`client_admission`（Key 的 RPM/并发原子准入与热状态恢复）、`coordination_buffer`（准入释放、circuit 反馈的后台写队列）、`provider_circuit`、`provider_session_affinity` / `provider_session_exclusion`、`native_continuation`、`oauth_pending`（一次性 claim）、`auth`（统一登录会话与双层固定窗口限流）、`runtime_change`（revision 提交后的 Pub/Sub 通知）、`worker_lease`（leader lease）、`artifact_profile`（官方制品画像缓存）、`admin_account_runtime`。

**备份**（`src/backup/`）：`pg_dump.rs`（子进程与本地暂存）、`staging.rs`（受控暂存目录：权限、磁盘空间、原子改名）、`s3.rs`（S3 兼容对象存储，含 R2）。

**Worker**（`src/workers.rs`）：过期请求恢复（30 秒）、保留清理（1 小时）、`model_requests` / Key 用量 / 准入释放 / circuit 反馈的 flush Daemon。`PostgresHealthProbe` 提供健康探针。

**插件账号目录**（`src/plugin.rs`）：`PluginAccountDirectory` 实现 `PluginAccounts`，复用 Admin 账号投影与 `model_access` 判定，不暴露 credential。

## 文件表

| 文件或目录 | 职责 |
| --- | --- |
| `src/lib.rs` `src/bundle.rs` `src/config.rs` | 入口、Bundle 组装、`StoreConfig`（URL / 密码环境变量、连接池预算、vault 路径、部署槽） |
| `src/value.rs` | `StoreError` / `StoreResult`、跨层错误与审计映射 |
| `src/vault.rs` | 静态加密 |
| `src/admin_adapter.rs` | 认证与设置端口适配 |
| `src/postgres/` `src/redis/` `src/backup/` `src/workers.rs` `src/plugin.rs` | 见上 |
| `backend/migrations/` | 编号 SQL 迁移与冻结清单 `.frozen-sha256`，规则见 `backend/migrations/README.md` |

## 数据、配置与接口

- 主要表：`admin_users` `admin_audit_events` `client_api_keys` `runtime_settings` `provider_accounts` `account_groups` `account_group_accounts` `client_api_key_groups` `model_requests` `ops_events` `backup_settings` `backup_records`，其余见 `backend/migrations/`（0001–0025）。
- 配置段 `store`：`vault_key_file`、`database.url|password`、`redis.url|password`、`pool.*`、`deployment_slot`；环境变量 `CPR_DATABASE_URL` `CPR_REDIS_URL` `CPR_DATABASE_PASSWORD` `CPR_REDIS_PASSWORD`。
- 测试需要 `CPR_TEST_DATABASE_URL` / `CPR_TEST_REDIS_URL`；本地缺失时 PG/Redis 用例静默跳过，CI 缺失则失败（见 `backend/migrations/README.md`）。

## 改动指引

- 新增迁移：只追加新编号文件，并在同一提交执行 `sha256sum 000N_xxx.sql >> .frozen-sha256`；已冻结文件一个字节都不能改。涉及 schema 的读取同步 `postgres/snapshot.rs`、对应 `rows.rs` 与测试。
- 需要 Redis 状态时先确认它真的“可丢失、可重建”；业务事实写 PostgreSQL。
- Store 不做业务判断：新规则放 Core / Admin，这里只加端口实现。费用结算用 `client_budgets.rs` 的幂等累计，不要绕过 Key 行锁。
- 改共享行解析或 SELECT 列：运行受影响模块已有的真实库测试（含分组成员等间接入口）。
- 测试在 `tests/`（`tests/postgres/`、`tests/redis/` 等镜像 `src/`）。
