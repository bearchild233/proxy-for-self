//! OpenAI 请求头的 HTTP 传输边界。

/// 判断小写请求头是否属于传输层管理的字段，不得作为业务扩展头透传。
///
/// API 入站和 Provider 编码共用此分类；`Connection` 动态声明的逐跳头由入站额外剥离。
/// 只用于请求方向，不影响上游响应中的代理诊断信息。
#[must_use]
pub fn is_transport_managed_request_header(name: &str) -> bool {
    name.starts_with("sec-websocket-")
        || matches!(
            name,
            "connection"
                | "keep-alive"
                | "proxy-connection"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
                | "host"
                | "content-length"
                // 请求实体与响应压缩能力属于各段 transport，不能继承下游协商。
                | "content-encoding"
                | "accept-encoding"
                // 网关中间件也会生成该链路诊断 ID，不作为业务上下文转发。
                | "x-request-id"
        )
}
