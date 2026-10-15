# example-echo 插件（示例服务插件）

<!-- doc-sync paths: plugins/example-echo reviewed: f5b6b52 -->

## 职责与边界

最小的**服务插件**示例，用于验证内核协议、插件 SDK、updater 的服务插件监督链路和基准测量，不提供业务功能（也没有界面页面：`pages: []`）。它演示：清单的 `service` 段怎么写、`Plugin` trait 怎么实现、环境变量怎么用、怎么在进程测试里验证 SIGTERM 退出。开发新服务插件时参照它，不要把它当成生产功能依赖；清单里的名称与介绍仍是 `TODO(design)` 占位，需要设计负责人确定后才会进入预装列表。

- 依赖：`plugins/sdk/plugin-sdk`（见 [rust-plugin-sdk](rust-plugin-sdk.md)）、`kernel-protocol`。
- 调用方：`plugins/sdk/plugin-bench` 以它为被测进程；updater 的服务插件单元测试和本机阶段验证用它做安装/启用/热更新演练（`services/updater/config.example.json` 的 `service_plugins.allow` 里登记了它的能力上限）。

## 实现逻辑

`plugins/example-echo/service/src/main.rs`：
- `Echo` 实现 `Plugin`：`route_select` 原样返回候选账号作为优先顺序（有固定账号则 `Stay`，`ordinary` 为空）；`route_prepare` 返回 `Fail{not_supported}`；`response_head` 返回 `Accept`；`admin_call` 把参数原样返回（页面能力 `example-echo.echo` 经控制面转发到这里，用来验证管理连接链路）。
- `main`：读取环境变量 `CPR_PLUGIN_SHUTDOWN_MS` 作为关停预算（SDK 要求必须显式提供），构造 `Descriptor`（ID `example-echo`、`kernel_api{major: KERNEL_API.0, min_minor: 5}`、四个钩子、能力 `log`、不声明归属分组），`Server::bind(Settings::from_env()?, …).run()` 监听 SIGTERM。
- 清单 `plugins/example-echo/plugin.json`：`kind: ui` 且 `pages: []`，`service.binary = service/example-echo`、`kernelApi {major 0, minMinor 5}`、能力 `["log"]`、`fetchHosts []`、`memoryMax 64M`、`shutdownSeconds 30`；`capabilities` 里的 `example-echo.echo` 是服务插件自有管理能力（`<插件ID>.<方法>`）。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/example-echo/plugin.json` | 清单（服务段、占位名称与介绍） |
| `plugins/example-echo/service/Cargo.toml` | crate（包名必须与 `service.binary` 文件名一致） |
| `plugins/example-echo/service/src/main.rs` | 示例实现 |
| `plugins/example-echo/service/tests/process.rs` | 进程级测试：环境变量、echo、SIGTERM 退出 |

## 数据、配置与接口

环境变量：`CPR_PLUGIN_SOCKET`、`CPR_PLUGIN_DATA`（SDK 必需）、`CPR_PLUGIN_SHUTDOWN_MS`（本示例必需）。不使用数据目录。

## 改动指引

- 示例只在 SDK 或协议接口变化时跟着改；保持“最小”，不要往里加业务逻辑。
- 构建与测试：`wsl.exe -d ApiHubBuild-20260928 -u root -- bash "<仓库>/scripts/build-plugins-local.sh" "<仓库>" 4096 3 test example-echo`；打包 `node scripts/build-plugin.mjs example-echo`（会通过 WSL 构建 release 二进制）。
