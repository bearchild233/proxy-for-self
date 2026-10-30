# Agent Note: Account warmup controls and request settings

Status: implemented

## Problem

管理员需要在账号列表判断当前周期是否已预热，并直接预热单个账号；独立插件页面应专注自动化。固定 hi 和最低强度不能表达用户自定义提示词及推理强度的需求。批次最近结果不能代表每个账号，旧周期成功也不能证明当前 5 小时窗口有效。

## Decision

账号管理页拥有预热状态、单账号以及全部/选中手动入口，共用一份 read 轮询与提交状态。手动弹窗提供本次 model/prompt/reasoningEffort，默认从已保存设置和单账号覆盖读取，提交只覆盖本次任务快照。自动预热页拥有后台开关、每日/reset 计划、参与账号/分组、抖动、全局提示词/模型/强度与逐账号时间/模型/强度/延迟；历史和下次执行时间仅包含自动来源。表单说明放在控件下方，三列下拉占满列宽并沿同一基线排列；页面与侧栏均使用 FlameKindling 图标。

独立 Rust 服务和两个窄内核能力的所有权沿用[预热服务决定](2026-10-25-account-warmup-plugin.md)。本决定部分取代该文最初的固定 hi、最低强度和页面职责约束，队列、资格检查、身份授权、真实 resetAt、unknown 不重试和排空规则继续适用。

内核协议 0.8 的 run 增加 prompt（默认 hi）与 reasoningEffort（默认 auto）。prompt 保留原文，只允许非空、最多 4096 UTF-8 字节和正常换行/tab，拒绝其它控制符；不开放任意请求正文、工具、会话或返回内容。auto 选择最低受支持强度，显式 none/minimal/low/medium/high/xhigh 必须得到账号模型目录支持，否则发送前 skipped/reasoning_unsupported。模型目录经服务完整转发 reasoningEfforts；不支持推理的模型仅使用 auto。

Settings、TaskItem 为新增字段提供 serde 默认值，旧 SQLite JSON 可向前读取。入队冻结 prompt/model/effort，账号覆盖的强度为空时继承全局；活动任务合并时保留原任务参数。手动提交发生网络异常时，页面锁定原范围/参数并保留同一 submissionId，关闭弹窗也不丢失；重试只回读同一提交。旧服务缺少新字段时禁用新入口，不能把参数拒绝误当新功能可用。

read 返回按账号索引查询的 accountStates，不为 accounts 放开 history/configure 权限。活动任务优先显示 queued/running；有效真实 5 小时窗口显示 warmed，业务请求启动的窗口也成立，不冒认某次 unknown 任务成功。无有效窗口时区分 pending/unconfirmed/unknown/failed/skipped/unsupported；已过期的最新观测压住更老窗口，前端也在 resetAt 到期时撤销已预热标签。摘要不读取或返回任务提示词。

自动范围有 all/selected/groups 三种，groupIds 保存分组 ID，多组选取并集。Core 用现有 PluginAccounts 端口按 Provider 返回账号 ID 合并真实 groups；缺行、冲突或查询失败返回不可用，不把未知关系写成空组。只有可信合并成功时才声明 groupTargetsSupported；不放宽普通 accounts.read，也不申请 ownership/watch。账号分组的路由启用状态不替代账号本身的正常准入。

计划发现使用有界目录缓存；到期的纯自动分组任务在认领前必须刷新可信完整目录。出组账号的未发任务跳过；目录不可用时保留 queued 并继续寻找手动任务，不能误发也不能用队头阻塞手动执行。合并了有效手动批次的任务仍代表明确手动范围。设置变更与目录快照分别读取时，read/claim 必须保守处理代次不一致，不能用旧 all 目录冒充新的分组执行依据。普通空队列每秒 tick 不触发每秒全量数据库目录读取。

界面支持跨页账号勾选、筛选结果全选和多组成员选择。指定账号模式的勾选定义参与范围；全部/分组模式的勾选仅用于批量修改。批量时间/模型/强度/延迟/参与只修改显式字段，保持不变与跟随全局（null）分别表达，写入草稿后统一保存；上限 500 原子拒绝。分组批量修改只作用当前成员，新加入成员跟随全局，已离组账号不受该批次修改；不新增含优先级冲突的 groupOverrides。

## Related decisions audit

当前 proposed/implemented/rejected 的 warmup、预热、diagnostic 与养号检索中，[预热服务](2026-10-25-account-warmup-plugin.md)部分重叠，双方互链并保留持久队列及授权理由；[额度保护](2026-10-22-account-quota-protection.md)保持独立准入规则；控制面上线/恢复过程笔记不拥有请求字段与 UI 交互，保持不变。没有完全吸收或需要删除的提案。

## Alternatives considered

- 沿用固定 hi/最低强度：接口最窄且消耗小，但不能满足用户明确的自定义请求需求；保留有界文本与目录校验，仍不开放任意上游 body。
- 逐行调用历史接口：实现直观，却扩大 accounts 权限并产生 N 次轮询；服务在 read 内输出摘要，列表只使用一份状态源。
- 按最近手动批次或发送时间推算已预热：不需要额度事实，但会混淆账号、把旧周期或未确认发送报成成功；只采用真实窗口证据。
- 显式参数覆盖已有活动任务：符合立即使用新参数的直觉，却改变持久任务快照并威胁至多一次发送；合并继续沿用原任务，弹窗明确说明。
- 把分组展开成账号 ID 后保存：插件和内核接口改动少，但成员加入/退出不会自动跟随，且旧队列可能继续处理出组账号；持久化 groupIds，由可信目录提供动态成员。
- 独立分组覆盖层：能为未来成员自动继承不同组的独立设置，但多组账号需新增优先级和冲突规则。本次采用动态范围与当前成员批量账号覆盖，未来成员继承全局，界面明确两者的边界。

## Testing

分组与界面修正阶段通过前端预热/备注 37 项、服务真实 SQLite/socket 41 项、Core 能力 7 项（另有 registry 1 项）、协议能力 5 项、Provider warmup 相关 16 项，以及专用隔离 PostgreSQL/Redis 的真实目录成员测试 1 项。相关三包 Rust Clippy、类型检查、前端完整构建、定向 lint、格式和插件清单/ELF/资源哈希检查通过；插件还经过临时目录的真实安装验证。控制面预热桥 13 项及 updater 验证在请求设置阶段通过，本阶段没有修改其实现。updater 的 3 项可选 Rust echo 互通测试没有设置测试二进制，未覆盖。本地交互预览只包含合成账号，不调用真实上游；没有生产或真实账号验收。

## Consequences

列表状态可反映当前周期，手动执行与自动计划分别管理；保留备注、养号和共享调度限制。请求设置会消耗相应额度，显式不受支持强度不会静默降档。0.2.1 服务包最低内核 0.8，分组模式还须 groupTargetsSupported=true；首次升级需要兼容网关与 updater，侧栏图标随主壳发布。后续设置调整由插件完成。新设置保存后，旧 0.1/0.2.0 的严格 Settings 解码不能直接读取，跨版本恢复必须配套备份配置及 SQLite，不能仅回退服务二进制。前端预览确认与生产发布、Git push 分别按仓库约定执行。
