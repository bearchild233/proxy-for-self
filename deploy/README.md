# 部署与升级

本仓库提供通用模板，不包含某台服务器的账号、密码或一键覆盖脚本。生产推荐在反向代理后运行网关；PostgreSQL、Redis 和 Excel socket 均仅内部可达。

## Docker（Native 网关）

复制 `config.example.yaml` 为 `config.yaml`，分别用 `openssl rand -hex 24` 生成数据库和 Redis 密码，设置独立管理员初始密码（至少 12 字符）。模板的 YAML anchors 会将密码传给 Compose。

在仓库根目录创建运行目录并生成 vault 密钥：

```sh
mkdir -p .runtime/{data,logs,postgres,redis}
(umask 077; openssl rand -hex 32 > .runtime/data/vault-key.hex)
sudo chown -R 10001:10001 .runtime/data .runtime/logs
sudo chown "$(id -u):10001" deploy/config.yaml
chmod 640 deploy/config.yaml
docker compose -f deploy/compose.yaml up -d --build
```

`127.0.0.1:8080` 供反向代理访问。PostgreSQL/Redis 的本机端口默认 5432/6379，若已占用请改 Compose 端口及开发配置。备份 vault 密钥和数据库，两者缺一无法恢复加密凭据。不要对已有部署重新生成 vault 密钥。

Docker 模板只包含 Native 网关；Excel 使用下方的 Linux 私有 Worker 模板。这里没有自动发布的镜像，不要误以为存在 latest 发行版。

## Linux systemd 与可选 Worker

`systemd/` 是安装模板：服务用户 `proxy-for-self`、产品根目录 `/opt/proxy-for-self/current`、配置在该目录 `deploy/config.yaml`（可链接到 /etc 中受保护的配置）。修改路径后由管理员安装。后台数据库和 Redis 必须先就绪。

Worker 安装到独立虚拟环境：安装 `plugins/excel-bridge/requirements.lock` 后再安装 `plugins/excel-bridge/`。socket 目录 `/run/proxy-for-self-excel` 必须属于服务用户且权限 0700；可用随附 tmpfiles 模板创建。先启动 socket unit，网关配置 `openai.excel_worker_socket: /run/proxy-for-self-excel/worker.sock`；Worker 由 socket 按需启动，空闲退出。公网不得代理这个 socket。

## 升级与自动恢复

1. 在开发机/CI 构建和验证，服务器不承担大型 Rust 编译。
2. 备份数据库、vault、配置与旧发行目录，验证备份可读取；新产物写入新的发行目录。
3. 确认迁移兼容，等正在进行的模型请求排空，再切换发行目录并重启。不要把管理面板 SSE 当成模型请求；其取消信号必须能结束事件流。
4. systemd 的停止宽限必须大于 `host.drain_timeout_seconds + host.worker_shutdown_timeout_seconds`。模板为 650 秒，对应可配置的 600+30 秒排空；Compose 为 75 秒，对应默认 30+30 秒。修改任一超时必须同步另一端。
5. 检查健康接口、登录、请求、服务重启次数与错误日志。失败先检查迁移兼容性再回退；不能仅换旧二进制忽略新数据库结构。

systemd 模板 `Restart=on-failure` 保证异常退出重启。生产重启前必须验证 unit 能启动、路径/权限正确、自动恢复配置有效。单实例切换可能短暂断连；不能承诺零停机。不要为了更新前端随意重启反向代理并关闭已有 WebSocket。

内置更新器默认指向本仓库；源码构建保持 source 类型。在独立发行包流程经验证之前，使用上述人工发布流程，不从上游仓库覆盖本产品。
