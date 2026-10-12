//! 受控诊断传输。头部检测绝不消费 SSE 正文，也不执行传输重试。
use futures::StreamExt;
use reqwest::header::CONTENT_ENCODING;

use super::{
    CodexBackendClient, CodexRequestContext, endpoints::endpoint_url,
    protocol::responses::CodexResponsesRequest, response_meta,
};

pub(crate) struct ProbeObservation {
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
    pub(crate) async fn diagnostic_observation(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        headers_only: bool,
    ) -> Result<ProbeObservation, ()> {
        self.diagnostic_observation_inner(request, context, headers_only, false)
            .await
    }
    #[cfg(feature = "gateway-pool-experiment")]
    pub(crate) async fn cache_observation(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    fn client(base: String) -> CodexBackendClient {
        use crate::transport::profile::{
            CodexWireProfile, CodexWireProfileState, selection::ClientKind,
        };
        CodexBackendClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            base,
            CodexWireProfileState::new(CodexWireProfile {
                client_kind: ClientKind::Cli,
                originator: "codex_cli_rs".into(),
                codex_version: "1.2.3".into(),
                desktop_version: "1.2.3".into(),
                desktop_build: "123".into(),
                os_type: "linux".into(),
                os_version: "6.8".into(),
                arch: "x86_64".into(),
                terminal: "test".into(),
                residency: None,
                verified_at: chrono::Utc::now(),
            }),
        )
    }
    async fn server(body: Option<&'static str>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let size = stream.read(&mut buffer).await.unwrap();
                assert!(size > 0);
                data.extend_from_slice(&buffer[..size]);
                if let Some(end) = data.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&data[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|s| s.parse::<usize>().ok())
                        })
                        .unwrap();
                    if data.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            if let Some(body) = body {
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
                stream.write_all(body.as_bytes()).await.unwrap();
            } else {
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nx-codex-turn-state: fixed-state\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
                // 正文永不返回，探测仍必须立即结束且关闭 TCP，而非后台偷偷读取。
                let mut one = [0];
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(1), stream.read(&mut one))
                        .await
                        .unwrap()
                        .unwrap(),
                    0
                );
            }
        });
        (address, task)
    }
    fn request() -> CodexResponsesRequest {
        let mut request = CodexResponsesRequest::from_body(
            serde_json::json!({"model":"gpt-6-astra","input":[],"stream":true,"store":true})
                .as_object()
                .unwrap()
                .clone(),
        );
        request.force_http_sse = true;
        request
    }
    #[tokio::test]
    async fn header_probe_closes_hanging_sse_without_consuming_body() {
        let (base, task) = server(None).await;
        let observation = tokio::time::timeout(
            Duration::from_millis(500),
            client(base).diagnostic_observation(
                &request(),
                CodexRequestContext::auxiliary("Bearer demo", Some("demo"), "probe", None),
                true,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(observation.status, 200);
        assert_eq!(observation.state.as_deref(), Some("fixed-state"));
        assert!(observation.text.is_empty());
        task.await.unwrap();
    }
    #[tokio::test]
    async fn ordinary_customer_stream_preserves_entire_sse_body() {
        const BODY: &str = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"complete answer\"}\n\ndata: {\"type\":\"response.completed\"}\n\n";
        let (base, task) = server(Some(BODY)).await;
        let response = client(base)
            .create_response_stream_with_pool_account(
                &request(),
                CodexRequestContext::auxiliary("Bearer demo", Some("demo"), "business", None),
                Some("demo"),
            )
            .await
            .unwrap();
        let bytes = response
            .body
            .map(|part| part.unwrap())
            .collect::<Vec<_>>()
            .await
            .concat();
        assert_eq!(bytes, BODY.as_bytes());
        task.await.unwrap();
    }
    #[tokio::test]
    async fn full_diagnostic_preserves_completed_cache_usage() {
        const BODY: &str = "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":4096,\"input_tokens_details\":{\"cached_tokens\":3072},\"output_tokens\":1}}}\n\n";
        let (base, task) = server(Some(BODY)).await;
        let result = client(base)
            .diagnostic_observation(
                &request(),
                CodexRequestContext::auxiliary("Bearer demo", Some("demo"), "cache", None),
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            result.usage.unwrap()["input_tokens_details"]["cached_tokens"],
            3072
        );
        task.await.unwrap();
    }
    #[cfg(feature = "gateway-pool-experiment")]
    #[tokio::test]
    async fn cache_diagnostic_retains_error_code_without_error_message() {
        const BODY: &str = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"test_error\",\"message\":\"secret material never retained\"}}}\n\n";
        let (base, task) = server(Some(BODY)).await;
        let result = client(base)
            .cache_observation(
                &request(),
                CodexRequestContext::auxiliary("Bearer demo", Some("demo"), "cache", None),
            )
            .await
            .unwrap();
        assert_eq!(result.status, 200);
        assert!(result.usage.is_none());
        let failure = result.stream_failure.unwrap();
        assert_eq!(failure["code"], "test_error");
        assert!(!failure.to_string().contains("secret material"));
        task.await.unwrap();
    }
}
