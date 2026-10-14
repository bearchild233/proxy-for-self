# gateway-host

<!-- doc-sync paths: backend/crates/gateway-host -->

## 职责与边界

网关进程与操作系统能力：配置发现、结构化日志与文件轮转、HTTP 监听与优雅排空、Worker 监督、在线自更新、插件运行时（内核侧）、以及少量外部适配（价目源、客户端下载解析、代理出口探测、插件策略进程）。不含业务规则；通过 Admin / Core 定义的端口把能力交给上层。

- 依赖：`gateway-core`（Worker 契约、健康、连接生命周期、插件钩子端口）、`gateway-admin`（`SystemOperations`、`PricingSource`、`ProxyProbe`、`ClientDistributionResolver` 端口）、`kernel-protocol`。不依赖 Store 或具体 Provider（代理探测的 HTTP 客户端构造由组合根注入）。
- 调用方：组合根 `backend/apps/gateway/src/bootstrap.rs`；API 通过 `ConnectionLifecycle` 注册连接。

## 实现逻辑

**初始化与生命周期**（`src/lib.rs`）：`initialize(HostConfig)` 先启动日志，创建取消令牌、连接跟踪器、`WorkerSupervisor`（独立取消令牌：HTTP 排空期间计量和协调队列仍要写，Worker 在排空结束后才取消）、`ProcessSystemOperations`、`RgAdguardClientDistribution`。`serve(router)` 是进程唯一阻塞点：`serve_router` 绑定监听、处理 OS 信号、停止接收新连接并 drain HTTP/WS（`drain_timeout_seconds`），随后取消并等待 Worker（`worker_shutdown_timeout_seconds`）、关闭插件运行时；两段预算独立，部署侧 `stop_grace_period` 必须覆盖之和。

**配置**（`src/config.rs`）：`load_config::<T>()` 从当前目录或父目录寻找 deploy 目录下的 `config.yaml`，用 `config` crate 加载；未使用字段只在 stderr 警告并忽略（`services` 等外部区段除外），字段校验在各 crate 的 `resolve_and_validate`；`HostConfig` 含 `listen`、`runtime_data_dir`、`logging`、`system_update`、两个关停预算；环境变量 `CPR_SERVER_HOST` / `CPR_SERVER_PORT` 覆盖监听地址。

**日志**（`src/logging/`）：`mod.rs` 初始化 tracing（stdout 与文件独立开关，OAuth 恢复文件与请求转储为独立文件，默认关闭）；`sink.rs` 是有界文件队列（拥堵时背压，正常退出排空，写入失败置健康告警）；`writer.rs` 按大小轮转、压缩已关闭分片、按完整 UTC 日期组清理。`logging_health_probe` 把写入缺口计入 `/healthz`。

**Worker 监督**（`src/workers/mod.rs`）：`WorkerSupervisor::start` 校验各 crate 贡献的注册（ID 唯一、调度合法），按 Scheduled / Daemon 运行：周期任务先取 leader lease（Redis，带 fencing token）再执行一轮，失败指数退避；Daemon 崩溃按重启策略重启；汇总健康给 `WorkerHealthSource`。

**在线更新**（`src/system_update/`）：`mod.rs::ProcessSystemOperations` 实现 `SystemOperations`（版本、更新详情/状态/SSE 进度、更新、回滚、重启、登录防护、插件管理与请求）。两种模式：配置了 `managed_socket` 时全部委托给独立 updater（`managed.rs`，固定协议的私有 Unix socket，不传 shell 命令、路径或任意下载地址，对应 `services/updater/updater.py`）；否则本进程内用 `release.rs`（GitHub Release 发现、缓存、版本比较、同大版本限制）、`download.rs`（受限下载、重定向信任链、SHA-256）、`archive.rs`（安全解包）、`swap.rs`（二进制与 Web 制品交易式替换与回滚）、`state.rs`（状态文件、跨进程锁、临时目录）、`process.rs`（进程替换）。状态文件是更新结果的权威来源，SSE 终态在落盘后发送。

**插件运行时**（`src/plugin/`，协议见 [kernel-protocol](kernel-protocol.md)，设计见 [微内核设计](../microkernel.md)）：
- `registry.rs`：`PluginRuntimeConfig`（`registry_path` 与 `ownership_cache_path` 必须成对配置）、注册表 JSON（`id` `enabled` `socket` `uid` `grants` `fetchHosts`）的加载与校验（ID 唯一、socket 绝对路径、能力白名单）；`is_socket_switch_of` 判断“仅 socket 变化”的热更新。
- `runtime.rs::PluginRuntime`：`start` 恢复归属缓存后启动监督任务（监视注册表变化，为每个启用插件起连接任务）；实现 `PluginHooks`（每次调用在同一把状态锁内选连接并准入，保证 socket 切换时已准入调用排空）；`admin_call` 供控制面管理连接；`shutdown` 回收全部连接。
- `connection.rs`：单个插件的连接任务（握手、`ownership.changed` 处理、健康检查、能力请求的授权与分发、切换时旧会话退役排空）；`transport.rs`：`Peer`/`Outbound`、请求编号、准入计数；`state.rs`：每个插件的 `Entry` 与快照（`Available` / 不可用 / 未连接）、归属缓存文件读写（`plugin-ownership.json`，两个槽共用）；`capability.rs`：`log` 能力实现。
- 归属语义：已启用但未连接的插件按缓存归属让其分组账号失败关闭；禁用或移除则清除缓存归属，账号交还默认调度。

**外部适配**：`pricing/mod.rs`（models.dev 固定地址、禁止跳转，只读，不进推理路径）、`client_distribution.rs`（RG-Adguard Store 页面解析与官方回退，惰性：仅在管理员打开下载弹窗时请求）、`proxy_probe.rs`（与 Provider 一致的代理协议与超时/大小限制的出口测试）、`plugin_policy.rs`（备份/定价策略进程：环境变量 `CPR_PLUGIN_POLICY_RUNNER` 与 `CPR_PLUGIN_ROOT` 同时设置时，用清空环境的 `python3 -I` 启动 `services/plugin_host/policy_runner.py`，5 秒超时，输出上限 128 KiB）。

## 文件表

| 文件或目录 | 职责 |
| --- | --- |
| `src/lib.rs` | `HostBundle`、`initialize`、`serve` |
| `src/config.rs` | 配置发现、`HostConfig`、`LoadableConfig` |
| `src/serve.rs` | 监听、信号、连接跟踪、drain |
| `src/logging/` | 日志、轮转、健康 |
| `src/workers/mod.rs` | Worker 监督 |
| `src/system_update/` | 自更新与托管更新 |
| `src/plugin/` | 插件运行时 |
| `src/pricing/mod.rs` `src/client_distribution.rs` `src/proxy_probe.rs` `src/plugin_policy.rs` | 外部适配 |
| `build.rs` | 编译期注入 `CPR_VERSION`（`release/version.yaml` 为缺省来源）、`CPR_GIT_SHA`、`CPR_BUILD_TIME`、`CPR_BUILD_TYPE` |

## 数据、配置与接口

- 配置段 `host`（`listen`、`runtime_data_dir`、`logging.*`、`system_update.*`、`drain_timeout_seconds`、`worker_shutdown_timeout_seconds`）与 `plugins`（`registry_path`、`ownership_cache_path`），见 `deploy/config.example.yaml`。
- 文件：`runtime_data_dir` 下的更新状态与锁、备份暂存；日志目录；插件归属缓存。
- 与 updater 的 socket 协议见 [updater](updater.md)；插件注册表由 updater 写入，格式见 [updater](updater.md) 的服务插件监督一节。

## 改动指引

- 新增 Worker：在所属 crate 返回 `WorkerContribution`，不要在 Host 里写业务任务。
- 改关停顺序或预算要同步 `deploy/compose.yaml` 的 `stop_grace_period` 与 [部署文档](../../deploy/README.md)。
- 改自更新：校验 Release host、大小、SHA-256、归档路径和“同大版本”限制不能放宽；托管模式的 socket 协议同步 `services/updater/updater.py`。
- 改插件运行时：保持“选择连接与准入在同一把锁内”“旧连接排空后才关闭”“能力每次按当前注册表与归属授权”三条；协议变化走 [kernel-protocol](kernel-protocol.md) 的流程。
- 测试在 `tests/`，镜像 `src/`。
