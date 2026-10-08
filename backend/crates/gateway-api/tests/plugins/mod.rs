use axum::http::{Method, StatusCode, header};
use serde_json::json;
use tower::ServiceExt as _;

use crate::support::{
    RAW_KEY, cookie_request, empty_request, json_request, key_fixture, response_json,
};

#[tokio::test]
async fn plugin_calls_require_session_and_never_accept_client_identity() {
    let fixture = key_fixture().await;
    let app = crate::openai::api_router_with_admin(fixture.services);
    for path in ["/api/plugins", "/api/plugins/sample/pages/home"] {
        let response = app
            .clone()
            .oneshot(empty_request(Method::GET, path))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
    let response = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/auth/login",
            json!({"mode": "key", "apiKey": RAW_KEY}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let response = app
        .clone()
        .oneshot(cookie_request(Method::GET, "/api/plugins", &cookie))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = response_json(response).await;
    assert_eq!(
        data["data"]["principal"],
        json!({"role": "key", "id": "key-42"})
    );
    assert!(!data.to_string().contains(RAW_KEY));

    let mut request = json_request(
        Method::POST,
        "/api/plugins/sample/rpc/test",
        json!({"principal": {"role": "admin", "id": "other-key"}, "kind": "catalog"}),
    );
    request
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = response_json(response).await;
    assert_eq!(
        data["data"]["principal"],
        json!({"role": "key", "id": "key-42"})
    );
    assert_eq!(data["data"]["kind"], "rpc");
    assert_eq!(data["data"]["pluginId"], "sample");
    assert_eq!(data["data"]["name"], "test");
    let response = app
        .oneshot(cookie_request(
            Method::GET,
            "/api/admin/system/plugins",
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn device_entry_always_has_unprivileged_identity() {
    let fixture = key_fixture().await;
    let app = crate::openai::api_router_with_admin(fixture.services);
    let response = app
        .oneshot(json_request(
            Method::POST,
            "/api/plugin-devices/sample/rpc/renew",
            json!({"principal": {"role": "admin"}, "token": "plugin-device-token"}),
        ))
        .await
        .unwrap();
    let data = response_json(response).await;
    assert_eq!(
        data["data"]["principal"],
        json!({"role": "device", "id": ""})
    );
    assert_eq!(data["data"]["kind"], "rpc");
}
