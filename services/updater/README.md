# 独立系统更新服务

发布源固定为 `ckcyian23/proxy-for-self` 的正式 GitHub Releases。仅管理员点击更新后安装；普通 push 不发布、不部署。

网关通过私有 Unix socket 提交版本号。独立 systemd unit 下载并校验发布包，备份配置及 PostgreSQL，等待活跃模型请求结束，原子切换 current，再由 systemd 优雅重启网关。网关退出不会终止更新服务。健康检查失败恢复上一个程序版本，保留原来的 Restart 自动恢复策略。

第一版支持 Linux amd64，并要求目标迁移清单与当前数据库版本及 checksum **完全相同**。含数据库迁移的发行版会拒绝自动安装，需要维护者安排维护升级；不会为了回退旧程序自动恢复数据库、覆盖更新后的业务数据。主更新包仅包含 Rust 网关和前端，保留已安装 Worker、出口代理运行时和旧前端资产。

## 安装

1. 用 `config.example.json` 创建 root 所有的 `/etc/proxy-for-self/updater.json`（0600），按实例修改路径与数据库连接信息。
2. 创建 root 独占的 `updater.pgpass`（0600，标准 PostgreSQL passfile），不要把密码放入仓库、命令参数或管理 API。
3. 将 `updater.py` 和 `plugins.py` 放入 root 所有的 `/opt/proxy-for-self-updater/`。安装 `deploy/systemd/proxy-for-self-updater.service`；按实例修改 Group 和允许写入的路径。
4. 主配置 `host.system_update.managed_socket` 指向私有 socket，`self_restart_enabled: true`。版本由构建元数据提供，不在配置里固定旧版本。网关用户仅需要连接 socket 的组权限。
5. 确认主服务具有 `Restart=on-failure` 或 `always`，停止超时足以完成请求排空；启用独立 updater。首个正式 GitHub Release 发布前没有可安装更新。

`python -m unittest discover -s services/updater -v` 验证包路径、校验、数据库兼容与回退行为。

中断恢复：如果 updater 自身中断，状态标记 `recovery_required` 并拒绝新的写操作。维护者应先检查 current、主服务健康和备份，再修复状态；不能盲目删状态或重复安装。磁盘至少预留 1.2 GiB，并另行按数据量预留备份空间。旧 release 和数据库备份不自动删除。

## 插件管理（Linux amd64）

管理员入口：设置 → 插件管理。当前注册插件为 `excel-bridge`，协议 v1；支持安装并启用、禁用、重新安装/更新、卸载。只有管理员会话能调用 `/api/admin/system/plugins` 与 `/api/admin/system/plugins/action`。API 密钥登录用户仍可在“导入配置”中下载、复制自己的配置或导入 CCSwitch，不能操作插件或其他密钥。

插件运行目录独立于主 release：`/opt/proxy-for-self-plugins/excel-bridge/runtime`。网关配置 `openai.excel_plugin_state_file`，Worker 环境 `EXCEL_PLUGIN_STATE_FILE` 指向其上级 `state.json`。文件 root 所有且 0644，父目录 0755；同目录 `activity.lock` root 所有且 0644。普通网关用户只读状态。禁用先原子写入 enabled=false，阻止新请求；Worker 在整个 HTTP/SSE 生命周期持有共享 flock，管理器取得独占锁才停止 socket/service。超时不杀已有请求。卸载删除 runtime，保留状态、Key、用量和审计；重新安装使用同一 Key 配置。普通 Native 请求不受影响，不会将禁用的 Excel Key 偷偷转为 Native。

`updater.json` 增加 `plugins` 对象：`root`、受信任本地 `package` 绝对路径、`sha256`、`version`、`drain_seconds`（建议 620）。根目录及包由 root 维护，客户端不能提供路径、命令或任意安装包。`python services/updater/package_plugin.py --runtime <已验证Worker目录> --source <excel_codex_bridge源码目录> --output <包文件> --version 0.1.0` 构建含依赖的 Python 3.10 插件包，清单声明 id/version/protocol。安装器校验包 SHA、路径和协议后进行导入检查，再原子替换运行目录。上架新包并更新 root 配置后，面板可以独立更新插件，无需重新编译网关；当前没有开放任意第三方插件市场或远程自动下载插件。

Worker systemd ExecStart 使用独立 runtime/venv/bin/python；保持 `Restart=on-failure`、650 秒停止超时、原有内存和并发限制。管理器通过 enable/disable socket 持久化开机状态，systemd updater unit 需允许写入插件根目录和 `/etc/systemd/system`。启用先检查自动重启保护，再开启 socket；Worker 无请求时仍按需退出。系统更新与插件操作互斥。管理器重启遇到未完成插件操作会关闭新请求并标记失败，可重新安装或卸载恢复；不会盲目重放操作。

卸载保留的 root 受信任安装包、备份和历史主 release 用于恢复，不是活跃插件实例；维护者可按备份策略清理。协议升级必须同时更新主网关兼容检查。停机、升级或卸载正在运行的插件时不要绕过管理器直接 rm 或 kill。


### 插件部署必须检查实际服务依赖

主网关不得通过 Requires、Requisite、BindsTo 或 PartOf 依赖可禁用的 Excel socket/service。显式停止依赖会连带停止网关，`Restart=on-failure` 对这种停止不生效。检查 `systemctl show <网关> -p Requires -p Requisite -p BindsTo -p PartOf` 的实际值；systemd 不能通过空的 drop-in 依赖字段删除原 unit 的依赖，需修正定义后 daemon-reload 再检查。插件管理器在受理和实际停止前均检查此条件；违反时不得改变准入状态或停止服务。

整批部署应由独立 systemd unit 执行，并配置独立 OnFailure 恢复 unit、有限执行时间、受保护的旧版本/配置备份和健康检查。恢复任务不得依赖正在更新的 API 或交互式终端。单实例二进制切换仍可能需要客户端重连；服务自动恢复和当前流不中断是不同保证，不能宣称无感更新。


启用必须先在仍禁止业务准入时通过私有 `/healthz` 检查（15秒上限），失败维持禁用，不能只凭 socket active 宣告成功。PluginGate 允许此无上游访问的健康路径交回 Worker 校验UDS。首次部署根目录应显式 chmod 0755，不能假设 mkdir(mode=0755) 在 root 的 umask=0077 下仍产生可遍历目录。安装器创建的runtime及其内部目录也必须可供Worker用户读取。
