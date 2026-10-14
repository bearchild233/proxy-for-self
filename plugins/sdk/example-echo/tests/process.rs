use std::{path::PathBuf, time::Duration};

use plugin_sdk::{protocol::*, testing::MockKernel};
use serde_json::{Value, json};
use tokio::process::Command;

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn environment_echo_and_sigterm_process_exit() {
    let budget = Duration::from_secs(3);
    let directory =
        Directory(std::env::temp_dir().join(format!("cpr-echo-process-{}", std::process::id())));
    std::fs::create_dir(&directory.0).unwrap();
    let socket = directory.0.join("echo.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_example-echo"))
        .env("CPR_PLUGIN_SOCKET", &socket)
        .env("CPR_PLUGIN_DATA", directory.0.join("data"))
        .env("CPR_PLUGIN_SHUTDOWN_MS", "3000")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(budget, async {
        while !socket.exists() {
            assert!(child.try_wait().unwrap().is_none());
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let kernel = MockKernel::connect(
        &socket,
        HelloParams {
            kernel_api: KernelApi::default(),
            kernel: KernelInfo {
                version: "test".into(),
                slot: None,
            },
        },
        budget,
    )
    .await
    .unwrap();
    assert_eq!(kernel.hello.capabilities, vec!["log"]);
    let value: Value = kernel
        .call(
            "admin.call",
            &AdminCallParams {
                method: "echo".into(),
                params: json!({"nested":[1,null,"test"]}),
                actor: Actor {
                    role: "admin".into(),
                },
            },
            budget,
        )
        .await
        .unwrap();
    assert_eq!(value, json!({"nested":[1,null,"test"]}));
    let route: RoutePrepareResult = kernel
        .call(
            "route.prepare",
            &RoutePrepareParams {
                request_id: "r".into(),
                attempt: 1,
                model: "m".into(),
                deadline_unix_ms: 1,
                replay_safe: true,
                accounts: vec![],
                ordinary_available: true,
            },
            budget,
        )
        .await
        .unwrap();
    assert!(matches!(route, RoutePrepareResult::Fallback { .. }));
    let status = Command::new("kill")
        .args(["-TERM", &child.id().unwrap().to_string()])
        .status()
        .await
        .unwrap();
    assert!(status.success());
    assert!(
        tokio::time::timeout(budget, child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!socket.exists());
}
