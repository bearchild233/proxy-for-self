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

管理员可在「系统设置 → 网关调度 → 网关容量」保存全网关并发、单请求正文和在途正文总预算。配置持久化到数据库并随运行快照热更新，新请求立即使用；调低上限保留在途占用，超出时拒绝新请求直到释放。已有 WebSocket 的收帧上限在握手时确定，调大正文上限后需重连。面板保存时同步 Responses 解压上限，避免双重大小设置。

数据库尚未设置覆盖值时，使用下述 `api.inference_limits` 启动默认值。已有数据库覆盖值优先于配置文件；常规调整应在管理员面板进行。

低内存实例可设置 `api.inference_limits.max_requests`（同时处理的推理请求数）和 `max_body_bytes`（正文上限，字节）；默认 0 不额外限制。实例并发许可由 HTTP/SSE 和 WS 每轮生成共用，流结束或取消后释放。超过容量返回 `503 gateway_busy`，HTTP 附带 `Retry-After: 1`；管理、健康检查和模型列表继续可用。该上限独立于账号与 Key 并发配置。正文上限同时约束 Responses 解压后数据；超大 HTTP 原始正文返回 413。设置时按实际请求大小和内存预算留出解析、响应和后台任务余量。

默认配置持有数据库排他实例锁。固定 A/B 发布显式设置 `store.deployment_slot: a|b`，共用数据库和 Redis；另一槽加入时保留活动请求和租约，只在两个槽均退出后的冷启动执行恢复。部署合同、首次迁入和开机选择见 [A/B 发布](ab-slots/README.md)。

`api.inference_limits.max_in_flight_body_bytes` 限制同时处理的正文总预算，默认0关闭；启用时必须不小于正数的单请求上限。HTTP在读取前预留额度，压缩或未知长度先按单请求上限预留，读取/解压后归还多余额度；额度持有至响应流结束或取消。WS每轮在解码前按已收到的消息长度申请同一预算。预算不足返回503 gateway_busy，不在此层排队。正文预算不等于实际堆内存，也不包含WS收帧前的缓冲、空闲连接保留的会话状态及后台缓存，部署仍需预留这些开销。

VLESS / Hysteria2 是可选的独立出口桥接服务，安装、资源限制与支持范围见 [出口桥接说明](../services/egress/README.md)。旧 HTTP/SOCKS 代理不依赖该服务；新增代理仍可选择表单或完整链接。

1. 在开发机/CI 构建和验证，服务器不承担大型 Rust 编译。
2. 备份数据库、vault、配置与旧发行目录，验证备份可读取；新产物写入新的发行目录。
3. 确认迁移兼容，在备用槽启动并验收，切换稳定入口后排空旧槽。准备失败保持活动槽运行。不要把管理面板 SSE 当成模型请求；其取消信号必须能结束事件流。
4. systemd 的停止宽限必须大于 `host.drain_timeout_seconds + host.worker_shutdown_timeout_seconds`。模板为 650 秒，对应可配置的 600+30 秒排空；Compose 为 75 秒，对应默认 30+30 秒。修改任一超时必须同步另一端。
5. 检查健康接口、登录、请求、服务重启次数与错误日志。失败先检查迁移兼容性再回退；不能仅换旧二进制忽略新数据库结构。

systemd 模板 `Restart=on-failure` 保证异常退出重启。生产重启前必须验证 unit 能启动、路径/权限正确、自动恢复配置有效。单实例切换可能短暂断连；不能承诺零停机。不要为了更新前端随意重启反向代理并关闭已有 WebSocket。

内置更新器默认指向本仓库；源码构建保持 source 类型。在独立发行包流程经验证之前，使用上述人工发布流程，不从上游仓库覆盖本产品。

## 仅前端更新（无需重启）

前端构建通过后，把 `frontend/dist` 中的文件按相对路径打包（只包含普通文件），校验 SHA256，再执行：

```sh
python3 scripts/deploy_frontend.py --archive /path/frontend.tar.gz --sha256 <SHA256> \
  --destination /path/current/frontend --backup-root /path/backups \
  --verify-url http://127.0.0.1:8080/
```

脚本备份现有前端，先发布资源、最后原子替换 index；保留旧资源供已打开页面加载。不修改数据库、配置、服务进程或反向代理。需要回退时，从回执记录的备份恢复 index.html（旧 hash 资源仍在）。后端接口必须与新前端兼容，涉及 Rust 逻辑的改动不能只按此流程发布。
# 面板系统更新

GitHub 发布源为 `ckcyian23/proxy-for-self`。维护者手动运行 `Publish release` 工作流才会发布版本，普通 push 不发布也不部署。Linux amd64 的独立更新服务与 systemd 接入见 [updater 文档](../services/updater/README.md)。

自动安装限于数据库迁移清单完全相同的版本；先备份、等待请求结束，再由独立服务切换程序并检查健康。数据库迁移版本需要维护升级。Excel Worker 和出口代理运行时独立保留，不随主程序更新覆盖。


### 插件部署必须检查实际服务依赖

主网关不得通过 Requires、Requisite、BindsTo 或 PartOf 依赖可禁用的 Excel socket/service。显式停止依赖会连带停止网关，`Restart=on-failure` 对这种停止不生效。检查 `systemctl show <网关> -p Requires -p Requisite -p BindsTo -p PartOf` 的实际值；systemd 不能通过空的 drop-in 依赖字段删除原 unit 的依赖，需修正定义后 daemon-reload 再检查。插件管理器在受理和实际停止前均检查此条件；违反时不得改变准入状态或停止服务。

整批部署应由独立 systemd unit 执行，并配置独立 OnFailure 恢复 unit、有限执行时间、受保护的旧版本/配置备份和健康检查。恢复任务不得依赖正在更新的 API 或交互式终端。单实例二进制切换仍可能需要客户端重连；服务自动恢复和当前流不中断是不同保证，不能宣称无感更新。
