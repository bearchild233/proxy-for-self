# 原生 Worker 合同

页面包与插件平台合同见 [插件平台](plugin-migration.md)。本文仅描述 Excel 等需要独立 Linux 运行环境的 Worker 安装器。

更新器配置 `plugins.registry` 指向 root 管理的 JSON 数组，读取时热加载。每项包含 id、root、package、sha256、version、socket、drain_seconds；可选 service、socket_unit、gateway_service 指定实际 systemd 单元。默认 Excel 单元为 api-hub-excel.service/socket，默认网关为 api-hub.service。

包根目录的 `plugin.json` 使用原生 `protocol:1`，与界面包 `protocol:2` 是两个合同。Linux Python3.10运行包包含锁定依赖、Worker源码、许可证；普通文件归档禁止链接、路径穿越和安装脚本。运行包更新入口仅接受安装器注册的已校验包。

更新器的私有 Unix socket 提供 `/plugins` 和 `/plugins/action`。插件控制面已完成管理员身份校验后才调用固定的 Worker 操作。原生管理 API 同样要求管理员身份；设备 RPC 必须由插件单独验证设备凭据，不能获得管理员能力。

生命周期：关闭新准入 → 获取全流活动锁并排空 → 停止旧Worker → 原子替换runtime → 检查自动重启策略与私有healthz → 开放准入。超时不停止活动请求；候选失败恢复旧目录与原启用状态。安装器重启检测未完成操作，独立恢复，界面控制面随后同步结果。

主网关不能 Requires/Requisite/BindsTo/PartOf 依赖可停用的Worker；操作前检查systemd实际配置。Worker使用Restart=on-failure，停止宽限覆盖排空期限。

Excel的 `openai.excel_plugin_state_file` 与 Worker `EXCEL_PLUGIN_STATE_FILE` 必须指向同一state.json；`openai.excel_worker_socket` 指向私有socket。长期推理请求持有activity.lock共享锁直到流结束，取得锁后再次检查准入。healthz允许在禁止业务准入时调用，以完成候选验证。

卸载删除runtime、关闭开机启动；账号、密钥绑定与用量保留。暂停Excel密钥不会自动变成Native。重新安装沿用控制状态。移除原生注册项前必须先卸载。

验证入口：`python -m unittest discover -s services/updater`；`python tools/test-excel-worker.py -k 'plugin_status_rpc or plugin_gate'` 使用临时隔离依赖环境。
