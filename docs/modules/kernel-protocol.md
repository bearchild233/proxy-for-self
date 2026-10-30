# kernel-protocol

<!-- doc-sync paths: backend/crates/kernel-protocol -->

## 职责与边界

内核与服务插件共同依赖的 wire 合同（`KERNEL_API` 当前 0.8）：消息信封、握手、钩子参数与结果、内核能力参数与结果、帧编解码。叶子 crate，只依赖 serde / serde_json / tokio 的 io-util，不依赖任何 workspace crate，不含调度、授权或连接实现。

使用方：`gateway-core::plugin`（`pub use kernel_protocol::*`，加上钩子与能力端口）、`gateway-host` 的插件运行时（连接、握手、分发）、插件 SDK（`plugins/sdk/plugin-sdk`）。Python 侧 `services/updater/kernel_protocol.py` 是只含管理连接的最小客户端，帧格式与本 crate 一致。设计背景见 [微内核设计](../microkernel.md)。

## 实现逻辑

- 帧（`src/frame.rs`）：4 字节大端长度 + UTF-8 JSON，上限 `MAX_FRAME_BYTES`（1 MiB）。超限长度在分配缓冲区之前拒绝；读到截断、尾随字节或非法 JSON 都按协议错误处理，调用方必须断开连接。
- 消息（`src/message.rs`）：`Message` 为 `request{id,method,params}`、`response{id,result|error}`、`notify{method,params}` 三种，按 `kind` 标签区分；两端各自编号。错误码固定六个：`not_supported` `invalid_params` `unavailable` `timeout` `forbidden` `internal`。
- 版本与握手：`HelloParams`（内核发出：`kernelApi`、内核版本与槽位、`role`）与 `HelloResult`（插件返回：插件 ID 与内容版本、SDK 版本、`kernelApi{major,minMinor}`、`hooks`、`capabilities`、`ownedGroups`、`watchedGroups`）。`is_compatible`：主版本相同、插件要求的最低次版本不高于内核；0.x 期间最低次版本必须 ≥ 4。`ConnectionRole` 缺省 `kernel`，`admin` 只允许 `health` 与 `admin.call`。`ownership.changed` 通知携带最新归属与观察分组。
- 钩子（`src/hooks.rs`）：
  - `route.select`：入参候选账号、是否有普通账号可用、固定账号；结果 `preferred`（优先顺序）、`live`（持窗账号）、`blocked`（不可用账号与原因）、`pinnedDecision`（`stay`/`escape`）、可选 `ordinary`（本次按普通账号调度的成员，内核不再为它们调用请求钩子；旧内核忽略该字段，这些账号仍按池成员处理并失败关闭）。`watchedGroups` 条目除具体分组外可用 `*`（所有不归插件所有的账号）与 `!分组`（排除），规则见 [gateway-core](gateway-core.md)。
  - `route.prepare`：入参账号、尝试序号、期限、是否可安全重放；结果为 `use{accountId,leaseId,upstream{cookie,turnState}}`、`wait{retryAfterMs,reason}`、`fail{reason}`。
  - `response.head`：入参状态码、是否返回 turn state、Set-Cookie、耗时；结果 `accept` / `retry{delayMs}` / `switch` / `fail`。
  - `request.finish`：通知，携带结果（completed/failed/cancelled/interrupted）、耗时、首字节耗时、用量。
  - `response.observe`：观察分组账号的响应头，结果 `accept` 或 `switchAccount`；`canSwitch` 表示内核是否允许换号。
  - `admin.call`：管理面直连，`{method, params, actor.role}`。
- 能力（`src/capabilities.rs`）：`upstream.probe`（含 `includeAccountCookies`，可选 `requestId`：同一次打票的多次探测共用的上游请求 ID）、`http.fetch`（`Egress::Direct` 或 `Account`）、`accounts.read`（可带 `model`，结果含 `modelAllowed`）、`accounts.warmup.read/run`（安全账号和模型强度目录、有界提示词的一次性维护请求及 `sendState`/`shortTerm` 结果）、`groups.move`、`quota.read`（可带 `refreshIfMissing`）、`log`（通知）。0.8 的预热参数增加 `prompt`/`reasoningEffort`，缺省仍为 `hi`/`auto`，模型目录的 `reasoningEfforts` 缺省为空；其它资格与发送边界不变。

## 文件表

| 文件 | 职责 |
| --- | --- |
| `src/lib.rs` | 模块声明与 re-export |
| `src/message.rs` | 信封、错误码、`KERNEL_API`、握手与归属、健康与排空结果、版本兼容判断 |
| `src/frame.rs` | 帧编解码与异步读写 |
| `src/hooks.rs` | 五个请求路径钩子的参数与结果 |
| `src/capabilities.rs` | 内核能力的参数与结果，包括账号预热 read/run |

## 数据、配置与接口

无存储与配置。对外接口就是上述类型；序列化字段名全部 camelCase，未知字段忽略，新增字段必须带 `#[serde(default)]`。

## 改动指引

- 新增钩子或能力、改字段语义都是协议变化：递增 `KERNEL_API` 次版本，同步 `services/updater/kernel_protocol.py` 与 `services/updater/service_manifest.py` 的版本常量、`plugins/sdk` 的 README、[微内核设计](../microkernel.md) 的钩子与能力表，并在汇报中写明偏离。
- 字段只增不改；旧插件必须仍能握手。`backend/crates/gateway-host/src/plugin/registry.rs` 的注册表加载里有授权能力白名单，新增能力要同步。
- 帧上限、错误码是长期合同，不要为单个插件调整。
- 测试在 `tests/`，与 `src/` 文件一一对应（`frame.rs`、`hooks.rs`、`capabilities.rs`、`message.rs`），入口 `tests/main.rs`。
