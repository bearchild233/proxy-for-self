# groups 插件（分组管理）

<!-- doc-sync paths: plugins/groups -->

## 职责与边界

基础界面插件（页面 `/groups`，仅管理员，不可停用）：账号分组的列表、创建、编辑、启用/停用、删除，展示成员数、容量与用量（成员关系在账号编辑里维护），管理分组显示顺序。纯界面，**没有后端进程**；分组规则（Key 的账号范围 = 无分组全部账号 / 启用分组并集；绑定分组为空则空池）由后端执行，见 [架构](../architecture.md) 第 6 节。

- 依赖：`/api/admin/account-groups*`（`backend/crates/gateway-api/src/admin/account_groups.rs`）、Key 列表与绑定接口（统计哪些 Key 绑定了分组）、主壳基础组件。
- 网关池插件把“网关池分组”作为归属分组使用（见 [网关池](../gateway-pool.md)）；动态分组的成员变更由内核 `groups.move` 能力写入，本页只读展示结果。

## 实现逻辑

- `ui/page/index.vue`：表格、筛选、创建/编辑弹窗、启用停用与删除确认。
- `ui/page/composables/useAccountGroups.ts`：列表查询与全部变更。创建/更新提交 `AccountGroupFormValue`（名称、描述、颜色、`disableFast` Fast 限制开关），启用/停用/删除后重新加载；绑定 Key 数通过 Key 列表与绑定接口补齐；顺序保存走 `display-order.save`（`scope: groups`，控制面限制只能改自己的范围）。
- `components/`：`AccountGroupFormModal`（表单）、`AccountGroupMetricsCell`（容量与用量）、`AccountGroupActions`、`AccountGroupFilters`。
- 页面可见时每 30 秒静默同步，切页保留筛选与草稿（平台通用行为）。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/groups/plugin.json` | 清单与 11 个能力 |
| `plugins/groups/ui/entries.json` | 页面入口 |
| `plugins/groups/ui/page/index.vue` | 页面 |
| `plugins/groups/ui/page/components/` | 表单、指标单元格、操作与筛选 |
| `plugins/groups/ui/page/composables/useAccountGroups.ts` | 数据与变更 |
| `plugins/groups/ui/page/constants.ts` | 颜色与列常量 |
| `plugins/groups/tsconfig.json` | 单插件类型检查配置 |

## 数据、配置与接口

能力：`account-groups.{get,create,update,enable,disable,delete}`、`api-keys.get.admin.client-keys`、`api-keys.post.admin.client-keys.bindings`、`display-order.save`、`auth.*`。无本地存储。

## 改动指引

- 分组字段增减：后端 wire / Admin 用例 / Store 先改，再改 `frontend/src/api/modules/account-groups.ts` 与本插件；删除分组会级联清理绑定，改动后核对 `gateway-store` 的分组仓库测试。
- 校验：`node scripts/check-plugin.mjs groups`、`node scripts/build-plugin.mjs groups`。
