# client-access 插件（客户端接入）

<!-- doc-sync paths: plugins/client-access -->

## 职责与边界

可选界面插件（`clients` 分类），提供一个**嵌入式槽位**页面 `key-config`（管理员与 Key 持有者都可见，无独立路由）：为某个 Client Key 生成客户端配置文件（Codex `config.toml`、`auth.json`，Excel Key 另有模型目录 JSON，另提供 Hermes `config.yaml` 片段与能力说明）、复制内容，以及通过 CC Switch deeplink 一键导入。**没有后端进程**；调用方是管理端 Key 列表（`keys` 插件的“使用密钥”）和 Key 自助页（`usage-analytics` 的 key-usage），由宿主 `openPlugin('client-access', {apiKey, apiBaseUrl})` 以弹窗方式打开。

- 依赖：模型列表经能力 `client.models`（控制面用该 Key 请求网关 `/v1/models` 并按 ID 去重返回）、设置与客户端下载读取、`key-usage.*` 能力（Key 身份）。
- 主壳对本插件的 `utils/ccswitchImport.ts`、`utils/codexConfig.ts` 做 re-export（`frontend/src/utils/ccswitchImport.ts`、`frontend/src/utils/codexConfig.ts`），所以生成逻辑只有一份，管理端 Key 列表也复用。

## 实现逻辑

- `ui/ClientAccess.vue`：槽位入口，读取宿主上下文（`apiKey`：名称、明文 key、路由模式、固定账号、`excelBridgeEnabled`；`apiBaseUrl`），挂载 `ConfigPanel`，关闭时 `host('close')`，复制走 `host('copy')`。
- `ui/ConfigPanel.vue`：弹窗主体。打开且有 Key 时加载模型列表（`getClientModels`，可取消，Key 或 Excel 开关变化重载）；Excel Bridge 启用的 Key 使用 Excel 专用默认模型并检查 Excel 插件可用性；区分已绑定账号/分组 Key 与普通 Key 的提示；展示生成的文件内容并逐个复制，提供 CC Switch 导入按钮。明文仅在弹窗打开期间存在于内存，关闭即清除。
- `utils/codexConfig.ts`：`buildCodexConfigFiles` 按基础地址、Key、模型、是否启用 WebSocket、是否 Excel 通道生成配置（默认模型 `CODEX_DEFAULT_MODEL`，Excel 默认 `CODEX_EXCEL_DEFAULT_MODEL`，provider ID `custom`，WebSocket 默认关闭且 Excel 通道强制关闭；`requires_openai_auth = false`，并带 `X-OpenAI-Actor-Authorization` 标记声明服务端托管账号认证，客户端据此开放原生生图——该头不是密钥）。
- `utils/ccswitchImport.ts`：`buildCodexCcSwitchImportDeeplink` 组装 `ccswitch://` 导入链接。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/client-access/plugin.json` | 清单：槽位页 `key-config`（admin + key）、13 个能力 |
| `plugins/client-access/ui/entries.json` | 页面入口 |
| `plugins/client-access/ui/ClientAccess.vue` | 槽位入口 |
| `plugins/client-access/ui/ConfigPanel.vue` | 配置弹窗 |
| `plugins/client-access/ui/utils/codexConfig.ts` `plugins/client-access/ui/utils/ccswitchImport.ts` | 配置与 deeplink 生成 |

## 数据、配置与接口

能力：`client.models`、`key-usage.get.key-usage.{config,overview,records,version}`、`settings.get.admin.settings{,.admin-api-key,.client-downloads.codex-desktop.windows}`、`api-keys.post.admin.client-keys.bindings`、`auth.*`。无存储，Key 明文不持久化。

## 改动指引

- 客户端配置格式或默认值变化只改 `utils/codexConfig.ts`（同时更新 `deploy/README.md` 的客户端配置示例与 [架构](../architecture.md) “Codex 原生生图与认证”）。
- 修改 `ConfigPanel` 时保证关闭后清除明文；不要把 Key 放进 URL 查询串或日志。
- 校验：`node scripts/check-plugin.mjs client-access`、`node scripts/build-plugin.mjs client-access`；`frontend/tests/key-config.test.mjs` 覆盖配置生成。
