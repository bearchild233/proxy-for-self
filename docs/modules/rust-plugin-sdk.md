# rust-plugin-sdk（Rust 插件 workspace 与 SDK）

<!-- doc-sync paths: plugins/sdk plugins/Cargo.toml plugins/package.json -->

## 职责与边界

`plugins/Cargo.toml` 是**独立于 backend 的 Rust workspace**，承载所有服务插件（常驻进程）的 crate 以及它们共用的运行时：

- `plugins/sdk/plugin-sdk`：Unix socket 插件运行时。插件实现 `Plugin` trait，SDK 负责监听 socket、握手、帧收发、钩子分发、能力客户端、排空与关停；授权和业务策略不在这里（内核授权，插件自决）。
- `plugins/sdk/plugin-bench`：基准测量工具（启动 example-echo、统计钩子延迟与 RSS）。
- 成员还有 `plugins/example-echo/service`（示例）与 `plugins/gateway-pool/service`（网关池，见 [网关池](../gateway-pool.md)）。

依赖 `backend/crates/kernel-protocol`（路径依赖；edition / license / rust-version / lints 从 backend workspace 继承，两个 workspace 各有 `Cargo.lock`）。SDK 版本取 workspace 的 `0.0.0`；协议版本 0.6 由 `kernel-protocol` 决定。

## 实现逻辑

- **描述符**（`plugins/sdk/plugin-sdk/src/plugin.rs`）：`Descriptor` 显式声明插件 ID 与内容版本、`kernel_api{major,min_minor}`、钩子名、能力名、`owned_groups`、`watched_groups` 与 `shutdown_timeout`（没有默认值，必须由插件给）；`Settings::from_env` 读取 `CPR_PLUGIN_SOCKET` 与 `CPR_PLUGIN_DATA`（socket 父目录由监督进程准备，SDK 清理残留 socket、拒绝覆盖普通文件、监听权限 0660，数据目录 0700）。
- **`Plugin` trait**：`health`（默认 ready）、`route_select` / `route_prepare` / `response_head` / `request_finish` / `admin_call`（默认 `not_supported`）、`response_observe`（默认 accept）、`start(kernel)`（取后台任务用的内核句柄）、`stop_background`（drain / shutdown 共用的同步停止信号，必须幂等不阻塞）。方法返回 `Send` future，可直接写 `async fn`。
- **运行时**（`plugins/sdk/plugin-sdk/src/runtime.rs`）：`Server::bind(settings, descriptor, plugin)` → `run()`（监听 SIGTERM）或 `run_until(信号)`。每条连接独立握手与编号，reader 把请求分发为并发 task，独立 writer 写完整帧（取消或超时不会中断半帧写）；超限长度、非法帧、握手前非 hello 消息、未知 `role` 使连接关闭。连接角色：`kernel`（缺省）承担全部钩子与能力并接收归属通知；`admin` 只处理 `health` 与 `admin.call`，其他方法返回 `forbidden`，通知类钩子不处理，也不参与后台 `Kernel` 句柄选路。
- **能力客户端**（`plugins/sdk/plugin-sdk/src/client.rs`）：`Context::call::<_, R>(method, &params, timeout)` 在原连接上调用内核能力，`Context::log` 发日志通知，`Context::data_dir()`；后台任务用 `Kernel`（`Server::kernel()` / `Context::kernel()`）选取首条仍可用的内核连接，不自动重试，无连接返回 `unavailable`；管理连接上处理 `admin.call` 时发出的能力调用改走内核连接。`Kernel::set_owned_groups` / `set_ownership` 发送 `ownership.changed`。调用预算由调用方显式提供，没有隐含超时。
- **传输**（`plugins/sdk/plugin-sdk/src/transport.rs`）：`Peer` / `Outbound`，请求编号、未匹配响应丢弃。
- **排空与关停**：`drain` 后 `route.prepare` 返回 `fail/plugin_draining`，`response.head` 与 `request.finish` 继续处理；`inFlight` 是已准入未完成写回的 SDK 调用数（不含 drain 本身与管理连接调用）。`shutdown` / SIGTERM 停止接受新连接与调用，等待在途调用完成，超过 `shutdown_timeout` 则取消剩余任务返回超时。SDK 不持有业务租约，租约持久化由插件负责。
- **测试装置**（`plugins/sdk/plugin-sdk/src/testing.rs`，feature `testing`）：`MockKernel::connect()` 完成握手，`call` / `notify` 发钩子，`push_reply(CapabilityReply)` 按顺序脚本化能力应答（不匹配返回 `invalid_params`，用尽返回 `not_supported`），`notifications()` 取日志通知，`remaining_replies()` 检查是否消费完；`set_handler` 可动态应答；把 `HelloParams.role` 设为 `Admin` 可模拟管理连接。

## 文件表

| 文件 | 职责 |
| --- | --- |
| `plugins/Cargo.toml` | workspace 成员、共享依赖版本（serde / serde_json / tokio）、lints、release 配置（thin LTO、`panic = abort`） |
| `plugins/package.json` | 插件目录的 Node 元数据（私有包标记） |
| `plugins/sdk/README.md` | SDK 使用说明、基准测量、依赖许可证表 |
| `plugins/sdk/plugin-sdk/src/lib.rs` `plugin.rs` `runtime.rs` `client.rs` `transport.rs` `testing.rs` | 见上 |
| `plugins/sdk/plugin-sdk/tests/runtime.rs` | 运行时契约测试（需要 `testing` feature） |
| `plugins/sdk/plugin-bench/src/main.rs` | 基准工具 |

## 数据、配置与接口

无持久化。环境变量 `CPR_PLUGIN_SOCKET` / `CPR_PLUGIN_DATA`（由 updater 在 env 文件里设置，见 [updater](updater.md)）。插件构建约定：服务 crate 位于 `plugins/<插件ID>/service/Cargo.toml`，包名与 `plugin.json` 的 `service.binary` 文件名一致；发布包含 `x86_64-unknown-linux-gnu` release 二进制，由 `scripts/build-plugin.mjs` 经 WSL 构建。

## 改动指引

- 新增服务插件：建 `plugins/<id>/service/`，加入 `plugins/Cargo.toml` 成员，`plugin.json` 写 `service` 段（`kernelApi`、`capabilities`、`fetchHosts`、`memoryMax`、`shutdownSeconds`），updater 配置里登记 `allow` 上限（见 [updater](updater.md)）；参照 [example-echo](example-echo.md)。
- 协议字段变化先改 [kernel-protocol](kernel-protocol.md)，再改本 SDK 的 `Descriptor` / `Plugin` 默认实现与 README。
- 本机构建与测试（Git Bash 先 `export MSYS_NO_PATHCONV=1`）：`wsl.exe -d ApiHubBuild-20260928 -u root -- bash "<仓库>/scripts/build-plugins-local.sh" "<仓库>" 4096 3 <check|test|clippy|build|release> [包...]`；target 独立在 `/work/proxy-for-self/plugins-target`，不部署。
- SDK 不实现内核能力，也不应加入业务策略；不要为单个插件调整帧上限或错误码。
