# plugin-host（Python 插件控制面）

<!-- doc-sync paths: services/plugin_host -->

## 职责与边界

插件平台的管理面，进程独立于 Rust 网关，不在推理路径上：登录态转发、插件目录与页面租约、插件包安装与版本切换、页面能力桥（命名能力 → 网关管理 API 的受控转发）、按需后台任务、Excel / 服务插件生命周期编排、定价/备份策略进程入口。网关 HTTP/WS 推理流量不经过它；它故障不会停止网关。

- 依赖：`packages/plugin-sdk/policies.json`（平台策略：插件名称、分类、是否基础模块）与 `operations.json`（能力 → 网关管理 API 的白名单映射）；`services/updater/static_plugins.py`（UI 包存储）、`services/updater/kernel_protocol.py`（管理连接）；网关管理 API（本机回环）；updater 私有 socket。
- 调用方：浏览器（经反向代理 `/api/plugin-platform/*`，部署见 [插件平台部署](../../deploy/plugin-platform/README.md)）、网关的 `policy_runner`（定价/备份策略）。
- 平台合同（包格式、能力、租约、热更新）见 [插件平台](../plugin-migration.md)，Worker 合同见 [原生 Worker](../plugins.md)，服务插件见 [微内核设计](../microkernel.md)。

## 实现逻辑

**HTTP 服务**（`server.py`）：`ThreadingHTTPServer` 子类 `BoundedServer`（32 个并发名额，满则直接断开），只监听回环。`Handler.dispatch` 校验 `Host` 与 POST 的 `Origin` 必须等于配置的 origin；来源 IP 取唯一合法 `X-Real-IP` 放入 `CLIENT_IP` ContextVar（后台任务复制上下文）。路由：
- 登录与系统：`/api/auth/*`、`/api/admin/system/*` 的固定路径原样转发到网关（`Platform.forward`，带 Cookie / Origin / X-Real-IP，响应上限 8 MiB）。
- `/api/plugin-platform/catalog|manage|page/<id>/<version>/<page>|call/<id>/<version>/<name>|lease|action|loading|order|register|upload`：目录（按角色过滤页面）、管理目录（含当前与可回退版本的发布说明）、页面 HTML + 租约、能力调用、版本租约续期/释放、生命周期操作、懒加载偏好、排序、注册新插件、上传插件包（base64 + SHA-256）。
- `/api/plugin-platform/internal/gateway-pool`：网关池旧运行时入口，Bearer 能力密钥（`gateway-pool-runtime.key`，0640）校验，仅在旧 Python 路径使用。
- `local-login`：仅 `--local-access` 与默认本机 origin 时的本地测试便捷登录。
- 其余 GET 按静态资源处理（`--assets` 目录，发布目录 symlink 原子切换，路径越界拒绝）；`/api/*` 未知返回 404。

**能力桥**（`Platform.call`）：先校验插件已启用（排空任务例外）、`name` 在该版本清单的 `capabilities` 里，再按名称分派：
- `<插件ID>.*`（服务插件管理能力，仅管理员）→ `service_calls.py` 读 root 管理的内核注册表取 socket，经管理连接 `admin.call` 转给插件进程，错误码映射为 400/403/503（超时 10 秒）。
- `gateway-pool.*`：仅管理员；清单带 `service` 段时走上一条，否则回落旧 Python `gateway_pool.py`（回退旧包即恢复 Python）。
- `tasks.*` `templates.*` `diagnostics.*`：诊断插件的任务、模板、历史，经 `jobs.py` 启动 Worker 子进程；`pricing.schedule.status` 读定价运行状态；`accounts.avatar`、`excel.status`、`client.models` 是少量内置组合能力。
- 其余名称查 `operations.json`，校验角色，GET 转查询串、POST 转 JSON，转发到网关管理 API（`display-order.save` 额外限制插件只能改自己的顺序范围）。

**安装与注册**：`upload` 校验哈希与清单后，新插件需通过 `Platform.register`（ID 格式、能力只能是平台已提供或自己的 `<id>.<方法>`、页面必须使用 `/extensions/<id>/…` 路由或合法槽位）；带 `service` 段的包需要 updater 可用且无并发操作，安装后交 `lifecycle.launch_service`。版本存储由 `StaticPluginStore`（见 [updater](updater.md)）管理。

**生命周期**（`lifecycle.py::Lifecycle`）：操作日志 `lifecycle.json`。Excel 走原生 Worker 路径：先关准入，再通过 updater 或网关 `/api/admin/system/plugins/action` 触发安装/更新/启用/停用/卸载并轮询（最长 650 秒），成功后同步 UI 存储，失败恢复；控制面重启时对未完成的 Excel 操作启动 `recover`，无法确认就关闭入口并标记失败。服务插件走“存储切换 → updater `/services/action` → 轮询 `/services/status`（240 秒上限）→ 失败恢复存储”；控制面重启时服务操作一律标记失败（不自动重放）。本机测试环境通过 `--local-excel-wsl` 改用 `scripts/plugin-stage-excel.py`。

**后台任务**（`jobs.py`）：每任务锁定包版本，以 `python -I -B -u` 子进程运行插件 Worker，不继承凭据环境；stdin/stdout 逐行 JSON，最多 3 次回调能力（范围校验：诊断只能操作发起账号）、90 秒超时、4 个并发名额、保留最近 128 条；任务属于发起会话（Cookie 哈希）。

**策略进程**（`policy_runner.py`）：网关 `backend/crates/gateway-host/src/plugin_policy.rs` 通过它运行 `backup` / `pricing` 插件包内的策略脚本（包内 worker/policy.py，见 [backup](backup.md)、[pricing](pricing.md)）（数据入、决策出，不接触凭据）；校验 `active.json`、清单 `policyWorker` 与文件哈希，3 秒超时；定价计划状态写 `pricing-runtime/status.json`（带文件锁）。

**旧网关池 Python 实现**（`gateway_pool.py`、`gateway_pool_history.py`、`gateway_pool_history_query.py`、`gateway_pool_queue.py`）：SQLite 状态机、历史与队列；已被 Rust 服务插件取代，只在清单无 `service` 段的旧包回退时使用，P2 删除。规则与口径见 [网关池](../gateway-pool.md)，不要在此加功能。

## 文件表

| 文件 | 职责 |
| --- | --- |
| `services/plugin_host/server.py` | HTTP 服务、`Platform`（目录、能力桥、注册、安装）、CLI（`serve` / `install` / `status`） |
| `services/plugin_host/lifecycle.py` | Excel 与服务插件的生命周期编排、操作日志与恢复 |
| `services/plugin_host/jobs.py` | 后台任务（Worker 子进程） |
| `services/plugin_host/service_calls.py` | 服务插件管理调用转发 |
| `services/plugin_host/policy_runner.py` | 网关调用的策略进程入口 |
| `services/plugin_host/gateway_pool.py` | 旧 Python 网关池状态机与运行时动作（停用） |
| `services/plugin_host/gateway_pool_history.py` | 旧网关池历史写入（停用） |
| `services/plugin_host/gateway_pool_history_query.py` | 旧网关池历史只读投影（停用） |
| `services/plugin_host/gateway_pool_queue.py` | 旧网关池共用账号队列（停用） |
| `services/plugin_host/test_*.py` | 单元测试（平台、服务控制、网关池、定价计划） |

## 数据、配置与接口

- 命令行：`server.py serve --root <数据根> --gateway-port <网关管理端口> --port --origin --assets [--local-access] [--updater-socket] [--service-registry] [--local-excel-wsl]`；`install` 把 `.build/plugins/<id>/package` 打包安装；`status` 输出管理目录。
- 数据根：`plugins/<id>/{active.json, releases/<sha256>/, .leases/}`（UI 包存储）、`registry.json`（运行时注册的插件）、`order.json` `loading.json` `lifecycle.json`、`diagnostic-history.json`（最近 200 条）、`diagnostic-templates.json`、`pricing-runtime/status.json`、`gateway-pool-runtime.key`、旧 `gateway-pool-runtime/state.sqlite3`。
- 对外接口即上述 `/api/plugin-platform/*`；对内依赖 updater 的 `/plugins`、`/plugins/action`、`/services/action`、`/services/status`。

## 改动指引

- 新增插件能力：在 `packages/plugin-sdk/operations.json` 增加映射（method/path/roles），插件 `plugin.json` 声明该能力，网关对应管理接口已存在；服务插件的新方法不需要改这里（`<插件ID>.*` 通用转发）。
- 新增平台策略项（名称、分类、基础模块）：`packages/plugin-sdk/policies.json`。
- 改安全校验（Host/Origin、角色、版本租约、包校验）后跑 `python -m unittest discover -s services/plugin_host -p "test_*.py"`；`StaticPluginStore` 的改动同时影响 updater。
- 服务插件操作的时序（先存储后 updater、失败恢复存储）不要调换；控制面重启不得自动重放服务操作。
- 不读取或打印生产凭据；本机验证见 [本机测试](../plugin-stage.md)。
