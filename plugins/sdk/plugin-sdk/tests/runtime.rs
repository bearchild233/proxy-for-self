use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use plugin_sdk::{
    Context, Descriptor, Kernel, Plugin, RpcResult, Server, Settings,
    protocol::*,
    testing::{CapabilityReply, MockKernel},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::{Notify, oneshot},
    task::JoinHandle,
};

const BUDGET: Duration = Duration::from_secs(3);
static NEXT: AtomicU64 = AtomicU64::new(1);

#[tokio::test]
async fn ownership_broadcasts_to_both_slots_and_new_handshakes_see_latest() {
    let f = Fixture::start();
    let a = f.connect(Some(KernelSlot::A)).await;
    let b = f.connect(Some(KernelSlot::B)).await;
    assert_eq!(a.owned_groups(), vec!["initial"]);
    f.kernel
        .set_owned_groups(vec!["latest".into()])
        .await
        .unwrap();
    tokio::time::timeout(BUDGET, async {
        while a.owned_groups() != vec!["latest"] || b.owned_groups() != vec!["latest"] {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let new = f.connect(None).await;
    assert_eq!(new.hello.owned_groups, vec!["latest"]);
}

#[derive(Default)]
struct Gates {
    starts: AtomicU64,
    stops: AtomicU64,
    entered: Notify,
    release: Notify,
    finished: Notify,
}
struct TestPlugin(Arc<Gates>);

impl Plugin for TestPlugin {
    fn start(&self, _: Kernel) {
        self.0.starts.fetch_add(1, Ordering::SeqCst);
    }
    fn stop_background(&self) {
        self.0.stops.fetch_add(1, Ordering::SeqCst);
    }
    async fn route_select(
        &self,
        _: Context,
        params: RouteSelectParams,
    ) -> RpcResult<RouteSelectResult> {
        Ok(RouteSelectResult {
            preferred: params.accounts,
            live: vec![],
            blocked: vec![],
            pinned_decision: params.pinned.map(|_| PinnedDecision::Stay),
            ordinary: vec![],
        })
    }
    async fn route_prepare(
        &self,
        _ctx: Context,
        _params: RoutePrepareParams,
    ) -> RpcResult<RoutePrepareResult> {
        Ok(RoutePrepareResult::Fail {
            reason: Reason {
                code: "test".into(),
                message: "test".into(),
            },
        })
    }
    async fn response_head(
        &self,
        _ctx: Context,
        _params: ResponseHeadParams,
    ) -> RpcResult<ResponseHeadResult> {
        Ok(ResponseHeadResult::Accept)
    }
    async fn request_finish(&self, _ctx: Context, _params: RequestFinishParams) -> RpcResult<()> {
        self.0.finished.notify_one();
        Ok(())
    }
    async fn admin_call(&self, ctx: Context, params: AdminCallParams) -> RpcResult<Value> {
        match params.method.as_str() {
            // 结果超过单帧上限：运行时必须改回错误响应，不能让调用方等到超时。
            "huge" => Ok(Value::String("x".repeat(MAX_FRAME_BYTES))),
            "slow" => {
                // 先登记释放等待者再通知已进入，测试释放时每个等待者都已注册，许可不会丢失。
                let released = self.0.release.notified();
                tokio::pin!(released);
                released.as_mut().enable();
                self.0.entered.notify_one();
                released.await;
                Ok(params.params)
            }
            "capability" => {
                ctx.call(
                    "quota.read",
                    &QuotaReadParams {
                        refresh_if_missing: false,
                        account_id: "test".into(),
                    },
                    BUDGET,
                )
                .await
            }
            "timeout" => {
                ctx.call(
                    "quota.read",
                    &QuotaReadParams {
                        refresh_if_missing: false,
                        account_id: "test".into(),
                    },
                    Duration::from_millis(20),
                )
                .await
            }
            "log" => {
                ctx.log(
                    &LogParams {
                        level: "info".into(),
                        message: "test".into(),
                        fields: json!({}),
                    },
                    BUDGET,
                )
                .await?;
                Ok(Value::Null)
            }
            _ => Ok(params.params),
        }
    }
}

struct Fixture {
    directory: PathBuf,
    socket: PathBuf,
    task: JoinHandle<std::io::Result<()>>,
    stop: Option<oneshot::Sender<()>>,
    gates: Arc<Gates>,
    kernel: Kernel,
}

impl Fixture {
    fn start() -> Self {
        Self::with_budget(BUDGET)
    }
    fn with_budget(shutdown_timeout: Duration) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "cpr-sdk-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let socket = directory.join("test.sock");
        let gates = Arc::new(Gates::default());
        let descriptor = Descriptor {
            plugin: PluginInfo {
                id: "test".into(),
                version: "0.0.0".into(),
            },
            kernel_api: PluginApi {
                major: 0,
                min_minor: 4,
            },
            hooks: vec!["admin.call".into()],
            capabilities: vec!["quota.read".into(), "log".into()],
            owned_groups: vec!["initial".into()],
            watched_groups: vec![],
            shutdown_timeout,
        };
        let server = Server::bind(
            Settings {
                socket: socket.clone(),
                data: directory.join("data"),
            },
            descriptor,
            TestPlugin(gates.clone()),
        )
        .unwrap();
        let kernel = server.kernel();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(server.run_until(async {
            let _ = stopped.await;
        }));
        Self {
            directory,
            socket,
            task,
            stop: Some(stop),
            gates,
            kernel,
        }
    }

    async fn connect(&self, slot: Option<KernelSlot>) -> Arc<MockKernel> {
        self.connect_as(slot, ConnectionRole::Kernel).await
    }

    async fn connect_as(&self, slot: Option<KernelSlot>, role: ConnectionRole) -> Arc<MockKernel> {
        Arc::new(
            MockKernel::connect(&self.socket, hello_as(slot, role), BUDGET)
                .await
                .unwrap(),
        )
    }

    async fn raw(&self) -> UnixStream {
        let mut stream = UnixStream::connect(&self.socket).await.unwrap();
        write_frame(
            &mut stream,
            &Message::Request {
                id: 1,
                method: "hello".into(),
                params: serde_json::to_value(hello(None)).unwrap(),
            },
        )
        .await
        .unwrap();
        let _: Message = read_frame(&mut stream).await.unwrap();
        stream
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn hello(slot: Option<KernelSlot>) -> HelloParams {
    hello_as(slot, ConnectionRole::Kernel)
}
fn hello_as(slot: Option<KernelSlot>, role: ConnectionRole) -> HelloParams {
    HelloParams {
        kernel_api: KernelApi::default(),
        kernel: KernelInfo {
            version: "test".into(),
            slot,
        },
        role,
    }
}
fn admin(method: &str, value: Value) -> AdminCallParams {
    AdminCallParams {
        method: method.into(),
        params: value,
        actor: Actor {
            role: "admin".into(),
        },
    }
}
fn route() -> RoutePrepareParams {
    RoutePrepareParams {
        request_id: "r".into(),
        attempt: 1,
        model: "m".into(),
        deadline_unix_ms: 1,
        replay_safe: true,
        account_id: "a".into(),
    }
}
fn head() -> ResponseHeadParams {
    ResponseHeadParams {
        request_id: "r".into(),
        lease_id: "l".into(),
        attempt: 1,
        status: Some(200),
        error_kind: None,
        state_returned: false,
        turn_state: None,
        set_cookies: vec![],
        duration_ms: 1,
        replay_safe: true,
    }
}
async fn closed(stream: &mut UnixStream) {
    let mut byte = [0];
    let result = tokio::time::timeout(BUDGET, stream.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
}

#[tokio::test]
async fn handshake_permissions_and_two_slots() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::start();
    assert_eq!(
        std::fs::metadata(&f.socket).unwrap().permissions().mode() & 0o777,
        0o660
    );
    assert_eq!(
        std::fs::metadata(f.directory.join("data"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let a = f.connect(Some(KernelSlot::A)).await;
    let b = f.connect(Some(KernelSlot::B)).await;
    assert_eq!(a.hello.plugin.id, "test");
    assert_eq!(a.hello.kernel_api.min_minor, 4);
    let empty = EmptyParams {};
    let (a_result, b_result) = tokio::join!(
        a.call::<_, HealthResult>("health", &empty, BUDGET),
        b.call::<_, HealthResult>("health", &empty, BUDGET)
    );
    assert!(a_result.unwrap().ready && b_result.unwrap().ready);
}

#[tokio::test]
async fn incompatible_and_pre_handshake_messages_disconnect() {
    let f = Fixture::start();
    for api in [
        KernelApi { major: 1, minor: 1 },
        KernelApi { major: 0, minor: 0 },
    ] {
        let mut params = hello(None);
        params.kernel_api = api;
        assert!(
            MockKernel::connect(&f.socket, params, BUDGET)
                .await
                .is_err()
        );
    }
    for message in [
        Message::Request {
            id: 1,
            method: "health".into(),
            params: json!({}),
        },
        Message::Notify {
            method: "shutdown".into(),
            params: json!({}),
        },
        Message::Response {
            id: 1,
            payload: ResponsePayload::Result(json!({})),
        },
    ] {
        let mut stream = UnixStream::connect(&f.socket).await.unwrap();
        write_frame(&mut stream, &message).await.unwrap();
        closed(&mut stream).await;
    }
    assert!(!f.task.is_finished());
}

#[tokio::test]
async fn concurrent_hooks_do_not_block_and_capabilities_use_origin_connection() {
    let f = Fixture::start();
    let a = f.connect(Some(KernelSlot::A)).await;
    let b = f.connect(Some(KernelSlot::B)).await;
    let slow_kernel = a.clone();
    let slow = tokio::spawn(async move {
        slow_kernel
            .call::<_, Value>("admin.call", &admin("slow", json!("slow")), BUDGET)
            .await
    });
    tokio::time::timeout(BUDGET, f.gates.entered.notified())
        .await
        .unwrap();
    assert!(
        a.call::<_, HealthResult>("health", &EmptyParams {}, BUDGET)
            .await
            .unwrap()
            .ready
    );
    for (kernel, value) in [(&a, "a"), (&b, "b")] {
        kernel.push_reply(CapabilityReply {
            method: "quota.read".into(),
            params: json!({"accountId":"test","refreshIfMissing":false}),
            result: Ok(json!({"snapshot":value})),
        });
    }
    let call = admin("capability", json!({}));
    let (ra, rb) = tokio::join!(
        a.call::<_, Value>("admin.call", &call, BUDGET),
        b.call::<_, Value>("admin.call", &call, BUDGET)
    );
    assert_eq!(ra.unwrap(), json!({"snapshot":"a"}));
    assert_eq!(rb.unwrap(), json!({"snapshot":"b"}));
    assert!(!slow.is_finished());
    f.gates.release.notify_one();
    assert_eq!(slow.await.unwrap().unwrap(), json!("slow"));
}

#[tokio::test]
async fn background_capability_and_log_notification() {
    let f = Fixture::start();
    assert_eq!(
        f.kernel
            .call::<_, Value>("quota.read", &json!({}), BUDGET)
            .await
            .unwrap_err()
            .code,
        UNAVAILABLE
    );
    let a = f.connect(None).await;
    a.push_reply(CapabilityReply {
        method: "accounts.read".into(),
        params: json!({"ids":null,"model":null}),
        result: Ok(json!({"accounts":[]})),
    });
    let result: AccountsReadResult = f
        .kernel
        .call(
            "accounts.read",
            &AccountsReadParams {
                ids: None,
                model: None,
            },
            BUDGET,
        )
        .await
        .unwrap();
    assert!(result.accounts.is_empty());
    a.call::<_, Value>("admin.call", &admin("log", Value::Null), BUDGET)
        .await
        .unwrap();
    assert_eq!(
        a.notifications(),
        vec![(
            "log".into(),
            json!({"level":"info","message":"test","fields":{}})
        )]
    );
    assert_eq!(a.remaining_replies(), 0);
}

#[tokio::test]
async fn drain_is_global_preserves_existing_head_and_finish() {
    let f = Fixture::start();
    let a = f.connect(Some(KernelSlot::A)).await;
    let b = f.connect(Some(KernelSlot::B)).await;
    let k = a.clone();
    let slow = tokio::spawn(async move {
        k.call::<_, Value>("admin.call", &admin("slow", Value::Null), BUDGET)
            .await
    });
    tokio::time::timeout(BUDGET, f.gates.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        b.call::<_, DrainResult>("drain", &EmptyParams {}, BUDGET)
            .await
            .unwrap()
            .in_flight,
        1
    );
    assert_eq!(f.gates.starts.load(Ordering::SeqCst), 1);
    assert_eq!(f.gates.stops.load(Ordering::SeqCst), 1);
    for kernel in [&a, &b] {
        let result: RoutePrepareResult = kernel
            .call("route.prepare", &route(), BUDGET)
            .await
            .unwrap();
        assert!(
            matches!(result, RoutePrepareResult::Fail { reason } if reason.code == "plugin_draining")
        );
        assert_eq!(
            kernel
                .call::<_, ResponseHeadResult>("response.head", &head(), BUDGET)
                .await
                .unwrap(),
            ResponseHeadResult::Accept
        );
    }
    a.notify(
        "request.finish",
        &RequestFinishParams {
            request_id: "r".into(),
            lease_id: "l".into(),
            outcome: RequestOutcome::Completed,
            duration_ms: 1,
            first_byte_ms: None,
            usage: None,
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(BUDGET, f.gates.finished.notified())
        .await
        .unwrap();
    f.gates.release.notify_one();
    slow.await.unwrap().unwrap();
}

#[tokio::test]
async fn shutdown_waits_for_inflight_and_flushes_response() {
    let mut f = Fixture::start();
    let a = f.connect(None).await;
    let k = a.clone();
    let slow = tokio::spawn(async move {
        k.call::<_, Value>("admin.call", &admin("slow", json!(7)), BUDGET)
            .await
    });
    tokio::time::timeout(BUDGET, f.gates.entered.notified())
        .await
        .unwrap();
    a.notify("shutdown", &EmptyParams {}).await.unwrap();
    tokio::task::yield_now().await;
    assert!(!f.task.is_finished());
    assert!(!slow.is_finished());
    f.gates.release.notify_one();
    assert_eq!(slow.await.unwrap().unwrap(), json!(7));
    tokio::time::timeout(BUDGET, &mut f.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!f.socket.exists());
    assert!(f.gates.stops.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn external_shutdown_and_deadline_cancel_stuck_calls() {
    let mut f = Fixture::with_budget(Duration::from_millis(20));
    let a = f.connect(None).await;
    let k = a.clone();
    let slow = tokio::spawn(async move {
        k.call::<_, Value>("admin.call", &admin("slow", Value::Null), BUDGET)
            .await
    });
    tokio::time::timeout(BUDGET, f.gates.entered.notified())
        .await
        .unwrap();
    f.stop.take().unwrap().send(()).unwrap();
    let error = tokio::time::timeout(BUDGET, &mut f.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(slow.await.unwrap().unwrap_err().code, UNAVAILABLE);
}

#[tokio::test]
async fn capability_timeout_and_late_response_leave_connection_usable() {
    let f = Fixture::start();
    let mut stream = f.raw().await;
    write_frame(
        &mut stream,
        &Message::Request {
            id: 2,
            method: "admin.call".into(),
            params: serde_json::to_value(admin("timeout", Value::Null)).unwrap(),
        },
    )
    .await
    .unwrap();
    let capability: Message = tokio::time::timeout(BUDGET, read_frame(&mut stream))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(capability, Message::Request { id: 1, .. }));
    let response: Message = tokio::time::timeout(BUDGET, read_frame(&mut stream))
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(response, Message::Response { id:2, payload:ResponsePayload::Error(error) } if error.code == TIMEOUT)
    );
    write_frame(
        &mut stream,
        &Message::Response {
            id: 1,
            payload: ResponsePayload::Result(json!({})),
        },
    )
    .await
    .unwrap();
    write_frame(
        &mut stream,
        &Message::Request {
            id: 3,
            method: "health".into(),
            params: json!({}),
        },
    )
    .await
    .unwrap();
    let response: Message = tokio::time::timeout(BUDGET, read_frame(&mut stream))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        response,
        Message::Response {
            id: 3,
            payload: ResponsePayload::Result(_)
        }
    ));
}

#[tokio::test]
async fn oversize_disconnects_before_and_after_handshake() {
    let f = Fixture::start();
    for mut stream in [UnixStream::connect(&f.socket).await.unwrap(), f.raw().await] {
        stream
            .write_all(&((MAX_FRAME_BYTES + 1) as u32).to_be_bytes())
            .await
            .unwrap();
        closed(&mut stream).await;
    }
    assert!(
        f.connect(None)
            .await
            .call::<_, HealthResult>("health", &EmptyParams {}, BUDGET)
            .await
            .unwrap()
            .ready
    );
}

#[tokio::test]
async fn fragmented_frame_survives_hook_completion() {
    let f = Fixture::start();
    let mut stream = f.raw().await;
    write_frame(
        &mut stream,
        &Message::Request {
            id: 2,
            method: "admin.call".into(),
            params: serde_json::to_value(admin("slow", Value::Null)).unwrap(),
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(BUDGET, f.gates.entered.notified())
        .await
        .unwrap();
    let frame = encode_frame(&Message::Request {
        id: 3,
        method: "health".into(),
        params: json!({}),
    })
    .unwrap();
    stream.write_all(&frame[..6]).await.unwrap();
    f.gates.release.notify_one();
    let _: Message = tokio::time::timeout(BUDGET, read_frame(&mut stream))
        .await
        .unwrap()
        .unwrap();
    stream.write_all(&frame[6..]).await.unwrap();
    let response: Message = tokio::time::timeout(BUDGET, read_frame(&mut stream))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(response, Message::Response { id: 3, .. }));
}

#[tokio::test]
async fn unknown_method_and_invalid_params_errors() {
    let f = Fixture::start();
    let a = f.connect(None).await;
    assert_eq!(
        a.call::<_, Value>("missing", &json!({}), BUDGET)
            .await
            .unwrap_err()
            .code,
        NOT_SUPPORTED
    );
    assert_eq!(
        a.call::<_, Value>("response.head", &json!({}), BUDGET)
            .await
            .unwrap_err()
            .code,
        INVALID_PARAMS
    );
}

#[tokio::test]
async fn disconnect_releases_queued_response_writers() {
    let mut f = Fixture::start();
    let mut stream = f.raw().await;
    for id in 2..10 {
        write_frame(
            &mut stream,
            &Message::Request {
                id,
                method: "admin.call".into(),
                params: serde_json::to_value(admin("echo", json!("x".repeat(MAX_FRAME_BYTES / 2))))
                    .unwrap(),
            },
        )
        .await
        .unwrap();
    }
    drop(stream);
    let a = f.connect(None).await;
    a.notify("shutdown", &EmptyParams {}).await.unwrap();
    tokio::time::timeout(BUDGET, &mut f.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn shutdown_keeps_capability_response_channel_for_existing_hook() {
    let mut f = Fixture::start();
    let mut stream = f.raw().await;
    write_frame(
        &mut stream,
        &Message::Request {
            id: 2,
            method: "admin.call".into(),
            params: serde_json::to_value(admin("capability", Value::Null)).unwrap(),
        },
    )
    .await
    .unwrap();
    let capability: Message = tokio::time::timeout(BUDGET, read_frame(&mut stream))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(capability, Message::Request { id: 1, .. }));
    write_frame(
        &mut stream,
        &Message::Notify {
            method: "shutdown".into(),
            params: json!({}),
        },
    )
    .await
    .unwrap();
    // 关停后的新调用不能越过准入边界；已有调用仍能收到能力响应。
    write_frame(
        &mut stream,
        &Message::Request {
            id: 3,
            method: "health".into(),
            params: json!({}),
        },
    )
    .await
    .unwrap();
    write_frame(
        &mut stream,
        &Message::Response {
            id: 1,
            payload: ResponsePayload::Result(json!({"snapshot":7})),
        },
    )
    .await
    .unwrap();
    let response: Message = tokio::time::timeout(BUDGET, read_frame(&mut stream))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        response,
        Message::Response {
            id: 2,
            payload: ResponsePayload::Result(json!({"snapshot":7}))
        }
    );
    closed(&mut stream).await;
    tokio::time::timeout(BUDGET, &mut f.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn watched_groups_broadcast_and_observe_accepts_by_default() {
    let f = Fixture::start();
    let a = f.connect(Some(KernelSlot::A)).await;
    let b = f.connect(Some(KernelSlot::B)).await;
    let selected: RouteSelectResult = a
        .call(
            "route.select",
            &RouteSelectParams {
                request_id: "r".into(),
                model: "m".into(),
                accounts: vec!["a".into()],
                ordinary_available: false,
                pinned: Some("a".into()),
            },
            BUDGET,
        )
        .await
        .unwrap();
    assert_eq!(selected.preferred, vec!["a"]);
    assert_eq!(selected.pinned_decision, Some(PinnedDecision::Stay));
    f.kernel
        .set_ownership(vec!["own".into()], vec!["watch".into()])
        .await
        .unwrap();
    tokio::time::timeout(BUDGET, async {
        while a.watched_groups() != vec!["watch"] || b.watched_groups() != vec!["watch"] {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.kernel.set_owned_groups(vec!["new".into()]).await.unwrap();
    let new = f.connect(None).await;
    assert_eq!(new.hello.owned_groups, vec!["new"]);
    assert_eq!(new.hello.watched_groups, vec!["watch"]);
    let result: ResponseObserveResult = a
        .call(
            "response.observe",
            &ResponseObserveParams {
                sse: true,
                request_id: "r".into(),
                account_id: "a".into(),
                attempt: 1,
                model: "m".into(),
                request_had_turn_state: false,
                can_switch: true,
                status: Some(200),
                error_kind: None,
                state_returned: false,
                turn_state: None,
                set_cookies: vec![],
                replay_safe: true,
            },
            BUDGET,
        )
        .await
        .unwrap();
    assert_eq!(result, ResponseObserveResult::Accept);
}

#[tokio::test]
async fn unknown_connection_role_disconnects_before_registration() {
    let f = Fixture::start();
    let mut stream = UnixStream::connect(&f.socket).await.unwrap();
    let mut params = serde_json::to_value(hello(None)).unwrap();
    params["role"] = json!("root");
    write_frame(
        &mut stream,
        &Message::Request {
            id: 1,
            method: "hello".into(),
            params,
        },
    )
    .await
    .unwrap();
    // 未知角色不能退化为内核连接，握手必须失败并断开。
    closed(&mut stream).await;
    assert!(!f.task.is_finished());
}

#[tokio::test]
async fn admin_connection_allows_only_health_and_admin_call() {
    let f = Fixture::start();
    let kernel = f.connect(Some(KernelSlot::A)).await;
    let admin_conn = f.connect_as(None, ConnectionRole::Admin).await;
    let error = admin_conn
        .call::<_, Value>("admin.call", &admin("huge", json!(null)), BUDGET)
        .await
        .unwrap_err();
    assert_eq!(error.code, INTERNAL);
    let empty = EmptyParams {};
    assert!(
        admin_conn
            .call::<_, HealthResult>("health", &empty, BUDGET)
            .await
            .unwrap()
            .ready
    );
    assert_eq!(
        admin_conn
            .call::<_, Value>("admin.call", &admin("echo", json!("ok")), BUDGET)
            .await
            .unwrap(),
        json!("ok")
    );
    // 其他钩子与 drain 在管理连接上返回 forbidden，且不会改变排空状态。
    let error = admin_conn
        .call::<_, RoutePrepareResult>("route.prepare", &route(), BUDGET)
        .await
        .unwrap_err();
    assert_eq!(error.code, FORBIDDEN);
    let error = admin_conn
        .call::<_, DrainResult>("drain", &empty, BUDGET)
        .await
        .unwrap_err();
    assert_eq!(error.code, FORBIDDEN);
    assert_eq!(
        kernel
            .call::<_, DrainResult>("drain", &empty, BUDGET)
            .await
            .unwrap()
            .in_flight,
        0
    );
    // 管理连接上的通知类钩子不被处理：shutdown 不停止插件，request.finish 不进入插件。
    admin_conn
        .notify(
            "request.finish",
            &RequestFinishParams {
                request_id: "r".into(),
                lease_id: "l".into(),
                outcome: RequestOutcome::Completed,
                duration_ms: 1,
                first_byte_ms: None,
                usage: None,
            },
        )
        .await
        .unwrap();
    admin_conn
        .notify("shutdown", &EmptyParams {})
        .await
        .unwrap();
    // 同一连接上的请求按序处理，echo 返回即说明前面的通知已读取完毕。
    assert_eq!(
        admin_conn
            .call::<_, Value>("admin.call", &admin("echo", json!("after")), BUDGET)
            .await
            .unwrap(),
        json!("after")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), f.gates.finished.notified())
            .await
            .is_err()
    );
    assert!(!f.task.is_finished());
}

#[tokio::test]
async fn admin_connection_is_excluded_from_ownership_fanout() {
    let f = Fixture::start();
    let kernel = f.connect(Some(KernelSlot::A)).await;
    let admin_conn = f.connect_as(None, ConnectionRole::Admin).await;
    f.kernel
        .set_owned_groups(vec!["latest".into()])
        .await
        .unwrap();
    tokio::time::timeout(BUDGET, async {
        while kernel.owned_groups() != vec!["latest"] {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // 归属变化只发给内核连接；管理连接保持握手时的归属，且没有收到通知。
    assert!(
        admin_conn
            .notifications()
            .iter()
            .all(|(method, _)| method != "ownership.changed")
    );
    assert_eq!(admin_conn.hello.owned_groups, vec!["initial"]);
}

#[tokio::test]
async fn admin_capability_calls_use_kernel_connection_or_fail_unavailable() {
    let f = Fixture::start();
    let admin_conn = f.connect_as(None, ConnectionRole::Admin).await;
    let call = admin("capability", Value::Null);
    // 没有内核连接时能力调用返回 unavailable，不能落到管理连接上。
    assert_eq!(
        admin_conn
            .call::<_, Value>("admin.call", &call, BUDGET)
            .await
            .unwrap_err()
            .code,
        UNAVAILABLE
    );
    let kernel = f.connect(Some(KernelSlot::B)).await;
    kernel.push_reply(CapabilityReply {
        method: "quota.read".into(),
        params: json!({"accountId":"test","refreshIfMissing":false}),
        result: Ok(json!({"snapshot":"kernel"})),
    });
    assert_eq!(
        admin_conn
            .call::<_, Value>("admin.call", &call, BUDGET)
            .await
            .unwrap(),
        json!({"snapshot":"kernel"})
    );
    assert_eq!(kernel.remaining_replies(), 0);
}

#[tokio::test]
async fn drain_counts_only_kernel_calls_and_admin_calls_keep_running() {
    let f = Fixture::start();
    let kernel = f.connect(Some(KernelSlot::A)).await;
    let admin_conn = f.connect_as(None, ConnectionRole::Admin).await;
    let k = kernel.clone();
    let kernel_slow = tokio::spawn(async move {
        k.call::<_, Value>("admin.call", &admin("slow", Value::Null), BUDGET)
            .await
    });
    tokio::time::timeout(BUDGET, f.gates.entered.notified())
        .await
        .unwrap();
    let a = admin_conn.clone();
    let admin_slow = tokio::spawn(async move {
        a.call::<_, Value>("admin.call", &admin("slow", Value::Null), BUDGET)
            .await
    });
    tokio::time::timeout(BUDGET, f.gates.entered.notified())
        .await
        .unwrap();
    let other = f.connect(Some(KernelSlot::B)).await;
    // 两个慢调用都在途，但只有内核连接的那个计入 drain 返回的在途数（本次 drain 自身不计）。
    assert_eq!(
        other
            .call::<_, DrainResult>("drain", &EmptyParams {}, BUDGET)
            .await
            .unwrap()
            .in_flight,
        1
    );
    // 排空中管理连接的 admin.call 照常处理。
    assert_eq!(
        admin_conn
            .call::<_, Value>("admin.call", &admin("echo", json!("during-drain")), BUDGET)
            .await
            .unwrap(),
        json!("during-drain")
    );
    f.gates.release.notify_one();
    f.gates.release.notify_one();
    assert_eq!(kernel_slow.await.unwrap().unwrap(), Value::Null);
    assert_eq!(admin_slow.await.unwrap().unwrap(), Value::Null);
}
