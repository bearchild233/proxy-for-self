# egress（VLESS / Hysteria2 出口桥接）

<!-- doc-sync paths: services/egress -->

## 职责与边界

可选的本机 HTTP CONNECT 桥接服务：让账号出口可以使用 VLESS 与 Hysteria2 分享链接。网关原有 HTTP / HTTPS / SOCKS5 / SOCKS5H 出口直接走已有传输，不依赖本服务；本服务只负责把节点 URI 变成一个本机 CONNECT 代理。它不改变服务器默认路由，不开启 TUN，不提供公网入口、通用明文 HTTP 转发或 UDP 入口。

- 依赖：官方 sing-box 可执行文件（单独下载，GPL-3.0-or-later，不随仓库分发，固定版本与 SHA256 见 [出口桥接说明](../../services/egress/README.md)）；Python >= 3.10 标准库。
- 调用方：网关各 Provider 的账号出口（Core 把原始节点 URI 编码为回环 HTTP 代理认证信息，连接 `127.0.0.1:18323`）、Excel Worker、Host 的出口探测，都用同一个回环地址。数据库保存原始 URI，列表只返回脱敏端点。
- 失败即失败：桥接不存在、节点不可达、参数不支持或容量不足都不回退直连。

## 实现逻辑

**`bridge.py`**：
- `Bridge.handle`：监听 `127.0.0.1:18323`，只接受 `CONNECT host:port HTTP/1.1`（头部上限 16 KiB），目标不能注入头或改写为 absolute-form；从 `Proxy-Authorization` 取出节点 URI（编码不是加密，认证信息本身是秘密，只能走同机回环）。
- `Manager.acquire(uri)`：按**实际节点配置**去重（URL 参数顺序和备注不触发新实例），为每个不同配置启动一个独立 sing-box 子进程；子进程只通过标准输入拿配置，监听随机本机端口和随机本地认证信息；之后桥接把 CONNECT 转给该子进程（只向子进程发送其随机认证，节点 URI 永不转发给目标服务器），双向 `relay`。
- 容量与回收：最多 64 条隧道，默认同时 2 个节点（环境变量 `EGRESS_MAX_NODES`，同一节点可承载多账号）；无活跃连接的节点 120 秒后由 `reap` 回收，也可为新节点让位；正在使用的节点不被驱逐，超过容量返回失败。节点子进程异常后新连接会重新拉起，已有长连接随进程结束。
- 二进制路径来自 `SING_BOX_BINARY`（默认 `/opt/proxy-for-self-egress/sing-box`）。

**`node_config.py`**：
- `outbound(uri)`：把分享链接转为 sing-box 出站配置。VLESS 支持 TCP（无 TLS / TLS / Reality，可选 `xtls-rprx-vision`）与 WebSocket（无 TLS / TLS）；Hysteria2（`hy2://` 同义）支持密码、SNI、证书校验开关、ALPN、Salamander 混淆。接受 `udp=true/false/1/0`（关闭时限制节点 TCP，开启不代表提供 UDP 入口）。
- 严格白名单：URI 长度、控制字符、查询参数个数与键集合、布尔取值都校验；XHTTP、gRPC、端口跳跃、证书 pin、ECH 等未支持参数明确拒绝，不默默忽略。
- `configuration(node, port, password)`：生成带本地 HTTP 入站（随机端口与密码）的完整 sing-box 配置。

## 文件表

| 文件 | 职责 |
| --- | --- |
| `services/egress/bridge.py` | CONNECT 服务、节点子进程管理（`Manager`）、容量与回收 |
| `services/egress/node_config.py` | 分享链接 → sing-box 配置 |
| `services/egress/README.md` | 支持范围、安装、systemd 隔离、验证与许可 |
| `services/egress/test_bridge.py` `services/egress/test_node_config.py` | 单元测试；设置 `EGRESS_TEST_BINARY` 后另跑真实 sing-box 的隧道测试 |

## 数据、配置与接口

- 无持久化。环境变量：`SING_BOX_BINARY`、`EGRESS_MAX_NODES`。
- 部署单元 `deploy/systemd/proxy-for-self-egress.service`：`DynamicUser`、只读文件系统、`MemoryMax=128M`、`RestrictAddressFamilies` 必须含 `AF_NETLINK`（sing-box 检查网卡），不授予 `NET_ADMIN`。
- 与网关必须共享回环网络空间；Docker 部署不能只在容器外启动本服务后假设容器能访问宿主机 127.0.0.1。

## 改动指引

- 新增协议或选项：先在 `node_config.py` 的白名单加入并补测试；未验证的选项保持拒绝。
- 不要记录认证头或原始异常；任何日志都不得出现节点 URI。
- 升级桥接代码或 sing-box 会中断经过它的隧道，先排空相应账号；普通面板新增节点不需要重启桥接或网关。
- 验证：`python3 -m unittest discover -s services/egress -p 'test_*.py'`。
