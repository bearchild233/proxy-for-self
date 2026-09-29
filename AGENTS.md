# proxy-for-self 维护约定

- 本目录是独立主仓库；不要依赖旧 excel-bridge/research 布局或开发者绝对路径。
- 保持 API → Admin/Core → Store 分层；先读 docs/architecture.md 和 CONTRIBUTING.md。
- Rust/Vue/Python 各组件有独立职责。保留现有修改，不回退其他人的工作。
- 中文注释说明新增复杂逻辑；最小改动并完成相关验证。
- backend/migrations 中已冻结 SQL 不得改字节；新迁移追加哈希。
- 不提交任何线上凭据、账号资料、运行数据、私有日志、备份或构建产物。
- 构建、推送和合并不隐含生产部署授权；升级时保证正在进行的请求和自动重启可恢复。
- 账号轮转不得破坏会话、工具状态和已交付输出语义。
