# excel-bridge 插件（Excel Bridge 原生 Worker）

<!-- doc-sync paths: plugins/excel-bridge -->

## 职责与边界

可选插件（`extensions` 分类，设置页 `settings` 槽位，管理员与 Key 可见），由两部分组成：

1. **界面**（`ui/Excel.vue`、`ui/overview.html`）：显示 Worker 的安装/启用/版本/操作状态和使用说明；每 30 秒轮询 `excel.status`。管理员的安装、更新、启用、停用、卸载在“插件管理”里触发。
2. **私有 Python Worker**（`src/excel_codex_bridge/`，包名 `proxy-for-self-excel-worker`，入口 `proxy-for-self-excel`）：运行在独立 Linux 环境（Python 3.10 运行包），通过**私有 Unix socket** 被 Rust 网关调用，把 Excel add-in 的上游当作一个额外的 Responses/Images 后端。Worker 不读取本机认证、不自行换账号：身份和凭据由已鉴权的 Rust 网关在每次请求里提供。

该 Worker 不是服务插件（没有 `service` 段、不走内核协议），而是**原生 Worker**（`protocol: 1` 清单，生命周期由 updater 的 `PluginManager` 管理，见 [updater](updater.md) 和 [原生 Worker 合同](../plugins.md)）。网关侧的接入在 `backend/crates/providers/openai/src/provider/excel.rs`（Key 级 Excel 分发，共享 Worker，账号 lease 与请求生命周期仍由 Core 保持）；配置 `openai.excel_worker_socket` 与 `openai.excel_plugin_state_file` 必须分别指向 Worker socket 与同一个 `state.json`（Worker 环境变量 `EXCEL_PLUGIN_STATE_FILE`）。

来源与许可：`excel_upstream.py`、`excel_stream.py`、`sse.py`、`item_ids.py` 等取自 Unlicense 项目 ghcp_proxy / excel-codex-bridge，尽量保持原样以便重新同步；`plugins/excel-bridge/LICENSE` 为许可证。

## 实现逻辑

- **服务入口**（`hub_worker.py::HubWorker`，ASGI/uvicorn）：只接受 UDS（有 client 地址或 Origin 头即 403；拒绝 WebSocket）；`GET /healthz`（允许在禁止业务准入时调用，用于安装器候选验证）、`POST /plugins/rpc/status`（仅 admin，返回并发与模型数）、`GET /internal/catalog`（Excel 模型目录）、`POST /internal/responses|images/generations|images/edits`（业务）。并发默认 4（`--max-concurrency`），满额 503；请求超时 600 秒；正文上限 8 MiB；空闲 300 秒无请求后优雅退出（socket 仍由 systemd 持有）。每个请求先 `_prepare(envelope)`：要求已启用且带版本的 Excel 绑定、凭据账号与绑定一致、访问令牌有效，校验出站代理；拒绝 `previous_response_id`（必须带完整历史）与 `input_file`；令牌轮换隔离旧回放，不跨凭据代次续接。会话回放只在内存命名空间里，不写磁盘，请求结束清除。
- **业务适配**（`server.py::Bridge`）：`responses` 校验模型必须属于 Excel 模型集合（`excel_upstream.MODEL_IDS`），把 Codex 请求转换为 add-in 上游请求；Excel 上游拒绝客户端声明的工具，`excel_upstream.py` 把 Codex 工具描述放进提示词，模型通过 add-in 的 `run_officejs` 函数调用，`excel_stream.py` 把该流式调用转回 Codex 的工具调用事件；`images.py` 把内联图片上传到 add-in 的上传接口（上游拒绝内联时），`image_generation.py` 把 `image_gen` 工具的生成/编辑请求映射到 add-in 的图片接口（保留原桥参数与 PNG 限制）；`codex_config.py` 提供 Excel 模型目录；`request_body.py` 有界解码（含压缩）；`sse.py` SSE 解析与 OpenAI 风格错误。
- **准入门**（`plugin_gate.py::PluginGate`）：长期推理请求持有 `activity.lock` 共享锁直到流结束，获得锁后再次检查准入；管理器排空时拒绝新请求，保证更新不会切断在途流。
- **打包**：`services/updater/package_plugin.py` 从已验证的 Linux Python 3.10 环境生成运行包（锁定依赖 `requirements.lock`、Worker 源码、许可证）；运行包与界面包分开发布。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/excel-bridge/plugin.json` | 界面包清单（`settings` 槽位页，能力仅 `excel.status`） |
| `plugins/excel-bridge/ui/` | `Excel.vue`、`overview.html`、`entries.json` |
| `plugins/excel-bridge/src/excel_codex_bridge/hub_worker.py` | UDS ASGI 入口、并发/超时/空闲退出、请求准备与校验 |
| `plugins/excel-bridge/src/excel_codex_bridge/server.py` | `Bridge`：Responses 与 Images 适配 |
| `plugins/excel-bridge/src/excel_codex_bridge/excel_upstream.py` | Excel add-in 上游支持（模型集合、提示词工具化、请求构造）——Unlicense 引入 |
| `plugins/excel-bridge/src/excel_codex_bridge/excel_stream.py` | 工具调用流转换 |
| `plugins/excel-bridge/src/excel_codex_bridge/images.py` `image_generation.py` | 图片上传与生图/改图映射 |
| `plugins/excel-bridge/src/excel_codex_bridge/codex_config.py` `request_body.py` `sse.py` `item_ids.py` `plugin_gate.py` | 目录、有界解码、SSE 工具、item ID、准入门 |
| `plugins/excel-bridge/tests/` | pytest 用例（Worker、上游、图片、流完成、准入门、回放范围） |
| `plugins/excel-bridge/pyproject.toml` `requirements.lock` `requirements-test.lock` | 打包与锁定依赖 |

## 数据、配置与接口

- 命令行：`--socket` 或 `--fd`（继承 systemd socket）、`--max-concurrency`、`--idle-timeout`；环境变量 `EXCEL_PLUGIN_STATE_FILE`。socket 目录必须归 Worker 用户所有且权限 0700 级别。
- 生命周期：关闭新准入 → 取全流活动锁排空 → 停旧 Worker → 原子替换 runtime → 检查自动重启策略与私有 `healthz` → 开放准入；失败恢复旧目录与启用状态；主网关不得 Requires / BindsTo Worker。卸载删除 runtime，账号、密钥绑定与用量保留；暂停 Excel Key 不会自动变成 Native。
- 能力 `excel.status`：控制面对管理员返回安装器状态，对 Key 身份固定返回已安装已启用。

## 改动指引

- Worker 业务改动不需要编译 Rust，但**运行包**必须重新用 Linux Python 3.10 环境打包并经安装器更新；界面包的版本单独走插件管理。
- 测试：`python tools/test-excel-worker.py`（创建临时隔离环境，安装锁定依赖和实际安装包，`pip check` 后跑 pytest，可追加 pytest 参数）；不要用开发环境里直接跑 pytest 代替。新增依赖同步 `pyproject.toml` 与两个 lock。
- 与上游引入文件保持可重新同步，改动尽量放在适配层；错误正文和日志禁止出现输入、令牌或代理 URL。
- 内核侧 Excel 分发规则（Key 的 `excel_bridge_enabled`、不参与文本模型映射等）见 `backend/crates/providers/openai/src/provider/excel.rs` 与 [架构](../architecture.md)。
