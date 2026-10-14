# gateway-app（组合根）

<!-- doc-sync paths: backend/apps/gateway backend/Cargo.toml backend/rust-toolchain.toml -->

## 职责与边界

可执行包 `codex-proxy-rs`（二进制同名，库名 `codex_proxy_rs`），是整个后端唯一的组合根：读取顶层配置，按固定顺序初始化各 crate 的 Bundle，把具体实现在这里接到一起，然后把进程交给 Host 监听。不含业务规则、协议或 SQL。其余 crate 只暴露自己的配置段、端口和 Bundle，彼此不自行定位实现（分层见 [架构](../architecture.md) 第 2、3 节）。

## 实现逻辑

启动流程（`src/bootstrap.rs::run`），顺序固定，任一步失败整体退出，监听前不对外服务：

1. `gateway_host::load_config::<GatewayConfig>()`：从当前目录或父目录找 deploy 目录下的 `config.yaml`，反序列化 `schema_version`（必须为 1）、`host` `store` `admin` `client` `api` `openai` `plugins` 各段；`services` 段属于 Compose 凭据桥接，只在加载时忽略；未知字段忽略，已知字段按各 crate 的 `resolve_and_validate` 校验（相对路径以配置文件目录为基准）。
2. Host：`gateway_host::initialize`（日志、worker 监督、系统更新）。
3. Store：`gateway_store::initialize`（vault 主密钥、PostgreSQL 连接与迁移或槽模式校验、Redis、各端口）。
4. Provider：`provider_openai::initialize`、`provider_xai::initialize`，用 `ProviderRegistry::new` 注册。
5. Core：`gateway_core::initialize`（准入恢复、首个 `RuntimeSnapshot` 编译，失败即拒绝启动）。
6. Admin：`gateway_admin::initialize`，在 `AdminRuntimePorts` 里注入跨层能力：定价/备份策略进程、models.dev 价目源、Provider 管理适配、快照控制、账号探测、代理探测（复用 OpenAI 的自定义 CA HTTP 客户端构造）、客户端下载解析、系统操作、Client Key 校验。
7. API：`gateway_api::initialize` 得到 router。
8. Worker：汇总 Store / Core / OpenAI / xAI / Admin 的 `WorkerContribution`，`host.start_workers`。
9. `store.complete_startup()`（槽模式完成启动屏障）。
10. 插件运行时：配置了 `plugins.registry_path` 与 `ownership_cache_path` 时，`PluginRuntime::start` 注入 `KernelCapabilities`（账号目录来自 Store、上游能力来自 OpenAI、分组移动来自 Store、日志来自 Host），并把它设为 OpenAI Provider 的 `PluginHooks`；未配置则不启用插件。
11. `host.serve(api.router())`：监听、信号处理、优雅排空。

`src/main.rs`：安装 rustls provider，只接受无参数或 `serve`，在多线程 tokio 运行时中调用 `run`。

## 文件表

| 文件 | 职责 |
| --- | --- |
| `src/main.rs` | CLI 入口与 tokio 运行时 |
| `src/bootstrap.rs` | 顶层配置、初始化顺序、错误分类 `BootstrapError` |
| `src/lib.rs` | 导出 `bootstrap` |
| `tests/architecture.rs` | 冻结 workspace 成员、依赖 DAG、公开模块面、生产/测试镜像布局 |
| `tests/bootstrap.rs` | 配置加载（相对路径、密码注入、插件路径成对）、代理探测证书等 |
| `tests/main.rs` | 集成测试入口 |
| `../../Cargo.toml` | workspace 成员、lints（`unsafe_code = forbid`、clippy deny）、release 配置（thin LTO、`panic = abort`）和 hyper-util 补丁 |
| `../../rust-toolchain.toml` | 固定工具链 |

## 数据、配置与接口

- 配置文件 deploy 目录下的 `config.yaml`（模板 `deploy/config.example.yaml`，不入库），环境变量覆盖 `CPR_SERVER_HOST` / `CPR_SERVER_PORT`、`CPR_DATABASE_URL`、`CPR_REDIS_URL`、`CPR_WEB_DIST_DIR`；密码经 `CPR_*_PASSWORD` 注入。
- 版本信息来自 `release/version.yaml` 与构建环境变量 `CPR_VERSION` `CPR_GIT_SHA` `CPR_BUILD_TIME` `CPR_BUILD_TYPE`（`backend/crates/gateway-host/build.rs`）。
- 对外只有 HTTP 监听；无命令行子命令。

## 改动指引

- 新增 crate 或改依赖方向：先改 `tests/architecture.rs` 的冻结清单与 DAG，再改 [架构](../architecture.md) 第 3 节。
- 新增 Bundle 或跨层端口：在 `bootstrap.rs` 里按顺序注入，不要让下层 crate 自己 new 上层实现。
- 新增配置段：在对应 crate 定义配置类型与 `resolve_and_validate`，这里只加字段，并更新 `deploy/config.example.yaml` 和 [部署文档](../../deploy/README.md)。
- 启动后台任务一律通过 `WorkerContribution` 交给 Host 监督，不在这里 `tokio::spawn`。
