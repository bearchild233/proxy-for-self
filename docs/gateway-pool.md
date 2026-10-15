# 网关池插件

<!-- doc-sync paths: plugins/gateway-pool -->

为降智账号所在的分组，按“账号 × 网关”轮换满血网关。降智信号在交付前拦截，用户只感到等待稍长。插件由两部分组成：管理页面（UI 包，经控制面安装）与 Rust 服务进程（由 updater 监督，经内核请求钩子参与路由）；两者都可热更新，不重启主网关。

## 术语

| 术语 | 含义 |
| --- | --- |
| 网关 | OpenAI 上游的接入节点，名称形如 `unified-142`。同一账号经不同网关访问，可能得到不同质量的服务。 |
| 池服务 | 外部网关池服务（默认 `https://pool.0102400.xyz`），维护各网关的路由 Cookie。`/gateways` 列出该成员可配对的网关、Cookie 剩余秒数与地区，`/cookie` 返回指定网关的 `__oailb` 与 `__cflb`。使用单独的池服务 Bearer，与 OpenAI 凭据无关。 |
| 池成员标识 | 向池服务声明的账号身份，默认自动生成 `gwpool-member:<上游账号 ID>/<上游用户 ID>`；管理员可在设置中覆盖。 |
| 组合 | 一个“账号 × 网关”。满血窗口、静置、学习和统计都以组合为单位。 |
| 打票模型 | 检测网关用的模型，默认 `gpt-6.1-sol`，reasoning low，输入 `hi`，无工具。 |
| 票 / 找票 | 票是某个模型在某个组合上的 state 与配套 Cookie。找票指用打票模型为账号寻找判定为 21 的网关。 |
| S0 | 携带池 Cookie 第一次访问上游（bootstrap）返回的 `x-codex-turn-state`。此后该模型在该组合上的请求固定带这份 state，不采用上游后续返回的新 state。 |
| bootstrap | 取 S0 的第一次请求，不做判定。 |
| 21 / 29 | 携带票重放后，在 HTTP 响应头判定：HTTP 200 且**不返回** `x-codex-turn-state` 为 21（满血）；HTTP 200 且**返回**该字段为 29（降智）。只看是否出现，不比较取值。这是约定的实验信号，不代表已验证回答质量。 |
| 拿票 | 窗口内来了其他模型的请求、该模型还没有票时，用该模型发一次 `hi` 取 state，拿到响应头即断开，不做第二轮判定。 |
| 检测失败 | 401、402、429、超时、网络错误、缺少配套材料，或 `__oailb` JWT 的 `host` 与指定网关不同。记录真实状态码与失败类别，不记为 29；固定静置 5 分钟，不升档、不改起始档。 |
| 满血窗口 | 组合的使用时间，默认 180 秒，所有模型共用。用满后只准备备用票，当前窗口继续服务；窗口在遇到 29、失败、Cookie 将到期、一轮结束或账号退出调度时结束。备用票转正的窗口从第一次业务访问开始计时，其余从首次实际发送起算。 |
| 备用票 | 窗口用满后提前准备的下一个组合：已取 S0 且重放判 21，状态 `standby`，不接业务请求。当前窗口遇到 29、检测或业务失败、Cookie 将到期、被移出限定网关时立即转为服务中，不再找票。同一账号最多一张；自拿到起超过 50 分钟仍未启用就丢弃（S0 约 60 分钟过期），之后若窗口仍超过时长且有需求再准备新的。被丢弃的备用票从拿到它的时间（最后一次访问上游结束）起静置。 |
| 静置 | 组合退役后禁止访问的时间，从最后一次实际访问上游结束时起算；期间任何真实访问都会重新计时。 |
| 退避档 / 起始档 | 静置时长按档计。起始档是组合上一次静置后恢复（复测为 21）时的档，从未恢复过用初始档（默认 2h）。复测仍为 29 就升一档：加性 `+步长`（默认，步长默认 1h）或乘性 `×2`，不超过上限（默认 24h）。复测为 21 记录该档；同一组合连续两次在同一档恢复，下次起始档降一档（加性 `-步长`、乘性 `÷2`），不低于初始档。业务中途 29、窗口因 Cookie 到期等正常结束、备用票被丢弃，都从起始档开始静置。设置项 `backoffMode`（`additive`/`multiplicative`）与 `backoffStepSeconds`（60–86400 整数秒）。 |
| lazy / continuous | 懒模式：没有请求时不找票，窗口到期后由新请求触发准备；请求触发的找票只在该账号还有请求等待窗口时继续（最近 10 秒内有 `route.prepare` 轮询），请求结束即停。持续模式：近期有流量的账号在空闲时也按窗口轮换。 |

## 行为

**范围**：只对设置中选定降智组的 OpenAI OAuth 账号生效。依据本次请求冻结目录中的实际成员身份判断，Key 直接指定多个账号或绑定多个分组都适用。同一账号属于多个组时只计一次，不能通过普通组绕过它的网关池参与开关。只有最终选中降智账号时才使用网关池传输；该传输依赖 HTTP 响应头，无法支持原生 WS。普通账号继续使用原传输与策略。

**Key 优先级与回退**：账号范围仍为指定账号和动态分组二选一。授权范围内可用的降智账号按共用队列优先于普通账号；所有降智账号额度耗尽、关闭参与、网关均冷却或暂不能获取窗口时，普通账号按该 Key 的轮转策略（未指定则继承全局）接手。候选可用性查询只读取已有目录与冷却状态，不触发探测。后续可重新选号的请求仍先检查降智账号，恢复后重新优先使用；已发送请求和严格续接保持现有安全边界。池控制面失败时，已确认身份的降智账号失败关闭，普通候选仍可回退；配置从未成功读取、无法确定分类时保守报错。只绑定降智账号的 Key（或固定账号）不会因账号暂时不在可调度名单里被提前拒绝：请求会进入网关池路由并得到具体原因。

**账号优先级**：“账号与网关”沿用账号管理的拖拽手柄，顺序与右侧“参与网关池”开关保存在插件 SQLite，所有 Key 共用。这是优先级，不是排队：每次请求在 Key 授权范围、模型权限、额度和并发限制内，选择优先级最靠前且可调度的账号，不要求排在队首；队列优先于会话亲和和权重。可调度指账号持有或正在准备窗口，或还有可检测的网关。同一 Key 内持有窗口的账号先继续服务，其余按优先级。开关仅控制网关池，不改全局账号启停。关闭后该账号既不打票也不能被使用：不回落到普通调度（Key 里有其他账号时选号直接跳过它，只剩它时以 `queue_disabled` 失败），也不做恢复检测、不会被移回正常分组；已经发送的请求正常排空。参与开关下方的“允许降智使用”开启时，账号不打票，`route.select` 把它放进 `ordinary`，由内核按普通账号的选号、额度与并发规则直接走上游；期间没有票也就没有恢复检测，账号留在降智分组，直到关闭该开关。参与开关关闭时优先，账号不可用。两个开关切换时都立即退役该账号的窗口。拖拽使用版本校验，自动移尾或其他管理操作先发生时，旧排序保存会被拒绝并刷新。

**一轮结束**：开始找票时固定该轮候选网关；真正访问过的网关退役时从待完成清单移除，全部结束后账号自动移到全局队尾。新一轮优先访问尚未完成的候选，仍遵守静置限制；未访问上游的失败不消耗一轮。额度首次确认耗尽时也移到队尾并暂停调度，重复上报不重复移动，额度恢复后保持队尾位置。配置改变候选范围时重新建立清单。持续模式采用相同队列，懒模式仍由请求触发。备用票随账号退出调度一起丢弃并静置；从未转正的备用票没有服务过，不算访问过的网关，不消耗一轮。

**窗口按账号、一个 Key 一个账号**：满血窗口属于“账号×网关”。同一个 Key 同一时刻只用一个降智账号（持窗账号先用，其余按优先级），不同 Key 可各自使用自己的账号和窗口，不再整个分组只有一个窗口；两个 Key 选到同一账号时共用该账号的窗口。备用票转正或替代窗口就绪时只退役同一账号的旧窗口，已发送的请求保留原归属。只绑定一个降智账号的 Key 没有退路：该账号有可检测网关就直接开窗口，一轮网关轮完后继续新一轮而不是让位。带 `previous_response_id` 的降智账号续接：原账号自己持窗，或 Key 里没有退路时留在原账号；窗口在同一 Key 的另一个账号上而原账号没有窗口时，由核心按已有规则使用已保存历史安全重放，缺少历史或存在不能迁移的工具状态时按原有规则返回错误，不直接把状态交给另一账号。

**失败原因**：网关池不能完成路由时，错误诊断带原因码和中文说明，管理员在“错误排查”里能直接看到：`queue_disabled`（账号在队列里停用）、`quota_exhausted`（额度耗尽）、`catalog_empty`（池目录或限定网关里没有可用网关）、`all_resting`（可用网关全部静置，写明最早恢复时间）、`wait_timeout`（等待超过请求期限，写明最后状态）、`no_probe_model_access`、`account_disabled`、`rate_limited`、`account_error`、`pool_disabled`、`member_unknown`、`control_plane_unavailable`、`yielded`。

**动态分组**：设置里开启后指定“正常分组”和“降智分组”（须不同，降智分组须在适用分组内）。分组调整由 Rust 网关写入（同一事务提升配置版本并写审计，操作者 `gateway_pool`，只增删这两个分组的成员关系，保留账号其他分组），决定与结果记录在插件 SQLite，并显示在“分组变动”标签；不做影子模式。
- **检测范围**：`watchScope` 默认 `all`，检查所有不归网关池管理的普通账号（与旧实现相同）；`normal` 只检查正常分组。`unwatchedGroupIds` 里的分组不检测，优先于范围。插件把它们登记为观察规则：正常分组 + `*`（`all` 时）+ `!分组`。不在正常分组的账号确认降智后只加入降智分组（正常分组只在账号确实在里面时移除），恢复后加入正常分组。
- **普通账号 → 降智分组**：只判 HTTP SSE 的请求，并且请求实际携带了 state（账号自己的、同账号发往上游的）；没带 state 的请求（如首条）默认当作正常。响应又返回 state 时，用返回的 state、同账号、同模型、同 Cookie 与出口再发一次 `hi` 短请求，只读响应头：再次返回 state 才确认降智。确认后放弃本次响应并交给核心换号重放（仅当请求没有续接、没有指定账号、没有上游执行工具），账号移进降智分组；不能放弃时照常交付，仍记录并移组。同一账号 20 秒内只确认一次；10 分钟内不重复移同一方向。
- **降智分组 → 正常分组**：后台每秒为近期经过路由的降智分组账号领取恢复检测。条件：某一代次的 state 已用满 8 分钟（`recoverySeconds`）、整代只有满血判定、配套材料仍有效。检测：先用这份已超过 8 分钟的旧 state 重放，仍不返回新 state；再连续两次全新 bootstrap 加重放都是满血，才移回正常分组。任何一步判为降智则该账号暂停恢复检测 1 小时（`recoveryPauseSeconds`），无法检测（网络、额度）等待 5 分钟。懒模式也会按时检测，不依赖新请求。参与开关关闭或允许降智使用的账号不领取恢复检测。
- 依据：PRO20 实验里同一个 state 重放到 10 分钟仍满血，窗口外旧 state 返回新 state 就不是 state 过期。

**候选网关**：本轮尚未完成的候选优先，其内按既有 Cookie 有效期规则排序。默认跟随池服务。按账号的池成员标识查询 `/gateways`（缓存 60 秒），只用 `pair_ready` 的网关；网关从目录中消失就暂不作为候选，静置记录保留。设置中的“限定网关”非空时，只在其中且在目录中的网关里挑选；目录查询失败时退回限定列表或 5 分钟内的上一份目录。优先挑选 Cookie 剩余不少于一个窗口再加 60 秒的网关。

**池服务出口**：设置“池服务直连”默认开启，取目录与 Cookie 走服务器直连，避免账号代理无法访问池服务；打票、业务和额度请求始终走账号绑定的出口。

**Cookie 有效期**：票的到期时间取 Cookie JWT `exp` 与池服务 `valid_for_s` 的较早者，记在组合上。到期前 60 秒触发换窗口：有备用票立即接手，没有就按找票流程直接找替代；已过期的材料不会再被使用。

**找票**：取指定网关的池 Cookie → 打票模型 bootstrap 取 S0 → 带 S0 与 Cookie 重放判定；两次探测共用同一个上游请求 ID、不带会话头（与旧实现一致）。探测拿到响应头立即关闭，不读 SSE。错误最多重试一次，遵守 5 秒内的 Retry-After；仍失败的组合固定静置 5 分钟，不升档，失败类别与状态码单独记录。候选之间默认间隔 50ms，探测最多 4 路并发，同一账号同时只有一个找票任务，单组合准备预算 35 秒。额度耗尽、限流冷却、停用、归档或无打票模型权限的账号不找票。

**票按模型分开、窗口共用**：打票模型的 S0 随找票得到。窗口内来了其他模型的请求而该模型没有票时，先拿票（不判定），之后照常判定。同组合同模型的拿票在进程内合并，并写入共享存储供 A/B 两槽复用。组合退役或换代后，所有模型的票一并作废。

**业务请求**：覆盖客户端自带的 state，固定发送该模型的票与配套 Cookie。21 完整交付；29 在任何输出前关闭响应，组合立即退役，满足安全条件时同账号换网关重放原请求：无工具或仅客户端工具、没有已交付内容。含上游工具（如 web_search）的请求不盲目重放。跨账号回退交给原有会话与安全续接规则。

**轮换**：窗口用满后（有需求或持续模式）准备一张备用票：选一个网关，取 S0 并重放判 21 后进入 `standby`，当前窗口继续服务。当前窗口遇到 29、业务失败、Cookie 将到期、被移出限定网关或需要换窗口时，备用票立即转为服务中（不再找票），所有尚未发送的请求切换过去，已发送的照常完成；旧窗口最后一个在途请求结束后才开始静置。没有备用票时（Cookie 将到期、窗口失效、29 之后）按找票流程直接找替代窗口。备用票超过 50 分钟未启用就丢弃并静置。全部候选处于静置时交给原账号选择器尝试其他授权账号；无可用账号时明确报错，不会绕过静置或使用已停用的组合。

## 已验证事实

以下来自 2026-10-12 的真实实验，均以响应头信号为准。

- **保活**（测试账号 1，69 分钟，持续模式）：397 次请求中 29 共 29 次，切换 29 次；满血窗口 79–184 秒，中位数约 125 秒；找票中位数 3.3 秒。1h 静置后复测 24 个组合只有 1 个恢复，因此默认初始静置定为 2h。
- **换网关缓存**（测试账号 2，固定 session 与 `prompt_cache_key`）：7 次网关切换后的请求全部命中约 98.9%；追加对话 18 轮，除冷启动外全部命中。换网关不必然丢缓存。
- **同一张票双缓存**（测试账号 3）：同一 S0、同一网关交替发送两段不同前缀（各自 session 与 cache key），第二、三轮都命中 12,032 tokens。同一个 turn state 可以同时保留多份缓存。
- **跨模型**：Astra 的票上 6.1 Sol 返回 state（8/8，且不是回显原值），而夹在中间的 Astra 为 21；Sol 的票上 Astra 返回 state（3/3）。票必须按模型分开。
- **正常账号的 state 有效期约 1 小时**（2026-10-13，PRO20，6.1 Sol，单网关）：拿到一个 state 后，在 0、5、10、15、20、30、40、50、55、59 分钟用同一个 state 重放，带路由 Cookie 与不带 Cookie 两组全部不返回新 state；60 分钟那一刻两组都返回了新 state。每个时间点另取全新 state 立即重放作对照，22 次全是 21，账号一直满血。所以窗口外旧 state 继续满血说明账号满血；超过约 1 小时后重放返回新 state 是 state 过期，不是降智。

## 轮转统计

插件 0.3.0 的顶层标签为“账号与网关 / 轮转明细 / 轮转统计”。界面仅展示窗口时长和简单冷却汇总。

- 轮转明细按账号、网关、批次、窗口起止、窗口时长和状态展示，可筛选和分页。每行“详情”展开备用票拿到时间与启用时间（有备用票时）、验证时间、最后确认可用时间、窗口时长、结束原因（29、失败、Cookie 到期、备用票过期、一轮结束、配置变更等）、检测结果及该代次的检测与访问记录；不展示其他耗时或多层业务小汇总。
- 窗口时长从窗口计时起点（备用票转正的窗口取第一次业务访问，其余取首次上游访问）计到最后一次确认可用的时刻：最后一个判定为 21 的业务访问结束时，没有业务访问时为验证时刻；以 29 结束止于最后一次 21 的结束。进行中且窗口未超过 `windowSeconds`、有在途请求或最近 `windowSeconds` 内有使用的窗口计到查询时刻，之后又被使用且仍为 21 就从新的确认时刻继续算。未验证可用的检测不形成有效窗口，时长显示未知，不填零；从未转正的备用票时长为 0。时间筛选只累计窗口与筛选范围的交集。
- 轮转统计展示各网关的窗口时长分布和“最近 24 小时每个账号各网关的窗口时长”（同一口径，不受页面筛选影响），下方保留简洁的批次表。累计按代次去重，不把不同网关的窗口合计称为整轮自然跨度。
- 冷却汇总保留按实际再访问间隔分组的恢复比例柱图与样本数，同时显示已恢复次数、判定次数、总体恢复比例及恢复样本间隔中位数。比例为 21 / (21 + 29)，失败和待结果不参与，无判定样本留空。间隔按上次真实访问结束至再次访问开始计算，未知间隔不参与分组或中位数；不能据此推定上游准确恢复时刻。
- 账号卡片默认收起，展开后以紧凑标签和限高滚动区容纳约 100 个网关。点击单个网关显示当前状态、当前档、起始档、静置原因、下次可复测时间，以及最近若干个静置周期的完整历史（每一轮的起点、原因、档位变化、复测时间与结果）。

页面查询不触发上游请求。底层历史合同及已采集事实保留兼容，界面不再提供其他耗时指标。完整采集口径见 [设计说明](gateway-pool-usage-design.md)。

## 实现逻辑

规则见上文，本节只写代码如何实现。进程内的状态机与钩子分工、与内核的边界：

**进程与协议**：服务插件（清单 `plugins/gateway-pool/plugin.json` 的 `service` 段：二进制 service/gateway-pool、`kernelApi` 最低 0.6、`memoryMax 64M`、`shutdownSeconds 40`、内核能力 `accounts.read` `quota.read` `upstream.probe` `http.fetch` `groups.move` `log`、`fetchHosts` 只含池服务域名）。`main.rs` 读取 `CPR_PLUGIN_SOCKET` / `CPR_PLUGIN_DATA` / `CPR_PLUGIN_SHUTDOWN_MS`，打开 `$CPR_PLUGIN_DATA/state.sqlite3`，按已保存设置声明归属分组（`owned_groups`）与观察规则（`watched_groups`：正常分组、`*`、`!分组`），启动 SDK 服务；`configure` 保存设置后立即广播新的归属（`ownership.changed`）。声明的钩子：`route.select` `route.prepare` `response.head` `request.finish` `response.observe` `admin.call`。对外访问只走 `caps.rs` 封装的内核能力（读取类预算 5 秒）。

**状态与存储**：`store.rs::Store` 是 SQLite 入口（WAL，单写连接复用，写事务 `BEGIN IMMEDIATE`；管理页报表用独立只读快照；`checkpoint.rs` 后台 WAL checkpoint）。表：`settings`（单行设置）、`pairs`（账号×网关组合状态）、`leases` 与 `gp_hook_leases`（运行租约与钩子租约）、`materials`（配套 Cookie / state 材料）、`model_tickets`（按模型的票）、`catalogs`（网关目录缓存）、`observations` `switches` `group_events`（观测、换网关、分组变动）、`recovery_checks`（恢复检测暂停与领取）、`gp_account_queue` `gp_queue_revision`（共用账号队列与版本；0.6.0 起 `gp_account_queue.degraded_use` 记“允许降智使用”，打开库时缺列自动补）、历史表 `gp_history_meta` `gp_batches` `gp_windows` `gp_accesses` `gp_segments` `gp_cooling`。表结构只增不删，保证热更新回滚后旧版本仍能读取；新旧版本可并存读写，租约在库里，新版本能接管旧版本未结束的租约。

**请求路径**（钩子预算 2 秒，内部 `HOOK_BUDGET` 1.5 秒）：
- `route.select`（`routing/select.rs`）：读账号与额度，调 runtime `active` 得优先顺序、持窗账号和被挡原因，候选记入 `Pool.requests`；`active` 以 `degraded_use` 挡住的账号不进 `blocked`，改放 `ordinary` 交给内核按普通账号调度；原生续接固定账号时返回 `stay` 或 `escape`（固定账号在 `ordinary` 里时一律 `stay`）。
- `route.prepare`（`routing/prepare.rs`）：内核按 wait 间隔反复调。资格 → `plan` → 需要找票则 `probe.rs::start_prepare` 起后台任务（同一账号同时一个，单组合预算 35 秒，最多 4 路并发探测）→ 取材料 → `reserve` → 按模型取票（`ticket.rs`）→ `sent` → `use`（返回租约 ID 与池 Cookie / turn state）。
- `response.head`（`routing/head.rs`）：把业务结果写入状态并按条件归还租约（同一事务）；`next_step` 是事务内外共用的唯一决策：21 → `accept`，29 → `switch`（满足重放安全条件时），失败按类别处理。
- `request.finish`：补访问计时并归还租约。`response.observe`（`routing/observe.rs`）：普通账号降智确认（用返回的 state 同账号重放一次 `hi`，确认后 `switchAccount` 并经 `groups.move` 移组）。
- `background.rs`：每秒恢复检测（领取 → 旧 state 重放 → 两次全新 bootstrap 重放 → `groups.move` 移回）与持续模式找票。`probe.rs` 后台打票：`claim` 领候选 → 取池 Cookie（`pool_service.rs`、`cookie.rs`）→ bootstrap 取 S0 → 重放判 21/29 → `probeResult` 与租约归还同事务；失败类别随 `failure` 记入静置步骤。

**状态机**（`runtime.rs::Store::runtime` 的 `dispatch_action`）：动作 `config` `active` `quota` `plan` `claim` `reserve` `material` `ticket` `sent` `probeAttempt` `ticketResult` `probeResult` `businessResult` `invalidate` `finish` `regroup` `regroupResult` `recoveryCheck` `recoveryResult`；每个动作开头先回收过期租约、清理过期材料与票。组合状态 `unknown` `preparing` `standby` `ready` `draining` `cooling`：`standby.rs` 负责备用票到达（`publish_standby`）、转正（`promote_standby`）、过期与丢弃；`policy.rs` 负责 21/29 判定、窗口起点与到期、退役（`retire` + `settle`）和退避档（`escalate` / `recover` / `lower`）、候选排序；`queue.rs` 负责共用队列与“一轮”清单；`history.rs` 把组合状态同步为 `gp_windows` 等历史行（保存点写入，失败只记缺口，不回滚调度），`history_query.rs::window_figures` 是窗口时长唯一口径。改组合状态一律经 `Store::save`（同步历史并更新队列），退役必须经 `policy::retire` + `policy::settle`。

**管理方法**（`admin.rs::dispatch`，仅管理员角色）：`read`（页面数据：账号、组合、设置、分组变动；不带账号/网关筛选的全量读取里，每个组合的 `coolingSummary` 只有 `recorded`，完整冷却摘要只在筛选时返回，保证响应在 1 MiB 单帧上限内）、`configure`（校验 `settings.rs` 后保存并返回新归属）、`queue.update`（优先级顺序、参与开关 `enabled` 与允许降智使用 `degradedUse`，两个开关可单独或一起提交）、`history.read`（统计查询）、`cooling.reset`（手动调整正在进行的静置：不带参数取消全部，`accountId` 只处理该账号，`restSeconds` 把本次静置缩短到最后访问后该秒数；档位与起始档不变，周期最后一步记下调整时刻）；控制面把页面能力 `gateway-pool.<方法>` 转为这些调用。

**页面**（`ui/page/`）：`GatewayPoolPage.vue` 用 `usePluginPolling` 每 10 秒读 `gateway-pool.read`、每 60 秒读账号/分组目录；标签页“账号与网关 / 分组变动 / 轮转明细 / 轮转统计”；拖动排序与开关走 `gateway-pool.queue.update`，设置弹窗走 `gateway-pool.configure`，统计组件走 `gateway-pool.history.read`。

## 实现位置

网关池是 Rust 服务插件（`plugins/gateway-pool/`，清单 `plugin.json`）。内核经 socket 调用插件的请求路径钩子；管理页经控制面 `admin.call` 转发到插件的管理方法。插件对外访问（账号、额度、上游探测、池服务）全部经内核能力，状态保存在插件自己的 SQLite（`state.sqlite3`，管理视图不返回 state、Cookie、账号 Token 或池服务凭证）。

`plugins/gateway-pool/service/src/`：

| 文件 | 职责 |
| --- | --- |
| `main.rs` / `lib.rs` | 进程入口（读环境、打开存储、声明归属并启动服务）/ crate 导出 |
| `admin.rs` | `GatewayPoolService`：钩子分发；`admin.call` 方法分派（`read`、`configure`、`queue.update`、`history.read`） |
| `caps.rs` | 钩子与内核能力清单；内核能力（`accounts.read`、`quota.read`、`upstream.probe`、`http.fetch`、`groups.move`、`log`）的类型化封装 |
| `store.rs` | SQLite 入口：写事务、只读快照、`configure`、页面读取 `view`、统计入口、启动清理 |
| `runtime.rs` | 运行状态机 `Store::runtime`：`plan`/`claim`/`reserve`/`sent`/`probeResult`/`businessResult`/`finish`/`invalidate`/`quota`/恢复检测/分组等动作，租约与回收 |
| `standby.rs` | 备用票：到达（`publish_standby`）、转正（`promote_standby`）、过期与丢弃 |
| `policy.rs` | 设置默认值、判定 21/29、退役与退避档（起始档、升降档）、窗口到期、候选排序 |
| `settings.rs` | 设置校验（范围、格式、退避方式与步长） |
| `queue.rs` | 共用账号队列：顺序、开关、额度、本轮待完成网关 |
| `history.rs` | 历史表（窗口、访问、分段、批次、静置周期）的结构与写入，启动时标记残留访问为中断 |
| `history_query.rs` | 历史只读投影：窗口时长口径、批次、冷却分析、最近 24 小时 |
| `pool_service.rs` / `cookie.rs` | 池服务目录与 Cookie 的请求 / 路由 Cookie 解析、合并、校验 |
| `checkpoint.rs` / `ids.rs` / `num.rs` / `util.rs` / `error.rs` | WAL checkpoint 线程 / 标识来源 / 数值语义 / 数据库与 JSON 小工具 / 错误类型 |

`routing/`（请求路径钩子，状态机本体仍在 `runtime.rs`）：

| 文件 | 职责 |
| --- | --- |
| `mod.rs` | `Pool`：设置与目录缓存、材料与票的内存缓存、租约状态、访问计时 |
| `select.rs` | `route.select`：资格、额度、账号优先级与续接去留 |
| `prepare.rs` | `route.prepare`：计划窗口、预约、按模型取票、登记发送 |
| `probe.rs` | 后台打票（取 Cookie、bootstrap、重放）与失败类别 |
| `ticket.rs` | 窗口内其他模型拿票 |
| `head.rs` | `response.head` 判定（21/29）与 `request.finish` 收尾，业务结果与租约归还同事务 |
| `observe.rs` | `response.observe`：普通账号降智确认与移组 |
| `background.rs` | 每秒的恢复检测与持续模式找票 |
| `access.rs` | 账号资格与网关目录读取 |
| `persistence.rs` | 热更新接管用的租约事实持久化 |

测试（`service/tests/`）：`runtime_pool.rs`（状态机、备用票、退避）、`runtime_history.rs`（历史与窗口时长口径）、`runtime_queue.rs`、`settings.rs`、`storage.rs`、`queue.rs`、`read.rs`、`hooks_route.rs` / `hooks_select.rs` / `hooks_observe.rs` / `hooks_recovery.rs`（真实插件进程加模拟内核，`hooks_support/` 为装置）、`common/`（直接读写库的测试装置）。内核侧端到端是 `backend/crates/providers/openai/tests/provider/contract/gateway_pool_e2e.rs`。

页面（`ui/page/`）：`GatewayPoolPage.vue`（标签页与数据读取）、`components/AccountPoolCard.vue`（账号卡片与网关标签）、`PoolGatewayDetail.vue`（单个网关的状态与静置历史）、`PoolStatistics.vue`（轮转明细与轮转统计）、`PoolUsageCharts.vue`（窗口时长分布与最近 24 小时图）、`PoolCoolingSummary.vue`（冷却汇总）、`PoolGroupEvents.vue`（分组变动）、`PoolSettingsModal.vue`（设置）、`model.ts` / `history.ts`（类型与展示工具）。

## 部署

主网关配置（每个 A/B 槽目录下的 deploy/config.yaml，两槽保持一致；模板见 `deploy/config.example.yaml`）：

```yaml
plugins:
  registry_path: /etc/proxy-for-self-kernel/registry.json
  ownership_cache_path: /var/lib/api-hub/plugins/plugin-ownership.json
openai:
  plugin_routing: true   # 网关池经插件钩子参与选号；旧的 openai.gateway_pool 段已无效
```

- 网关进程需加入 `proxy-for-self-plugin-ipc` 组（连接插件 socket），并对归属缓存目录有写权限（单元 `ReadWritePaths`）。
- updater 配置的 `service_plugins` 段登记允许的能力（`accounts.read`、`quota.read`、`upstream.probe`、`http.fetch`、`groups.move`、`log`）与 `fetchHosts`（池服务域名）；updater 写 `/etc` 下注册表需要沙箱 drop-in 的 `ReadWritePaths`。细节见 [插件平台部署](../deploy/plugin-platform/README.md) 的“服务插件”一节。
- 控制面带 `--service-registry` 启动，页面的 `gateway-pool.*` 管理调用经它转给服务实例。
- 发布：控制面把新包装入插件存储 → updater `activate` 新版本（新实例健康后切换注册表，旧实例排空）；回退用 updater `rollback`。主网关只在内核协议变化时才需要 A/B 发布。
- 数据：`/var/lib/proxy-for-self-plugin-data/gateway-pool/state.sqlite3`（含设置与池服务 Bearer），备份时限制访问；升级前用 SQLite 在线备份做快照。
- 线上版本、端口与实例名以 VPS 实际状态和维护手册为准。

## 验证

- 插件服务：`scripts/build-plugins-local.sh <仓库路径> 4096 3 test gateway-pool`，覆盖状态机、备用票、退避、历史口径、设置、队列与钩子；`... clippy gateway-pool` 做 clippy。
- 内核端到端：`backend/crates/providers/openai/tests/provider/contract/gateway_pool_e2e.rs`（`gateway_pool_plugin_` 开头的忽略用例，真实插件进程加临时 PostgreSQL）。
- 前端：`node scripts/check-plugin.mjs gateway-pool`、`node scripts/build-plugin.mjs gateway-pool`、相关 lint 与 `pnpm --dir frontend typecheck`。
- 本机页面预览：暂无专用脚本（旧预览依赖已删除的 Python 实现）；需要时用 `docs/plugin-stage.md` 的隔离测试环境起真实插件进程。

## 尚未验证

- 2h 及以上静置档的恢复率。
- 原生 WS 下的判定方式（当前直接拒绝）。
- 池服务在大量账号并发下的限流行为；池服务自身的限流错误目前按检测失败处理。
