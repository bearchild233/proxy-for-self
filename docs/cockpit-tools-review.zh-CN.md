# Cockpit Tools 可借鉴项

调研日期：2026-09-29。阅读源码快照：`4ea6a34df6aa3b2d494c3cca5983bd09a84a9e3a`。以下保留调研时的建议，未安装或运行该应用。

实施状态：统一刷新队列和账号快捷筛选已独立实现并上线；订阅与资料成功结果共享 15 秒，额度仅合并重叠请求。快捷筛选包含 3 天内到期、额度不足、需要授权、订阅未知。用户决定暂不做面板额度告警，站外通知和账号回收站尚未实现。

## 定位与取舍

Cockpit Tools 主要管理本机 AI IDE 账号、应用实例和客户端配置；README 说明其 Codex API 服务由 CLIProxyAPI sidecar 驱动。proxy-for-self 是 VPS 上的集中式 Rust 网关和 Vue 管理面板。适合借鉴账号运维体验与后台调度设计，无需为了这些功能替换现有网关或另外运行一套桌面系统。

来源：[README](https://github.com/jlcodes99/cockpit-tools/blob/4ea6a34df6aa3b2d494c3cca5983bd09a84a9e3a/README.md)。

## 建议优先级

1. **统一刷新与数据新鲜度（优先）**。它的后端以账号 ID 合并重复刷新，区分手动和后台队列，限制并发、等待者和队列长度；前端调度错开启动时间。我们已有额度后台刷新和页面限并发，订阅目前有页面级缓存。下一步应核对这些入口是否共享同一后端刷新任务，并提供每项数据的最近成功时间、失败原因及重试入口。这样多设备打开面板时也不会重复打上游。
   - [后端刷新队列](https://github.com/jlcodes99/cockpit-tools/blob/4ea6a34df6aa3b2d494c3cca5983bd09a84a9e3a/src-tauri/src/modules/codex_quota_refresh_scheduler.rs)
   - [前端错峰调度](https://github.com/jlcodes99/cockpit-tools/blob/4ea6a34df6aa3b2d494c3cca5983bd09a84a9e3a/src/utils/autoRefreshScheduler.ts)
2. **状态快捷筛选与账号摘要（优先）**。它区分套餐、异常、额度耗尽、订阅过期、标签和分组。我们可在现有账号页加“即将到期”“额度不足”“需要重新授权”快捷入口，继续复用现有名称、套餐、剩余额度和订阅列。每个额度窗口独立展示，缺失窗口不伪装为无限额度。该源码的概览额度耗尽判断使用所有已报告窗口都为零的条件，不能直接当作本网关的可调度条件照搬。
   - [账号概览过滤与统计](https://github.com/jlcodes99/cockpit-tools/blob/4ea6a34df6aa3b2d494c3cca5983bd09a84a9e3a/src/utils/codexAccountOverview.ts)
3. **阈值告警及恢复记录（随后）**。它已有额度阈值、告警冷却、备用账号选择逻辑。建议我们先做站内“额度低于阈值 / 订阅即将到期 / 授权失效”提示，状态恢复后归档，避免刷新一次提醒一次。已有会话仍保持账号；备用账号建议优先作用于新会话，不能绕过现有 WS、工具回传与已交付内容的重试边界。
   - [额度告警与切换候选](https://github.com/jlcodes99/cockpit-tools/blob/4ea6a34df6aa3b2d494c3cca5983bd09a84a9e3a/src-tauri/src/modules/codex_account_mutations_quota.rs)
4. **账号回收站（随后）**。其回收站使用加密快照并支持恢复。我们可增加短期软删除和恢复，但删除后必须立即停止调度，恢复时校验账号身份与 Key/分组关系；不能自动恢复已经撤销的访问权限。
   - [加密回收站](https://github.com/jlcodes99/cockpit-tools/blob/4ea6a34df6aa3b2d494c3cca5983bd09a84a9e3a/src-tauri/src/modules/codex_account_recycle_bin.rs)

## 暂不建议

- 不直接移植 IDE 多开、修改本机认证文件和默认自动唤醒等桌面工作流；它们不是当前 VPS 网关的核心需求。
- 不重新实现已经具备的账号分组、Key 范围、轮转策略、拖动排序和订阅剩余显示，优先完善刷新一致性与可解释的异常提示。
- README 声明默认许可为 CC BY-NC-SA 4.0。本次只研究设计，没有复制对方源码；后续优先按现有项目架构独立实现，若需要直接引入代码，应单独核对其许可要求。
