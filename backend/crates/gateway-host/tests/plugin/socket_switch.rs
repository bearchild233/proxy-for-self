//! socket 平滑切换：旧连接上的在途调用正常完成，新调用落到新 socket，归属来自新连接。
//!
//! 测试端点按 0.5 协议手写在 Unix socket 上，而不是 SDK：这样可以观察旧连接何时被关闭，
//! 并让慢调用的完成时机由测试精确控制。
use gateway_core::plugin::{Ownership, PluginHooks};
use gateway_host::plugin::{ConnectionState, LogCapabilities, PluginRuntime, PluginRuntimeConfig};
use kernel_protocol::*;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{mpsc, watch},
    task::JoinHandle,
};

const WAIT: Duration = Duration::from_secs(15);

/// 端点的可观察状态。`gate` 关闭时 `slow` 管理调用会一直挂起，直到测试释放。
struct Shared {
    name: &'static str,
    plugin_id: &'static str,
    owned: Vec<String>,
    accepted: AtomicUsize,
    closed: AtomicUsize,
    received: AtomicUsize,
}

struct Endpoint {
    shared: Arc<Shared>,
    gate: watch::Sender<bool>,
    listener: JoinHandle<()>,
}

impl Endpoint {
    fn start(socket: PathBuf, name: &'static str, plugin_id: &'static str, owned: &[&str]) -> Self {
        let listener = UnixListener::bind(&socket).unwrap();
        let (gate, gate_rx) = watch::channel(false);
        let shared = Arc::new(Shared {
            name,
            plugin_id,
            owned: owned.iter().map(|group| (*group).to_owned()).collect(),
            accepted: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            received: AtomicUsize::new(0),
        });
        let task_shared = shared.clone();
        let listener = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                task_shared.accepted.fetch_add(1, Ordering::Relaxed);
                tokio::spawn(serve(stream, task_shared.clone(), gate_rx.clone()));
            }
        });
        Self {
            shared,
            gate,
            listener,
        }
    }
    fn release(&self) {
        self.gate.send_replace(true);
    }
    fn accepted(&self) -> usize {
        self.shared.accepted.load(Ordering::Relaxed)
    }
    fn closed(&self) -> usize {
        self.shared.closed.load(Ordering::Relaxed)
    }
    fn received(&self) -> usize {
        self.shared.received.load(Ordering::Relaxed)
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

async fn serve(mut stream: UnixStream, shared: Arc<Shared>, gate: watch::Receiver<bool>) {
    let Ok(Message::Request { id, method, .. }) = read_frame::<_, Message>(&mut stream).await
    else {
        return;
    };
    if method != "hello" {
        return;
    }
    let hello = json!({
        "plugin": {"id": shared.plugin_id, "version": shared.name},
        "sdk": {"version": "fixture"},
        "kernelApi": {"major": 0, "minMinor": 4},
        "hooks": ["admin.call", "health"],
        "capabilities": ["log"],
        "ownedGroups": shared.owned,
        "watchedGroups": [],
    });
    let response = Message::Response {
        id,
        payload: ResponsePayload::Result(hello),
    };
    if write_frame(&mut stream, &response).await.is_err() {
        return;
    }
    let (mut reader, mut writer) = stream.into_split();
    let (tx, mut outbound) = mpsc::unbounded_channel::<Message>();
    let writer_task = tokio::spawn(async move {
        while let Some(message) = outbound.recv().await {
            if write_frame(&mut writer, &message).await.is_err() {
                break;
            }
        }
    });
    loop {
        match read_frame::<_, Message>(&mut reader).await {
            Ok(Message::Request { id, method, params }) => {
                if method == "admin.call" && params["method"] == "crash" {
                    // 模拟进程崩溃：不回应，直接断开连接。
                    break;
                }
                if method == "admin.call" {
                    shared.received.fetch_add(1, Ordering::Relaxed);
                }
                let tx = tx.clone();
                let mut gate = gate.clone();
                let shared = shared.clone();
                tokio::spawn(async move {
                    let result = match method.as_str() {
                        "health" => json!({"ready": true}),
                        _ => {
                            if params["method"] == "slow" {
                                let _ = gate.wait_for(|open| *open).await;
                            }
                            json!({"servedBy": shared.name, "echo": params["params"]})
                        }
                    };
                    let _ = tx.send(Message::Response {
                        id,
                        payload: ResponsePayload::Result(result),
                    });
                });
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    shared.closed.fetch_add(1, Ordering::Relaxed);
    writer_task.abort();
}

fn write_registry(root: &Path, socket: &Path) {
    let temp = root.join("registry.tmp");
    fs::write(
        &temp,
        serde_json::to_vec(&json!({
            "plugins": [{"id": "switch", "enabled": true, "socket": socket, "grants": ["log"]}]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::rename(temp, root.join("registry.json")).unwrap();
}

fn start(root: &Path) -> Arc<PluginRuntime> {
    Arc::new(
        PluginRuntime::start(
            PluginRuntimeConfig {
                registry_path: Some(root.join("registry.json")),
                ownership_cache_path: Some(root.join("plugin-ownership.json")),
            },
            KernelInfo {
                version: "fixture".into(),
                slot: Some(KernelSlot::B),
            },
            Arc::new(LogCapabilities),
        )
        .unwrap()
        .unwrap(),
    )
}

async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture condition deadline");
}

async fn available(runtime: &PluginRuntime) {
    until(|| {
        runtime
            .snapshot()
            .plugins
            .iter()
            .any(|plugin| plugin.state == ConnectionState::Available)
    })
    .await;
}

async fn wait_for_error(runtime: &PluginRuntime, text: &'static str) {
    until(|| {
        runtime.snapshot().plugins[0]
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains(text))
    })
    .await;
}

fn owned(plugin: &str, available: bool) -> Ownership {
    Ownership::Owned {
        plugin: plugin.into(),
        available,
    }
}

fn admin(method: &str, params: Value) -> AdminCallParams {
    AdminCallParams {
        method: method.into(),
        params,
        actor: Actor {
            role: "admin".into(),
        },
    }
}

#[tokio::test]
async fn socket_switch_finishes_inflight_on_old_and_routes_new_calls_to_replacement() {
    let root = tempfile::tempdir().unwrap();
    let old_socket = root.path().join("old.sock");
    let old = Endpoint::start(old_socket.clone(), "old", "switch", &["group-old"]);
    write_registry(root.path(), &old_socket);
    let runtime = start(root.path());
    available(&runtime).await;

    // 旧连接上有一个慢调用在途。
    let slow = {
        let runtime = runtime.clone();
        tokio::spawn(async move {
            runtime
                .admin_call("switch", admin("slow", json!("slow-payload")))
                .await
        })
    };
    until(|| old.received() >= 1).await;

    // 新 socket 的进程已经就绪；修改注册表 socket，其他字段不变。
    let replacement_socket = root.path().join("replacement.sock");
    let replacement = Endpoint::start(
        replacement_socket.clone(),
        "replacement",
        "switch",
        &["group-new"],
    );
    replacement.release();
    write_registry(root.path(), &replacement_socket);
    until(|| runtime.ownership("group-new") == owned("switch", true)).await;

    // 归属以新连接 hello 为准；旧归属已撤销，但慢调用仍留在旧连接上。
    assert_eq!(runtime.ownership("group-old"), Ownership::NotOwned);
    assert!(!slow.is_finished());
    assert_eq!(
        old.closed(),
        0,
        "old connection must stay open for in-flight calls"
    );

    // 切换后的新调用全部发往新进程。
    let fresh = runtime
        .admin_call("switch", admin("echo", json!("fresh")))
        .await
        .unwrap();
    assert_eq!(fresh["servedBy"], "replacement");
    assert_eq!(fresh["echo"], "fresh");

    // 释放旧连接上的慢调用：返回旧进程的结果，之后旧连接关闭；新连接保持打开。
    old.release();
    let slow = slow.await.unwrap().unwrap();
    assert_eq!(slow["servedBy"], "old");
    until(|| old.closed() >= 1).await;
    assert_eq!(replacement.closed(), 0);
    assert_eq!(
        runtime.snapshot().plugins[0].state,
        ConnectionState::Available
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn failed_replacement_handshake_keeps_old_connection_serving() {
    let root = tempfile::tempdir().unwrap();
    let old_socket = root.path().join("old.sock");
    let old = Endpoint::start(old_socket.clone(), "old", "switch", &["group-old"]);
    write_registry(root.path(), &old_socket);
    let runtime = start(root.path());
    available(&runtime).await;

    // 新 socket 上的进程报告了另一个插件 ID，握手被拒绝。
    let broken_socket = root.path().join("broken.sock");
    let _broken = Endpoint::start(
        broken_socket.clone(),
        "impostor",
        "impostor",
        &["group-new"],
    );
    write_registry(root.path(), &broken_socket);
    wait_for_error(&runtime, "plugin ID mismatch").await;

    // 握手失败期间旧连接继续服务：调用正常返回，状态与归属不变。
    let reply = runtime
        .admin_call("switch", admin("echo", json!("still-old")))
        .await
        .unwrap();
    assert_eq!(reply["servedBy"], "old");
    assert_eq!(
        runtime.snapshot().plugins[0].state,
        ConnectionState::Available
    );
    assert_eq!(runtime.ownership("group-old"), owned("switch", true));
    assert_eq!(runtime.ownership("group-new"), Ownership::NotOwned);
    assert_eq!(old.accepted(), 1, "old socket must not be reconnected");
    runtime.shutdown().await;
}

#[tokio::test]
async fn superseded_connection_that_dies_is_not_reconnected_to_old_socket() {
    let root = tempfile::tempdir().unwrap();
    let old_socket = root.path().join("old.sock");
    let old = Endpoint::start(old_socket.clone(), "old", "switch", &["group-old"]);
    write_registry(root.path(), &old_socket);
    let runtime = start(root.path());
    available(&runtime).await;

    let broken_socket = root.path().join("broken.sock");
    let _broken = Endpoint::start(
        broken_socket.clone(),
        "impostor",
        "impostor",
        &["group-new"],
    );
    write_registry(root.path(), &broken_socket);
    wait_for_error(&runtime, "plugin ID mismatch").await;

    // 替换未成功时旧连接崩溃：它不能重连到已被替换的旧 socket，归属失败关闭。
    assert!(
        runtime
            .admin_call("switch", admin("crash", Value::Null))
            .await
            .is_err()
    );
    until(|| runtime.ownership("group-old") == owned("switch", false)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(old.accepted(), 1, "superseded task must not reconnect");
    assert_eq!(runtime.ownership("group-old"), owned("switch", false));
    runtime.shutdown().await;
}
