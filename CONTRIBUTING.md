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
- Worker：`python -m venv .venv`，安装 `plugins/excel-bridge/requirements.lock` 和 `pip install -e 'plugins/excel-bridge[test]'`，执行 `python -m pytest plugins/excel-bridge -q`。
- 迁移：在 backend/migrations 执行 `sha256sum --check --strict .frozen-sha256`。已登记迁移的内容和哈希不得改变；新增迁移追加登记。

## 规则

API 负责协议，Admin/Core 负责用例，Store 负责持久化。前端不得绕过管理 API。Worker 不可读取本机认证或自行换账号。对已输出内容、结果未知的请求和未完成工具调用，不得盲目重试。

PR 说明问题、最终行为、验证和兼容风险；不要提交日志、真实请求、账号、Token、截图中的隐私或部署配置。新增依赖与复制代码保留许可证。版本写入 `release/version.yaml`；发布与数据库迁移必须有备份、恢复方案和排空检查。
