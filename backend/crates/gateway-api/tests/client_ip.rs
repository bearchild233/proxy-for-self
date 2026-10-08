use crate::support::{RAW_KEY, json_request};
use axum::{
    extract::ConnectInfo,
    http::{Method, StatusCode},
};
use tower::ServiceExt as _;

#[tokio::test]
async fn trusted_proxy_ip_is_shared_by_login_and_untrusted_headers_cannot_override_it() {
    let fixture = crate::support::key_fixture().await;
    let store = fixture.auth.clone();
    let app = crate::openai::api_router_with_config(
        fixture.services,
        gateway_api::ApiConfig {
            trusted_proxy_ips: vec!["127.0.0.1".parse().unwrap()],
            inference_limits: Default::default(),
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-request-id".to_owned(),
        },
    );
    for (peer, real, expected) in [
        ("127.0.0.1:4000", "198.51.100.8", "198.51.100.8"),
        ("127.0.0.1:4000", "2001:db8::8", "2001:db8::8"),
        ("203.0.113.10:4000", "198.51.100.9", "203.0.113.10"),
    ] {
        let mut request = json_request(
            Method::POST,
            "/api/auth/login",
            serde_json::json!({"mode":"key","apiKey":RAW_KEY}),
        );
        request
            .extensions_mut()
            .insert(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
        request
            .headers_mut()
            .insert("x-real-ip", real.parse().unwrap());
        request
            .headers_mut()
            .insert("cf-connecting-ip", "192.0.2.99".parse().unwrap());
        request
            .headers_mut()
            .insert("x-forwarded-for", "192.0.2.99".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            *store.login_sources.lock().unwrap().last().unwrap(),
            expected.parse::<std::net::IpAddr>().unwrap()
        );
    }
    for value in [None, Some("garbage"), Some("198.51.100.1, 198.51.100.2")] {
        let mut request = json_request(
            Method::POST,
            "/api/auth/login",
            serde_json::json!({"mode":"key","apiKey":RAW_KEY}),
        );
        request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:4000".parse::<std::net::SocketAddr>().unwrap(),
        ));
        if let Some(value) = value {
            request
                .headers_mut()
                .insert("x-real-ip", value.parse().unwrap());
        }
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }
    let mut request = json_request(
        Method::POST,
        "/api/auth/login",
        serde_json::json!({"mode":"key","apiKey":RAW_KEY}),
    );
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:4000".parse::<std::net::SocketAddr>().unwrap(),
    ));
    request
        .headers_mut()
        .append("x-real-ip", "198.51.100.1".parse().unwrap());
    request
        .headers_mut()
        .append("x-real-ip", "198.51.100.2".parse().unwrap());
    assert_eq!(
        app.oneshot(request).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(store.login_sources.lock().unwrap().len(), 3);
}
