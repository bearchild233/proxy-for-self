# 模块总表

<!-- doc-sync paths: scripts/check-doc-map.py -->

全仓每个模块的路径、一句话职责和详细文档。改代码前先读对应模块文档（实现逻辑、文件表、改动指引），不要从头通读源码；分层与不变量见 [架构](architecture.md)，微内核与插件见 [微内核设计](microkernel.md)，网关池规则见 [网关池](gateway-pool.md)。

文档维护规则（详见 [CONTRIBUTING.md](../CONTRIBUTING.md) “文档维护”）：开发中不改模块文档；功能做完、提交前运行 `python scripts/check-doc-map.py`，按“待更新”清单重写对应章节，与代码同一提交；新增、删除或移动模块时同步本表；阶段收尾运行 `python scripts/check-doc-map.py --strict`。每份模块文档首行注释 `doc-sync paths:` 列出它覆盖的代码路径，脚本据此判断文档是否落后于代码；纯重构或只改测试时，把文档里的 `reviewed:` 改成当前提交即可清除标记。

## 后端（`backend/`，Rust workspace）

依赖方向：Core 不依赖 HTTP / 数据库 / Redis / 具体 Provider；Provider 之间互不依赖；API 只调用 Core / Admin；Store 只实现端口；具体实现只在组合根相遇。

| 路径 | 职责 | 文档 |
| --- | --- | --- |
| `backend/apps/gateway` | 唯一组合根：读取顶层配置，按 Host → Store → Provider → Core → Admin → API → Worker 顺序初始化并监听；另含 `backend/Cargo.toml`（workspace、lints、release 配置）与 `backend/rust-toolchain.toml` | [gateway-app](modules/gateway-app.md) |
| `backend/crates/kernel-protocol` | 内核与服务插件共享的 0.8 wire 合同：帧、消息、握手、钩子、能力；叶子 crate | [kernel-protocol](modules/kernel-protocol.md) |
| `backend/crates/gateway-protocol` | 跨层共享的 OpenAI wire 编解码：SSE、用量与限流解析、Codex 请求语义、传输头分类；叶子 crate | [gateway-protocol](modules/gateway-protocol.md) |
| `backend/crates/gateway-core` | 数据面核心：operation、canonical event、运行时快照、路由计划、账号选择、准入与限额、attempt 协调与提交边界、计量、Worker 契约、插件端口 | [gateway-core](modules/gateway-core.md) |
| `backend/crates/gateway-admin` | 管理控制面语义层：Command/Result、用例（账号、分组、Key、代理、设置、观测、备份、系统）、外部能力端口，以及备份 / 定价同步 / 冻结恢复三个后台任务 | [gateway-admin](modules/gateway-admin.md) |
| `backend/crates/gateway-api` | HTTP / WebSocket / SSE 协议适配：OpenAI 数据面、`/api/admin/*`、登录、Key 自助用量、插件控制面入口、静态资源 | [gateway-api](modules/gateway-api.md) |
| `backend/crates/gateway-store` | PostgreSQL / Redis / S3-R2 / `pg_dump` 适配器、凭据 vault 加密、Store 的 Worker 与健康探针；含 `backend/migrations` 迁移与冻结清单 | [gateway-store](modules/gateway-store.md) |
| `backend/crates/gateway-host` | 配置发现、日志轮转、HTTP 监听与排空、Worker 监督、自更新、插件运行时（内核侧）、价目源 / 客户端下载 / 代理探测 / 策略进程适配 | [gateway-host](modules/gateway-host.md) |
| `backend/crates/providers/openai` | OpenAI / Codex Provider：OAuth 与 API Key 账号、选号与租约、目录、额度与订阅、客户端画像、Responses / Images / Search 的 HTTP-SSE-WebSocket transport、插件路由桥；含待删除的旧网关池传输桥 | [provider-openai](modules/provider-openai.md) |
| `backend/crates/providers/xai` | xAI（Grok Build）Provider：OAuth session 账号、实时目录与订阅、Responses ↔ Grok wire 翻译 transport | [provider-xai](modules/provider-xai.md) |

## 服务（`services/`，Python 3.10）

| 路径 | 职责 | 文档 |
| --- | --- | --- |
| `services/plugin_host` | 插件控制面：登录态转发、插件目录 / 页面租约 / 版本、页面能力桥、按需后台任务、Excel 与服务插件生命周期编排、定价 / 备份策略进程入口；`gateway_pool*.py` 是待删除的旧网关池实现 | [plugin-host](modules/plugin-host.md) |
| `services/plugin_host/server.py` | 控制面 HTTP 服务、`Platform`、命令行入口 | [plugin-host](modules/plugin-host.md) |
| `services/plugin_host/lifecycle.py` | Excel 与服务插件的生命周期编排、操作日志与恢复 | [plugin-host](modules/plugin-host.md) |
| `services/plugin_host/jobs.py` | 后台任务（Worker 子进程）管理 | [plugin-host](modules/plugin-host.md) |
| `services/plugin_host/service_calls.py` | 服务插件管理调用经管理连接转给插件进程 | [plugin-host](modules/plugin-host.md) |
| `services/plugin_host/policy_runner.py` | 网关调用的备份 / 定价策略进程入口 | [plugin-host](modules/plugin-host.md) |
| `services/updater` | root 运行的独立更新与插件监督：网关更新 / A/B 槽发布、UI 插件包存储、原生 Worker 管理、服务插件监督、登录防护 | [updater](modules/updater.md) |
| `services/updater/updater.py` | 主程序：状态机、私有 socket 服务、旧单实例更新 | [updater](modules/updater.md) |
| `services/updater/slots.py` | A/B 槽发布 CLI（准备、验收、切入口、排空、恢复） | [updater](modules/updater.md) |
| `services/updater/release_package.py` | 构建带迁移兼容信息的发行包 | [updater](modules/updater.md) |
| `services/updater/static_plugins.py` | UI 插件版本存储（控制面共用） | [updater](modules/updater.md) |
| `services/updater/plugin_manifest.py` | 原生 Worker 清单（protocol 1）校验 | [updater](modules/updater.md) |
| `services/updater/plugins.py` | 原生 Worker（Excel）安装 / 排空 / 替换 / 恢复 | [updater](modules/updater.md) |
| `services/updater/plugin_registry.py` | 原生插件注册表热加载与页面 / RPC 转发 | [updater](modules/updater.md) |
| `services/updater/package_plugin.py` | 从 Linux Python 3.10 环境生成 Excel 运行包 | [updater](modules/updater.md) |
| `services/updater/service_plugins.py` | 服务插件监督：运行目录、systemd 模板单元、内核注册表 | [updater](modules/updater.md) |
| `services/updater/service_manifest.py` | 清单 `service` 段校验 | [updater](modules/updater.md) |
| `services/updater/kernel_protocol.py` | 管理连接最小协议客户端（hello / health / admin.call） | [updater](modules/updater.md) |
| `services/updater/login_security.py` | fail2ban jail 管理与 Caddy forward_auth 只读检查 | [updater](modules/updater.md) |
| `services/egress` | 可选 VLESS / Hysteria2 出口桥接（本机 HTTP CONNECT，每节点一个 sing-box 子进程） | [egress](modules/egress.md) |
| `services/egress/bridge.py` | CONNECT 服务与节点进程管理 | [egress](modules/egress.md) |
| `services/egress/node_config.py` | 分享链接 → sing-box 配置 | [egress](modules/egress.md) |

## 前端主壳（`frontend/`）

| 路径 | 职责 | 文档 |
| --- | --- | --- |
| `frontend/src/api` | 请求层与各管理接口的类型 / 调用封装 | [frontend-shell](modules/frontend-shell.md) |
| `frontend/src/components` | 基础 UI 组件（`base/`）、图表、客户端画像编辑器与共用展示 | [frontend-shell](modules/frontend-shell.md) |
| `frontend/src/composables` | 分页、轮询、异步动作、顺序共享等组合式函数 | [frontend-shell](modules/frontend-shell.md) |
| `frontend/src/directives` | `v-loading` 指令 | [frontend-shell](modules/frontend-shell.md) |
| `frontend/src/layout` | 侧边栏布局与系统更新弹窗 | [frontend-shell](modules/frontend-shell.md) |
| `frontend/src/plugins` | 插件平台前端运行时：目录、租约、预载、页面承载（组件 / iframe）、插件管理页 | [frontend-shell](modules/frontend-shell.md) |
| `frontend/src/router` | 路由表与登录守卫 | [frontend-shell](modules/frontend-shell.md) |
| `frontend/src/stores` | Pinia：认证、主题、系统更新、UI | [frontend-shell](modules/frontend-shell.md) |
| `frontend/src/styles` | 全局样式与 token CSS | [frontend-shell](modules/frontend-shell.md) |
| `frontend/src/theme` | 主题引擎（预设、自定义、token 派生） | [frontend-shell](modules/frontend-shell.md) |
| `frontend/src/utils` | 日期 / 数字 / 代理连接串 / 目录等工具 | [frontend-shell](modules/frontend-shell.md) |
| `frontend/src/views` | 登录页 | [frontend-shell](modules/frontend-shell.md) |

## 插件与共享包（`plugins/`、`packages/`）

功能页面全部在插件里；每个插件目录含 `plugin.json`（清单）、`ui/`（界面）和可选的 `service/`（Rust 服务）或 `worker/`（Python）。

| 路径 | 职责 | 文档 |
| --- | --- | --- |
| `plugins/accounts` | 账号管理页（基础，界面） | [accounts](modules/accounts.md) |
| `plugins/account-warmup` | 账号预热设置页与持久任务服务（可选，界面 + Rust 服务插件） | [account-warmup](modules/account-warmup.md) |
| `plugins/groups` | 分组管理页（基础，界面） | [groups](modules/groups.md) |
| `plugins/keys` | API 密钥管理页（基础，界面） | [keys](modules/keys.md) |
| `plugins/usage-analytics` | 概览、使用统计与 Key 自助页（基础，界面） | [usage-analytics](modules/usage-analytics.md) |
| `plugins/gateway-settings` | 网关设置三页（基础，界面） | [gateway-settings](modules/gateway-settings.md) |
| `plugins/appearance` | 主题外观编辑（可选，界面） | [appearance](modules/appearance.md) |
| `plugins/proxy-management` | 出口代理管理（可选，界面） | [proxy-management](modules/proxy-management.md) |
| `plugins/pricing` | 模型定价与每日同步计划（可选，界面 + 策略脚本） | [pricing](modules/pricing.md) |
| `plugins/backup` | 数据备份设置与记录（可选，界面 + 策略脚本） | [backup](modules/backup.md) |
| `plugins/client-access` | 客户端配置与 CC Switch 导入（可选，槽位界面） | [client-access](modules/client-access.md) |
| `plugins/account-diagnostics` | 账号诊断与快速降智检测（可选，槽位界面 + 诊断 Worker） | [account-diagnostics](modules/account-diagnostics.md) |
| `plugins/excel-bridge` | Excel Bridge 状态页与私有 Python Worker | [excel-bridge](modules/excel-bridge.md) |
| `plugins/gateway-pool` | 网关池：Rust 服务插件 + 管理页 + 旧策略脚本 | [网关池](gateway-pool.md) |
| `plugins/example-echo` | 最小示例服务插件（协议 / SDK / 监督链路验证） | [example-echo](modules/example-echo.md) |
| `plugins/sdk` | Rust 插件 SDK（`plugin-sdk`）与基准工具（`plugin-bench`）；与 `plugins/Cargo.toml`（插件 Rust workspace）、`plugins/package.json` 一并说明 | [rust-plugin-sdk](modules/rust-plugin-sdk.md) |
| `packages/plugin-sdk` | 插件与宿主的合同：能力白名单 `operations.json`、策略表 `policies.json`、构建期桥接实现 | [packages](modules/packages.md) |
| `packages/ui-kit` | 插件共用的格式化、轮转选项、账号用量窗口与套餐徽标 | [packages](modules/packages.md) |

## 脚本（`scripts/`）

| 路径 | 用途 | 谁在什么时候用 |
| --- | --- | --- |
| `scripts/check-doc-map.py` | 模块文档检查：覆盖、失效引用、过时 | 提交前运行；阶段收尾 `--strict` |
| `scripts/build-local.sh` | 在 WSL 构建环境检查 / 测试 / clippy / 构建 backend（cgroup 限额、增量缓存，不部署） | 开发者本机 Rust 验证的统一入口 |
| `scripts/build-local.ps1` | `build-local.sh` 的 Windows 包装，同步源码到 WSL 镜像 | Windows 开发者 |
| `scripts/build-plugins-local.sh` | 同上，针对 `plugins/` Rust workspace；与 backend 共用构建锁 | 服务插件开发 / `build-plugin.mjs` 内部调用 |
| `scripts/build.sh` | 源码构建：前端 build + `cargo build --release` + 产物复制到 `dist/` | 手工源码构建 |
| `scripts/check-plugin.mjs` | 单插件 `vue-tsc` 类型检查（纯服务插件跳过） | 改插件界面后 |
| `scripts/build-plugin.mjs` | 构建单个插件包到 `.build/plugins/<id>/package`（组件 / iframe 打包、资源哈希、Worker 与服务二进制） | 改插件后；`build-all-plugins.mjs` 与发布工作流调用 |
| `scripts/build-all-plugins.mjs` | 按 `policies.json` 逐个构建全部插件 | 打平台包 / 本机测试环境初始化 |
| `scripts/package-ui-plugin.py` | 把已构建的界面包打成 `.build/plugin-releases/<id>.tar.gz` | 手动上传到插件管理 |
| `scripts/package-plugin-platform.py` | 打包主壳、控制面与预装插件的平台初始化包（不含私有运行数据） | 首次迁入 / 平台升级 |
| `scripts/deploy_frontend.py` | 发布前端静态文件（先写新目录，最后原子切换 `index.html`，不重启服务） | 前端热更新上线 |
| `scripts/start-plugin-stage.ps1` | 启动本机隔离测试环境（WSL 网关、可选重启网关） | 本机集成验证，见 [本机测试](plugin-stage.md) |
| `scripts/plugin-stage-wsl.py` | WSL 内启动测试网关（隔离 PostgreSQL / Redis，不接受生产地址） | 由 `start-plugin-stage.ps1` 调用 |
| `scripts/plugin-stage-excel.py` | 本机 Excel Worker 生命周期适配器（准备 / 安装 / 状态） | 控制面 `--local-excel-wsl` 与测试环境 |
| `scripts/preview-plugin-stage.py` | 回环 UI 夹具：在测试环境上叠加演示数据 | 人工查看界面效果 |
| `scripts/test-plugin-stage.py` | 测试环境接口冒烟：鉴权、来源、能力边界、资源、分组 CRUD | 本机集成验证 |
| `scripts/test-plugin-lifecycle.py` | 角色、可选插件生命周期与配置检查 | 本机集成验证 |
| `scripts/test-plugin-hot-update.py` | 单包热更新、租约、坏包拒绝、回退，网关 PID 不变 | 本机集成验证 |
| `scripts/test-pricing-stage.py` | 隔离环境里驱动一次到期定价同步并恢复覆盖 | 本机集成验证 |
| `scripts/test-ab-slots.py` | 隔离 PostgreSQL / Redis / 真实网关 / Caddy 的 A/B 切换集成测试 | 改槽执行器后 |

## 部署（`deploy/`）

部署参数、客户端配置、人工恢复见 [deploy/README.md](../deploy/README.md)。

| 路径 | 用途 |
| --- | --- |
| `deploy/Dockerfile` `deploy/Dockerfile.dockerignore` | 应用镜像构建 |
| `deploy/compose.yaml` | Compose 编排（应用、PostgreSQL、Redis） |
| `deploy/config.example.yaml` | `config.yaml` 模板（真实配置不入库） |
| `deploy/ab-slots` | 网关 A/B 槽：Caddy 稳定入口、槽 A/B 与开机协调 / 恢复 systemd 单元、`slots.example.json`，见 [A/B 发布](../deploy/ab-slots/README.md) |
| `deploy/login-security` | 登录防护：fail2ban filter / jail / action，见 [登录防护](../deploy/login-security/README.md) |
| `deploy/plugin-platform` | 插件平台：控制面单元、插件模板单元 `proxy-for-self-plugin@.service`、网关策略 drop-in、nginx 路由，见 [插件平台安装](../deploy/plugin-platform/README.md) |
| `deploy/systemd` | 主服务、updater、egress 桥接、Excel Worker 的 systemd 单元模板 |

## 工具与 CI

| 路径 | 用途 |
| --- | --- |
| `tools/ci-scope.py` | 按 Git 变更范围选择 CI 检查，无法判断时全量（测试 `tools/test_ci_scope.py`） |
| `tools/patch-hermes-fast.py` | 为自有 Responses 代理启用 Hermes `/fast` 转发的补丁器，见 [Hermes Fast](hermes-fast.md)（测试 `tools/test-patch-hermes-fast.py`） |
| `tools/test-excel-worker.py` | 在一次性隔离环境里跑 Excel Worker 检查，本地与 CI 同一入口 |
| `.github/workflows` | `ci.yml` 与 `_quality.yml`（按范围选择 backend / frontend / Python 检查）、`plugin-release.yml`（手动发布单个插件）、`release.yml`（手动发布） |
| `release/version.yaml` | 版本号来源（编译期注入） |

## 文档地图

| 文档 | 内容 |
| --- | --- |
| [架构](architecture.md) | 分层、请求流程、状态所有权、不变量 |
| [微内核设计](microkernel.md) | 内核 / 插件分层、协议、钩子、能力、生命周期、分期 |
| [接口文档](api.md) | 管理接口字段、权限、错误与账号生命周期 |
| [插件平台](plugin-migration.md)、[原生 Worker](plugins.md)、[本机测试](plugin-stage.md) | UI 插件包、能力授权、租约、热更新；Excel Worker；本机隔离环境 |
| [网关池](gateway-pool.md)、[统计口径](gateway-pool-usage-design.md) | 网关池规则、术语与实现位置 |
| [主题](theme.md)、[显示顺序](shared-display-order.md) | 主题 token 规则；共享显示顺序 |
