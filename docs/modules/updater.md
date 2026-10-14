# updater（独立更新与插件监督）

<!-- doc-sync paths: services/updater -->

## 职责与边界

以 root 运行的独立 systemd 服务，通过私有 Unix socket 接收固定操作，负责主网关的版本管理和所有“需要特权”的插件相关动作；网关与控制面本身不持有这些权限。

1. 网关版本：旧单实例的原地更新 / 回滚 / 重启；A/B 槽发布执行器（`slots.py`）。
2. UI 插件包存储（`static_plugins.py`，与控制面共用）。
3. 原生 Worker（Excel）的安装、排空、替换与恢复（`plugins.py` + `plugin_registry.py`）。
4. 服务插件监督（`service_plugins.py` + `service_manifest.py`）：systemd 模板单元、运行目录、内核注册表。
5. 登录防护（`login_security.py`）：fail2ban jail 管理与 Caddy forward_auth 只读检查。

- 依赖：systemd / systemctl、PostgreSQL 客户端工具（备份与迁移比对）、Caddy 管理 API（A/B 切换）、fail2ban（登录防护）。仅用 Python 标准库，Python 3.10。
- 调用方：Rust `backend/crates/gateway-host/src/system_update/managed.rs`（socket 协议）、控制面 `services/plugin_host/lifecycle.py`（`/plugins/action`、`/services/action`）、运维 CLI（`slots.py`）。
- 部署与恢复见 [更新器说明](../../services/updater/README.md)、[A/B 发布](../../deploy/ab-slots/README.md)、[登录防护](../../deploy/login-security/README.md)、[插件平台](../../deploy/plugin-platform/README.md)。

## 实现逻辑

**Socket 协议**（`updater.py::Handler`，所有写操作只接受固定字段，非法即 409）：GET `/status` `/login-security` `/plugins` `/services/status`；POST `/operation`（`update` / `rollback` / `restart`）、`/plugins/action`（`{id, action}`）、`/plugins/request`（UI 插件目录/页面/RPC 转发）、`/services/action`（`{id, action, version?}`）、`/login-security/policy|unban`。更新器自身中断（状态为 running 重启）时置 `recovery_required` 并拒绝后续写操作，需维护者核对后手工清除。

**网关更新**（`Updater.execute`）：旧单实例流程——磁盘余量检查（1.2 GiB）→ 备份 → 下载并校验 GitHub Release（仓库、重定向信任链、SHA-256、解包上限）→ 比对目标迁移与当前数据库完全一致（含迁移的发行版拒绝自动安装）→ 等运行中请求结束（最长 180 秒）→ 切换 `current` 链接 → 重启 → 健康检查；失败自动恢复上一版本，恢复失败置 `recovery_required`。重启动作直接交给宿主优雅排空。配置 `slot_config` 的实例拒绝旧入口，发布走 `slots.py`。`release_package.py` 构建带迁移兼容信息的发行包。

**A/B 槽**（`slots.py`）：`SlotController.deploy` 按阶段（preparing → candidate 启动并连续健康 → 切入口 → draining → idle）持久化状态；`SystemdCaddyDriver` 用 systemctl 启停槽、用 Caddy 配置 API 的 ETag 条件更新唯一网关 upstream 并核对进程身份；切换确认前失败先恢复旧入口再停候选，确认后排空失败只保留 `draining`；`resume` 恢复中断阶段，`boot` 开机协调只启动活动槽。

**UI 插件包存储**（`static_plugins.py::StaticPluginStore`，由控制面进程使用）：`plugins/<id>/active.json`（enabled / current / previous）、`releases/<sha256>/` 不可变版本目录、`.leases/` 页面租约（24 小时过期，最多 128 个）；`install` 校验 tar（路径穿越、链接、单文件 4 MiB / 总 16 MiB、`protocol: 2` 清单、资源哈希、`runtime` / `hostApi`、`worker` 与 `policyWorker` 授权、页面与路由、能力不超出平台策略、`service` 段）后原子切换；`action` 做启用/停用/卸载/回退（基础模块不可停用，回退校验期望版本防止误回退）；`cleanup` 只保留当前版、上一版和有租约的版本。

**原生 Worker 管理**（`plugins.py::PluginManager`）：每个 Worker 一个 root 管理的根目录（`state.json`、`activity.lock`、`runtime/`），操作流程“关闭准入 → 取全流活动锁排空 → 停旧 Worker → 原子替换 runtime → 检查自动重启策略与私有 `healthz`（15 秒）→ 开放准入”，失败回滚旧目录与启用状态；Worker 单元不得被主网关 Requires / BindsTo。`plugin_registry.py::PluginRegistry` 热加载 `plugins.registry` JSON，提供目录、页面与 RPC（经 Worker 私有 socket，上限 512 KiB，共用活动锁共享模式），身份限 `admin` / `key` / `device`。`package_plugin.py` 把验证过的 Linux Python 3.10 Worker 环境打成运行包。

**服务插件监督**（`service_plugins.py::ServicePlugins`，操作 `activate` / `enable` / `disable` / `rollback` / `uninstall`，一次只做一个，与系统更新、Worker 操作互斥）：
1. `_package`：从存储取版本包，`service_manifest.validate_service` 校验 `service` 段（二进制路径、`kernelApi` 兼容范围、能力集合、`fetchHosts`、`memoryMax` 16–512 MiB、`shutdownSeconds` 5–600、x86_64 ELF 头）。
2. `_stage`：把二进制复制到 root 所有的运行目录 `<runtime_root>/<实例>/`，复制后再次校验哈希与 ELF，插件用户改不了运行中的文件。
3. `_registration`：能力与 `fetchHosts` 取清单申请值与 `config.allow[插件ID]` 上限的交集；env 文件与 drop-in 写入（`MemoryMax` 等），实例名 `<插件ID>--<内容版本前 12 位>`，单元 `proxy-for-self-plugin@<实例>.service`。
4. `_start` → `_wait_healthy`（通过 `kernel_protocol.health` 在新 socket 上握手，15 秒）→ `_registry_write` 原子改写内核注册表（root 所有、网关可读）→ 等内核切换连接（10 秒）→ 停旧实例（`shutdownSeconds` 作为停止宽限）→ `_retain` 只保留当前与上一版本实例。
5. 失败：新实例整体删除，注册表与旧实例不变；`disable` 先在注册表置 `enabled:false` 再停进程；`uninstall` 删注册表条目与实例，数据目录不动。
状态文件 `service_plugins.state_file`；updater 重启时发现 running 操作则标记 `interrupted` 并要求人工核对（见 README）。`kernel_protocol.py` 是只含 `hello` / `health` / `admin.call` 的最小管理连接客户端。

**登录防护**（`login_security.py`）：`LoginSecurity` 管理固定 jail（`proxy-api-login`、`proxy-panel-login`）的策略（最大失败次数、窗口、封禁时长、上限）与解封；`GuardServer` 是独立只读 socket，Caddy 在登录入口 forward_auth 调用 `check(site, ip)`，失败时阻止登录；封禁镜像用私有 SQLite，跨进程重启保留。

## 文件表

| 文件 | 职责 |
| --- | --- |
| `services/updater/updater.py` | 主程序：状态机、socket 服务、网关更新 |
| `services/updater/slots.py` | A/B 槽发布 CLI 与 Caddy / systemd 驱动 |
| `services/updater/release_package.py` | 构建发行包（网关 + 前端 + 迁移兼容信息） |
| `services/updater/static_plugins.py` | UI 插件版本存储（控制面共用） |
| `services/updater/plugin_manifest.py` | 原生 Worker 清单（`protocol: 1`：页面槽位、operations、展示信息、资源路径）校验 |
| `services/updater/plugins.py` `services/updater/plugin_registry.py` `services/updater/package_plugin.py` | 原生 Worker 生命周期、注册表与 RPC、运行包构建 |
| `services/updater/service_plugins.py` `services/updater/service_manifest.py` `services/updater/kernel_protocol.py` | 服务插件监督、`service` 段校验、管理连接客户端 |
| `services/updater/login_security.py` | 登录防护 |
| `services/updater/config.example.json` | 配置模板（`release_root`、`socket`、数据库、`service_plugins.*`、`login_security`、`plugins`） |
| `services/updater/test_*.py` | 单元测试，`python -m unittest discover -s services/updater -v` |

## 数据、配置与接口

- 配置 `/etc/proxy-for-self/updater.json`（root 0600）；状态文件 `state_file`、服务插件状态、A/B 状态均原子写入；PostgreSQL 密码通过 root 独占的 pgpass 文件。
- 内核注册表（`service_plugins.registry_file`）：`{"plugins":[{id, enabled, socket, grants, fetchHosts, uid?}]}`，格式由 `gateway-host` 读取（见 [gateway-host](gateway-host.md)）。
- 协议版本常量 `KERNEL_API_MAJOR/MINOR`（当前 0.6，最低 0.4）与 `KERNEL_CAPABILITIES` 必须与 `backend/crates/kernel-protocol` 保持一致。

## 改动指引

- 任何写操作都保持“固定字段、不接受路径/命令/下载地址”；新增动作先加到 `Handler` 的白名单，再在 `Updater` 内加互斥判断。
- 服务插件的时序（先启动新实例、健康通过后才改注册表、再停旧实例）不要调换；注册表写入必须原子。
- 校验上限（清单 64 KiB、单文件 4 MiB）要与 `static_plugins.py` 和 `service_plugins.py` 两处保持一致。
- 协议版本变化同步 `kernel_protocol.py` / `service_manifest.py` 与 `backend/crates/kernel-protocol`。
- 恢复路径一次失败后禁止循环重启；A/B 的 `attention` 状态需要人工检查。
