use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header::AUTHORIZATION},
};
use tower::ServiceExt;

use super::api_router_with_origins;
use super::models::ModelsExecution;

const REMOVED_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;

#[tokio::test]
async fn hot_limits_preserve_in_flight_usage_when_lowered_and_raised() {
    let execution = ModelsExecution::new();
    let app = api_router_with_origins(execution.clone(), Vec::new()).await;
    let limits = |requests, body, budget| gateway_api::InferenceLimits {
        max_requests: requests,
        max_body_bytes: body,
        max_in_flight_body_bytes: budget,
    };
    let request = |bytes: usize| {
        Request::post("/v1/responses")
            .header(AUTHORIZATION, "Bearer sk_models_test")
            .header("content-type", "application/json")
            .header("content-length", bytes)
            .body(Body::from(format!("{{}}{}", " ".repeat(bytes - 2))))
            .unwrap()
    };
    *execution.limits.lock().unwrap() = Some(limits(2, 50, 100));
    let first = app.clone().oneshot(request(40)).await.unwrap();
    let second = app.clone().oneshot(request(40)).await.unwrap();
    assert_eq!(first.status(), StatusCode::BAD_REQUEST);
    assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    *execution.limits.lock().unwrap() = Some(limits(1, 50, 50));
    assert_eq!(
        app.clone().oneshot(request(2)).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(first);
    assert_eq!(
        app.clone().oneshot(request(2)).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    // 提高并发后仍须计入旧请求的 40 字节，不能凭配置替换清空预算。
    *execution.limits.lock().unwrap() = Some(limits(3, 50, 50));
    assert_eq!(
        app.clone().oneshot(request(20)).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        app.clone().oneshot(request(10)).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    *execution.limits.lock().unwrap() = Some(limits(3, 10, 50));
    assert_eq!(
        app.clone().oneshot(request(11)).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    drop(second);
    *execution.limits.lock().unwrap() = Some(limits(3, 50, 50));
    let full = app.clone().oneshot(request(50)).await.unwrap();
    assert_eq!(full.status(), StatusCode::BAD_REQUEST);
    // 消费完响应与主动取消都必须归还占用。
    axum::body::to_bytes(full.into_body(), 4096).await.unwrap();
    assert_eq!(
        app.oneshot(request(50)).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn body_budget_allows_fifteen_small_requests_but_bounds_large_requests() {
    let admin = crate::admin::AdminTestFixture::new().await;
    let app = gateway_api::initialize(
        gateway_api::ApiConfig {
            trusted_proxy_ips: Vec::new(),
            inference_limits: gateway_api::InferenceLimits {
                max_requests: 15,
                max_body_bytes: 50,
                max_in_flight_body_bytes: 128,
            },
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-request-id".into(),
        },
        ModelsExecution::new(),
        admin.services,
        Vec::new(),
        std::sync::Arc::new(super::EmptyWorkerHealth),
        std::sync::Arc::new(super::TestLifecycle::default()),
    )
    .unwrap()
    .router();
    let request = |length: usize| {
        Request::post("/v1/responses")
            .header(AUTHORIZATION, "Bearer sk_models_test")
            .header("content-type", "application/json")
            .header("content-length", length)
            .body(Body::from(format!("{{}}{}", " ".repeat(length - 2))))
            .unwrap()
    };
    let mut responses = Vec::new();
    for _ in 0..15 {
        let response = app.clone().oneshot(request(2)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        responses.push(response);
    }
    assert_eq!(
        app.clone().oneshot(request(2)).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(responses);
    let first = app.clone().oneshot(request(48)).await.unwrap();
    let second = app.clone().oneshot(request(48)).await.unwrap();
    assert_eq!(first.status(), StatusCode::BAD_REQUEST);
    assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    let busy = app.clone().oneshot(request(48)).await.unwrap();
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy.headers()["retry-after"], "1");
    assert_eq!(
        app.clone().oneshot(request(2)).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    drop(first);
    assert_eq!(
        app.clone().oneshot(request(48)).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    drop(second);
    let mut understated = request(2);
    understated
        .headers_mut()
        .insert("content-length", "1".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(understated).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let mut compressed = request(2);
    compressed
        .headers_mut()
        .insert("content-encoding", "gzip".parse().unwrap());
    let compressed = app.clone().oneshot(compressed).await.unwrap();
    let big = app.clone().oneshot(request(48)).await.unwrap();
    assert_eq!(big.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        app.clone().oneshot(request(48)).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(compressed);
    drop(big);
    let mut unknown = request(2);
    unknown.headers_mut().remove("content-length");
    let unknown = app.clone().oneshot(unknown).await.unwrap();
    let big = app.clone().oneshot(request(48)).await.unwrap();
    assert_eq!(
        app.clone().oneshot(request(48)).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    drop((unknown, big));
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(br#"{"model":"a","input":"b"}"#).unwrap();
    let body = encoder.finish().unwrap();
    assert!(body.len() <= 48);
    let compressed = Request::post("/v1/responses")
        .header(AUTHORIZATION, "Bearer sk_models_test")
        .header("content-type", "application/json")
        .header("content-encoding", "gzip")
        .header("content-length", body.len())
        .body(Body::from(body))
        .unwrap();
    let compressed = app.clone().oneshot(compressed).await.unwrap();
    let first = app.clone().oneshot(request(40)).await.unwrap();
    let second = app.oneshot(request(40)).await.unwrap();
    assert_eq!(first.status(), StatusCode::BAD_REQUEST);
    assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    drop((compressed, first, second));
}

#[tokio::test]
async fn memory_guard_bounds_bodies_holds_stream_permits_and_keeps_models_available() {
    let admin = crate::admin::AdminTestFixture::new().await;
    let app = gateway_api::initialize(
        gateway_api::ApiConfig {
            trusted_proxy_ips: Vec::new(),
            inference_limits: gateway_api::InferenceLimits {
                max_requests: 1,
                max_body_bytes: 32,
                max_in_flight_body_bytes: 0,
            },
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-request-id".into(),
        },
        ModelsExecution::new(),
        admin.services,
        Vec::new(),
        std::sync::Arc::new(super::EmptyWorkerHealth),
        std::sync::Arc::new(super::TestLifecycle::default()),
    )
    .unwrap()
    .router();
    let request = |body: Body| {
        Request::post("/v1/responses")
            .header(AUTHORIZATION, "Bearer sk_models_test")
            .header("content-type", "application/json")
            .body(body)
            .unwrap()
    };
    let oversized = app
        .clone()
        .oneshot(request(Body::from("x".repeat(33))))
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let first = app
        .clone()
        .oneshot(request(Body::from("{}")))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::BAD_REQUEST);
    let busy = app
        .clone()
        .oneshot(request(Body::from_stream(futures::stream::poll_fn(|_| {
            panic!("busy request must not read its body");
            #[allow(unreachable_code)]
            std::task::Poll::<Option<Result<axum::body::Bytes, std::io::Error>>>::Ready(None)
        }))))
        .await
        .unwrap();
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy.headers()["retry-after"], "1");
    let models = app
        .clone()
        .oneshot(
            Request::get("/v1/models")
                .header(AUTHORIZATION, "Bearer sk_models_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(models.status(), StatusCode::OK);
    drop(first);
    let available = app.oneshot(request(Body::from("{}"))).await.unwrap();
    assert_eq!(available.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unsupported_protocols_are_explicit_and_still_require_authentication() {
    for path in [
        "/v1/chat/completions",
        "/v1beta/models",
        "/v1beta/models/test:generateContent",
    ] {
        let app = api_router_with_origins(ModelsExecution::new(), Vec::new()).await;
        for authenticated in [false, true] {
            let mut request = Request::post(path);
            if authenticated {
                request = request.header(AUTHORIZATION, "Bearer sk_models_test");
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if authenticated {
                    StatusCode::NOT_IMPLEMENTED
                } else {
                    StatusCode::UNAUTHORIZED
                }
            );
            if authenticated {
                let body = axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["error"]["code"], "unsupported_endpoint");
                assert!(
                    body["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("/v1/responses")
                );
            }
        }
    }
}

#[tokio::test]
async fn excel_key_rejects_unsupported_search_and_chat_before_execution() {
    let app = api_router_with_origins(ModelsExecution::excel(), Vec::new()).await;
    for path in ["/v1/chat/completions", "/v1/alpha/search"] {
        let response = app
            .clone()
            .oneshot(
                Request::post(path)
                    .header(AUTHORIZATION, "Bearer sk_models_test")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("Excel Bridge"));
    }
}

#[tokio::test]
async fn excel_websocket_handshake_is_rejected_instead_of_wrapping_http() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let app = api_router_with_origins(ModelsExecution::excel(), Vec::new()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!(
        "GET /v1/responses HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer sk_models_test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut response = [0_u8; 4096];
    let length = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        socket.read(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    server.abort();
    assert!(String::from_utf8_lossy(&response[..length]).starts_with("HTTP/1.1 501"));
}

#[tokio::test]
async fn browser_origin_controls_http_admin_sessions_without_configuration() {
    use axum::body::to_bytes;
    use serde_json::{Value, json};
    for (origins, secure) in [
        (vec!["http://admin.example.test"], false),
        (vec!["http://192.0.2.1:8080"], false),
        (vec!["http://[::1]:8080"], false),
        (vec!["https://admin.example.test"], true),
        (vec![], true),
        (vec!["null"], true),
        (vec!["invalid-origin"], true),
        (vec!["http://admin.example.test/path"], true),
        (vec!["http://admin.example.test?https"], true),
        (vec!["http://user@admin.example.test"], true),
        (
            vec!["http://admin.example.test", "https://admin.example.test"],
            true,
        ),
    ] {
        let app = api_router_with_origins(ModelsExecution::new(), Vec::new())
            .await
            .layer(axum::Extension(axum::extract::ConnectInfo(
                std::net::SocketAddr::from(([127, 0, 0, 1], 41000)),
            )));
        let request = |path: &str| {
            let mut builder = Request::post(path)
                .header("content-type", "application/json")
                .header("x-forwarded-proto", "http")
                .header("forwarded", "proto=http");
            for origin in &origins {
                builder = builder.header("origin", *origin);
            }
            builder
        };
        let response = app
            .clone()
            .oneshot(
                request("/api/auth/login")
                    .body(Body::from(
                        json!({"mode": "admin", "username": "admin_1", "password": "strong-admin-password"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()["set-cookie"].to_str().unwrap();
        let session = cookie.split(';').next().unwrap().to_owned();
        let attrs: Vec<_> = cookie.split(';').map(str::trim).collect();
        assert_eq!(attrs.contains(&"Secure"), secure);
        for attribute in ["Path=/", "HttpOnly", "SameSite=Lax"] {
            assert!(attrs.contains(&attribute));
        }

        let response = app
            .clone()
            .oneshot(
                Request::get("/api/auth/status")
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["data"]["authenticated"], true);

        let response = app
            .clone()
            .oneshot(
                request("/api/auth/logout")
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()["set-cookie"].to_str().unwrap();
        assert!(cookie.starts_with("cpr_session=;"));
        let attrs: Vec<_> = cookie.split(';').map(str::trim).collect();
        assert_eq!(attrs.contains(&"Secure"), secure);
        for attribute in ["Path=/", "HttpOnly", "SameSite=Lax", "Max-Age=0"] {
            assert!(attrs.contains(&attribute));
        }

        let response = app
            .oneshot(
                Request::get("/api/auth/status")
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["data"]["authenticated"], false);
    }
}

#[tokio::test]
async fn removed_responses_review_route_should_not_reach_the_responses_handler() {
    let response = api_router_with_origins(ModelsExecution::new(), Vec::new())
        .await
        .oneshot(
            Request::post("/v1/responses/review")
                .header(AUTHORIZATION, "Bearer sk_models_test")
                .body(Body::empty())
                .expect("build removed review request"),
        )
        .await
        .expect("route removed review request");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn removed_models_catalog_extension_should_use_standard_model_lookup() {
    let response = api_router_with_origins(ModelsExecution::new(), Vec::new())
        .await
        .oneshot(
            Request::get("/v1/models/catalog")
                .header(AUTHORIZATION, "Bearer sk_models_test")
                .body(Body::empty())
                .expect("build removed model catalog request"),
        )
        .await
        .expect("route removed model catalog request");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn removed_model_info_route_should_return_not_found() {
    let response = api_router_with_origins(ModelsExecution::new(), Vec::new())
        .await
        .oneshot(
            Request::get("/v1/models/model-a/info")
                .header(AUTHORIZATION, "Bearer sk_models_test")
                .body(Body::empty())
                .expect("build removed model info request"),
        )
        .await
        .expect("route removed model info request");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn responses_body_should_accept_payload_above_the_removed_private_limit() {
    let router = api_router_with_origins(ModelsExecution::new(), Vec::new()).await;
    let response = router
        .oneshot(
            Request::post("/v1/responses")
                .body(Body::from(vec![b'a'; REMOVED_BODY_LIMIT_BYTES + 1]))
                .expect("build request above the removed limit"),
        )
        .await
        .expect("route request above the removed limit");

    // The handler sees the body and rejects the missing credentials. A restored body limit
    // would return 413 before authentication runs.
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn configured_cors_origin_assembles_router_and_answers_preflight() {
    let router = api_router_with_origins(
        ModelsExecution::new(),
        vec!["https://app.example.com".into()],
    )
    .await;

    let response = router
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/models")
                .header("origin", "https://app.example.com")
                .header("access-control-request-method", "GET")
                .header("access-control-request-headers", "authorization")
                .body(Body::empty())
                .expect("build preflight request"),
        )
        .await
        .expect("route preflight request");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("https://app.example.com")
    );
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-credentials")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    let allow_headers = response
        .headers()
        .get("access-control-allow-headers")
        .and_then(|value| value.to_str().ok())
        .expect("allow-headers present");
    assert!(allow_headers.contains("authorization"));
    assert!(allow_headers.contains("x-api-key"));
    assert!(allow_headers.contains("x-request-id"));
}
