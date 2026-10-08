//! 在所有 HTTP/WS 路由之前统一校验来源，业务层只读取规范化后的 ConnectInfo。
use std::net::{IpAddr, SocketAddr};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

pub(crate) async fn resolve(
    State(trusted): State<Vec<IpAddr>>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(ConnectInfo(peer)) = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .copied()
        && trusted.contains(&peer.ip())
        && request.uri().path() != "/healthz"
    {
        let mut values = request.headers().get_all("x-real-ip").iter();
        let ip = values
            .next()
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<IpAddr>().ok());
        // 配置为可信代理后必须提供唯一 IP，缺失或歧义不能静默退回共享代理地址。
        let Some(ip) = ip.filter(|_| values.next().is_none()) else {
            return (StatusCode::BAD_REQUEST, "Invalid proxy client address").into_response();
        };
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip, peer.port())));
    }
    // 来源头不再参与业务选择，也不向上游转发访客可控的第二套来源。
    for name in [
        "x-real-ip",
        "x-forwarded-for",
        "forwarded",
        "cf-connecting-ip",
        "cf-connecting-ipv6",
        "true-client-ip",
    ] {
        request.headers_mut().remove(name);
    }
    next.run(request).await
}
