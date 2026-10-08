# 本机插件测试

入口：<http://127.0.0.1:18335/>。这是完整主壳与 12 个真实插件包，使用隔离的 Rust 管理 API。自动登录仅在本机准确地址开放，不能部署到公网。

| 组件 | 位置 |
| --- | --- |
| 控制面与主壳 | Windows loopback 18335，services/plugin_host/server.py |
| Rust 网关 | WSL loopback 18336，/work/plugin-stage |
| PostgreSQL | 15432，独立库 cpr_plugin_stage |
| Redis | 独立 16380，32 MiB |
| 插件运行数据 | .build/plugin-platform |
| Excel Worker | /work/plugin-stage/excel，私有 Unix socket |

无真实账号、生产 Key 或生产数据库连接。账号诊断的真实上游调用需自行导入测试账号后手动发起；本批自动验证使用模拟上游，不消耗生产额度。

## 启动与更新

```powershell
pnpm --dir frontend build
node scripts/build-all-plugins.mjs
python services/plugin_host/server.py install --root .build/plugin-platform
./scripts/start-plugin-stage.ps1
```

Rust内核首次或有改动时先通过 `scripts/build-local.ps1 -Task build -Package codex-proxy-rs` 构建，再显式使用 `start-plugin-stage.ps1 -RestartGateway`。普通插件更新不执行这一步。

```powershell
node scripts/check-plugin.mjs groups
node scripts/build-plugin.mjs groups
python services/plugin_host/server.py install --root .build/plugin-platform --plugin groups
```

刷新或重新打开对应页面使用新版本。回退在插件管理中执行。主壳/控制面源代码变化需要更新主壳或重启控制面，与普通插件包更新分开。

Excel 测试运行包准备入口：WSL 内执行 `python3 scripts/plugin-stage-excel.py prepare`，然后 `install`。本机进程适配器用于隔离测试；生产采用独立 systemd 安装器，具备异常重启保护。

## 验证入口

- `scripts/test-plugin-stage.py`：真实接口鉴权、来源限制、能力边界、12 包资源与分组 CRUD/排序。
- `scripts/test-plugin-hot-update.py`：真实单包更新、旧页租约、坏包拒绝和回退，记录网关 PID/启动时间及健康探针。
- `scripts/test-plugin-lifecycle.py`：Key 用户权限、可选插件停用后的旧 RPC 拒绝、模板持久化、Excel 启停联动。
- `services/plugin_host/test_platform.py`：按版本排空任务、任务所有权、诊断材料归属和备份策略。
- `scripts/test-pricing-stage.py`：仅在本机将价格计划置为到期，实测后台同步并核对自定义倍率保留，最后恢复原倍率。收据在 `.build/plugin-stage/pricing-verification.json`。
- `services/updater`：包校验、基础保护、排空超时、坏候选恢复、更新中断恢复和系统更新保护。

最新本机实测收据在忽略目录 `.build/plugin-stage/verification.json`。健康探针不能替代真实上游长流；另有故意不发送 SSE 正文的原始 TCP 测试及 Worker 全流活动锁测试。

查看插件管理分类、拖动与展开详情；账号测试在账号三个点菜单，配置导入位于密钥操作或 Key 用量页。基础模块只能更新和回退，可选插件停用后对应入口消失。

本机验收后再决定生产上线；当前未部署生产、未 push。

## 共享 Vue 组件版

12 个界面包共用主应用的 Vue 和基础组件，版本见各自 `plugin.json`。真实测试入口为 `http://127.0.0.1:18335/settings/pricing`。只下载预载代码，表格和表单按首次进入加载；切页保留实例。

构建后运行 `node --test frontend/plugin-tests/component-packages.test.mjs`，检查实际 12 个模块未内置 Vue、未创建第二个应用、工厂初始化不发业务请求。单插件发布流程通过 `PLUGIN_ID` 只检查所选包。`test-plugin-stage.py` 与 `test-plugin-hot-update.py` 同时支持组件和 iframe 包。

本机真实分组 CRUD、单包更新、损坏包拒绝、旧版本租约及回退已验证；网关 PID/启动时间保持不变。浏览器确认组件版无 iframe，设置页标题保持不变、未保存编辑和筛选可保留。静态包体和运行内存分开评估，尚未完成最初单体版的同条件内存对比。

### 共享 Vue 本机验收

- 账号列表默认展示全部列，保留用户主动保存的列偏好；列菜单重置后恢复全列。
- 账号诊断使用宽版双栏弹窗，结果和配置独立滚动，底部测试按钮固定可见；模板编辑复用公共输入控件。
- 同一插件版本的多个页面复用租约，最后一个页面退出时释放；登录身份变化后不复用旧租约。
- 本机类型检查、构建、12 个实际组件包契约、租约并发/失败/会话切换测试及独立环境接口检查通过。视觉验收由用户完成。

### 账号界面演示数据

`python -X utf8 scripts/preview-plugin-stage.py` 在本机 18335 端口运行演示外层（与普通本机控制面二选一）。它复用隔离环境登录与插件包，增加两条标注“演示”的账号、订阅和额度；账号菜单中的诊断返回明确标注的模拟长回答及快速检测结果。演示账号无凭据，不写入数据库，不调用上游。该启动脚本不进入生产发布包；Rust 网关无需重启。

生产控制面 B:18337 运行，旧控制面 A:18335 停止；线上不包含本机演示数据。生产插件版本及恢复材料以 VPS 维护手册和平台目录为准。

### 自动同步本机验收

使用统计、密钥用额、分组、出口代理、价格、备份、系统设置、安全状态与插件状态在页面可见时每30秒同步，切回页面立即补刷；隐藏时暂停，慢请求不叠加。备份进行中每2秒读取进度。概览与账号保留自动刷新。

后台读取不显示整表加载、不清空数据、不重置筛选和分页、不覆盖未保存表单。日志第一页追踪新请求，后续页保留时间快照；统计卡片仍更新到当前时间。诊断与代理连接测试由用户触发，不自动调用上游。

刷新逻辑检查：`node --test frontend/tests/page-polling.test.mjs`。本机入口 `/groups`、`/keys`、`/usage`。此批已获用户确认并于2026-10-08热更新上线；网关和控制面未重启，生产恢复材料见VPS维护手册08章。
