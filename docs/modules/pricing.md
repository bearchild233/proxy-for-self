# pricing 插件（模型定价）

<!-- doc-sync paths: plugins/pricing -->

## 职责与边界

可选运维类插件（设置页 `/settings/pricing`，`layout: settings`，仅管理员）：展示各 Provider 的内置价目、同步价目与人工覆盖，支持批量改价、清除覆盖、删除、手动同步预览与确认，并显示每日自动同步的计划状态。插件由两部分组成：

1. **界面**（`ui/`）：纯前端，调用网关管理 API。
2. **策略脚本**（`worker/policy.py`）：只做“什么时候该同步”的计划决策，不接触凭据、不访问网络；由 Rust 网关的定价同步任务通过 `services/plugin_host/policy_runner.py` 调用，清单字段 `policyWorker: worker/policy.py`。

价格数据的来源和合并规则在后端：Provider 持有内置价目与计费规则（`backend/crates/providers/openai/src/transport/usage.rs`），Admin 管理人工覆盖与手动同步（`backend/crates/gateway-admin/src/use_case/settings.rs`、`backend/crates/gateway-admin/src/pricing_sync.rs`），Host 固定读取 models.dev（`backend/crates/gateway-host/src/pricing/mod.rs`），Store 保存来源层、人工层与审计（`backend/crates/gateway-store/src/postgres/pricing.rs`）；快照编译时合并同步与人工配置，不可变价格集合随 RoutingPlan 冻结，推理请求不读外部价目。同步不覆盖人工项，更新只改选中的模型。停用本插件后，已保存的价格继续生效，但不再领取新的同步任务。

## 实现逻辑

- **策略**（`worker/policy.py::decide`）：输入 `{action, now, state, runId?, success?}`。`claim`：首次无 `nextRun` 时排到下一个 03:00（Asia/Shanghai，UTC+8），到点则标记 `running`、记录 `runId`、把 `nextRun` 暂设为 `now+300`（防止重复领取）并返回 `run: true`；`finish`（`runId` 匹配）：成功则 `nextRun` 为次日 03:00 并记 `lastSuccess`，失败保留现有价格并 1 小时后重试。状态由 `services/plugin_host/policy_runner.py` 落盘到 `pricing-runtime/status.json`（带文件锁，0640）。
- **后台执行**：Admin 的 `PricingSyncTask`（周期 Worker，60 秒一轮，带 300 秒 leader lease）每轮向策略进程 `claim`；需要运行时在 120 秒内复用手动同步的预览与校验、以 `System` 身份写审计，随后 `finish`（成功与否）；不依赖浏览器或管理员会话。控制面能力 `pricing.schedule.status` 只读返回 `status/lastAttempt/lastSuccess/nextRun/checkedAt/error`。
- **界面**：`ui/page/usePricing.ts`（价目加载、按 Provider/来源/搜索筛选、分页、批量选择，筛选变化清空选择；`save` 改价/重置/删除；`startSync` 预览 → `confirmSync` 按模型确认；页面可见时轮询）、`model.ts`（价目行模型：默认/同步/人工三层与来源标注）、`PricingTable` `PricingToolbar` `PricingEditor` `PricingBatchModal` `PricingRateFields`（Fast 倍率等）`PricingUnit` `PricingSyncModal`（同步差异预览）`PricingSchedule`（计划状态展示）。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/pricing/plugin.json` | 清单：设置页、7 个能力、`policyWorker` |
| `plugins/pricing/ui/entries.json` | 页面入口 |
| `plugins/pricing/ui/page/` | 页面与组件、`usePricing.ts`、`model.ts` |
| `plugins/pricing/worker/policy.py` | 每日同步计划决策（纯函数，stdin/stdout JSON） |

## 数据、配置与接口

能力：`pricing.get.admin.settings.pricing`、`pricing.post.admin.settings.pricing.{sync,sync.preview,update}`、`pricing.schedule.status`。运行状态文件 `pricing-runtime/status.json`（控制面数据根下）。网关需要 `CPR_PLUGIN_POLICY_RUNNER` 与 `CPR_PLUGIN_ROOT` 才会启用策略进程（见 [gateway-host](gateway-host.md)）。

## 改动指引

- 改计划规则（时间、重试间隔）只改 `worker/policy.py`，可热更新；策略脚本的路径、哈希和 `sdk: 1` 由 `services/plugin_host/policy_runner.py` 与 `StaticPluginStore` 校验，文件名必须是 `worker/policy.py`。
- 价格字段或计算口径变化属于后端（Provider 价目 → `metering` → Store 费用快照），再同步 `frontend/src/api/modules/pricing.ts` 与本界面；自定义倍率只作用于本地计算费用，上游报告金额不变。
- 手动同步与自动同步必须共用同一套校验与审计，不要在界面或策略里另写一份。
- 校验：`node scripts/check-plugin.mjs pricing`、`node scripts/build-plugin.mjs pricing`；本机阶段验证 `scripts/test-pricing-stage.py`；控制面单测 `services/plugin_host/test_pricing_schedule.py`。
