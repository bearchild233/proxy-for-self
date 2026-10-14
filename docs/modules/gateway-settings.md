# gateway-settings 插件（网关设置）

<!-- doc-sync paths: plugins/gateway-settings -->

## 职责与边界

基础界面插件（不可停用），使用 `layout: settings` 的三个设置页：

| 页面 ID | 路由 | 内容 |
| --- | --- | --- |
| `main` | `/settings` | 基础设置：入口容量（并发/正文预算）、运行设置、轮转策略、请求队列、账号自动冻结 |
| `upstream` | `/settings/upstream` | 上游配置：Token 刷新、客户端画像（OpenAI / xAI）、全局请求位置、模型别名 |
| `access` | `/settings/access` | 安全与访问：管理员 API Key、管理员密码、登录防护（封禁/解封）、客户端版本策略与下载 |

**没有后端进程**；设置的校验、持久化、revision 推进与快照发布都在后端 `backend/crates/gateway-admin/src/use_case/settings.rs` 与 `backend/crates/gateway-store/src/postgres/runtime_settings.rs`。页头与“保存/撤销”由主壳 `SettingsHostHeader` 持有，本插件通过宿主操作 `settings-actions` 报告 `changed/saving/saveDisabled/resetDisabled`（仅本插件的设置页允许）。

## 实现逻辑

- `ui/page/index.vue`：按路由名选分区，分区首次访问才挂载（`visited` 集合），切换保留表单草稿；基础与上游设置共用一个表单 `useSettingsForm`，统一保存/撤销。
- `composables/useSettingsForm.ts`：读取 `settings.get`，维护草稿与脏标记，整体保存 `settings.update`（只提交实际字段）；全局请求位置单独保存（`settings.request-location`，串行并合并连续选择，只更新位置字段，不覆盖其他管理员的并发设置）；请求期间用户开始编辑或保存时，不用旧读取结果覆盖新表单。
- 卡片：`InferenceLimitsCard` `RuntimeSettingsCard` `RotationStrategyCard` `RequestQueueCard` `AccountAutoFreezeCard`（容量熔断开关与阈值）`TokenRefreshCard` `ClientProfileCard`（调用 `client-profiles` 预览生效画像）`RequestLocationCard` `ModelAliasesCard`；访问分区：`SettingsAccessSection`（`AdminApiKeyCard` 与 `useAdminApiKey` 管理员 API Key 生成/删除、`AdminPasswordCard` 与 `ChangePasswordModal`、`LoginProtectionCard` 登录防护、`client-version/` 客户端最低版本策略与 Windows 离线下载，`useClientDownloads` 惰性请求下载信息）。
- `components/PluginsSection.vue`：原生插件（Excel 等）的启停入口，经 `/api/admin/system/plugins` 与 `PluginSlot`。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/gateway-settings/plugin.json` | 清单：3 个设置页、16 个能力 |
| `plugins/gateway-settings/ui/entries.json` | 三个页面入口（同一个 `ui/page/index.vue`） |
| `plugins/gateway-settings/ui/page/index.vue` | 分区编排与保存/撤销 |
| `plugins/gateway-settings/ui/page/composables/` | `useSettingsForm`、`useAdminApiKey` |
| `plugins/gateway-settings/ui/page/components/` | 各设置卡片、`client-version/` 子目录、`PluginsSection` |

## 数据、配置与接口

能力：`settings.{get,update,request-location}`、管理员 API Key 的读取与 regenerate / delete、`client-downloads.codex-desktop.windows`、`client-profiles.{openai,xai}` 读取与 preview、`system.get.admin.system.login-protection` 与 policy / unban、`auth.*`。设置字段和范围以后端校验为准，前端只做易用性约束。

## 改动指引

- 新增设置项：后端 `RuntimeSettings` 模型与 Store 列（迁移只追加）→ Admin 用例与 API wire → `frontend/src/api/modules/settings.ts` → 本插件表单与卡片；设置变更需要推进 `config_revision` 并发布快照（后端负责）。
- 保存逻辑保持“只提交显式字段”，并发保存不得用旧读取结果覆盖。
- 管理员 API Key 明文只在生成响应中出现，页面不得持久化。
- 校验：`node scripts/check-plugin.mjs gateway-settings`、`node scripts/build-plugin.mjs gateway-settings`；前端测试 `frontend/tests/settings-actions.test.mjs`。
