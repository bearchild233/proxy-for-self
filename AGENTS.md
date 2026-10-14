# proxy-for-self 维护约定

proxy-for-self 是自用的 OpenAI/xAI 推理网关：Rust 主网关（`backend/`）、Vue 管理主壳（`frontend/`）、Python 插件控制面与更新器（`services/`）、按包热更新的插件（`plugins/`）。正在按 [微内核设计](docs/microkernel.md) 把业务功能迁到插件。和用户交流使用中文。

提交：只 `git add` 具体文件，不用 `git add -A`/`.`；提交信息沿用 `type(scope): 英文说明`；不加 AI 署名。不 push，不做生产部署或 VPS 操作，除非用户在对话中明确授权。

验证节奏：一批改动做完验证一次，只验证改动涉及的部分；没改 Rust 不编译 Rust，没改行为不跑测试；全量测试、clippy、fmt 只在阶段收尾运行。等待后台构建或测试时不用 sleep 轮询。

- 从仓库根目录工作。开始修改前核对分支、工作区、worktree 和正在运行的维护任务。
- 本目录是独立主仓库；不要依赖旧 excel-bridge/research 布局或开发者绝对路径。
- 保持 API → Admin/Core → Store 分层；先读 docs/architecture.md 和 CONTRIBUTING.md。
- Rust/Vue/Python 各组件有独立职责。保留现有修改，不回退其他人的工作。
- 中文注释说明新增复杂逻辑；最小改动并完成相关验证。
- backend/migrations 中已冻结 SQL 不得改字节；新迁移追加哈希。
- 不提交任何线上凭据、账号资料、运行数据、私有日志、备份或构建产物。
- 构建、推送和合并不隐含生产部署授权；升级时保证正在进行的请求和自动重启可恢复。
- 账号轮转不得破坏会话、工具状态和已交付输出语义。

- 用户要求默认轻量验证：优先相关文件 lint/typecheck/构建；除非用户明确要求或提供测试步骤，不做逐页浏览器可视化验收。部署后给出检查页面与预期结果。
- 用户要求先在已授权的线上实例部署，让用户查看效果；只有用户明确同意 push 后才推送 GitHub。不得把功能开发或部署授权当作 push 授权。此偏好自 2026-09-29 起执行。
- VPS 有独立管理 bot。生产变更后同步 `/root/维护手册/`：README 作入口、07 章记协同操作、10 章记当前配置、08 章记恢复材料。文档直接陈述当前事实和操作步骤，过时内容删除，省略修改经过及纠错对比。写操作先检查运行中的更新/恢复任务，按同一维护流程协调。

- 修改共享数据库行解析字段时，逐一核对所有 SELECT 调用方，并运行受影响模块已有的真实数据库测试（含分组成员等间接入口）。
- Windows 本地 Rust 检查与编译优先使用 `scripts/build-local.ps1`，复用 WSL Linux 磁盘里的源码镜像和 target；按改动选择包。PowerShell 执行策略拦截该入口时，用 `wsl.exe -d ApiHubBuild-20260928` 直接调用 `scripts/build-local.sh <仓库路径> 4096 3 <任务> <包...>`。不要复用 `.build` 中旧的一次性构建脚本或关闭开发增量缓存。该入口不部署；正式发布仍走发布流程。

- 小幅界面文案及已有布局的显示细节修正，完成轻量验证后可直接热更新上线。较大布局或交互修改先展示具体预览并取得确认；后端实现和轻量验证可继续推进。部署或预览确认不代表 GitHub push 授权。
- 全面插件化迁移先在独立端口、独立数据的测试环境部署验证，再供用户查看；只有用户确认测试结果后才能上线生产。不得把方案或外观确认当成生产部署授权。
- 发布按 A/B 阶段执行：A 持续服务，B 在独立端口完成真实启动和接口验收，通过后才进入入口切换阶段。准备失败不得停止或重启 A；恢复按已完成阶段执行，一次恢复失败后禁止循环重启。主网关的单实例锁与在途会话必须有明确交接方案，不能以绕过锁或先停线上服务来验证新程序。

## 按改动读取的文档

| 文档 | 内容 |
| --- | --- |
| [CONTRIBUTING.md](CONTRIBUTING.md) | 工具链、本地构建与检查入口 |
| [docs/microkernel.md](docs/microkernel.md) | 微内核插件架构、钩子与能力、插件生命周期、分期计划 |
| [docs/gateway-pool.md](docs/gateway-pool.md) | 网关池规则、术语与判据 |
| [docs/architecture.md](docs/architecture.md) | Rust 分层、冷流执行、调度与数据约束 |
| [docs/api.md](docs/api.md) | 管理接口、字段、权限与账号生命周期 |
| [docs/plugin-migration.md](docs/plugin-migration.md) | 插件 UI 包、能力授权、租约和热更新 |
| [docs/plugins.md](docs/plugins.md) | Worker 与 Excel 生命周期 |
| [docs/plugin-stage.md](docs/plugin-stage.md) | 本机隔离测试环境与接口验收 |
| [docs/hermes-fast.md](docs/hermes-fast.md) | VPS 客户端补丁与升级核查 |
| [deploy/ab-slots/README.md](deploy/ab-slots/README.md) | 主网关 A/B 发布、排空与恢复 |
| [deploy/plugin-platform/README.md](deploy/plugin-platform/README.md) | 插件控制面身份、目录权限与发布 |
| [deploy/login-security/README.md](deploy/login-security/README.md) | 可信来源、封禁和管理入口 |

`docs/validation.md` 是历史验证记录，不能替代当前代码检查或现场验收。文档与源码冲突时，以当前源码、用户最新要求和实际服务状态为准，并修正过时文档。

## 生产状态

线上活动槽、二进制、插件版本和运行配置以 VPS 实际状态与 `/root/维护手册/` 为准，不以仓库版本字段或文档中的端口推断。
