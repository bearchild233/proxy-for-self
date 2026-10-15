# Rust 插件 SDK（kernelApi 0.6）

`plugins/Cargo.toml` 是独立 workspace。`kernel-protocol` 是 backend workspace 成员，
插件通过路径依赖使用它；其 edition、license、rust-version 与 lints 从所属的 backend
workspace 继承，不从消费它的 plugins workspace 继承。两个 workspace 各自维护 Cargo.lock。
插件构建镜像保留 backend workspace 元数据和成员，Cargo 不构建无关 backend crate。

## 编写插件

实现 `Plugin`，参照 `example-echo/service/src/main.rs`（服务插件的 crate 位于 `plugins/<插件ID>/service/`）。方法返回 `Send` future，可以用 `async fn`
实现。默认 `health` 返回 ready，`response.observe` 返回 accept；其他 hook 默认返回 `not_supported`。`request.finish` 是
通知，其处理错误不会产生响应。通用请求错误使用协议的六个标准码。

创建 `Descriptor`，显式声明内容版本、最低 API 次版本、hooks、capabilities 和
`shutdown_timeout`。SDK 版本来自本 crate 的包版本（目前为 workspace 的 `0.0.0`）。
声明能力不意味着授权；内核负责每次调用的权限检查，本 SDK 不实现内核能力。

协议 0.5 的插件声明 `minMinor: 4` 即可，不接受 0.1～0.3 插件；0.5 新增的 `hello.role` 可选，缺省为内核连接。调度前的 `route.select` 接收
内核过滤后的账号，返回 `preferred/live/blocked/pinnedDecision`；选定账号后才调用
`route.prepare`，其参数使用 `accountId`，结果仅有 `use/wait/fail`。
`response.observe.canSwitch` 表示内核是否允许换号；`request.finish.firstByteMs` 表示
发送开始至首个交付字节的耗时。`upstream.probe.includeAccountCookies` 缺省 false，
开启时内核在账号 Cookie 上合并插件 Cookie，账号 Cookie 不会返回插件。
`upstream.probe.requestId` 可选（1–128 位字母、数字、`-`、`_`），作为上游请求 ID；探测不带会话头。
协议 0.6 新增 `accounts.read.model`（逐账号返回该模型是否允许）与 `quota.read.refreshIfMissing`（没有快照时由内核先刷新一次），使用它们的插件声明 `minMinor: 6`。

`Settings::from_env()` 要求 `CPR_PLUGIN_SOCKET` 和 `CPR_PLUGIN_DATA`。
socket 父目录由监督进程准备；SDK 清理残留 socket，拒绝覆盖普通文件，监听权限为 0660。
数据目录创建后设置为 0700。生产调用 `Server::run()` 监听 SIGTERM。

在 hook 中通过 `Context::call::<_, ResultType>(method, &params, timeout)` 调用原连接上的
能力；`Context::log(params, timeout)` 发送日志通知。`Context::data_dir()` 返回私有数据目录。
后台任务使用 `Server::kernel()` 或 `Context::kernel()` 获取 `Kernel`，调用形式相同。
后台调用选取首条仍可用的内核连接，不自动重试；无连接返回 `unavailable`。
能力调用只走内核连接：在管理连接上处理 `admin.call` 时调用 `Context::call` 或 `Context::log`，
同样改由内核连接发出，没有内核连接则返回 `unavailable`。
所有调用预算由调用方显式提供，无隐含超时默认值。

```rust,ignore
let snapshot: plugin_sdk::protocol::QuotaReadResult = ctx.call(
    "quota.read",
    &plugin_sdk::protocol::QuotaReadParams { account_id },
    configured_timeout,
).await?;
```

每条连接独立握手和编号，内核从 1（hello）开始，插件的能力请求从 1 开始。
reader 分发请求为并发 task，独立 writer 写入完整帧；取消或超时不会中断半帧写入。
迟到或未匹配的能力响应丢弃。超限长度、非法帧、握手前非 hello 消息、未知 `role` 使连接关闭。

连接角色由 `hello.role` 决定：`kernel`（缺省）承担全部钩子与能力，并接收归属通知；
`admin` 是插件管理面的直连，只处理 `health` 与 `admin.call`，其他方法返回 `forbidden`，
通知类钩子（含 `shutdown` 与 `request.finish`）不处理。管理连接不参与后台 `Kernel` 句柄的选路与归属广播。

`drain` 对插件进程整体生效：后续 `route.prepare` 返回 `fail/plugin_draining`；
`response.head` 与 `request.finish` 继续处理。`inFlight` 是当前已准入且尚未完成写回的
SDK 调用数（包含正在处理的通知，不包含 drain 本身，也不包含管理连接上的调用），不是业务租约数。
管理连接的 `admin.call` 在排空中照常处理。SDK 不持有业务租约，租约持久化由插件负责。

`shutdown` / SIGTERM 停止接受新连接与调用，保留在途调用需要的能力响应通道；
等待已有调用完成并写回后退出。超过描述提供的预算则取消剩余任务并返回超时错误。
后台任务由插件持有和管理，不计入 SDK 调用数；进程退出前的持久化应放在受跟踪调用中。

## 模拟内核与验证

启用 `plugin-sdk` 的 `testing` feature，使用 `testing::MockKernel::connect()` 完成握手，
`call()` 发送请求、`notify()` 发送通知。`push_reply(CapabilityReply)` 按收到能力请求的
顺序匹配方法和参数；不匹配返回 `invalid_params`，脚本用尽返回 `not_supported`。
`notifications()` 获取日志通知；`remaining_replies()` 检查脚本是否消费完。
`MockKernel` 可以放进 `Arc`，在同一连接并发发送钩子；丢弃它会断开连接并停止读写任务。
模拟管理连接时，把 `HelloParams.role` 设为 `ConnectionRole::Admin` 再调用 `connect()`。

从仓库根目录在 WSL 中执行（示意变量 `repo` 为当前仓库 Linux 路径）：

```bash
bash "$repo/scripts/build-local.sh" "$repo" 4096 3 check kernel-protocol
bash "$repo/scripts/build-local.sh" "$repo" 4096 3 test kernel-protocol
bash "$repo/scripts/build-local.sh" "$repo" 4096 3 clippy kernel-protocol --all-targets -- -D warnings
bash "$repo/scripts/build-plugins-local.sh" "$repo" 4096 3 check --all-features
bash "$repo/scripts/build-plugins-local.sh" "$repo" 4096 3 test --all-features
bash "$repo/scripts/build-plugins-local.sh" "$repo" 4096 3 clippy --all-targets --all-features -- -D warnings
bash "$repo/scripts/build-plugins-local.sh" "$repo" 4096 3 release
```

新入口还接受 `build`，任务后可列多个包名，例如 `check plugin-sdk example-echo`。
它共用 backend 构建锁和 cgroup，target 独立放在 `/work/proxy-for-self/plugins-target`，
dev 与 release 都保留增量缓存。同步目前限于本任务的 `sdk/` Rust 成员；新增其他目录的
Rust 插件时需扩展同步范围。任何构建命令均不部署。

## 基准测量

release 构建后执行：

```bash
/work/proxy-for-self/plugins-target/release/plugin-bench \
  /work/proxy-for-self/plugins-target/release/example-echo \
  "$warmup_count" "$call_timeout_ms" "$shutdown_ms"
```

工具启动独立 example-echo 进程，使用临时 socket 和私有目录，测量完成后通知关停并清理。
进程异常或测量失败会终止子进程。统计为 nearest-rank p50/p99/max；并发测量为同连接
8 个任务各 2000 次，顺序测量为 10000 次。RSS 从插件子进程的 `/proc/<pid>/status` 读取，
分别记录预热后和全部测量后；输出为纯文本，实验报告保存在 `.build/experiments/2026-10-13/`。
示例插件的 `CPR_PLUGIN_SHUTDOWN_MS` 由该工具的第四个参数传入。

TODO(design): 任务未指定示例插件的发布关停预算、基准预热次数和调用超时预算；这些参数
均为必填，本次实验取值必须在报告中记录，不构成生产默认值。

`groups.move.reason` 复用任务说明统一的 `{code,message}` 理由类型。

TODO(design): `drain.inFlight` 未明确是调用数还是租约数；当前 SDK 仅跟踪调用数，需在
业务租约接入前确认是否需要插件参与报告。

TODO(design): 未给出握手超时、连接数、并发调用数和写队列上限；当前不设置未授权阈值，
不能将本 SDK 直接视作具备恶意对端资源隔离的生产运行时。

TODO(design): 网关的 `backend/apps/gateway/tests/architecture.rs` 冻结了 9 个 workspace
成员；加入 kernel-protocol 后为 10 个。任务 001 不允许修改其他 backend crate，需另行
授权扩展该架构测试的成员清单与依赖 DAG，才能恢复网关全量架构检查。

## 依赖

新增 crate 只直接依赖项目已有的三个外部库，没有引入 async-trait、RPC 框架或测试框架。

| 依赖 | 清单版本 / 锁定版本 | 许可证 | 用途与替代方案 |
| --- | --- | --- | --- |
| serde | 1.0.228 / 1.0.229 | MIT OR Apache-2.0 | 协议类型派生；替代为手写序列化，易偏离合同 |
| serde_json | 1.0.150 / 1.0.151 | MIT OR Apache-2.0 | JSON 正文与任意 JSON 字段；替代为其他 JSON 库会扩大依赖与转换 |
| tokio | 1.52.3 / 1.52.3 | MIT | 异步 socket、IO、task、信号和子进程；协议 crate 只启用 io-util；替代为 std 阻塞 IO 加自建线程/调度器 |

清单沿用 backend 的兼容版本范围，锁文件保留当前已解析版本，没有降级或升级 backend
既有依赖。内部 kernel-protocol 与 plugin-sdk 均为 0.0.0、Apache-2.0。
完整间接依赖由 `plugins/Cargo.lock` 锁定，实验报告附有 Cargo metadata 许可证清单。
