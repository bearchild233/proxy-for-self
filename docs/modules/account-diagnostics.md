# account-diagnostics 插件（账号诊断）

<!-- doc-sync paths: plugins/account-diagnostics -->

## 职责与边界

可选账号工具插件（`account-tools` 分类），在账号列表的行菜单（槽位 `account-menu`，仅管理员）里打开一个诊断弹窗：对**指定账号**运行文本测试，或对 OpenAI OAuth 账号运行“快速降智检测”。插件由三部分组成：

1. **界面**（`ui/Diagnostics.vue`）：选择测试模板、模型、推理强度，发起任务、展示结果与历史、编辑模板。
2. **后台 Worker**（`worker/main.py`）：诊断规则（“怎么测、怎么判定”）。以子进程运行，stdin/stdout 逐行 JSON，**没有凭据、网络和数据库**；需要上游时只能回调宿主能力 `diagnostic.observe`。
3. **模板数据**（`templates.json`，清单 `dataFiles`）：默认模板（连接测试 `hi`、糖果推理题等）；管理员修改后保存在控制面数据根的 `diagnostic-templates.json`。

执行侧：控制面 `services/plugin_host/jobs.py` 启动 Worker（见 [plugin-host](plugin-host.md)），`diagnostic.observe` 在 `packages/plugin-sdk/operations.json` 里映射到网关管理接口 `/api/admin/accounts/diagnostic`，由 OpenAI Provider 的 `backend/crates/providers/openai/src/credential/diagnostic.rs` 执行：Cookie、turn state、凭据材料只留在 Provider 内存，插件只拿到观察结果和随机 `materialRef`；探测只读响应头，不消费 SSE 正文，不做传输重试（`backend/crates/providers/openai/src/transport/probe.rs`）。

## 实现逻辑

- **判定规则**（`worker/main.py::run`）：`mode=text`——单次 `phase: text` 观察，返回内核执行结果；`mode=quick`——`phase: prepare` 取得配套材料（Cookie + turn state，返回 `materialRef`），没有则返回“首次获取不判定”；有则 `phase: probe` 用该材料重放一次 `hi`。仅当 `status == 200`、`sentMaterialRef` 等于刚发送的材料且未失败时才分类：响应返回新 state 为 `29`（降智），未返回为 `21`（满血）；标签只属于被发送的材料，新 state 未经重放不能标注。这是用户约定的实验信号规则（详见 [网关池术语](../gateway-pool.md)），不代表已验证回答质量。
- **任务编排**（控制面 `Platform.call` → `Jobs.start`）：仅管理员；参数校验（`accountId` 字符串、`mode ∈ {quick,text}`、`prompt ≤ 32768`）；每任务 90 秒超时、最多 3 次回调、回调的 `accountId` 必须等于发起账号；任务属于发起会话，可取消；结果写入诊断历史（保留最近 200 条，按账号取最近 30 条展示，只保存阶段、状态、是否返回 state、耗时、时间、分类等字段，不含 Cookie / state 原文）。
- **界面行为**：`quickSupported` 仅当账号 Provider 为 OpenAI 时显示快速检测；模型列表默认 `gpt-6-astra` / `gpt-6.1-sol` / `gpt-6-sol`，再合并 `accounts.models` 的账号可用模型；模板增删改经 `templates.write`（最多 32 条，名称 ≤ 80、提示词 ≤ 32768 字符，ID 唯一）；运行中禁用控件，取消调用 `tasks.cancel`。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/account-diagnostics/plugin.json` | 清单：`account-menu` 槽位页、10 个能力、`worker`、`dataFiles` |
| `plugins/account-diagnostics/ui/Diagnostics.vue` | 诊断弹窗 |
| `plugins/account-diagnostics/ui/entries.json` | 页面入口 |
| `plugins/account-diagnostics/worker/main.py` | 诊断规则 |
| `plugins/account-diagnostics/templates.json` | 默认模板 |

## 数据、配置与接口

能力：`tasks.{start,status,cancel}`、`templates.{read,write}`、`diagnostics.history`、`diagnostic.observe`、`accounts.get.admin.accounts.models`、`auth.*`。数据：控制面数据根下的 `diagnostic-templates.json`、`diagnostic-history.json`。

## 改动指引

- 改判定规则或阶段只改 `worker/main.py`，但 `phase` 的取值与参数受 Provider 端 `backend/crates/providers/openai/src/credential/diagnostic.rs` 的 `deny_unknown_fields` 约束，新增字段要两端一起改；Worker 不得读取凭据或自行联网。
- 任务参数与范围校验在控制面（`services/plugin_host/server.py`、`services/plugin_host/jobs.py`），不要只依赖界面限制。
- 校验：`node scripts/check-plugin.mjs account-diagnostics`、`node scripts/build-plugin.mjs account-diagnostics`。
