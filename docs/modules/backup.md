# backup 插件（数据备份）

<!-- doc-sync paths: plugins/backup -->

## 职责与边界

可选运维类插件（设置页 `/settings/backup`，`layout: settings`，仅管理员）：配置 S3 / Cloudflare R2 备份存储、备份计划（Cron + 时区、保留天数与份数）、手动创建备份、查看备份记录、生成下载链接、删除备份。由两部分组成：

1. **界面**（`ui/`）：纯前端，调用网关管理 API。
2. **策略脚本**（`worker/policy.py`，清单 `policyWorker`）：只做“准入 / 是否到期 / 清理哪些”的决策，输入输出都是脱敏数据，不接触对象存储密钥或数据库导出。网关的备份 Daemon 通过 `services/plugin_host/policy_runner.py` 调用它。

对象存储密钥、`pg_dump`、暂存目录、记录状态机和禁止误删最新备份的约束都在内核：`backend/crates/gateway-admin/src/backup/`（策略与 Worker）、`backend/crates/gateway-admin/src/use_case/backup.rs`（用例）、`backend/crates/gateway-store/src/backup/`（`pg_dump`、暂存、S3 适配）与 `backend/crates/gateway-store/src/postgres/backup.rs`（表）。插件停用后不再领取新任务，已执行的任务完成后停止；已保存的设置继续存在。

## 实现逻辑

- **策略**（`worker/policy.py::decide`）：`schedule`——返回 `schedule: due`（内核已按 Cron/时区算出的到期事实，策略只有同意权）；`retention`——给定 `records`（按新到旧）、`retentionDays`、`retentionCount`、`now`，返回要删除的记录 ID（跳过最新一条；按份数超出或完成时间早于保留天数；单次最多 10 个）。其余动作默认放行（`enabled: true`）。
- **内核调用点**（`backend/crates/gateway-admin/src/backup/task.rs` 的 `BackupTask::run_cycle`）：单个可取消 Daemon，每轮先问策略 `admission`（返回 `enabled:true` 才继续；策略不可用或返回非 enabled 就整轮跳过，不影响推理内核）→ `advance_schedule`（按 Cron/时区推进游标并幂等插入 scheduled 任务，到期事实先经策略 `schedule` 确认）→ 恢复中间态任务 → 完成待删除收敛 → 处理下一个任务（`pg_dump` → 暂存 → 上传）→ 保留清理批次。保留清理由内核先算出候选（`decide_retention`），再用策略 `retention` 返回的 ID **取交集**——插件只能缩小删除范围，不能扩大；每批有数量上限；策略调用出错则本轮清理失败退出，不删除。
- **界面**：`ui/page/BackupPage.vue` 挂载 `SettingsBackupSection`；`BackupStorageCard`（存储配置与连接测试，`secretAccessKey` 由 GET 回填、保存时整体提交）、`BackupScheduleCard`（Cron、时区、保留）、`BackupRecordsCard`（记录表、状态徽标 `BackupStatusBadge`、手动创建、下载、删除）、`R2GuideModal`（R2 配置指引）；`composables/useBackupSettings.ts`（存储与计划共用的加载/保存/测试）与 `useBackupRecords.ts`（分页与轮询，备份进行中每 2 秒更新进度，空闲时仍发现定时备份）。下载链接由宿主操作 `backup-download` 取得（仅 `backup` 插件可用，仅接受 https 地址）并交给浏览器，插件不接触文件内容。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/backup/plugin.json` | 清单：设置页、10 个能力、`policyWorker` |
| `plugins/backup/ui/entries.json` | 页面入口 |
| `plugins/backup/ui/page/` | 页面与各卡片 |
| `plugins/backup/ui/composables/` | 设置与记录的数据逻辑 |
| `plugins/backup/worker/policy.py` | 备份准入、到期与保留决策 |

## 数据、配置与接口

能力：`backups.get.admin.settings.backups{,.records}`、`backups.post.admin.settings.backups.{create,delete,download-url,schedule.update,storage.test,storage.update}`、`auth.*`。无本地存储；备份记录和设置在 PostgreSQL（`backup_settings`、`backup_records`），对象在 S3/R2。

## 改动指引

- 备份规则（保留、到期、准入）只改 `worker/policy.py` 即可热更新，但不得放宽内核约束（不能删最新备份、非终态任务不被删除）；新增策略动作要同步 `BackupTask` 的调用点和 `gateway-admin` 的 `BackupPolicyPort` 契约。
- 恢复属于人工维护操作：没有部署级维护模式开关，需先停应用、离线处理快照中的非终态任务和计划游标，再恢复（见 [部署文档](../../deploy/README.md) 的“人工恢复数据库”）。
- 校验：`node scripts/check-plugin.mjs backup`、`node scripts/build-plugin.mjs backup`。
