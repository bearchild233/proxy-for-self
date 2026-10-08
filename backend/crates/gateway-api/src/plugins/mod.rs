//! 插件公共控制面：身份仅从登录会话解析，不接收客户端声明的角色或 Key ID。
use axum::{
    Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use gateway_admin::model::{auth::SessionSubject, system::PluginRequest};
use serde_json::{Value, json};

use crate::{
    admin::{AdminEnvelope, AdminError, AdminJson, AdminResponse, wire::map_admin_service_error},
    auth::SessionState,
    session_cookie,
};

pub(crate) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/plugins", get(catalog::<S>))
        .route("/api/plugins/{id}/pages/{name}", get(page::<S>))
        .route("/api/plugins/{id}/rpc/{name}", post(rpc::<S>))
        .route("/api/plugin-devices/{id}/rpc/{name}", post(device_rpc::<S>))
        .layer(DefaultBodyLimit::max(60 * 1024))
        .layer(middleware::map_response(no_store))
}

async fn dispatch<S: SessionState + Send + Sync>(
    state: &S,
    headers: &HeaderMap,
    kind: &str,
    plugin_id: Option<String>,
    name: Option<String>,
    input: Value,
) -> Result<Response, AdminError> {
    let session = state
        .admin_services()
        .auth()
        .session(session_cookie::value(headers).as_deref())
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    let principal = match session.subject {
        SessionSubject::Admin { admin_user_id, .. } => {
            json!({"role": "admin", "id": admin_user_id})
        }
        SessionSubject::Key { client_key_id } => {
            json!({"role": "key", "id": client_key_id.as_str()})
        }
    };
    let data = state
        .admin_services()
        .system()
        .plugin_request(PluginRequest {
            kind: kind.to_owned(),
            plugin_id,
            name,
            principal,
            input,
        })
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)).into_response())
}

async fn catalog<S: SessionState + Send + Sync>(
    State(state): State<S>,
    headers: HeaderMap,
) -> Result<Response, AdminError> {
    dispatch(&state, &headers, "catalog", None, None, Value::Null).await
}

async fn page<S: SessionState + Send + Sync>(
    State(state): State<S>,
    headers: HeaderMap,
    Path((id, name)): Path<(String, String)>,
) -> Result<Response, AdminError> {
    dispatch(&state, &headers, "page", Some(id), Some(name), Value::Null).await
}

async fn rpc<S: SessionState + Send + Sync>(
    State(state): State<S>,
    headers: HeaderMap,
    Path((id, name)): Path<(String, String)>,
    AdminJson(input): AdminJson<Value>,
) -> Result<Response, AdminError> {
    dispatch(&state, &headers, "rpc", Some(id), Some(name), input).await
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// 设备续期等插件自有凭据入口；无网关身份权限，必须由插件验证自己的设备凭据。
async fn device_rpc<S: SessionState + Send + Sync>(
    State(state): State<S>,
    Path((id, name)): Path<(String, String)>,
    AdminJson(input): AdminJson<Value>,
) -> Result<Response, AdminError> {
    let data = state
        .admin_services()
        .system()
        .plugin_request(PluginRequest {
            kind: "rpc".to_owned(),
            plugin_id: Some(id),
            name: Some(name),
            principal: json!({"role": "device", "id": ""}),
            input,
        })
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)).into_response())
}
