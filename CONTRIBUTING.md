# 开发与协作

`main` 保存可评审的产品源码。每人从 main 创建自己的 `feat/xxx` 或 `fix/xxx` 分支，一个改动提交一个 Pull Request。前端、后端、Worker 可分工并行开发；跨组件 API 改动先在 PR 说明请求/响应及兼容方式。

```sh
git clone https://github.com/ckcyian23/proxy-for-self.git
cd proxy-for-self
git switch -c feat/account-ui
# 修改、检查、提交
git push -u origin feat/account-ui
```

仓库管理员在 Settings → Collaborators 邀请成员；公开仓库的外部贡献者也可 Fork 后提交 PR。建议在有第二位维护者后启用 main 的 PR 审查规则。CI 配置已提供；生产发布由维护者单独执行，合并代码不会自动重启生产。

## 检查

- 前端：`pnpm --dir frontend install --frozen-lockfile`、`pnpm --dir frontend lint`、`pnpm --dir frontend build`、`node --test frontend/tests/*.test.mjs`。
- Rust：在 backend 执行 `cargo fmt --check`、`cargo clippy --all-targets --all-features --locked -- -D warnings`。完整测试需要测试专用 PostgreSQL/Redis，设置 `CPR_TEST_DATABASE_URL`、`CPR_TEST_REDIS_URL` 后执行 `cargo test --test main --locked`；CI 提供隔离服务。
- Worker：Python 3.10+ 执行 `python tools/test-excel-worker.py`。与 CI 共用入口：每次创建临时隔离环境，安装锁定依赖及实际安装包，执行 `pip check` 和测试后清理。禁止以开发环境里直接运行 pytest 的结果替代此检查；新增依赖同步维护 `pyproject.toml` 与对应 lock。可追加 pytest 参数缩小检查范围。
- 迁移：在 backend/migrations 执行 `sha256sum --check --strict .frozen-sha256`。已登记迁移的内容和哈希不得改变；新增迁移追加登记。

## Windows 本地 Rust 编译

已准备 `builder` 用户、Rust、rsync 和 cgroup v2 的 WSL 构建环境可使用：

```powershell
./scripts/build-local.ps1 -Task check -Package provider-openai
./scripts/build-local.ps1 -Task test -Package provider-openai -CargoArgs @('--test', 'main', 'admin::openai_bundle_exposes_one_core_provider_and_drains_worker_contributions_once', '--', '--exact')
./scripts/build-local.ps1 -Task release
```

默认使用 WSL 默认发行版，可通过 `-Distribution` 指定。入口自动同步 backend 到 `/work/proxy-for-self/backend`，复用该目录的 target；CPU 上限 4 核，依据 Windows 可用内存选择 1–3 个 Cargo 任务及 2–4 GiB 内存上限。开发编译启用增量缓存并关闭调试符号，默认只检查指定包。调试断言仍保留；需要源码级调试时使用普通 Cargo 命令及单独的 target。测试数据库需按上文单独准备。此入口只构建，不打包、推送或部署。

## CI 范围与速度

push/PR 按改动选择 backend、frontend 或 Python 组件；文档改动保留工作流检查，发布/部署配置或未知路径改动执行全部检查。手动 CI 和正式发布保持全量校验。

后端 CI 关闭 dev/test 调试符号，保留 Clippy、完整测试、调试断言和迁移冻结检查；Rust 缓存包含工作区模块，失败时也保存。首次配置或依赖变化仍需重建，观察 Actions 的 Compile Rust tests 与 Rust tests 分别判断编译和执行耗时。

## 规则

API 负责协议，Admin/Core 负责用例，Store 负责持久化。前端不得绕过管理 API。Worker 不可读取本机认证或自行换账号。对已输出内容、结果未知的请求和未完成工具调用，不得盲目重试。

PR 说明问题、最终行为、验证和兼容风险；不要提交日志、真实请求、账号、Token、截图中的隐私或部署配置。新增依赖与复制代码保留许可证。版本写入 `release/version.yaml`；发布与数据库迁移必须有备份、恢复方案和排空检查。
