# 插件平台安装

本目录提供安装模板。生产采用已验收的部署参数；变更先在独立环境验证，用户授权后发布。

1. 构建主壳及全部插件，执行 `python3 scripts/package-plugin-platform.py --output dist`。校验归档 SHA256，将内容放入新的只读平台发行目录，并将 `/opt/proxy-for-self/platform-current` 指向它。
2. 创建独立系统用户 `proxy-for-self-plugins`；运行目录 `/var/lib/proxy-for-self/plugins` 由该用户写入。网关增加插件只读组，包目录 0750、文件 0640，不得写入平台代码。另建该目录下的 `pricing-runtime`，仅网关用户可写、控制面组可读（目录 2750、状态文件 0640），用于每日价目同步状态与锁。控制面不能读取网关 vault、数据库密码或真实账号认证文件。
3. 使用该用户执行 `python3 services/plugin_host/server.py install --root /var/lib/proxy-for-self/plugins`，初始化 12 个包。CLI不会管理 Excel Worker；它由独立更新器安装。
4. 修改 service 中的站点 Origin、网关端口、updater socket 路径与组。独立安装器组提供固定的本机维护能力，不开放公网。网关策略 drop-in 中两个路径必须指向同一平台/数据目录。
5. Excel service 指向安装器管理的 `runtime/venv/bin/python -I -m excel_codex_bridge.hub_worker`，环境设置 `EXCEL_PLUGIN_STATE_FILE=<Excel根目录>/state.json`，网关 `openai.excel_plugin_state_file` 指向同一文件。单独 socket、Restart=on-failure，停止宽限覆盖排空时限；网关不能强依赖它。安装器注册项的 service、socket_unit、gateway_service 与实际单元保持一致。
6. 安装插件 control-plane unit，检查服务启动、基础页面、角色鉴权和 Worker 状态。反向代理参考 `nginx.locations.conf`，保留现有真实 IP、登录防护与 TLS 配置；`$connection_upgrade` 使用既有 http 层 map。生产不得启用 local-access 或 local-excel-wsl。
7. 网关升级使用 [A/B 发布合同](../ab-slots/README.md)，先完成首次迁入的交接验收。确认平台协议2可用后，在更新器配置中声明 `plugin_platform_protocol: 2`。主站后续网关发布只更新网关/主壳；平台自身更新单独按兼容协议维护。

发布分准备、验收、切换三个阶段。新控制面先绑定回环独立端口，使用独立服务用户验证主壳、登录、角色权限、插件页面和策略文件权限；生产入口继续指向旧服务。准备和验收失败只停止候选服务，不重启网关。恢复必须记录已经发生的切换步骤，最多执行一次，健康检查失败时保留故障状态供检查，禁止自动重复重启。

线上旧网关持有生产数据库排他实例锁，不能与槽版本共同持锁。新版本 A/B 模式支持共享数据库；控制面独立端口验收不等于生产网关已经迁入槽位。主壳可通过 `--assets /opt/api-hub/current/frontend` 逐请求跟随发布链接；反向代理必须覆盖 `X-Real-IP` 后转发到控制面。

插件包普通更新直接从管理页上传；独立发行使用 `plugin-release.yml`。Excel 界面包与 Worker 运行包是两个产物：界面可单独更新；更换 Worker 时先将对应版本/哈希写入受信任安装器配置，再从插件管理执行更新。不要将运行包冒充 UI 包上传。

更新需要跨 UI/Worker 的不兼容协议变化时，应提升平台合同并统一验收；不承诺此类变更无重启。Python插件是受信任代码，systemd硬化不等同于可以运行任意第三方恶意插件。

上线时同步 VPS 维护文档的服务名、目录、升级/恢复命令和当前版本；不复制旧的过时说明。

当前生产控制面 B 使用 `proxy-for-self-plugins-b.service`、回环18337；A 服务 `proxy-for-self-plugins.service` 在18335保留配置且停止、自启禁用。共享 Vue 主壳与12个内置组件包已上线；服务 `--assets` 指向当前平台发行目录的 `frontend/dist`。后续框架升级先启动空闲控制面，再条件切换公网 Caddy 中的两条控制面 upstream；持久化配置同步，验收后关闭原服务。插件包更新无需重启推理网关。第三方共享 Vue 运行时仅接受宿主显式信任的包。
