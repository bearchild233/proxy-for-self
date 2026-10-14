# gateway-protocol

<!-- doc-sync paths: backend/crates/gateway-protocol -->

## 职责与边界

跨层共享的 OpenAI wire 编解码，以及没有业务 owner 的解析事实。叶子 crate，不依赖其他 workspace crate，不含 HTTP、数据库、账号或路由逻辑。

调用方：`backend/crates/gateway-api`（入站请求头分类、SSE 编码）、`backend/crates/providers/openai`（上游 SSE 解码、用量与限流提取、Codex 语义）、`backend/crates/providers/xai`、`backend/crates/gateway-store`（用量与限流文档解析）。透明边界约定：SSE 原始字节按顺序交付，旁路解析结果只用于观测，不能反向改写或否决原始数据（见 [架构](../architecture.md) 第 5 节）。

## 实现逻辑

- SSE（`src/openai/sse.rs`）：`SseEventDecoder` 是增量解码器，支持任意分块边界和首行 BOM；`push_frames` / `finish_frames` 同时保留每个完整帧的原始字节（`SseFrame::raw`）和旁路解析出的 `SseEvent`。单帧超过旁路观测预算（`MAX_SSE_EVENT_BUFFER_BYTES`，64 MiB）时改为只透明交付，直到遇到分隔符才恢复解析；旁路解析失败不丢原始帧。`response_failed_sse_event*` 编码网关自造的 `response.failed` 事件。
- 事件与用量（`src/openai/events.rs`）：`ResponseModelObservation` 从原始事件记录响应声明的模型（终态优先，缺失不用请求模型补齐）；`extract_usage` / `extract_sse_usage` 提取标准化 `TokenUsage`（输入、输出、缓存读写、reasoning、图片工具 token）；`billable_usage_is_complete` 供计价确认零值有明确上游事实；`retry_after_seconds_from_*` 解析错误体里的重置秒数；`RateLimitWindow` / `RateLimitDetails` 等是限流窗口的标准化结构，HTTP 头、SSE 事件和 WebSocket 附加限流共用。
- Codex 语义（`src/openai/codex.rs`）：从请求头与 `client_metadata` 提取 session / thread ID 和请求语义（`codex_responses_request_semantics*`）。
- 请求头分类（`src/openai/headers.rs`）：`is_transport_managed_request_header` 判断某个小写请求头属于 HTTP 传输层字段，API 入站与 Provider 编码共用，保证业务扩展头不混入传输头。
- 常量（`src/openai/mod.rs`）：Codex Responses Lite 头、WebSocket `client_metadata` 投影键、memgen 请求头。

## 文件表

| 文件 | 职责 |
| --- | --- |
| `src/lib.rs` | 仅声明 `openai` 模块 |
| `src/openai/mod.rs` | 常量与 re-export |
| `src/openai/sse.rs` | SSE 增量解码与编码 |
| `src/openai/events.rs` | 响应模型观测、用量、重试秒数、限流结构 |
| `src/openai/codex.rs` | Codex session / thread / 请求语义提取 |
| `src/openai/headers.rs` | 传输层请求头分类 |

## 数据、配置与接口

无存储与配置。公开接口即上述函数与类型，由其他 crate 直接 `use`。

## 改动指引

- 新增事件字段或限流形态时，保持“原始字节优先、旁路解析可失败”的原则；不要让解析失败影响交付。
- 用量字段的含义改变会影响计费与统计，要同步 `backend/crates/providers/openai/src/transport/usage.rs`（`openai_billing_breakdown`）与 `backend/crates/gateway-store` 的统计查询。
- 请求头分类变化同时影响入站（`backend/crates/gateway-api/src/openai/responses/request.rs`）与出站（`backend/crates/providers/openai/src/transport/headers.rs`），两处各有测试。
- 测试在 `tests/openai/`，一个源文件一个测试文件。
