//! 受控诊断传输。头部检测绝不消费 SSE 正文，也不执行传输重试。
use futures::StreamExt;
use reqwest::header::CONTENT_ENCODING;

use super::{
    CodexBackendClient, CodexRequestContext, endpoints::endpoint_url,
    protocol::responses::CodexResponsesRequest, response_meta,
};

/// 为旧网关池探测测试迁至集成测试临时公开；P2 删除旧路径时一并收回可见性。
pub struct ProbeObservation {
    pub status: u16,
    pub state: Option<String>,
    pub state_returned: bool,
    pub cookies: Vec<String>,
    pub text: String,
    pub usage: Option<serde_json::Value>,
    pub stream_failure: Option<serde_json::Value>,
    pub rate_limit_headers: Vec<(String, String)>,
    pub retry_after: Option<std::time::Duration>,
}

impl CodexBackendClient {
    /// 插件探测保留连接错误分类，头部到达后立即丢弃响应正文。
    pub(crate) async fn plugin_probe_headers(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> Result<ProbeObservation, &'static str> {
        let headers = self
            .request_headers_for_http_response(request, context)
            .map_err(|_| "protocol")?;
        let body = serde_json::to_vec(request.body()).map_err(|_| "protocol")?;
        let body =
            zstd::stream::encode_all(std::io::Cursor::new(body), 3).map_err(|_| "protocol")?;
        let response = self
            .client
            .post(endpoint_url(&self.base_url, self.protocol.responses_path()))
            .headers(headers)
            .header(CONTENT_ENCODING, "zstd")
            .body(body)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    "timeout"
                } else if error.is_connect() {
                    "connect"
                } else {
                    "protocol"
                }
            })?;
        Ok(ProbeObservation {
            status: response.status().as_u16(),
            state: response_meta::turn_state(response.headers()),
            state_returned: response.headers().contains_key("x-codex-turn-state"),
            cookies: response_meta::set_cookie_headers(response.headers()),
            text: String::new(),
            usage: None,
            stream_failure: None,
            rate_limit_headers: response_meta::rate_limit_headers(response.headers()),
            retry_after: super::client::retry_after_seconds(response.headers(), None)
                .map(std::time::Duration::from_secs),
        })
    }
    /// 取池 Cookie 使用账号选定出口，且只发送池服务自己的凭证。
    pub(crate) async fn gateway_pool_cookie(
        &self,
        base: &str,
        token: &str,
        account: &str,
        gateway: &str,
    ) -> Result<serde_json::Value, ()> {
        self.gateway_pool_get(
            base,
            token,
            "cookie",
            &[("account", account), ("gateway", gateway)],
            16384,
        )
        .await
    }
    /// 池服务当前为该成员维护的网关目录（含 Cookie 剩余秒数与地区）。
    pub(crate) async fn gateway_pool_catalog(
        &self,
        base: &str,
        token: &str,
        account: &str,
    ) -> Result<serde_json::Value, ()> {
        self.gateway_pool_get(base, token, "gateways", &[("account", account)], 262_144)
            .await
    }
    async fn gateway_pool_get(
        &self,
        base: &str,
        token: &str,
        path: &str,
        query: &[(&str, &str)],
        limit: usize,
    ) -> Result<serde_json::Value, ()> {
        let response = self
            .client
            .get(format!("{}/{path}", base.trim_end_matches('/')))
            .header(reqwest::header::USER_AGENT, "proxy-for-self")
            .bearer_auth(token)
            .query(query)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|_| ())?;
        if response.status() != 200 {
            return Err(());
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ())?;
            if bytes.len() + chunk.len() > limit {
                return Err(());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| ())
    }
    pub(crate) fn base_url_for_diagnostic(&self) -> &str {
        &self.base_url
    }
    /// 为旧网关池探测测试迁至集成测试临时公开；P2 删除旧路径时一并收回可见性。
    pub async fn diagnostic_observation(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        headers_only: bool,
    ) -> Result<ProbeObservation, ()> {
        self.diagnostic_observation_inner(request, context, headers_only, false)
            .await
    }
    /// 为旧网关池探测测试迁至集成测试临时公开；P2 删除旧路径时一并收回可见性。
    #[cfg(feature = "gateway-pool-experiment")]
    pub async fn cache_observation(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> Result<ProbeObservation, ()> {
        // 缓存实验只消费已配对且无 29 信号的响应；探测仍使用头后取消。
        self.diagnostic_observation_inner(request, context, false, true)
            .await
    }
    async fn diagnostic_observation_inner(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        headers_only: bool,
        reject_state: bool,
    ) -> Result<ProbeObservation, ()> {
        let headers = self
            .request_headers_for_http_response(request, context)
            .map_err(|_| ())?;
        let body = serde_json::to_vec(request.body()).map_err(|_| ())?;
        let mut outbound = self
            .client
            .post(endpoint_url(&self.base_url, self.protocol.responses_path()))
            .headers(headers);
        let body = if self.protocol == super::client::OpenAiUpstreamProtocol::Codex {
            outbound = outbound.header(CONTENT_ENCODING, "zstd");
            zstd::stream::encode_all(std::io::Cursor::new(body), 3).map_err(|_| ())?
        } else {
            body
        };
        let response = outbound.body(body).send().await.map_err(|_| ())?;
        let mut observation = ProbeObservation {
            status: response.status().as_u16(),
            state: response_meta::turn_state(response.headers()),
            state_returned: response.headers().contains_key("x-codex-turn-state"),
            cookies: response_meta::set_cookie_headers(response.headers()),
            text: String::new(),
            usage: None,
            stream_failure: None,
            rate_limit_headers: response_meta::rate_limit_headers(response.headers()),
            retry_after: super::client::retry_after_seconds(response.headers(), None)
                .map(std::time::Duration::from_secs),
        };
        // 包括错误响应：读取头后立即释放 Response，不读错误正文。
        if headers_only || observation.status != 200 || reject_state && observation.state_returned {
            drop(response);
            return Ok(observation);
        }
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut total = 0usize;
        let mut last_event = None;
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(_) if reject_state => {
                    observation.stream_failure = Some(
                        serde_json::json!({"kind":"stream_read_failed","lastEvent":last_event,"bytes":total}),
                    );
                    return Ok(observation);
                }
                Err(_) => return Err(()),
            };
            total += chunk.len();
            if total > 1024 * 1024 {
                return Err(());
            }
            buffer.extend_from_slice(&chunk);
            while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                let line: Vec<_> = buffer.drain(..=end).collect();
                let Some(data) = line.strip_prefix(b"data:") else {
                    continue;
                };
                let Ok(event) = serde_json::from_slice::<serde_json::Value>(data) else {
                    continue;
                };
                last_event = event["type"].as_str().map(str::to_owned);
                match event["type"].as_str() {
                    Some("response.output_text.delta") => observation
                        .text
                        .push_str(event["delta"].as_str().unwrap_or_default()),
                    Some("response.completed") => {
                        observation.usage = event["response"]["usage"]
                            .as_object()
                            .map(|v| serde_json::Value::Object(v.clone()));
                        return Ok(observation);
                    }
                    Some("response.failed" | "error") if reject_state => {
                        // 仅保留协议错误码，绝不持久化可能含凭据/路由材料的错误原文。
                        let code = event["response"]["error"]["code"]
                            .as_str()
                            .or_else(|| event["error"]["code"].as_str())
                            .or_else(|| event["code"].as_str());
                        let code = code.filter(|s| {
                            s.len() <= 64
                                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        });
                        observation.stream_failure = Some(
                            serde_json::json!({"kind":"upstream_sse_error","event":last_event,"code":code,"bytes":total}),
                        );
                        return Ok(observation);
                    }
                    Some("response.failed" | "error") => return Err(()),
                    _ => {}
                }
            }
        }
        if reject_state {
            observation.stream_failure = Some(
                serde_json::json!({"kind":"incomplete_stream","lastEvent":last_event,"bytes":total}),
            );
            Ok(observation)
        } else {
            Err(())
        }
    }
}
