# Hermes Fast 转发

Hermes d177b11 的 `_fast_mode_route_supported` 仅允许官方域名。自定义代理可以显示 `/fast` 已开启，但实际请求不含 `service_tier`。proxy 接收 `service_tier=priority` 后按最终发送档位记录和估价；分组禁用 Fast 时仍以分组规则为准。

维护者使用 `python tools/patch-hermes-fast.py --source <Hermes>/hermes_cli/models.py --base-url https://api.ckcyi.com/v1` 检查补丁，追加 `--apply` 原子应用。只允许指定 HTTPS 地址及路径上的 OpenAI 模型；保留普通档、Fast 开关和 auto/cold 窗口语义，不强制所有请求走 Fast。升级 Hermes 后重新检查；源结构变化时拒绝自动修改。

应用后在会话空闲时重新加载 Hermes 进程。验证 `/fast fast` 的请求正文包含 `service_tier: priority`，`/fast normal` 不含该字段；面板使用统计按发送档位显示闪电。不要把响应回显的 default 当作客户端未请求 Fast 的唯一证据。
