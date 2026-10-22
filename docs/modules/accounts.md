# accounts 插件（账号管理）

<!-- doc-sync paths: plugins/accounts -->

## 职责与边界

基础界面插件（`kind: ui`、`runtime: vue-component`、页面 `/accounts`，仅管理员，不可停用/卸载）：账号列表与筛选、导入与 OAuth 授权/重新授权、编辑与批量编辑、额度/用量/订阅展示、个人资料、额度预测与重置卡、导入任务、归档视图。**没有后端进程**：全部数据来自网关管理 API，经平台命名能力调用（`plugin.json` 的 `capabilities` 逐项对应 `packages/plugin-sdk/operations.json`）。

- 依赖：`gateway-api` 的 `/api/admin/accounts*`（实现链见 `gateway-api` → `gateway-admin` → `gateway-store`）、主壳基础组件、`packages/ui-kit/accounts`（用量窗口、套餐徽标）。
- 业务规则（账号状态、归档、调度字段）归后端，这里只展示与发起操作。

## 实现逻辑

**养号设置**：账号编辑弹窗的 `AccountQuotaProtectionFields.vue` 提供每账号开关和 1–99 的整数使用上限，默认 90%，默认关闭。达到阈值时由网关拒绝新调度，界面通过独立的 `quotaProtectionReached` 标注本地保护，不改写官方额度与状态。关闭时保留打开表单时已保存的阈值，隐藏输入不阻止关闭；未修改养号时省略该字段，分组窄更新与批量普通设置不覆盖它。旧网关缺少合法 `quotaProtection` 时禁用控件；保存后回读验证，无法确认时不显示成功。无有效额度时不能保证预留，在途与观测延迟也可能超过阈值。

页面入口 `ui/page/index.vue`：表格列插槽、删除确认框、归档/当前视图切换、批量操作栏；所有数据流经 composables：
- `useAccountsQuery`：`usePagedQuery` 分页（默认 20 条），参数含 `archived`、搜索、provider、状态、分组、需处理（attention）、排序；30 秒页面可见时后台静默同步，并按需补查订阅（缓存）；`summary` 驱动概览卡。
- `useAccountMutations`：删除（单个/批量同一接口 `accounts.delete`，按 provider 分组）、导出、刷新凭据、刷新额度、恢复状态、主动重置额度。
- `useAccountOnboarding`：导入（AT/RT、OAuth JSON、文件）与 OAuth start/complete；重新授权复用同一弹窗（`oauth/start {accountId}`）。
- `useAccountEditor` / `useAccountBatchEditor`：单账号与批量编辑（分组、权重、并发、模型政策、代理、备注、临期优先），`batch-update` 只提交显式字段。
- `useAccountLifecycle`：归档视图“恢复到主列表”（`batch-update {restoreArchived:true}`）与临期优先开关。
- `useAccountImportTasks`：后台导入任务轮询、详情与停止；`useAccountPersonalInfo` `useAccountSubscriptions` `useAccountQuotaForecast` `useAccountResetCredits`：个人资料、订阅周期、额度预测、重置卡（幂等键、未决消费防重复）。

组件在 `ui/page/components/`，按职责分：`AccountFilters`（筛选与批量按钮，归档视图换成“恢复选中/删除选中”）、`AccountTableActions`（主列表行操作）、`AccountArchivedActions`（归档行：重新授权/恢复/删除）、`AccountStatusBadge/`（状态徽标与 presenter）、`AccountQuotaSummaryCell/`（用量列）、`AccountQuotaPanel/`（展开区额度与重置卡）、`AccountUsagePanel`、`AccountProfileModal/`、`AccountQuotaForecastModal/`、`AccountCreateModal/`（导入与 OAuth）、`AccountImportTasks/`、`AccountModelAccessField` 等。

生命周期与归档语义：归档由后端维护任务写入（`lifecycle_json.archived`，账号停用且不参与调度）；恢复只清归档标记并保持停用；重新授权在归档账号上走 `RotateProviderAccount.reactivate_archived`，同一事务内先离开归档并启用再轮换凭据；删除按 provider 分组在事务内级联删除分组绑定。接口字段见 [管理接口](../api.md) 的账号一节。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/accounts/plugin.json` | 清单：版本、页面 `/accounts`、35 个能力、`vue-component` |
| `plugins/accounts/ui/entries.json` | 页面入口声明（`main` → `ui/page/index.vue`） |
| `plugins/accounts/ui/page/index.vue` | 页面入口 |
| `plugins/accounts/ui/page/components/` | 表格单元格、弹窗、筛选、操作列 |
| `plugins/accounts/ui/page/composables/` | 查询、变更、导入、编辑、生命周期、资料/订阅/预测/重置卡 |
| `plugins/accounts/ui/page/utils/` | 需处理判定、OAuth 导出、刷新页、调度表单、上游 API Key 等纯函数 |
| `plugins/accounts/ui/page/constants.ts` | 列、选项、常量 |

## 数据、配置与接口

- 调用的能力：账号列表/详情/导出/导入/导入任务/OAuth/刷新/额度/批量更新/删除/恢复/轮换/重置卡/预测/个人信息、分组列表、代理列表、设置与客户端下载、`display-order.save`（账号顺序）、`accounts.avatar`（控制面把头像读成 data URL）。
- 无本地存储；列顺序/显示顺序由后端 `display-order` 保存。

## 改动指引

- 新增账号字段：后端 wire（`backend/crates/gateway-api/src/admin/accounts/wire.rs`、同目录的 presenter）→ `frontend/src/api/modules/accounts.ts` 类型 → 本插件组件；插件包不内置 Vue，基础组件来自主壳。
- 插件组件内任何后代错误（含 `@click` 处理器 rejected Promise）都会被宿主变成“插件运行失败”：事件处理器里的异步调用必须自己 catch（请求层已提示错误）。
- 新增接口权限要同步 `plugin.json` 能力和 `operations.json`；缺能力的旧平台会拒绝新包，不能删清单声明绕过。
- 校验：`node scripts/check-plugin.mjs accounts`、`node scripts/build-plugin.mjs accounts`；前端测试 `node --test frontend/tests/account-*.test.mjs`。
- 后端相关：`backend/crates/gateway-store/src/postgres/provider_accounts/`（分页、批量更新、删除事务）与 `backend/crates/providers/openai/src/credential/profile_statistics.rs`（归档维护）。
