# 登录防护

两层保护：入口每来源每分钟 10 次、每站点合计 200 次；fail2ban 按失败事件累计，默认 10 分钟内 5 次失败封 15 分钟，重复违规延长，最多 24 小时。API 原有 Redis 登录限流继续生效。

## 可信来源

HAProxy 到 Caddy 的 TLS 后端使用 `send-proxy-v2`。Caddy TLS listener 在 `tls` 前配置 `proxy_protocol`，仅允许本机来源。Caddy 回源统一覆盖 `X-Real-IP`、`X-Forwarded-For`，移除 `Forwarded`、`CF-Connecting-IP` 等外来来源头。网关 `api.trusted_proxy_ips` 显式配置 Caddy 回源 IP，默认空数组；可信代理必须提供唯一合法 `X-Real-IP`。HTTP 与 WS 的 ConnectInfo 统一规范化，登录与请求记录共用来源。

## 安装

- `proxy-login.conf` → `/etc/fail2ban/jail.d/proxy-login.conf`
- `action.conf` → `/etc/fail2ban/action.d/proxy-login.conf`
- `api-filter.conf` → `/etc/fail2ban/filter.d/proxy-api-login.conf`
- `panel-filter.conf` → `/etc/fail2ban/filter.d/proxy-panel-login.conf`
- 将 `proxy-api-login`、`proxy-panel-login` 的 Ban 日志从现有 recidive 过滤器排除，防止升级为全端口封禁。
- updater 同目录部署 `login_security.py`；服务配置新增 `login_security`，字段：`database`、`policy_file`、`socket`、`socket_group`。数据库由 root 持有，建议 `/var/lib/proxy-for-self-updater/login-bans.db`；策略文件 `/etc/fail2ban/jail.d/zz-proxy-login.local`；只读检查 socket `/run/proxy-for-self-login-guard/check.sock`，属组 caddy，权限 0660。
- systemd 创建 guard 的独立运行目录（0755），给予 updater 策略文件所在 jail.d 的写权限。私有管理 socket 仍只允许网关服务访问。
- Caddy 仅在两个 POST 登录路径执行 `forward_auth unix//run/proxy-for-self-login-guard/check.sock`，URI 为 `/check/api` 或 `/check/panel`，覆盖 `X-Login-Client-IP` 为 `{remote_host}`。其他路由不接入。

fail2ban 操作仅写 SQLite 封禁镜像。Caddy 的独立只读 socket 检查 IP，不需要每次封禁 reload 代理，不修改防火墙。检查服务不可用时登录返回错误，已登录管理请求和推理不受影响。封禁到期由时间戳判断，fail2ban 重启可从自身数据库恢复事件；SQLite 镜像跨 updater 重启保留。

## 管理

管理员「系统设置 → 安全与访问 → 登录防护」提供策略修改、状态、封禁列表和解封。API Key 用量身份无权访问。策略保存写入独立 jail.local 文件并只 reload 对应 jail，读取生效值确认。现有封禁到期时间不随策略修改缩短。

SSH 解封：`fail2ban-client set proxy-api-login unbanip <IP>`，VPS 面板使用 `proxy-panel-login`。面板原生的同 IP＋用户名锁定独立于 fail2ban，可能需等待其 15 分钟冷却；不自动重启 x-ui。

验证需覆盖：伪造来源头、IPv4/IPv6、不同来源隔离、封禁与解封、服务重启持久化、recidive 排除、已登录/推理路由不受封禁影响。只能用隔离的测试来源，不对真实管理员连续试错。
