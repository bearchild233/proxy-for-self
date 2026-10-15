use futures::StreamExt;
use provider_openai::transport::{
    CodexBackendClient, CodexRequestContext, protocol::responses::CodexResponsesRequest,
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn client(base: String) -> CodexBackendClient {
    use provider_openai::transport::profile::{
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
