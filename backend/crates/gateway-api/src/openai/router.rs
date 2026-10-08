//! OpenAI 客户端协议路由。

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};

use super::{
    auth::{authenticate_client, client_access_error_response},
    images::{image_edits, image_generations},
    models::{model_detail, models},
    responses::{responses, responses_websocket},
    search::standalone_search,
    usage,
};

use crate::ApiState;

/// 构造 OpenAI 客户端协议路由。
pub(crate) fn router(state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/v1/chat/completions", post(unsupported_protocol))
        .route("/v1beta", any(unsupported_protocol))
        .route("/v1beta/{*path}", any(unsupported_protocol))
        .route("/v1/images/generations", post(image_generations))
        .route("/v1/images/edits", post(image_edits))
        .route("/v1/alpha/search", post(standalone_search))
        .route("/v1/responses", get(responses_websocket).post(responses))
        .route("/v1/models", get(models))
        // 官方 OpenAI 模型详情合同使用 path ID；它不属于 Admin API 约束。
        .route("/v1/models/{model_id}", get(model_detail))
        .merge(usage::router())
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            super::resources::protect,
        ))
        // 正文上限由实例显式配置，不使用 Axum 默认的 2 MiB 限制。
        .layer(DefaultBodyLimit::disable())
}

/// 不把旧 CPA 的其他协议伪装成 Native 透传，也不为 Excel 静默换通道。
async fn unsupported_protocol(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let client = match authenticate_client(state.openai(), &headers) {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    unsupported_endpoint_response(client.policy().account_scope().excel_bridge_enabled())
}

pub(super) fn unsupported_endpoint_response(excel: bool) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": {
                "type": "invalid_request_error",
                "code": "unsupported_endpoint",
                "message": if excel {
                    "Excel Bridge supports HTTP Responses, models and JSON image endpoints only. Use POST /v1/responses with full history; Chat Completions, Gemini, WebSocket and standalone search are not supported."
                } else {
                    "This gateway exposes native Codex Responses, models, JSON images and standalone search. Chat Completions and Gemini conversion are not supported; configure your client to use /v1/responses."
                },
                "param": null
            }
        })),
    )
        .into_response()
}
