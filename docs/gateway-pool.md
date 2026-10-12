# 网关池插件

为降智账号所在的分组，按“账号 × 网关”轮换满血网关。降智信号在交付前拦截，用户只感到等待稍长。插件 UI 与策略可热更新；传输桥在 Rust 内核，首次启用需要内核与控制面升级。

## 术语

| 术语 | 含义 |
| --- | --- |
| 网关 | OpenAI 上游的接入节点，名称形如 `unified-142`。同一账号经不同网关访问，可能得到不同质量的服务。 |
| 池服务 | 外部网关池服务（默认 `https://pool.0102400.xyz`）。`/gateways` 列出可用网关，`/cookie` 返回指定网关的 `__oailb` 与 `__cflb` 路由 Cookie。使用单独的池服务 Bearer，与 OpenAI 凭据无关。 |
| 池成员标识 | 向池服务声明的账号身份，默认自动生成 `gwpool-member:<上游账号 ID>/<上游用户 ID>`；管理员可在设置中覆盖。 |
| 组合 | 一个“账号 × 网关”。满血窗口、静置、学习和统计都以组合为单位。 |
| 打票模型 | 检测网关用的模型，默认 `gpt-6.1-sol`，reasoning low，输入 `hi`，无工具。 |
| 票 / 找票 | 票是某个模型在某个组合上的 state 与配套 Cookie。找票指用打票模型为账号寻找判定为 21 的网关。 |
| S0 | 携带池 Cookie 第一次访问上游（bootstrap）返回的 `x-codex-turn-state`。此后该模型在该组合上的请求固定带这份 state，不采用上游后续返回的新 state。 |
| bootstrap | 取 S0 的第一次请求，不做判定。 |
| 21 / 29 | 携带票重放后，在 HTTP 响应头判定：HTTP 200 且**不返回** `x-codex-turn-state` 为 21（满血）；HTTP 200 且**返回**该字段为 29（降智）。只看是否出现，不比较取值。这是约定的实验信号，不代表已验证回答质量。 |
| 拿票 | 窗口内来了其他模型的请求、该模型还没有票时，用该模型发一次 `hi` 取 state，拿到响应头即断开，不做第二轮判定。 |
| 检测失败 | 401、402、429、超时、网络错误、缺少配套材料，或 `__oailb` JWT 的 `host` 与指定网关不同。记录真实状态码，不记为 29。 |
| 满血窗口 | 组合从首次实际发送起的使用时间，默认 180 秒，所有模型共用。到期后开始准备下一组合，旧组合继续服务直到新组合验证通过。 |
| 静置 | 组合退役后禁止访问的时间，从最后一次实际访问上游结束时起算；期间任何真实访问都会重新计时。 |
| 退避档 / 学习起点 | 静置按初始档（默认 2h）→ ×2 → … → 上限（默认 24h）。复测仍为 29 或失败就升档。同一组合连续两轮在同一档恢复，以后从该档开始；恢复档不同则回到初始档。只记最后一档，不累计。 |
| lazy / continuous | 懒模式：没有请求时不找票，窗口到期后由新请求触发准备。持续模式：近期有流量的账号在空闲时也按窗口轮换。 |

## 行为

**范围**：只对设置中选定分组的 OAuth 账号生效。分组内所有模型的 HTTP 请求经过网关池；原生 WS 请求没有逐次响应头，无法判定，直接拒绝。其他分组不受影响。

**找票**：取指定网关的池 Cookie → 打票模型 bootstrap 取 S0 → 带 S0 与 Cookie 重放判定。探测拿到响应头立即关闭，不读 SSE。错误最多重试一次，遵守 5 秒内的 Retry-After；仍失败的组合与 29 一样静置，但分开记录。候选之间默认间隔 50ms，探测最多 4 路并发，同一账号同时只有一个找票任务，单组合准备预算 35 秒。额度耗尽、限流冷却、停用、归档或无打票模型权限的账号不找票。

**票按模型分开、窗口共用**：打票模型的 S0 随找票得到。窗口内来了其他模型的请求而该模型没有票时，先拿票（不判定），之后照常判定。同组合同模型的拿票在进程内合并，并写入共享存储供 A/B 两槽复用。组合退役或换代后，所有模型的票一并作废。

**业务请求**：覆盖客户端自带的 state，固定发送该模型的票与配套 Cookie。21 完整交付；29 在任何输出前关闭响应，组合立即退役，满足安全条件时同账号换网关重放原请求：无工具或仅客户端工具、没有已交付内容。含上游工具（如 web_search）的请求不盲目重放。跨账号回退交给原有会话与安全续接规则。

**轮换**：窗口到期后准备下一组合；新组合验证通过后，所有尚未发送的请求切换过去，已发送的照常完成；旧组合最后一个在途请求结束后才开始静置。全部候选不可用时，在请求截止时间内等待，到期明确报错，不会使用已停用的组合。

## 已验证事实

以下来自 2026-10-12 的真实实验，均以响应头信号为准。

- **保活**（测试账号 1，69 分钟，持续模式）：397 次请求中 29 共 29 次，切换 29 次；满血窗口 79–184 秒，中位数约 125 秒；找票中位数 3.3 秒。1h 静置后复测 24 个组合只有 1 个恢复，因此默认初始静置定为 2h。
- **换网关缓存**（测试账号 2，固定 session 与 `prompt_cache_key`）：7 次网关切换后的请求全部命中约 98.9%；追加对话 18 轮，除冷启动外全部命中。换网关不必然丢缓存。
- **同一张票双缓存**（测试账号 3）：同一 S0、同一网关交替发送两段不同前缀（各自 session 与 cache key），第二、三轮都命中 12,032 tokens。同一个 turn state 可以同时保留多份缓存。
- **跨模型**：Astra 的票上 6.1 Sol 返回 state（8/8，且不是回显原值），而夹在中间的 Astra 为 21；Sol 的票上 Astra 返回 state（3/3）。票必须按模型分开。

## 实现位置

| 组件 | 位置 |
| --- | --- |
| Rust 传输桥：路由、找票、拿票、额度准入 | `backend/crates/providers/openai/src/gateway_pool.rs` |
| 业务执行：固定票、交付前拦截、安全重放 | `provider/execution.rs` 中 `GatewayPoolAttempt` |
| 分组判定与 WS 拒绝 | `provider/mod.rs` |
| 持久化运行时 | `services/plugin_host/gateway_pool.py`（SQLite：设置、组合、租约、样本、切换、私有材料与各模型的票） |
| 策略（判定、退避、学习、窗口） | `plugins/gateway-pool/worker/policy.py` |
| 管理页 | `plugins/gateway-pool/ui/page/` |
| 授权实验入口 | Cargo 特性 `gateway-pool-experiment` 与 `tests/admin/gateway_pool.rs` 中带 `#[ignore]` 的测试，默认构建不含 |

Rust 网关经回环私有接口 `POST /api/plugin-platform/internal/gateway-pool` 调用控制面，使用独立的 64 位十六进制能力密钥（`gateway-pool-runtime.key`），不接受客户端 API Key。管理视图只返回组合状态、样本和已拿票的模型名，不返回 state、Cookie、账号 Token 或池服务凭证。

## 部署

```toml
[openai.gateway_pool]
runtime_url = "http://127.0.0.1:<控制面端口>/api/plugin-platform/internal/gateway-pool"
key_file = "<控制面根目录>/gateway-pool-runtime.key"
```

- `runtime_url` 必须指向当前活动的控制面槽；切换控制面槽时同步网关配置。端口以 VPS 实际状态为准。
- 能力密钥由控制面生成（0640），网关进程需能读取。
- 首次启用需同时升级 Rust 内核（A/B 发布）和控制面 `server.py`；之后 UI 与策略可单独热更新。
- 状态库位于 `<控制面根目录>/gateway-pool-runtime/state.sqlite3`，备份时与能力密钥一起保留并限制访问。

## 验证

- Python：`python -m unittest discover -s services/plugin_host -p "test_gateway_pool.py"`，覆盖持久化、A/B 合并、退避学习、计时、各模型的票与作废、权限与策略包安装。
- Rust：`./scripts/build-local.ps1 -Task test -Package provider-openai -CargoArgs @('--features','gateway-pool-experiment','--test','main','admin::gateway_pool')`，mock 覆盖完整找票、错误重试、29 拦截与重放、工具副作用保护、额度耗尽、跨模型拿票。
- 前端：`node scripts/check-plugin.mjs gateway-pool`、`node scripts/build-plugin.mjs gateway-pool`、相关 lint 与 `pnpm --dir frontend typecheck`。
- 本机预览：`python .build/preview/gateway-pool-preview.py`（模拟数据，仅回环，不连上游），入口 `/extensions/gateway-pool`。

## 尚未验证

- 2h 及以上静置档的恢复率。
- 原生 WS 下的判定方式（当前直接拒绝）。
- 池服务在大量账号并发下的限流行为；池服务自身的限流错误目前按检测失败处理。
