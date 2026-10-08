# 固定 A/B 槽发布

A、B 使用独立端口、配置目录、日志和程序链接，共用 PostgreSQL、Redis、vault 与插件数据。只有一个槽接收新连接；旧槽排空后停止进程，保留程序用于下一次轮换或回退。

`services/updater/slots.py` 提供 Linux 发布执行器。阶段持久化到状态文件，并使用文件锁防止两个执行者同时发布：

1. 确认当前入口与活动槽一致，备用槽已停止。
2. 校验候选二进制哈希、槽配置和已应用迁移；只更新备用槽的 `current`。
3. 启动备用槽，连续通过健康检查。候选启动失败保持活动槽不动。
4. 使用 Caddy 配置 API 的 ETag 条件更新唯一网关 upstream，再核对入口返回的进程身份。
5. 入口验证通过后向旧槽发 SIGTERM；在当前请求完成、计费写入排空后退出。成功后状态恢复 `idle`。

切换确认前失败，先恢复旧入口并验证，再停止候选。确认后的排空失败只保留 `draining` 状态，不回退到已经收到 SIGTERM 的旧槽。执行者中断后使用 `resume`；`attention` 需要检查，不循环重启。

## 数据与连接约束

- `store.deployment_slot` 只能为 `a` 或 `b`。每槽持有独立锁，两个槽共持存活锁。B 加入时不将 A 的 running 请求标记中断，也不清理活动 Redis 租约；两个槽均退出后的冷启动才恢复全局占用。
- 迁移版本、成功标记、校验和必须完全一致。槽模式不会自动执行数据库迁移；不兼容 schema 拒绝切换。
- 后台任务继续通过 Redis 租约选主。HTTP 排空结束前不取消后台写入；WS 等当前轮输出和结算完成，再用 1001 关闭连接，下一轮由客户端重连。
- Caddy 网关 reverse_proxy 必须设置 `stream_close_delay 15m`，覆盖网关排空和退出宽限。配置更新默认会关闭 WS；执行器检查此字段，缺失时拒绝切换。见 [Caddy 流式连接文档](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy#streaming)。
- 请求并发和正文总预算仍有进程内限制，轮换重叠不能视为两倍 VPS 容量。生产接入前必须验证两个进程的峰值内存与停机时限；保留原账号/密钥的 Redis 并发约束。
- 程序替换不等于会话内存迁移；跨进程的续接依旧受原有安全续接规则限制。不得把 WS 重连承诺为所有客户端完全无感。

## 配置与调用

参考本目录 `slots.example.json` 和 service 模板；将实际路径、端口和 Caddy API 路径填入 root 所有的配置。`caddy_dial_path` 必须指向独立网关 reverse_proxy 的 `/upstreams/0/dial`，不要指向其他站点。保留生产真实 IP、TLS、鉴权、WS 与请求超时配置。

每槽工作目录下的 `deploy/config.yaml` 使用 JSON（YAML 的合法子集），与执行器校验保持一致；数据库、Redis 和 vault 指向同一份状态，host.listen 和 deployment_slot 分别固定。程序目录包含 `slot-manifest.json`：`{"protocol":1,"binarySha256":"实际 SHA256"}`，只能为已验证支持槽模式的二进制签发。

```sh
python3 services/updater/slots.py --config /etc/proxy-for-self/slots.json status
python3 services/updater/slots.py --config /etc/proxy-for-self/slots.json deploy --release /opt/api-hub/releases/已校验版本
python3 services/updater/slots.py --config /etc/proxy-for-self/slots.json resume
```

生产发布须由独立 systemd 任务执行，不能依赖 SSH 或网关请求存活；执行器失败仅调用一次 `resume`，恢复单元 `Restart=no`。不要同时 enable 两个槽开机启动；开机协调单元执行 `slots.py ... boot`，只启动状态文件中已确认的活动槽，中断的切换恢复最后确认入口。

稳定内部入口使用独立回环 Caddy，插件控制面连接该入口。路由配置放在服务用户可读取的 `/etc/caddy/slot-router/`，管理配置 `slots.json` 保持 root 私有；部署前必须用真实服务身份启动该入口并验收，不能只做 root 下语法检查。`current_link` 可同步当前发行版引用。生产切换增加内存门槛：`minimum_available_mib: 512`、`maximum_draining_mib: 128`；不满足就保留活动槽，待低负载时重试，不改用户的并发或正文配置。

旧更新器配置声明 `slot_config` 后会拒绝原地更新、回退和重启，避免绕过 A/B。当前槽执行器通过维护 CLI 调用；管理面板更新入口尚未接入槽执行器。

## 验证与上线状态

本机 `scripts/test-ab-slots.py` 使用隔离 PostgreSQL、Redis、真实网关与 Caddy，验证 A→B→A、失败候选不影响活动槽、入口连续可用、同库登录状态、旧槽退出。Rust 回归测试覆盖活请求不被另一槽恢复、后台租约保留、流式排空及 WS 完成后仅计费一次；Python 测试覆盖切换和恢复阶段。

2026-10-08 已经用户授权的一次维护交接迁入生产 A/B，当前 A 运行、B 停止。网关槽端口 8322/8323，固定内部入口 18340，控制面 18335；都仅绑定回环。旧单实例服务停止且关闭自启；开机协调、活动槽自动重启、共享数据库与插件控制面已核验。生产没有通过实际重启整台 VPS 测试开机过程。

首次从旧版迁入仍需维护交接：旧进程的排他实例锁不能与槽版本共存，禁止终止锁连接或绕过锁启动第二实例。首次交接的独立恢复任务只执行一次；完成后的任务不会主动回退。
