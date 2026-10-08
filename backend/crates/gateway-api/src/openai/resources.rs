//! 在正文缓冲/解析前限流，并把许可持有到响应流关闭，避免大请求并发耗尽内存。

use std::sync::{Arc, Mutex};

use axum::{
    Json,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures::{StreamExt, stream};
use gateway_core::error::{GatewayError, GatewayErrorKind};

use super::auth::{authenticate_client, client_access_error_response};
use super::error::gateway_error_response;
use crate::{ApiState, InferenceLimits};

#[derive(Clone, Default)]
pub(super) struct RequestResources {
    usage: Arc<Mutex<ResourceUsage>>,
    defaults: InferenceLimits,
}

#[derive(Default)]
struct ResourceUsage {
    requests: u64,
    bytes: u64,
}

/// 计数器属于进程，修改限制只影响准入判定，不替换在途占用。
pub(super) struct ResourceLease {
    usage: Arc<Mutex<ResourceUsage>>,
    requests: u64,
    bytes: u64,
}
impl Drop for ResourceLease {
    fn drop(&mut self) {
        let mut usage = self.usage.lock().unwrap_or_else(|error| error.into_inner());
        usage.requests -= self.requests;
        usage.bytes -= self.bytes;
    }
}

#[derive(Clone)]
pub(crate) struct BodyBudgetLease(Arc<Mutex<Option<ResourceLease>>>);

#[derive(Clone, Copy)]
pub(crate) struct AdmittedBodyLimit(pub(crate) usize);

impl BodyBudgetLease {
    pub(crate) fn shrink_to(&self, bytes: usize) {
        let mut guard = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(lease) = guard.as_mut() {
            let excess = lease.bytes.saturating_sub(bytes as u64);
            lease
                .usage
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .bytes -= excess;
            lease.bytes -= excess;
        }
    }
}

impl RequestResources {
    pub(super) fn new(defaults: InferenceLimits) -> Self {
        Self {
            defaults,
            ..Self::default()
        }
    }
    pub(crate) fn defaults(&self) -> InferenceLimits {
        self.defaults
    }

    pub(super) fn acquire(&self, limits: InferenceLimits) -> Result<ResourceLease, GatewayError> {
        let mut usage = self.usage.lock().unwrap_or_else(|error| error.into_inner());
        if limits.max_requests > 0 && usage.requests >= u64::from(limits.max_requests) {
            return Err(busy_error());
        }
        usage.requests += 1;
        Ok(ResourceLease {
            usage: self.usage.clone(),
            requests: 1,
            bytes: 0,
        })
    }

    pub(super) fn acquire_body(
        &self,
        limits: InferenceLimits,
        bytes: usize,
    ) -> Result<Option<ResourceLease>, GatewayError> {
        // 未配置正文限制的旧安装不为未知长度预留 usize::MAX。
        if bytes == usize::MAX && limits.max_in_flight_body_bytes == 0 {
            return Ok(None);
        }
        let mut usage = self.usage.lock().unwrap_or_else(|error| error.into_inner());
        let total = usage
            .bytes
            .checked_add(bytes as u64)
            .ok_or_else(busy_error)?;
        if limits.max_in_flight_body_bytes > 0 && total > u64::from(limits.max_in_flight_body_bytes)
        {
            return Err(busy_error());
        }
        usage.bytes = total;
        Ok(Some(ResourceLease {
            usage: self.usage.clone(),
            requests: 0,
            bytes: bytes as u64,
        }))
    }
}

fn busy_error() -> GatewayError {
    GatewayError::new(
        GatewayErrorKind::AccountCapacityUnavailable,
        "gateway request capacity or body budget is busy; retry shortly",
    )
    .with_client_code("gateway_busy")
    .with_retry_after(std::time::Duration::from_secs(1))
}

fn body_reservation(limits: InferenceLimits, headers: &HeaderMap) -> usize {
    // 压缩正文与未知长度预留单请求上限，不能用压缩长度低估解压后的占用。
    if headers
        .get(header::CONTENT_ENCODING)
        .is_some_and(|value| value != "identity")
    {
        return limits.body_limit();
    }
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_else(|| limits.body_limit())
        .min(limits.body_limit())
}

fn busy_response(error: &GatewayError) -> Response {
    let mut response = gateway_error_response(error);
    response
        .headers_mut()
        .insert("retry-after", axum::http::HeaderValue::from_static("1"));
    response
}

pub(super) async fn protect(
    State(state): State<ApiState>,
    request: Request,
    next: Next,
) -> Response {
    if request.method() != Method::POST
        || !matches!(
            request.uri().path(),
            "/v1/responses" | "/v1/images/generations" | "/v1/images/edits" | "/v1/alpha/search"
        )
    {
        return next.run(request).await;
    }
    let service = state.openai();
    let client = match authenticate_client(service, request.headers()) {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let limits = match service.inference_limits() {
        Ok(limits) => limits,
        Err(error) => return gateway_error_response(&error),
    };
    let permit = match service.resources.acquire(limits) {
        Ok(permit) => permit,
        Err(error) => return busy_response(&error),
    };
    let reservation = body_reservation(limits, request.headers());
    let body_permit = match service.resources.acquire_body(limits, reservation) {
        Ok(permit) => permit,
        Err(error) => return busy_response(&error),
    };
    // 预算启用时限制实际读取量，防止错误的长度声明绕过预留额度。
    let read_limit = if limits.max_in_flight_body_bytes > 0 {
        reservation
    } else {
        limits.body_limit()
    };
    let body_permit = BodyBudgetLease(Arc::new(Mutex::new(body_permit)));
    let (mut parts, body) = request.into_parts();
    let bytes = match to_bytes(body, read_limit).await {
        Ok(bytes) => bytes,
        Err(error) => {
            use std::error::Error;
            let too_large = error
                .source()
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>());
            return (if too_large { StatusCode::PAYLOAD_TOO_LARGE } else { StatusCode::BAD_REQUEST },
                Json(serde_json::json!({"error": {
                    "type": "invalid_request_error",
                    "code": if too_large { "request_body_too_large" } else { "request_body_read_error" },
                    "message": if too_large { "Request exceeds the configured gateway body limit." } else { "Failed to read request body." }
                }}))).into_response();
        }
    };
    // 无压缩的未知长度请求读取完毕即可归还多余预算；压缩请求由解码后归还。
    if !parts
        .headers
        .get(header::CONTENT_ENCODING)
        .is_some_and(|value| value != "identity")
    {
        body_permit.shrink_to(bytes.len());
    }
    parts.extensions.insert(body_permit.clone());
    parts.extensions.insert(AdmittedBodyLimit(
        limits
            .body_limit()
            .min(client.snapshot().responses_max_decompressed_body_bytes()),
    ));
    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    let (parts, body) = response.into_parts();
    // SSE 的 handler 返回后仍在生成；许可必须随正文消费、取消而释放。
    let body = Body::from_stream(stream::unfold(
        (body.into_data_stream(), permit, body_permit),
        |(mut stream, permit, body_permit)| async move {
            stream
                .next()
                .await
                .map(|frame| (frame, (stream, permit, body_permit)))
        },
    ));
    Response::from_parts(parts, body)
}
