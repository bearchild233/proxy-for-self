# 独立系统更新服务

发布源固定为 `ckcyian23/proxy-for-self` 的正式 GitHub Releases。正式发布需要明确授权；普通 push 不发布、不部署。

网关通过私有 Unix socket 提交版本号。旧单实例安装使用原地更新流程；配置 `slot_config` 的实例拒绝旧更新/重启入口，网关发布通过独立 [A/B 执行器](../../deploy/ab-slots/README.md)。备用槽启动和验收成功后才切入口，再排空旧槽；插件更新继续由独立平台处理。

第一版支持 Linux amd64，并要求目标迁移清单与当前数据库版本及 checksum **完全相同**。含数据库迁移的发行版会拒绝自动安装，需要维护者安排维护升级；不会为了回退旧程序自动恢复数据库、覆盖更新后的业务数据。主更新包仅包含 Rust 网关和前端，保留已安装 Worker、出口代理运行时和旧前端资产。

## 安装

1. 用 `config.example.json` 创建 root 所有的 `/etc/proxy-for-self/updater.json`（0600），按实例修改路径与数据库连接信息。
2. 创建 root 独占的 `updater.pgpass`（0600，标准 PostgreSQL passfile），不要把密码放入仓库、命令参数或管理 API。
3. 将 `updater.py`、`plugins.py`、`plugin_manifest.py`、`plugin_registry.py` 和 `login_security.py` 放入 root 所有的 `/opt/proxy-for-self-updater/`。安装 `deploy/systemd/proxy-for-self-updater.service`；按实例修改 Group 和允许写入的路径。
4. 主配置 `host.system_update.managed_socket` 指向私有 socket，`self_restart_enabled: true`。版本由构建元数据提供，不在配置里固定旧版本。网关用户仅需要连接 socket 的组权限。
5. 确认主服务具有 `Restart=on-failure` 或 `always`，停止超时足以完成请求排空；启用独立 updater。首个正式 GitHub Release 发布前没有可安装更新。

`python -m unittest discover -s services/updater -v` 验证包路径、校验、数据库兼容与回退行为。

中断恢复：如果 updater 自身中断，状态标记 `recovery_required` 并拒绝新的写操作。维护者应先检查 current、主服务健康和备份，再修复状态；不能盲目删状态或重复安装。磁盘至少预留 1.2 GiB，并另行按数据量预留备份空间。旧 release 和数据库备份不自动删除。

## 插件平台

配置、清单、身份隔离、前后端扩展和生命周期见 [插件平台](../../docs/plugin-migration.md) 与 [原生 Worker 合同](../../docs/plugins.md)。更新器读取 root 管理的动态注册表；插件更新与系统更新互斥，禁用插件不影响主网关。

### 插件部署必须检查实际服务依赖

主网关不得通过 Requires、Requisite、BindsTo 或 PartOf 依赖可禁用的 Excel socket/service。显式停止依赖会连带停止网关，`Restart=on-failure` 对这种停止不生效。检查 `systemctl show <网关> -p Requires -p Requisite -p BindsTo -p PartOf` 的实际值；systemd 不能通过空的 drop-in 依赖字段删除原 unit 的依赖，需修正定义后 daemon-reload 再检查。插件管理器在受理和实际停止前均检查此条件；违反时不得改变准入状态或停止服务。

整批部署应由独立 systemd unit 执行，并配置独立 OnFailure 恢复 unit、有限执行时间、受保护的旧版本/配置备份和健康检查。恢复任务不得依赖正在更新的 API 或交互式终端。单实例二进制切换仍可能需要客户端重连；服务自动恢复和当前流不中断是不同保证，不能宣称无感更新。


启用必须先在仍禁止业务准入时通过私有 `/healthz` 检查（15秒上限），失败维持禁用，不能只凭 socket active 宣告成功。PluginGate 允许此无上游访问的健康路径交回 Worker 校验UDS。首次部署根目录应显式 chmod 0755，不能假设 mkdir(mode=0755) 在 root 的 umask=0077 下仍产生可遍历目录。安装器创建的runtime及其内部目录也必须可供Worker用户读取。

明确执行重启时，独立更新器直接调用systemd，网关停止接收新请求并优雅排空；持续流量不会因前置空闲等待使重启反复超时。下载更新和回滚仍在切换版本前检查空闲，失败恢复由独立任务承担。

登录防护使用同进程的独立只读 Unix socket，配置和安装见 [登录防护](../../deploy/login-security/README.md)。私有管理 socket 提供固定 `/login-security`、`/login-security/policy`、`/login-security/unban` 协议；不接受任意命令、jail 或路径。策略和封禁数据跨更新器重启保留。部署时同目录安装 `login_security.py`，未配置时不启用监听。
