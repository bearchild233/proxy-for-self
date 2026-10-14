# gateway-api

<!-- doc-sync paths: backend/crates/gateway-api -->

## 职责与边界

客户端与管理端的 HTTP / WebSocket / SSE 协议适配层：请求解码、调用 Core / Admin 抽象、协议编码与交付、静态资源。不直接访问 Store 或具体 Provider，不做业务策略。

- 依赖：`gateway-core`（`ExecutionService`、健康、连接生命周期）、`gateway-admin`（`AdminServices`）、`gateway-protocol`（SSE、请求头分类）。
- 调用方：组合根 `gateway_api::initialize` 得到唯一 router，交给 Host 监听。
- HTTP 字段与错误合同以 [管理接口](../api.md) 为准，本文只记结构与流程。

## 实现逻辑

**Router 装配**（`src/lib.rs::initialize`）：`/healthz`、OpenAI 数据面（`/v1/*`）、管理面（`/api/admin/*`）、统一登录（`/api/auth/*`）、Key 自助用量（`/api/key-usage/*`）、插件控制面（`/api/plugins*`、`/api/plugin-devices/*`），其余走静态资源（`ServeDir` + `index.html` 回退，目录来自配置或 `CPR_WEB_DIST_DIR`）。外层依次叠加：可选 CORS（仅 GET/POST、显式头）、可选请求超时、trace（info span 带 request_id）、请求 ID 生成与回传、来源解析中间件。

**来源解析**（`src/client_ip.rs`）：只信任配置的 `trusted_proxy_ips` 对端提供的唯一合法 `X-Real-IP`，清除其他来源头，把规范化地址写入 `ConnectInfo`，HTTP 与 WS 共用；`/healthz` 对可信代理允许无来源头。

**数据面**（`src/openai/`）：
- `router.rs`：`/v1/responses`（POST 与 WebSocket 升级）、`/v1/models`、`/v1/models/{id}`、`/v1/images/generations|edits`、`/v1/alpha/search`、`/v1/usage`；`/v1/chat/completions` 与 `/v1beta*` 返回 `501 unsupported_endpoint`。全部经 `resources.rs::protect` 中间件。
- `resources.rs`：推理入口容量。在缓冲正文前获取请求数许可和（可选）在途正文预算，持有到响应流结束或取消；繁忙返回 `503 gateway_busy`，超大正文 `413`；限制值每次读取最新快照，调整不重置占用。
- `auth.rs`：Bearer Client Key 认证（`ExecutionService::authenticate`），识别 Codex Desktop/CLI 版本与手机远程后缀，低于最低版本在进入 Provider 前返回 `426`。
- `responses/`：`request.rs` 是 Responses JSON 的唯一解码边界（保留未知字段与顺序、业务扩展头原始字节，提取会话语义，只产出 Core 路由事实与不透明 wire 载荷）；`http.rs` 做 JSON/SSE 交付（`stream_execution_response` 在 commit 前缓冲，commit 后原样转发；`collect_execution_response` 汇聚非流式）；`response.rs` 是 Provider wire 到客户端 transport 的透明编码；`websocket/` 是下游 WS：`connection.rs`（单 owner 泵、有界收发）、`protocol.rs`（`response.create` 帧解码、事件映射）、`forward.rs`（把已启动的 execution 串行转发为 WS 事件）、`mod.rs`（升级、每个 `response.create` 独立准入的会话编排）；`error.rs` 稳定协议错误。
- `error.rs`：数据面稳定错误响应（`gateway_error_contract` 供 HTTP、SSE、WS 共用），并在交付边界把容量类错误码投影为客户端不重试的信号（`capacity_error_for_client`，真实上游事实保留）；`endpoint.rs`：原生非流式 JSON 端点的公共交付；`images/` `search/`：JSON 透明 adapter；`models.rs`：模型目录（原生目录或通用画像）；`usage.rs`：Bearer 额度查询；`service.rs::OpenAiService`：wire 到 Core 用例的唯一映射，持有入口容量。

**管理面**（`src/admin/`）：`mod.rs::router` 合并 account_groups / proxies / accounts / backups / client_keys / observability / settings / system 各路由，未知路径与方法返回统一 JSON 信封（`admin_not_found` / `method_not_allowed`），响应加 `Cache-Control: no-store`。每个功能目录通常有 `wire`（请求响应结构与校验）、`handlers`（路由与编排）、`presenter`（Admin 结果到安全 wire 的投影，账号目录在 `src/admin/accounts/presenter.rs`）；`wire.rs` 定义信封 `{code,message,data}` 与 `AdminError` 到 HTTP 状态的映射，`extract.rs` 把 JSON/Query 提取失败映射为同一信封，`auth.rs::AdminAuth` 从 `cpr_session` Cookie 解析会话并要求管理员身份、生成审计上下文。观测相关的 `src/admin/observability/presenter.rs` 是过渡期展示格式化。

**登录与 Key 用量**：`src/auth.rs`（`/api/auth/login|logout|status|password`，Cookie 属性在 `session_cookie.rs`）；`src/key_usage/`（只读字段白名单，查询不接受 Key/账号/Provider 范围）。

**插件控制面**（`src/plugins/mod.rs`）：`/api/plugins` 目录、`/api/plugins/{id}/pages/{name}`、`/api/plugins/{id}/rpc/{name}`，身份只取自登录会话（admin 或 key），转成 `PluginRequest` 交 `AdminServices::system().plugin_request`，由 Host 转发给 updater 的原生插件注册表（Excel 等）；`/api/plugin-devices/*` 无网关身份，必须由插件自己验证设备凭据。请求体上限 60 KiB。主壳的页面租约、能力桥走另一条路径（Python 控制面 `/api/plugin-platform/*`，见 [plugin-host](plugin-host.md)）。

## 文件表

| 文件或目录 | 职责 |
| --- | --- |
| `src/lib.rs` | `ApiConfig`、`initialize`、`ApiState`、CORS / 超时 / trace / 请求 ID 层 |
| `src/client_ip.rs` `src/session_cookie.rs` `src/health.rs` | 来源解析、会话 Cookie、`/healthz` 聚合 |
| `src/auth.rs` | 统一登录接口与 `SessionState` trait |
| `src/openai/` | 客户端协议：`router` `auth` `resources` `service` `models` `usage` `error` `endpoint` `responses/` `images/` `search/` |
| `src/admin/` | 管理面：`account_groups` `accounts/` `backups` `client_keys` `observability/` `proxies` `settings` `system`，公共 `wire` `extract` `auth` `presenter` |
| `src/key_usage/` | Key 自助用量 |
| `src/plugins/mod.rs` | 插件公共控制面 |

## 数据、配置与接口

- 配置段 `api`：`trusted_proxy_ips`、`inference_limits`（`max_requests` / `max_body_bytes` / `max_in_flight_body_bytes`，0 表示不限）、`asset_directory`、`cors_allowed_origins`、`request_timeout_seconds`、`request_id_header`。
- 无存储。对外接口全部在 [管理接口](../api.md) 与 [架构](../architecture.md)。

## 改动指引

- 新增或修改管理接口：先改 `gateway-admin` 用例与模型，再在对应 `wire` / `handlers` / `presenter` 加字段，同步 [管理接口](../api.md)、前端 `frontend/src/api/modules/` 与 `packages/plugin-sdk/operations.json`（插件能力白名单）。
- 响应里不得出现凭据、原始上游错误体或任意 `Display` 文本；管理错误一律走 `AdminError` 信封。
- 数据面透明约束：不改写 Responses 业务正文、未知字段和扩展头；协议修补放在 `backend/crates/providers/openai/src/transport/downstream/`，这里只做解码与交付。
- WS 与 HTTP 共用准入/限额语义：每个 `response.create` 独立准入，空闲连接不占名额。
- 测试在 `tests/`，镜像 `src/`；`tests/support.rs` 是共享装置。
