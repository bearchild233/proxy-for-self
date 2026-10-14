# provider-xai

<!-- doc-sync paths: backend/crates/providers/xai -->

## 职责与边界

xAI（Grok Build）Provider（crate `provider-xai`）：官方 Grok Build OAuth session 账号、选号与租约、实时模型目录、订阅/额度、CLI 版本画像，以及 Responses → Grok wire 的**翻译**型 transport。xAI 是翻译边界：Provider 把 Grok wire 转成 Responses wire，工具类型、item ID 与 `call_id` 配对保持；上游结构化错误的 message / code / type 可透出，但账号指纹先脱敏。API Key 不是受支持的 xAI 账号 credential。

- 依赖：`gateway-core`、`gateway-admin`（实现 `ProviderAdmin`）；不依赖 `gateway-store`，也不依赖 OpenAI Provider。
- 调用方：组合根 `provider_xai::initialize`，得到 core provider（`GrokBuildProvider`）、admin provider 与 Worker 贡献。

## 实现逻辑

**一次推理**（`src/provider/`）：`mod.rs::GrokBuildProvider` 实现 `Provider`。选号 `credential/selector.rs`（账号 session 选择、Redis lease、失败反馈）；`transport/request/` 把 OpenAI Responses 请求转换为 Grok Build wire：`mod.rs` 入口与协调，`model.rs`（模型能力与参数）、`tools.rs`（Codex custom 工具与 Grok function 工具双向转换，超限或转换失败终止流）、`history.rs`（OpenAI history 重建与清理）、`identity.rs`（会话身份、租户隔离与缓存路由）、`schema.rs`（工具 JSON Schema 校验与数值归一化）、`response.rs`（Grok 输出恢复为 Responses 所需状态）。`provider/stream.rs` 执行 inference / compaction 流并校验终态；`transport/canonical.rs` 把官方 Grok Responses SSE 转为 canonical event；`transport/compaction.rs` 处理压缩请求；`provider/failure.rs` 做失败分类、账号反馈和恢复决策；`provider/continuation.rs` 与 `src/reasoning_replay.rs` 处理续接与账号隔离的 reasoning replay。默认 `store: false` 的续接由现有会话 owner 重放完整历史；原生续接按上游约束处理 `instructions` 与 `previous_response_id`。

**transport**（`src/transport/`）：`network.rs` 是显式账号出口的 HTTP 栈（禁用重定向与业务重试，官方 host 的 DNS rebinding 防护，OAuth 只允许 `auth.x.ai:443`，Responses SSE transport）；`egress.rs` 有界管理出口 client 与 OIDC JWKS 单飞的生命周期；`inference.rs` 推理请求/响应与 transport 指标；`catalog.rs` 官方 Grok CLI proxy 模型目录 wire 与快照；`profile.rs` / `client_profile.rs` Grok CLI 运行时 wire 画像（发布版本与用户选择分别维护，不回写 YAML）；`headers.rs` `session.rs` `config.rs`（固定官方 inference 配置）。

**credential**（`src/credential/`）：`authorization_code.rs` `pkce.rs` `discovery.rs` `config.rs` 官方 OAuth 流；`oidc_verifier.rs` + `verification.rs` 官方 OIDC token 生产校验（JWKS 过期或失败不使用 stale fallback）；`token.rs` `client.rs` `http.rs` token 交换与刷新；`refresh.rs` 刷新状态机（lease 保证并发、credential revision CAS 保证写回）；`import.rs` 已有 Grok Build OAuth token 的归一化导入；`catalog.rs` 实时模型目录与可重建 TTL 缓存；`repository.rs` 对 `ProviderAccountStore` 的 credential 适配；`types.rs` `secret.rs` `error.rs`。刷新额度时同步查询官方订阅（`GROK_SUBSCRIPTION_URL`、`GROK_BILLING_URL`）并只把套餐事实写入 quota JSON：明确无付费订阅的个人账号显示 Free，查询失败、缺字段或团队身份不推断为 Free。

**管理适配**（`src/admin.rs`）：`ProviderAdmin` 实现（OAuth start/complete、导入、刷新、轮换、连接信息）与 Redis OAuth pending 适配。

**后台 Worker**（`src/provider/workers.rs`）：OAuth 刷新（30 秒）、额度与目录（5 分钟，额度耗尽账号另有较长复查间隔）、Grok CLI 发布检查（`xai-cli-release`）。

## 文件表

| 文件或目录 | 职责 |
| --- | --- |
| `src/lib.rs` | `initialize`、公开类型 re-export |
| `src/admin.rs` | 管理适配与 OAuth pending |
| `src/provider/` | `mod.rs`（Provider 适配）、`stream.rs`、`failure.rs`、`continuation.rs`、`workers.rs` |
| `src/credential/` | OAuth、校验、刷新、导入、目录、选号、仓库 |
| `src/transport/` | 请求转换（`request/`）、canonical 解码、压缩、网络栈、目录、画像 |
| `src/reasoning_replay.rs` | 账号隔离的 reasoning replay |

## 数据、配置与接口

无 YAML 配置段（组合根直接调用 `initialize`）；Grok CLI 画像选择由数据库保存、管理端设置。credential 存 PostgreSQL（Store 层加密），lease / cooldown / 目录缓存 / OAuth pending 在 Redis。对外实现 `Provider` 与 `ProviderAdmin`。

## 改动指引

- 工具、history 或 schema 转换变化：只改 `transport/request/`，保持 item ID 与 `call_id` 配对；加用例到 `tests/transport/`。
- OAuth / OIDC 校验是 fail closed，不要新增 stale 或跳过校验的路径。
- xAI 辅助请求（OAuth、目录、计费）使用内置官方画像，模型与压缩请求使用已保存的用户选择；两者都不能直连回退，账号代理失败即请求失败。
- 新增账号字段或额度形态要同步 `gateway-store` 行解析、Admin 投影和 [accounts 插件](accounts.md) 的展示。
- 测试在 `tests/`（`credential/` `provider/` `transport/` `admin.rs`）。
