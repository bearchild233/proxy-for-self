# 共享展示顺序与请求位置

账号、分组、API 密钥、代理列表左侧提供拖动手柄，也支持聚焦手柄后 Alt + 上/下键。
当前页拖动完成后自动保存到 PostgreSQL，其他设备重新加载时使用相同顺序。
后端先按共享顺序排序再分页；筛选后只交换可见项原有的位置，筛选外的项保持原位。
新增项追加在已排序项之后。按表头排序时暂停拖动，点击“恢复拖动顺序”返回共享顺序。
这不改变账号权重、分组优先级、轮转策略或正在进行的会话。

`POST /api/admin/settings/display-order`（管理员会话与现有 CSRF 防护）：

```json
{"scope":"accounts","originalIds":["acct_a","acct_b"],"orderedIds":["acct_b","acct_a"]}
```

scope 支持 accounts/groups/keys/proxies；两组 ID 必须是同一组不重复的 2～200 项。
事务内锁定该列表的顺序，验证 originalIds 仍与当前相对顺序相符，再替换这些位置。
过期编辑或已删除的项返回冲突，前端重读列表并提示重试；不同页的排序不会覆盖彼此。
API 密钥列表增加 `sortBy=manual&sortDirection=asc`，仍使用键集游标；列表变动后应重新从首页读取。

请求位置卡片修改后自动保存，显示保存中、已保存或失败重试状态。
`POST /api/admin/settings/request-location` 只更新位置与开关，不覆盖其他运行设置：

```json
{"enabled":true,"location":{"country":"US","region":"California","city":"Los Angeles","timezone":"America/Los_Angeles"}}
```

保存后热更新运行快照。关闭覆盖会保留已保存的位置；账号出站代理自定义位置仍优先。
迁移 22 将旧内置默认位置换为洛杉矶，保留其他自定义位置及现有开关状态。
刷新页面前等待“位置已保存到服务器”；保存失败会明确提示。

使用统计默认列顺序为：密钥、账号、平台、模型、推理强度、延迟、Token、费用、时间、端点、上游、接入、IP、User-Agent。
若浏览器已有自定义列顺序，列设置中的“恢复默认”可应用新版顺序。
