# usage-analytics 插件（概览与使用统计）

<!-- doc-sync paths: plugins/usage-analytics -->

## 职责与边界

基础界面插件（不可停用），一个包三个页面：

| 页面 ID | 路由 | 身份 | 内容 |
| --- | --- | --- | --- |
| `dashboard` | `/` | 管理员 | 概览：请求/Token/成本指标、请求趋势、请求健康时间线、账号概览、Wire 画像卡 |
| `usage` | `/usage` | 管理员 | 使用统计：请求明细表与详情、成本与性能卡、健康与诊断卡、Key×账号配额消耗交叉查询、错误排查（Ops 错误） |
| `key-usage` | `/key-usage` | Key | Key 持有者自助页：额度预算、趋势、记录、配置 |

**没有后端进程**。数据口径（完整交付才进入 Token/延迟/成本聚合、预热记录排除、成本按币种独立合计）由后端 Store 的观测查询定义（`backend/crates/gateway-store/src/postgres/observability/`、`backend/crates/gateway-store/src/postgres/usage_facts.rs`），本插件只展示。Key 页面只调用 `key-usage.*` 能力，范围由服务端会话决定，请求里没有 Key/账号范围参数。

## 实现逻辑

- **dashboard**（`ui/dashboard/`）：`composables/useDashboard.ts` 通过 `usePluginPolling` 每 30 秒（页面可见时）拉取 `dashboard.summary` 与 `dashboard.trend`；`useRequestTrendChart.ts` 把趋势序列转为 ECharts 配置（缺口处理在主壳 `frontend/src/components/charts/timeSeriesGap.ts`）；组件：`MetricCard`、`RequestTrendCard`、`RequestHealthTimelineCard`（含 `HealthTimelinePointPopover`）、`AccountOverviewCard`、`WireProfileCard`、`DashboardHeartbeat`。
- **usage**（`ui/usage/`）：`useUsageRecordsTable.ts` 管筛选（时间范围 `useUsageTimeRange`、Key、账号、模型、状态、传输等）、分页、排序和汇总；`useUsageRecordDetail` 与 `UsageRecordDetailModal` 展示单条记录（尝试、费用拆分、传输失败、诊断反馈包 `utils/diagnosticsBundle.ts`，只导出允许的关联/分类/阶段/计时字段）；`useOpsErrorsTable` 与 `OpsErrorPanel` / `OpsErrorDetailModal` 做错误排查；`UsageHealthCard` `UsageCostCard` `UsagePerformanceCard` `UsageDiagnosticCard` 对应 `usage.insights.*`；`KeyAccountBreakdownCard` 与 `KeyAccountQuotaCharts` 展示 `insights.key-account` 的 Key×账号交叉（环图按币种；混合币种提供币种选择，比例只比较同一币种）；`utils/records.ts` 汇总展示格式，`useAccountIdentityDirectory` 用身份快照补充账号名（归档账号保留名称，目录读取失败时降级展示账号 ID）。
- **key-usage**（`ui/key-usage/`）：`useKeyUsage.ts`（概览/记录/版本）、`useKeyConfig.ts`（显式读取当前 Key 配置，明文仅打开弹窗时取得）；组件 `KeyUsageBudget` `KeyUsageSummary` `KeyUsageTrend` `KeyUsageRecords` 等。该页独立于管理布局，不挂载管理员菜单、不请求管理接口。
- 配置弹窗与 Codex / CC Switch 配置生成由 [client-access](client-access.md) 的槽位提供。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/usage-analytics/plugin.json` | 清单：三个页面、28 个能力（admin 的 `dashboard.*` `usage.*`，key 的 `key-usage.*`） |
| `plugins/usage-analytics/ui/entries.json` | 三个页面入口 |
| `plugins/usage-analytics/ui/dashboard/` | 概览页（`index.vue`、`components/`、`composables/`、`constants.ts`） |
| `plugins/usage-analytics/ui/usage/` | 统计页（`components/` 约 25 个、`composables/`、`utils/`） |
| `plugins/usage-analytics/ui/key-usage/` | Key 自助页 |

## 数据、配置与接口

调用的能力：`dashboard.get.admin.dashboard.{summary,trend}`、`usage.get.admin.usage.records{,.detail,.summary}`、`usage.get.admin.usage.insights.{overview,diagnostics,key-account}`、`usage.get.admin.operations.errors`、`key-usage.get.key-usage.{config,overview,records,version}`，以及账号/Key/分组读取（筛选项）。`insights.key-account` 要求宿主 operations 映射 `usage.get.admin.usage.insights.key-account` 且 usage-analytics 策略授权，缺少该能力的旧平台会拒绝本包。

## 改动指引

- 展示口径变化先改后端 Admin 观测规则与 Store 查询，再同步 `frontend/src/api/modules/usage.ts` 与 `frontend/src/api/modules/dashboard.ts` 类型；不要在前端重算后端已给出的汇总，也不要匹配英文错误句子判断来源（用稳定字段）。
- 主题：本包可写主题（`host('theme')`），其他插件不行，见 [frontend-shell](frontend-shell.md)。
- 新增图表优先复用主壳 `frontend/src/components/charts/BaseChart.vue` 与 `useChartPalette`。
- 校验：`node scripts/check-plugin.mjs usage-analytics`、`node scripts/build-plugin.mjs usage-analytics`。
