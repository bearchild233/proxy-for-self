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
    pub cookies: Vec<String>,
    pub text: String,
}

impl CodexBackendClient {
    pub(crate) fn base_url_for_diagnostic(&self) -> &str {
        &self.base_url
    }
    pub(crate) async fn diagnostic_observation(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        headers_only: bool,
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
            cookies: response_meta::set_cookie_headers(response.headers()),
            text: String::new(),
        };
        // 包括错误响应：读取头后立即释放 Response，不读错误正文。
        if headers_only || observation.status != 200 {
            drop(response);
            return Ok(observation);
        }
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut total = 0usize;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ())?;
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
                match event["type"].as_str() {
                    Some("response.output_text.delta") => observation
                        .text
                        .push_str(event["delta"].as_str().unwrap_or_default()),
                    Some("response.completed") => return Ok(observation),
                    Some("response.failed" | "error") => return Err(()),
                    _ => {}
                }
            }
        }
        Err(())
    }
}
