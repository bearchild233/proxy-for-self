# Agent Note: Account warmup service plugin

Status: implemented

## Problem

管理员需要一键为全部或选中账号发送一次hi，并在浏览器关闭后按额度重置或每日时间自动预热；每个账号可覆盖时间和模型，还需随机延迟。现有诊断是管理员故障测试，会绕过调度限制；直接循环诊断不能用于自动化。5小时窗口是否启动必须以真实上游额度观测为准。

## Decision

采用独立Rust常驻`account-warmup`插件和自有SQLite配置、队列、结果及设置页。账号管理保留列表、备注和养号逻辑，只添加预热全部/选中及设置入口。内核提供两个窄维护能力，后续业务规则和页面随插件更新，不新增PostgreSQL迁移。

内核协议0.7新增`accounts.warmup.read`和`accounts.warmup.run`，只允许显式授权的`account-warmup`身份。维护范围不借用业务ownership/watchedGroups，也不改变原有分组授权。run固定单账号、hi、最低受支持推理强度、只读响应头、单次发送、不换号、不传出Cookie/state或任意提示词。复用账号出口及被动额度观测，发送前验证停用、认证、冷却、官方额度、养号、模型政策和共享账号调度租约。首版仅OpenAI OAuth可用；其它类型明示不支持，不伪报成功。返回accepted只表示上游收头成功，另以真实短窗口resetAt表示计时已确认。

设置默认自动关闭，Asia/Shanghai，每日08:00（每日开关默认关）、额度重置触发默认选中、重置后30秒、抖动5至15秒。抖动可设0至300秒；账号覆盖每日时间、模型、自动参与和重置延迟，空值继承全局。一个手动批次使用保存时的配置快照，全部由后台解析，不把当前页当全部。

手动和自动共用持久队列，全局串行。SQLite事务保证每账号最多一个活动任务，submissionId保证重复提交回读同一批次；自动任务用每日日期/真实resetAt作为幂等键。抖动抽样后保存，重启不重新抽样；错过计划只补最近一次，超出补跑窗口不补齐历史。最近5分钟成功的自动触发合并。未知resetAt不推算5小时；reset事件在观测到的真实窗口到期后执行。

新旧插件共用SQLite事务claim和有期限的执行者租约；只有过期的running记录才转unknown，不能在新进程启动时覆盖旧进程的在途任务。发送后超时/断连以及进程失联归unknown，同一计划不自动重发。停止插件不再认领，已发请求在关停预算内完成；队列与设置保留。

管理能力为`account-warmup.read/configure/run/history/cancel`。账号页面仅获read/run的显式跨插件桥，控制面校验admin、调用插件、当前目标启用状态和目标能力后转发；不新增通用跨插件授权。配置用expectedRevision比较并交换，不能覆盖其它管理员的更新。取消仅取消尚未发出的任务。

## Interface contract

所有JSON字段camelCase；时间为RFC3339 UTC字符串或null，时长以秒/毫秒明确命名。设置：

```json
{"enabled":false,"timezone":"Asia/Shanghai","dailyEnabled":false,"dailyTime":"08:00","resetEnabled":true,"resetDelaySeconds":30,"jitterMinSeconds":5,"jitterMaxSeconds":15,"targetMode":"all","accountIds":[],"defaultModel":"gpt-6.1-sol","accountOverrides":[]}
```

accountOverrides元素为`{accountId,enabled:true,dailyTime:null,model:null,resetDelaySeconds:null}`；enabled仅控制自动参与，手动显式目标仍可运行。每日时间均使用全局IANA时区。目标all/selected决定自动范围，手动run的scope独立决定本次目标。

- read：`{accountIds?,includeModels?,model?}`返回`{available:true,revision,settings,accounts,runtime:{ready,nextRunAt,lastRun}}`。accounts复用内核安全维护目录，不包含邮箱、Cookie、凭据或upstream账号标识。页面名称通过既有账号列表API合并；按指定accountIds/includeModels懒读模型目录。
- configure：`{expectedRevision,settings}`返回与read相同结构，revision冲突拒绝。
- run：`{submissionId,scope:"all"|"selected",accountIds?:[]}`返回`{run:RunSummary}`，受理不等于完成；所有参数严格限长，selected最多500项。
- history：`{limit?:20,runId?:string}`返回`{runs:RunSummary[],items:TaskItem[]}`，runId提供时只列该批明细。
- cancel：`{runId}`返回`{run:RunSummary}`，已发送项目继续收尾。
- RunSummary：`{id,source,status,createdAt,finishedAt,total,accepted,failed,skipped,unknown,pending}`。source为manual/daily/reset；status为queued/running/completed/cancelled。
- TaskItem：`{id,accountId,source,model,status,scheduledAt,dueAt,startedAt,finishedAt,durationMs,reasonCode,httpStatus,resetAt,timerConfirmed}`；status为queued/running/accepted/failed/skipped/unknown/cancelled。

内核read参数`{ids?:[],model?:string,includeModels?:bool}`返回`{accounts:[{id,provider,authenticationKind,enabled,status,supported,reasonCode,modelAllowed,models,shortTerm}]}`；models为`[{id,label}]`或null，shortTerm为`{observedAt,windowSeconds,usedPercent,resetAt}`或null。run参数`{accountId,model,timeoutMs,attemptId}`，timeoutMs最多15000，返回`{outcome,reasonCode,status,durationMs,sendState,shortTerm}`；outcome为accepted/skipped/failed/unknown，sendState为not_sent/sent/ambiguous。attemptId仅关联一次发送，不声称内核持久幂等；插件SQLite认领与不重试规则负责至多一次。

## Related decisions audit

检索当前proposed/implemented/rejected的warmup、预热、diagnostic、降智、调度和定时。现存[账号养号保护](../../implemented/feature/2026-10-22-account-quota-protection.md)部分相关：预热必须保留同一准入保护，但它不拥有后台预热计划；保留原决定并引用，不替换。没有发现已有预热计划owner。

## Alternatives considered

- 直接循环诊断Worker：复用最多，但依赖管理员会话，任务在控制面重启后丢失，诊断旁路又绕过账号保护，不能承载无人值守自动化。
- 把全部计划放进Admin/Store/Worker：可复用PostgreSQL及Redis任务租约，但业务规则每次都需网关发布，偏离用户明确的插件偏好；只在内核保留窄维护执行机制。
- 让插件声明监控所有账号：配置简单，但会与其它服务插件的账号归属冲突；维护授权独立于推理路由所有权。
- 新增通用内嵌toolbar槽协议：跨插件UI复用灵活，但现有PluginSlot是旧iframe目录，需改宿主组件和context协议；本次选择明确只给accounts的read/run桥。

## Testing

验证自动默认关闭、全部与跨页选中语义、独立时间/模型/继承、时区和DST、持久抖动、每日/reset幂等、重启和双实例claim、unknown不重试、取消、停用排空、配置revision冲突、无凭据泄漏、准入不绕过及真实额度头更新。用合成账号和本地伪上游，不主动调用生产账号。

已完成的证据包括：账号预热 Rust 服务测试 15 项、clippy `-D warnings`、专属 cargo fmt 检查；Provider warmup 过滤 12 项、Provider plugin 22 项、gateway-core plugin 5 项、gateway-host warmup grant 1 项、kernel-protocol capabilities 4 项；前端预热测试 8 项、控制面桥测试 13 项、service manifest 测试 12 项；两个插件的构建、资源哈希、服务 ELF/清单校验及 tar 路径安全检查。

## Consequences

预热仍可能消耗一次额度；只收头不代表生成完整回答，也不承诺上游一定创建5小时窗口。SQLite只用于单机共享插件数据，不支持多机扩容。网关、控制面、updater grants 和服务包必须一起升级，能力缺失时页面明确不可用。大交互改动已生成具体预览；按仓库约定，隔离环境验收后仍需用户确认才能上线生产。
