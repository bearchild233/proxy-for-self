# 可选 VLESS / Hysteria2 出口

网关原有 HTTP、HTTPS、SOCKS5、SOCKS5H 直接走已有传输。VLESS / Hysteria2 由独立桥接服务调用 sing-box 完成协议连接；不改变服务器默认路由，不开启 TUN，不安装公网代理入口。

支持范围：

- VLESS：TCP（无 TLS、TLS、Reality），WebSocket（无 TLS、TLS）；TCP TLS/Reality 可选 `xtls-rprx-vision`。
- 接受分享链接的 `udp=true/false/1/0`；关闭时限制节点 TCP，开启不代表桥接提供 UDP 入口。
- Hysteria2 / hy2：密码、SNI、证书校验、ALPN、Salamander 混淆。
- 未支持的参数（例如 XHTTP、gRPC、Hysteria2 端口跳跃、证书 pin、ECH）明确拒绝，不默默忽略。
- 该桥接服务支持 HTTP CONNECT，覆盖产品使用的 HTTPS/WSS 上游；不提供通用明文 HTTP 转发或 UDP 代理入口。

## 数据与生命周期

数据库保留原始节点链接，沿用原有代理持久化及访问控制。管理列表仅返回脱敏端点，不返回 UUID、密码、查询参数或备注。这里没有为原有代理表新增静态加密；数据库与备份仍应按凭据保管。

Core 将节点 URI 编码为回环 HTTP 代理认证信息。**编码不是加密**，该认证信息仍是秘密，只可传给同机 `127.0.0.1:18323`。桥接进程校验节点配置，为每个不同配置启动独立 sing-box；子进程只通过标准输入获取配置，使用随机本机入口密码。节点认证不转发到目标服务器、不写入日志或进程参数。

最多 64 条隧道、默认 2 个同时使用的节点（同一节点可以承载多账号）。无活跃连接的节点 120 秒后回收，也可为新节点让位；正在使用的节点不被驱逐。超过容量返回失败，可按机器容量通过 `EGRESS_MAX_NODES` 调整。已有长连接不会因保存其他节点而重启。TLS/WS 字节在 CONNECT 之后直接双向转发；单向空闲 600 秒会关闭隧道。

桥接不存在、节点不可达、参数不支持或容量不足均失败，不回退直连。节点子进程异常后，新连接重新拉起；桥接服务由 systemd 自动恢复。网关与桥接独立，原 HTTP/SOCKS 出口不依赖此服务。

## 安装

适用于 Linux，Python >= 3.10。固定验证的运行时为 [sing-box v1.14.2](https://github.com/SagerNet/sing-box/releases/tag/v1.14.2)，从官方发布页下载安装，校验其发布资产 SHA256。amd64 tar.gz 的 SHA256 为 `a684484d7477d1437282ee411f4d131d0340aaad60a7868841ebd5d87dd8a0c6`。

把本目录的 `bridge.py`、`node_config.py` 和官方 `sing-box` 可执行文件安装到 `/opt/proxy-for-self-egress/`（root 拥有，目录 0755、源码 0644、二进制 0755）。保留官方 LICENSE 和版本/校验来源。安装 `deploy/systemd/proxy-for-self-egress.service` 后：

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now proxy-for-self-egress.service
sudo systemctl show proxy-for-self-egress.service -p ActiveState -p Restart -p MemoryCurrent
```

模板使用 DynamicUser、只读文件系统、128 MiB 内存上限，不以 root 运行。网关和 Excel Worker 必须与桥接共享回环网络空间；Docker 部署不能仅在容器外启动此服务然后假设容器可访问宿主机的 127.0.0.1，需要单独配置共同网络命名空间。当前 Compose 模板未自动安装桥接。

`RestrictAddressFamilies` 必须包含 `AF_NETLINK`，供 sing-box 检查网络接口；无需 `CAP_NET_ADMIN`，不启用 TUN。只在 root shell 测通节点不能替代 systemd 隔离环境验证，缺少该地址族会导致 sing-box 启动失败。

升级桥接二进制或服务代码会中断经过它的隧道，应先排空相应账号。普通面板新增节点不需要重启桥接或网关。不要在有新协议节点仍被账号绑定时卸载服务。

## 验证与许可

```sh
python3 -m unittest discover -s services/egress -p 'test_*.py'
EGRESS_TEST_BINARY=/path/to/sing-box python3 -m unittest discover -s services/egress -p 'test_*.py'
```

第二条在本机临时启动 VLESS TCP、Reality、WebSocket TLS、Hysteria2 节点及回显目标，验证实际隧道、节点复用、容量边界与无直连回退；不使用线上账号或节点。

桥接源码为本项目独立实现。sing-box 是单独下载并运行的 GPL-3.0-or-later 程序，其许可证及额外名称限制见官方发行包 LICENSE；本仓库不包含其二进制或源码，不将其许可证改为本项目许可证。配置参考：[VLESS](https://sing-box.sagernet.org/configuration/outbound/vless/)、[TLS/Reality](https://sing-box.sagernet.org/configuration/shared/tls/)、[Hysteria2](https://sing-box.sagernet.org/configuration/outbound/hysteria2/)。
