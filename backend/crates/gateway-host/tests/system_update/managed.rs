//! 通过真实私有 socket 验证独立重启委托，网关不能自行退出或拉起替代进程。
use super::*;

#[cfg(unix)]
#[tokio::test]
async fn managed_restart_delegates_without_cancelling_gateway() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let fixture = Fixture::new();
    let socket = fixture.root.path().join("updater.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = vec![0; 4096];
        let mut used = 0;
        loop {
            used += stream.read(&mut bytes[used..]).await.unwrap();
            if bytes[..used].ends_with(b"}") {
                break;
            }
        }
        let request = String::from_utf8(bytes[..used].to_vec()).unwrap();
        assert!(request.starts_with("POST /operation"));
        assert!(request.contains("\"action\":\"restart\""));
        let body = "{\"operation_id\":\"managed-test\"}";
        stream.write_all(format!("HTTP/1.1 202 Accepted\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.managed_socket = Some(socket);
    config.self_restart_enabled = true;
    let cancellation = CancellationToken::new();
    let service = ProcessSystemOperations::new(cancellation.clone(), config);
    let accepted = service.restart().await.unwrap();
    assert!(
        matches!(accepted,SystemOperationAccepted::Restart { operation_id, .. } if operation_id=="managed-test")
    );
    server.await.unwrap();
    assert!(!cancellation.is_cancelled());
}

#[cfg(unix)]
#[tokio::test]
async fn unavailable_managed_executor_never_falls_back_to_self_restart() {
    let fixture = Fixture::new();
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.managed_socket = Some(fixture.root.path().join("missing.sock"));
    config.self_restart_enabled = true;
    let cancellation = CancellationToken::new();
    let service = ProcessSystemOperations::new(cancellation.clone(), config);
    assert!(service.restart().await.is_err());
    assert!(!cancellation.is_cancelled());
}
