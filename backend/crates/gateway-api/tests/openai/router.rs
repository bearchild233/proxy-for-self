use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header::AUTHORIZATION},
};
use tower::ServiceExt;

use super::api_router_with_origins;
use super::models::ModelsExecution;

const REMOVED_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;

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
