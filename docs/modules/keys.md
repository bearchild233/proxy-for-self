# keys 插件（API 密钥）

<!-- doc-sync paths: plugins/keys -->

## 职责与边界

基础界面插件（页面 `/keys`，仅管理员，不可停用）：Client API Key 的列表、创建、编辑、启停、删除，账号/分组绑定与范围，限额（日/七天金额、并发、RPM）与金额重置，查看明文并生成客户端配置。**没有后端进程**；准入、限额与结算由 `gateway-core` / `gateway-store` 执行，见 [架构](../architecture.md) 第 6 节“Client Key 限额与结算”。

- 依赖：`/api/admin/client-keys*`（`backend/crates/gateway-api/src/admin/client_keys.rs`）、账号与分组列表（绑定选择）、客户端配置生成（`plugins/client-access` 的配置面板与 `ccswitchImport` / `codexConfig` 工具，经主壳 `frontend/src/utils/` 的 re-export 使用）。

## 实现逻辑

- `ui/page/index.vue`：表格、筛选、批量操作、各弹窗编排。
- `useApiKeysQuery`：分页查询、排序、搜索、状态筛选；`useApiKeyMutations`：创建（`create`，可同时绑定账号/分组 `create-bound`）、更新、启停、删除、绑定（`set-binding` / `bindings`）、金额重置（`reset-budget`）、客户端画像选择（OpenAI / xAI）；`useApiKeyUse`：“使用密钥”弹窗——reveal 得到明文后补挂到行上，推导服务根地址、生成 Codex 配置与 CC Switch deeplink。
- `components/`：`ApiKeyCreateModal`（账号范围模式：单账号 / 账号池 / 分组；限额；轮转方式；Excel Bridge 开关；画像）、`ApiKeyBudgetCell` / `ApiKeyBudgetResetModal`（金额进度与重置）、`ApiKeyScopeCell`、`ApiKeyPrefixCell`、`ApiKeyStatusBadge`、`ApiKeyIdentityCell`、`ApiKeyActions`、`ApiKeyFilters`。
- 明文只在显式 reveal 时取得，关闭弹窗即清除，不持久化到浏览器。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/keys/plugin.json` | 清单与 24 个能力 |
| `plugins/keys/ui/entries.json` | 页面入口 |
| `plugins/keys/ui/page/index.vue` | 页面 |
| `plugins/keys/ui/page/components/` | 创建/重置弹窗与各单元格 |
| `plugins/keys/ui/page/composables/` | 查询、变更、使用密钥 |
| `plugins/keys/ui/page/constants.ts` | 列与选项 |

## 数据、配置与接口

能力：`api-keys.*`（列表、reveal、create、create-bound、update、enable、disable、delete、set-binding、bindings、reset-budget）、账号与分组读取、`display-order.save`（`scope: keys`）、设置读取。限额零表示不限；修改限额不重置已用金额；手动重置在同一事务清零所选周期、推进计费起点并写审计。

## 改动指引

- Key 字段变化：后端 Admin 模型 → `backend/crates/gateway-store/src/postgres/client_keys.rs` → `frontend/src/api/modules/api-keys.ts` → 本插件；绑定语义（单账号、显式账号池、动态分组）不要在前端重新解释。
- 绑定与 Key 级插件设置在 `backend/crates/gateway-store/src/postgres/key_bindings.rs` 的同一事务里写。
- 校验：`node scripts/check-plugin.mjs keys`、`node scripts/build-plugin.mjs keys`；前端测试 `frontend/tests/key-config.test.mjs`、`frontend/tests/key-import-action.test.mjs`。
