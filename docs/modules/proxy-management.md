# proxy-management 插件（出口代理）

<!-- doc-sync paths: plugins/proxy-management -->

## 职责与边界

可选运维类界面插件（页面 `/proxies`，仅管理员，可停用）：命名出口代理的列表、创建、编辑、删除、连通性测试与探测，查看绑定到代理的账号并单个移除绑定。**没有后端进程**；代理的持久化、事务、导入保护和 VLESS / Hysteria2 桥接都在后端（见 [架构](../architecture.md)“账号出站代理”）与 [egress](egress.md)。停用插件后，已保存的代理配置继续生效。

- 依赖：`/api/admin/proxies*`（`backend/crates/gateway-api/src/admin/proxies.rs`）、账号列表（绑定展示）。

## 实现逻辑

- `ui/page/index.vue`：代理表格（顺序可拖动，`display-order.save` 的 `proxies` 范围）、新建/编辑/删除、测试结果展示。
- `ui/page/components/ProxyFormModal.vue` + `ProxyConnectionFields.vue`：连接字段（协议 HTTP / HTTPS / SOCKS5 / SOCKS5H / VLESS / Hysteria2、地址、认证），连接串解析与脱敏展示在主壳 `frontend/src/utils/proxyConnection.ts`；保存前可 `probe` 未保存的连接串，已保存的代理用 `test`（带 revision）。
- `ui/page/components/ProxyAccountsModal.vue` + `ui/page/composables/useProxyAccounts.ts`：按代理分页读取绑定账号（复用分页查询，删除末页最后一项自动回到有效页；切换代理丢弃旧请求避免串数据），`accounts.remove` 清除账号的代理 ID 与 URL 而保留其他设置。
- 已绑定的代理不可删除；连接配置变化后后端在同一事务同步关联账号并清除旧测试结果。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/proxy-management/plugin.json` | 清单与 20 个能力 |
| `plugins/proxy-management/ui/entries.json` | 页面入口 |
| `plugins/proxy-management/ui/page/index.vue` | 页面 |
| `plugins/proxy-management/ui/page/components/` | 表单、连接字段、绑定账号弹窗 |
| `plugins/proxy-management/ui/page/composables/useProxyAccounts.ts` | 绑定账号分页与移除 |

## 数据、配置与接口

能力：`proxies.{get,create,update,delete,probe,test,accounts,accounts.remove}`、账号读取、`display-order.save`。无本地存储。

## 改动指引

- 新增代理协议：先在 `services/egress`（若需桥接）与后端 `backend/crates/gateway-core/src/account/proxy.rs` 值对象 / Provider 传输支持，再改连接字段与解析；不支持的参数应明确拒绝，不静默忽略。
- 测试结果只是诊断信息，不作为选择或绑定的准入条件；不要在前端据此禁用绑定。
- 校验：`node scripts/check-plugin.mjs proxy-management`、`node scripts/build-plugin.mjs proxy-management`；前端测试 `frontend/tests/proxy-connection.test.mjs`。
