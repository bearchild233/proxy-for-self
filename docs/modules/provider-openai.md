# provider-openai

<!-- doc-sync paths: backend/crates/providers/openai -->

## 职责与边界

OpenAI / Codex Provider（crate `provider-openai`）：独占 OpenAI credential schema、OAuth 与 API Key 账号、账号选择与 lease、模型目录、额度与订阅、客户端身份画像，以及 Responses / Images / standalone Search 的上游 transport（HTTP/SSE 与 WebSocket）。OpenAI 是**透明边界**：Responses 请求保留未知字段与顺序，SSE / WS / Images / Search 正文按原始字节转发，canonical facts 从同一数据旁路提取（见 [架构](../architecture.md) 第 5 节）。

- 依赖：`gateway-core`（实现 `Provider`、使用账号/路由/事件/错误/插件端口）、`gateway-admin`（实现 `ProviderAdmin`）、`gateway-protocol`（SSE、用量、请求头分类）、`kernel-protocol`（经 core 的插件类型）。不依赖 `gateway-store`（只用 core 声明的 `ProviderStorePorts`）和另一个 Provider。
- 调用方：组合根 `initialize`，得到 `ProviderBundle`（`core_provider()`、`admin_provider()`、`plugin_upstream()`、Worker 贡献、`set_plugin_hooks()`）。
- Excel 请求在本 crate 内转发给私有 Python Worker（`provider/excel.rs`，UDS），Worker 实现见 [excel-bridge](excel-bridge.md)。

## 实现逻辑

**账号养号**：只从仍有效的account-wide窗口按实际时长分类到短期/周/其它三路，各类取最大使用率。5小时与7天分别比较自己的阈值，不能用primary/secondary位置推断周期，也不重复进入legacy比较；月及其它周期沿用legacy。任一有效窗口触发就阻止新调度，独立code review等桶不触发全账号保护。有reset的窗口独立到期，无reset的观测最多十分钟；排序四舍五入不影响阈值判断。普通、固定、续接、插件选定路径及探测复用 `reached_usage`，沿用 `QuotaExhausted` 和安全恢复合同。Admin展示消费同一投影，不把本地保护持久化为上游用尽。

**一次推理 attempt**（`src/provider/`）：
1. `mod.rs::CodexProvider` 实现 `gateway_core::engine::provider::Provider`。`execute` 先按 operation 分派：模型请求走 `execution.rs`，Images / Search / 原生 JSON 端点走 `execute_image` / `execute_search` / `execute_raw_json_endpoint`，Excel 范围的 Key 走 `excel.rs`。
2. 选号：`credential/selector.rs::CodexCredentialSelector::select` 综合账号启停与归档、credential 状态、quota 信号、Redis cooldown、并发上限与账号租约（`CodexCredentialLease`）、权重/请求间隔/会话亲和（`credential/affinity.rs` 派生不透明亲和键）、管理员模型政策（先于额度/亲和/租约）、cyber-policy 作用域。`plugin_candidates` 把候选交给插件钩子。
3. 发送：`transport/request.rs` 把 Core `Generate` operation 严格编码为 Codex Responses wire；`transport/headers.rs` 按运行时画像组装官方头和账号身份头；`transport/client*.rs`（HTTP/SSE）或 `transport/websocket/`（WS）发送。是否走 WS 由账号/Key 策略和 `session_transport.rs`（根会话连续 WS 失败预算，耗尽后保持 HTTP）决定。
4. 响应：`transport/canonical.rs` 把上游 SSE 旁路解码为 canonical event，原始字节同时保留（`cold_response_stream`）；`provider/observation.rs` 归一化请求/响应/transport 观测；`transport/usage.rs` 解析用量并折算计费（`openai_billing_breakdown`）。提交边界前最多保留 64 KiB 原始 chunk 与短暂的结构事件，期间收到明确拒绝可无感换号。
5. 失败：`provider/failure.rs` 做上游失败分类、恢复决策与稳定错误投影：额度拒绝先隔离账号再投影 `ClientReplayRequired`（丢弃未交付错误帧，让客户端提交完整历史开新链）；WS 分阶段错误与重试退避；账号反馈（`apply_failure`）和权威额度刷新调度。
6. 插件路由：`provider/plugin_routing.rs` 是通用插件桥，`plugin_routing` 开启时在选号前调 `route.select`、选中后 `route.prepare`、收到响应头后 `response.head`、结束 `request.finish`，不归任何插件所有的账号按 `PluginHooks::watchers` 得到监控插件，收到头后调 `response.observe`；`route.select` 结果里的 `ordinary` 账号不计入池成员与归属，按普通账号选号、直连上游、不调请求钩子；只维护资格、交付和重放边界（每请求最多 4 次尝试、已提交后忽略 retry/switch、不可安全重放时不执行），不解释业务原因码。`provider/replay.rs` 约束跨账号只重放完整、可移植的输入。

**WebSocket**（`src/transport/websocket/`）：`pool/`（连接池：reservation / lease / 状态 / 维护与驱逐监督）、`pump.rs`（常驻后台泵，对齐官方 CLI 的连接管理）、`handshake.rs`（opening 与首帧）、`coordinator.rs`（pre-send / post-send 与池、熔断编排）、`breaker.rs`（冷建连 origin 级熔断）、`exchange/`（帧交换、流式转发、结果归约）、`model.rs`（请求与续接模型）、`audit.rs`。WS pool 与 continuation 按凭据版本隔离。

**Credential 与账号生命周期**（`src/credential/`）：
- `oauth.rs`：Authorization Code + PKCE / OIDC 管理流（pending 状态在 Redis，一次性 claim）；`token_client.rs`：token 交换、刷新与 PAT 验证；`admin.rs`：导入（AT/RT、OAuth JSON、官方 `auth.json`）的校验与 command 准备；`api_key.rs`：API Key 账号（Base URL / 密钥 / 传输策略随 credential revision）。
- `refresh.rs`：AT/RT 刷新状态机（Redis lease + `ProviderAccountStore` CAS，无 SQL），按退避延迟，终态失效才写终态；`refresh_queue.rs` 合并只读上游查询。
- `quota/`：额度权威事实、展示投影、主动/被动同步与 429 冷却；`document.rs` 解析 `/usage`，`snapshot.rs` 聚合窗口与调度信号，`recovery.rs` 分批恢复，`evidence.rs` 把失败分类为规范化账号事实。
- `catalog.rs`：账号 entitlement 快照与按套餐/账号/凭据版本/客户端版本隔离的模型目录缓存（Redis 模型 ID 缓存与进程内原生对象缓存分开）；`profile_statistics.rs`：个人资料统计、订阅、头像，以及 `maintain_lifecycle`（订阅观察、临期优先、归档写入，`compare_and_swap_lifecycle` CAS）；`cookie.rs`（账号 Cookie 合并）、`diagnostic.rs`（管理诊断：材料留在内存，插件只得到观察结果与随机引用）、`recovery_log.rs`（独立 OAuth 恢复日志）、`security.rs`（credential JSON schema 校验与日志脱敏）、`repository.rs`（行转换，无 SQL）。

**客户端画像**（`src/transport/profile/`）：Codex Desktop 上游请求画像（`mod.rs`），管理端选择的唯一解析入口 `selection.rs`；官方发布资料来自 `cli_release.rs`（npm 包）、`desktop_artifact.rs`（Desktop ZIP 内嵌 Core 版本）、`platform_release/`（Windows MSIX / Linux DEB 按平台架构核验，`range.rs` / `xz.rs` 做有界 Range 与流式解压）；用户选择与官方发布资料分开，版本缓存在 Redis。

**其他 transport**：`transport/probe.rs`（受控诊断，头部检测不消费 SSE 正文）、`reset_credits.rs`（主动额度重置卡，UUIDv4 幂等键）、`profile_statistics.rs` / `profile_avatar.rs` / `subscription.rs`（个人信息 HTTP 合同）、`catalog.rs`（官方目录 wire）、`tls.rs`（rustls provider 与自定义 CA）、`downstream/`（对非官方客户端请求的 Codex 兼容：`body.rs` 顶层参数与历史形状、`headers.rs` 环境头过滤）。

**插件能力实现**（`src/plugin.rs`、`src/plugin_warmup.rs`）：`OpenAiPluginUpstream` 实现 `PluginUpstream`：`quota.read`、`upstream.probe`（只读响应头探测，走账号出口，按账号限速；不带会话头，上游请求 ID 用插件传入的 `requestId`，省略时随机生成）、`http.fetch`（白名单主机，直连或账号出口），以及账号预热的安全目录与一次文本请求。预热提示词默认 `hi`，原文最多 4096 UTF-8 字节、非空且拒绝除换行/tab外的控制符；强度 `auto` 使用最低受支持档位，显式强度不受目录支持时返回 `skipped/reasoning_unsupported`。目录返回各模型的真实 `reasoningEfforts`。预热只接受 OpenAI OAuth 账号，复用模型政策、养号保护、冷却和调度租约；不返回 Cookie 或 turn state，发送结果同时带 `sendState`，只有明确的 5 小时额度窗口才产生 `shortTerm.resetAt`。

Provider 不查询分组，目录初始 `groups=[]/groupTargetsSupported=false`；由 Core 成功合并现有账号目录中的真实成员关系后声明分组能力，保持 Provider 端口和数据库 SQL 不变。

**后台 Worker**（`src/provider/workers.rs`）：OAuth 刷新（30 秒）、额度与目录健康（30 秒）、模型目录与 ETag、Desktop / CLI 发布检查、账号生命周期维护（60 秒）；owner 常量集中在该文件。

## 文件表

| 文件或目录 | 职责 |
| --- | --- |
| `src/lib.rs` | `initialize`、`ProviderBundle`、对外 re-export |
| `src/config.rs` | `OpenAiConfig`：`api`、`ws_pool`、`quota`、`auth`、`stream_max_retries`、`residency`、`excel_worker_socket`、`excel_plugin_state_file`、`pinned_cli_profile`、`pinned_cli_auto_update`、`plugin_routing`（旧配置里的 `gateway_pool` 段被忽略） |
| `src/admin/mod.rs` | `OpenAiAdminProvider`（实现 `ProviderAdmin`）：导入、OAuth、刷新、个人资料、头像、重置卡、模型政策；`OpenAiOAuthPendingStore` |
| `src/provider/` | Provider 适配：`mod.rs`、`execution.rs`、`failure.rs`、`observation.rs`、`plugin_routing.rs`、`replay.rs`、`excel.rs`、`workers.rs` |
| `src/credential/` | 账号 credential、选号、刷新、额度、目录、资料、OAuth（见上） |
| `src/transport/` | 上游 transport、画像、协议类型（`protocol/`）、WebSocket |
| `src/session_transport.rs` `src/plugin.rs` `src/plugin_warmup.rs` | 上游 transport、插件能力与账号预热维护执行 |

## 数据、配置与接口

- 状态：credential JSON 存 PostgreSQL（Store 层加密）；cooldown、lease、affinity、continuation、OAuth pending、目录缓存、制品画像在 Redis；本 crate 无 SQL。
- 配置段 `openai`（见 `deploy/config.example.yaml`）；客户端身份（Desktop / CLI 画像选择）不在 YAML，存数据库，由管理端设置。
- 对外：`Provider`、`ProviderAdmin`、`PluginUpstream` 三组 trait 实现。

## 改动指引

- 上游协议或客户端头变化：改 `transport/headers.rs`、`transport/profile/`；非官方客户端兼容只改 `transport/downstream/`，不要碰账号身份保护与会话规范化。
- 新增账号状态/失败类别：`provider/failure.rs`（分类与恢复）→ `credential/selector.rs::record_failure`（账号反馈）→ `gateway-core` 的错误类型；注意“额度拒绝先隔离账号再要求客户端重放”的顺序不能变。
- 凭据或额度字段变化：`credential/types.rs`、`security.rs`、`quota/`，并同步 `gateway-store` 的行解析和 Admin 投影；credential 刷新不等于 quota 刷新，两者独立。
- 改 WS：连接池与 continuation 按凭据版本隔离，响应终态前的 Close 1000 仍是失败；改动后跑 `tests/transport/` 的 WS 用例。
- 账号归档/订阅逻辑在 `credential/profile_statistics.rs::maintain_lifecycle`，由前端“归档”视图消费，改动同步 [accounts 插件](accounts.md)。
- 测试在 `tests/`（`credential/` `provider/` `transport/` `admin/` `plugin.rs`）；`tests/provider/contract/gateway_pool_e2e.rs` 是内核侧网关池端到端（见 [网关池](../gateway-pool.md)）。
