use gateway_core::plugin::{Ownership, PluginCapabilities, PluginHooks};
use gateway_host::plugin::{
    ConnectionState, LogCapabilities, PluginRegistry, PluginRuntime, PluginRuntimeConfig,
};
use plugin_sdk::{Context, Descriptor, Kernel, Plugin, RpcResult, Server, Settings, protocol::*};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Notify, oneshot},
    task::JoinHandle,
};

const TEST_WAIT: Duration = Duration::from_secs(15);

#[derive(Default)]
struct Controls {
    slow: AtomicBool,
    health_fail: AtomicBool,
    health_hang: AtomicBool,
    unready: AtomicBool,
    health_calls: AtomicUsize,
    route_calls: AtomicUsize,
    head_calls: AtomicUsize,
    admin_calls: AtomicUsize,
    finish: Notify,
}
struct TestPlugin(Arc<Controls>);
impl Plugin for TestPlugin {
    async fn response_observe(
        &self,
        _: Context,
        _: ResponseObserveParams,
    ) -> RpcResult<ResponseObserveResult> {
        if self.0.slow.load(Ordering::Relaxed) {
            std::future::pending::<()>().await;
        }
        Ok(ResponseObserveResult::Accept)
    }

    async fn route_prepare(
        &self,
        _: Context,
        _: RoutePrepareParams,
    ) -> RpcResult<RoutePrepareResult> {
        self.0.route_calls.fetch_add(1, Ordering::Relaxed);
        if self.0.slow.load(Ordering::Relaxed) {
            std::future::pending::<()>().await;
        }
        Ok(RoutePrepareResult::Fail {
            reason: Reason {
                code: NOT_SUPPORTED.into(),
                message: "fixture".into(),
            },
        })
    }
    async fn response_head(
        &self,
        _: Context,
        _: ResponseHeadParams,
    ) -> RpcResult<ResponseHeadResult> {
        self.0.head_calls.fetch_add(1, Ordering::Relaxed);
        if self.0.slow.load(Ordering::Relaxed) {
            std::future::pending::<()>().await;
        }
        Ok(ResponseHeadResult::Accept)
    }
    async fn request_finish(&self, _: Context, _: RequestFinishParams) -> RpcResult<()> {
        self.0.finish.notify_one();
        Ok(())
    }
    async fn admin_call(&self, ctx: Context, params: AdminCallParams) -> RpcResult<Value> {
        self.0.admin_calls.fetch_add(1, Ordering::Relaxed);
        if params.method == "slow" {
            std::future::pending::<()>().await;
        }
        if params.method == "capability" {
            return ctx
                .call(
                    params.params["method"].as_str().unwrap(),
                    &params.params["params"],
                    TEST_WAIT,
                )
                .await;
        }
        Ok(params.params)
    }
    async fn health(&self, _: Context) -> RpcResult<HealthResult> {
        self.0.health_calls.fetch_add(1, Ordering::Relaxed);
        if self.0.health_hang.load(Ordering::Relaxed) {
            std::future::pending::<()>().await;
        }
        if self.0.health_fail.load(Ordering::Relaxed) {
            return Err(RpcError::new(INTERNAL, "fixture health failed"));
        }
        Ok(HealthResult {
            ready: !self.0.unready.load(Ordering::Relaxed),
            detail: None,
        })
    }
}

fn descriptor(id: &str, groups: &[&str]) -> Descriptor {
    Descriptor {
        plugin: PluginInfo {
            id: id.into(),
            version: "test-0.2".into(),
        },
        kernel_api: PluginApi {
            major: 0,
            min_minor: 4,
        },
        hooks: vec![
            "response.observe".into(),
            "route.prepare".into(),
            "response.head".into(),
            "request.finish".into(),
            "admin.call".into(),
        ],
        capabilities: vec!["log".into(), "quota.read".into()],
        owned_groups: groups.iter().map(|g| (*g).into()).collect(),
        watched_groups: vec![],
        shutdown_timeout: Duration::from_secs(1),
    }
}

struct Fixture {
    kernel: Kernel,
    controls: Arc<Controls>,
    task: JoinHandle<std::io::Result<()>>,
    stop: Option<oneshot::Sender<()>>,
}
impl Fixture {
    fn start(root: &Path, id: &str, groups: &[&str]) -> Self {
        Self::with_descriptor(root, id, descriptor(id, groups))
    }
    fn with_descriptor(root: &Path, socket: &str, descriptor: Descriptor) -> Self {
        let controls = Arc::new(Controls::default());
        let server = Server::bind(
            Settings {
                socket: root.join(format!("{socket}.sock")),
                data: root.join(format!("{socket}-data")),
            },
            descriptor,
            TestPlugin(controls.clone()),
        )
        .unwrap();
        let kernel = server.kernel();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(server.run_until(async {
            let _ = stopped.await;
        }));
        Self {
            kernel,
            controls,
            task,
            stop: Some(stop),
        }
    }
    async fn stop(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        let _ = (&mut self.task).await.unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn registration(root: &Path, id: &str, enabled: bool, grants: &[&str]) -> Value {
    json!({"id":id,"enabled":enabled,"socket":root.join(format!("{id}.sock")),"grants":grants})
}
fn registry(root: &Path, plugins: Vec<Value>) {
    let temp = root.join("registry.tmp");
    fs::write(
        &temp,
        serde_json::to_vec(&json!({"plugins":plugins})).unwrap(),
    )
    .unwrap();
    fs::rename(temp, root.join("registry.json")).unwrap();
}
fn start(root: &Path) -> PluginRuntime {
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
    .unwrap()
}
async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(TEST_WAIT, async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture condition deadline");
}
async fn available(runtime: &PluginRuntime, id: &str) {
    until(|| {
        runtime
            .snapshot()
            .plugins
            .iter()
            .any(|p| p.id == id && p.state == ConnectionState::Available)
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
fn route() -> RoutePrepareParams {
    RoutePrepareParams {
        request_id: "test".into(),
        attempt: 1,
        model: "test".into(),
        deadline_unix_ms: 0,
        replay_safe: true,
        account_id: "a".into(),
    }
}
fn head() -> ResponseHeadParams {
    ResponseHeadParams {
        request_id: "test".into(),
        lease_id: "lease".into(),
        attempt: 1,
        status: Some(200),
        error_kind: None,
        state_returned: false,
        turn_state: None,
        set_cookies: vec![],
        duration_ms: 0,
        replay_safe: true,
    }
}

#[tokio::test]
async fn observe_has_twenty_second_budget_while_head_and_prepare_keep_two_seconds() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::start(root.path(), "test", &["g"]);
    registry(
        root.path(),
        vec![registration(root.path(), "test", true, &[])],
    );
    let runtime = start(root.path());
    available(&runtime, "test").await;
    fixture.controls.slow.store(true, Ordering::Relaxed);
    let params = ResponseObserveParams {
        sse: true,
        request_id: "r".into(),
        account_id: "a".into(),
        attempt: 1,
        model: "m".into(),
        request_had_turn_state: true,
        status: Some(200),
        error_kind: None,
        state_returned: true,
        turn_state: Some("s".into()),
        set_cookies: vec![],
        replay_safe: true,
        can_switch: true,
    };
    let started = std::time::Instant::now();
    let (observe, head, prepare) = tokio::join!(
        async {
            let r = runtime.response_observe("test", params).await;
            (r, started.elapsed())
        },
        async {
            let r = runtime.response_head("test", head()).await;
            (r, started.elapsed())
        },
        async {
            let r = runtime.route_prepare("test", route()).await;
            (r, started.elapsed())
        }
    );
    for (error, elapsed) in [
        (head.0.unwrap_err(), head.1),
        (prepare.0.unwrap_err(), prepare.1),
    ] {
        assert_eq!(error.code, UNAVAILABLE);
        assert!(elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(4));
    }
    assert_eq!(observe.0.unwrap_err().code, UNAVAILABLE);
    assert!(observe.1 >= Duration::from_secs(20) && observe.1 < Duration::from_secs(23));
    runtime.shutdown().await;
}

#[test]
fn registry_validation_and_disabled_configuration() {
    let root = tempfile::tempdir().unwrap();
    let good = registration(root.path(), "test", true, &["log"]);
    for bad in [
        json!({"id":"test","enabled":true,"socket":"relative.sock","grants":[]}),
        json!({"id":"test","enabled":true,"socket":"/tmp/test.sock","grants":["unknown"]}),
        json!({"id":"test","enabled":true,"socket":"/tmp/test.sock","grants":["accounts.warmup.run"]}),
    ] {
        registry(root.path(), vec![bad]);
        assert!(PluginRegistry::load(&root.path().join("registry.json")).is_err());
    }
    registry(root.path(), vec![good.clone(), good]);
    assert!(PluginRegistry::load(&root.path().join("registry.json")).is_err());
    assert!(
        PluginRuntime::start(
            PluginRuntimeConfig::default(),
            KernelInfo {
                version: "test".into(),
                slot: None
            },
            Arc::new(LogCapabilities)
        )
        .unwrap()
        .is_none()
    );
    let mut config = PluginRuntimeConfig {
        registry_path: Some("registry.json".into()),
        ownership_cache_path: None,
    };
    assert!(config.resolve_and_validate(root.path()).is_err());
    config.ownership_cache_path = Some("cache.json".into());
    config.resolve_and_validate(root.path()).unwrap();
    assert!(config.registry_path.unwrap().is_absolute());
}

#[tokio::test]
async fn registry_reload_rejects_invalid_then_disables_and_removes() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::start(root.path(), "test", &["group"]);
    let good = registration(root.path(), "test", true, &["log"]);
    registry(root.path(), vec![good.clone()]);
    let runtime = start(root.path());
    available(&runtime, "test").await;
    registry(root.path(), vec![good.clone(), good]);
    until(|| runtime.snapshot().registry_error.is_some()).await;
    assert_eq!(runtime.ownership("group"), owned("test", true));
    assert!(
        runtime
            .admin_call("test", admin("echo", json!(1)))
            .await
            .is_ok()
    );
    registry(
        root.path(),
        vec![registration(root.path(), "test", false, &["log"])],
    );
    until(|| runtime.ownership("group") == Ownership::NotOwned).await;
    assert_eq!(
        runtime.snapshot().plugins[0].state,
        ConnectionState::Disabled
    );
    let cache: Value =
        serde_json::from_slice(&fs::read(root.path().join("plugin-ownership.json")).unwrap())
            .unwrap();
    assert!(cache["plugins"].get("test").is_none());
    tokio::time::timeout(TEST_WAIT, async {
        loop {
            if f.kernel
                .log(
                    &LogParams {
                        level: "info".into(),
                        message: "test".into(),
                        fields: json!({}),
                    },
                    Duration::from_millis(10),
                )
                .await
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    registry(root.path(), vec![]);
    until(|| runtime.snapshot().plugins.is_empty()).await;
    runtime.shutdown().await;
    f.stop().await;
}

#[tokio::test]
async fn ownership_notifications_cache_restart_and_conflicts() {
    let root = tempfile::tempdir().unwrap();
    let a = Fixture::start(root.path(), "first", &["group"]);
    registry(
        root.path(),
        vec![registration(root.path(), "first", true, &["log"])],
    );
    let runtime = start(root.path());
    available(&runtime, "first").await;
    let b = Fixture::start(root.path(), "second", &["group", "second"]);
    registry(
        root.path(),
        vec![
            registration(root.path(), "first", true, &["log"]),
            registration(root.path(), "second", true, &["log"]),
        ],
    );
    available(&runtime, "second").await;
    assert_eq!(runtime.ownership("group"), owned("first", true));
    assert!(
        runtime
            .snapshot()
            .plugins
            .iter()
            .find(|p| p.id == "second")
            .unwrap()
            .last_error
            .as_ref()
            .unwrap()
            .contains("conflict")
    );
    a.kernel
        .set_owned_groups(vec!["latest".into()])
        .await
        .unwrap();
    until(|| runtime.ownership("latest") == owned("first", true)).await;
    assert_eq!(runtime.ownership("group"), Ownership::NotOwned);
    let path = root.path().join("plugin-ownership.json");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let cache: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(cache["plugins"]["first"], json!(["latest"]));
    runtime.shutdown().await;
    a.stop().await;
    b.stop().await;
    let restarted = start(root.path());
    assert_eq!(restarted.ownership("latest"), owned("first", false));
    until(|| restarted.ownership("latest") == owned("first", false)).await;
    assert_eq!(
        restarted
            .route_prepare("first", route())
            .await
            .unwrap_err()
            .code,
        UNAVAILABLE
    );
    restarted.shutdown().await;
}

#[tokio::test]
async fn handshake_rejects_wrong_id_and_incompatible_version() {
    let root = tempfile::tempdir().unwrap();
    let wrong = Fixture::with_descriptor(root.path(), "wrong", descriptor("other", &[]));
    let mut incompatible = descriptor("incompatible", &[]);
    incompatible.kernel_api.major = 99;
    let version = Fixture::with_descriptor(root.path(), "incompatible", incompatible);
    registry(
        root.path(),
        vec![
            registration(root.path(), "wrong", true, &[]),
            registration(root.path(), "incompatible", true, &[]),
        ],
    );
    let runtime = start(root.path());
    until(|| {
        runtime.snapshot().plugins.len() == 2
            && runtime
                .snapshot()
                .plugins
                .iter()
                .all(|p| p.last_error.is_some())
    })
    .await;
    assert!(
        runtime
            .snapshot()
            .plugins
            .iter()
            .all(|p| p.state != ConnectionState::Available)
    );
    assert!(
        runtime
            .snapshot()
            .plugins
            .iter()
            .find(|p| p.id == "wrong")
            .unwrap()
            .last_error
            .as_ref()
            .unwrap()
            .contains("ID mismatch")
    );
    runtime.shutdown().await;
    wrong.stop().await;
    version.stop().await;
}

#[tokio::test]
async fn hooks_and_capability_intersection() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::start(root.path(), "test", &["group"]);
    registry(
        root.path(),
        vec![registration(
            root.path(),
            "test",
            true,
            &["log", "upstream.probe"],
        )],
    );
    let runtime = start(root.path());
    available(&runtime, "test").await;
    f.kernel
        .log(
            &LogParams {
                level: "info".into(),
                message: "SDK notification".into(),
                fields: json!({}),
            },
            TEST_WAIT,
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .admin_call("test", admin("echo", json!("after notification")))
            .await
            .unwrap(),
        json!("after notification")
    );
    assert_eq!(
        runtime.snapshot().plugins[0].granted_capabilities,
        vec!["log"]
    );
    assert_eq!(
        runtime.snapshot().plugins[0].version.as_deref(),
        Some("test-0.2")
    );
    assert_eq!(
        runtime.snapshot().plugins[0].kernel_api,
        Some(PluginApi {
            major: 0,
            min_minor: 4
        })
    );
    assert!(runtime.snapshot().plugins[0].peer_uid.is_some());
    assert!(matches!(
        runtime.route_prepare("test", route()).await.unwrap(),
        RoutePrepareResult::Fail { .. }
    ));
    assert_eq!(
        runtime.response_head("test", head()).await.unwrap(),
        ResponseHeadResult::Accept
    );
    runtime
        .request_finish(
            "test",
            RequestFinishParams {
                request_id: "test".into(),
                lease_id: "lease".into(),
                outcome: RequestOutcome::Completed,
                duration_ms: 1,
                first_byte_ms: None,
                usage: None,
            },
        )
        .await;
    tokio::time::timeout(TEST_WAIT, f.controls.finish.notified())
        .await
        .unwrap();
    for method in ["quota.read", "upstream.probe", "unknown"] {
        assert_eq!(
            runtime
                .admin_call(
                    "test",
                    admin("capability", json!({"method":method,"params":{}}))
                )
                .await
                .unwrap_err()
                .code,
            FORBIDDEN
        );
    }
    assert!(runtime.admin_call("test", admin("capability", json!({"method":"log","params":{"level":"info","message":"fixture log","fields":{}}}))).await.is_ok());
    runtime.shutdown().await;
    f.stop().await;
}

#[tokio::test]
async fn hook_and_admin_timeouts_fail_closed_without_retry() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::start(root.path(), "test", &["group"]);
    registry(
        root.path(),
        vec![registration(root.path(), "test", true, &[])],
    );
    let runtime = start(root.path());
    available(&runtime, "test").await;
    f.controls.slow.store(true, Ordering::Relaxed);
    let (route_result, head_result, admin_result) = tokio::join!(
        runtime.route_prepare("test", route()),
        runtime.response_head("test", head()),
        runtime.admin_call("test", admin("slow", Value::Null))
    );
    for result in [
        route_result.map(|_| ()),
        head_result.map(|_| ()),
        admin_result.map(|_| ()),
    ] {
        assert_eq!(result.unwrap_err().code, UNAVAILABLE);
    }
    assert!(runtime.snapshot().plugins[0].last_error_at.is_some());
    assert_eq!(f.controls.route_calls.load(Ordering::Relaxed), 1);
    assert_eq!(f.controls.head_calls.load(Ordering::Relaxed), 1);
    assert_eq!(f.controls.admin_calls.load(Ordering::Relaxed), 1);
    runtime.shutdown().await;
    f.stop().await;
}

#[tokio::test]
async fn consecutive_health_failures_and_unready_disconnect_then_recover() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::start(root.path(), "test", &["group"]);
    f.controls.health_fail.store(true, Ordering::Relaxed);
    registry(
        root.path(),
        vec![registration(root.path(), "test", true, &["log"])],
    );
    let runtime = start(root.path());
    available(&runtime, "test").await;
    until(|| f.controls.health_calls.load(Ordering::Relaxed) == 1).await;
    assert_eq!(runtime.ownership("group"), owned("test", true));
    until(|| runtime.snapshot().plugins[0].last_error.is_some()).await;
    assert!(f.controls.health_calls.load(Ordering::Relaxed) >= 2);
    f.controls.health_fail.store(false, Ordering::Relaxed);
    available(&runtime, "test").await;
    f.controls.unready.store(true, Ordering::Relaxed);
    until(|| {
        runtime.snapshot().plugins[0]
            .last_error
            .as_ref()
            .is_some_and(|e| e.contains("ready=false"))
    })
    .await;
    f.controls.unready.store(false, Ordering::Relaxed);
    available(&runtime, "test").await;
    runtime.shutdown().await;
    f.stop().await;
}

// 真正的独立插件进程复用当前测试二进制，使用 SDK 监听 socket；父测试 kill 后重新启动。
#[test]
fn sdk_process_helper() {
    let Some(root) = std::env::var_os("CPR_TEST_PLUGIN_PROCESS_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            Server::bind(
                Settings {
                    socket: root.join("process.sock"),
                    data: root.join("process-data"),
                },
                descriptor("process", &["group"]),
                TestPlugin(Arc::new(Controls::default())),
            )
            .unwrap()
            .run_until(std::future::pending())
            .await
            .unwrap();
        });
}
struct Process(std::process::Child);
impl Process {
    fn start(root: &Path) -> Self {
        Self(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "plugin::runtime::sdk_process_helper"])
                .env("CPR_TEST_PLUGIN_PROCESS_ROOT", root)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn killed_process_reconnects_with_backoff_and_restores_ownership() {
    let root = tempfile::tempdir().unwrap();
    let process = Process::start(root.path());
    registry(
        root.path(),
        vec![registration(root.path(), "process", true, &["log"])],
    );
    let runtime = start(root.path());
    available(&runtime, "process").await;
    drop(process);
    until(|| runtime.ownership("group") == owned("process", false)).await;
    let first = runtime.snapshot().plugins[0].last_error_at;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        runtime.snapshot().plugins[0].last_error_at,
        first,
        "100 ms initial retry budget"
    );
    let process = Process::start(root.path());
    available(&runtime, "process").await;
    assert_eq!(runtime.ownership("group"), owned("process", true));
    assert_eq!(
        runtime
            .admin_call("process", admin("echo", json!("recovered")))
            .await
            .unwrap(),
        json!("recovered")
    );
    runtime.shutdown().await;
    drop(process);
}

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[tokio::test]
async fn log_truncates_utf8_and_normalizes_level_and_other_capabilities_are_unsupported() {
    let output = LogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(output.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    LogCapabilities
        .call(
            &TestAccess,
            "log",
            json!({"level":"unknown","message":"中".repeat(2000),"fields":{"key":"value"}}),
        )
        .await
        .unwrap();
    let event: Value = serde_json::from_slice(&output.0.lock().unwrap()).unwrap();
    assert_eq!(event["fields"]["plugin_id"], "fixture");
    assert_eq!(event["level"], "INFO");
    assert_eq!(event["fields"]["message"].as_str().unwrap().len(), 4095);
    assert_eq!(
        LogCapabilities
            .call(&TestAccess, "quota.read", json!({}))
            .await
            .unwrap_err()
            .code,
        NOT_SUPPORTED
    );
    assert_eq!(
        LogCapabilities
            .call(&TestAccess, "log", json!({}))
            .await
            .unwrap_err()
            .code,
        INVALID_PARAMS
    );
}

#[tokio::test]
async fn health_timeouts_disconnect_and_handshake_deadline_is_bounded() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::start(root.path(), "test", &["group"]);
    f.controls.health_hang.store(true, Ordering::Relaxed);
    let listener = tokio::net::UnixListener::bind(root.path().join("silent.sock")).unwrap();
    registry(
        root.path(),
        vec![
            registration(root.path(), "test", true, &[]),
            registration(root.path(), "silent", true, &[]),
        ],
    );
    let runtime = start(root.path());
    let (mut stream, _) = listener.accept().await.unwrap();
    let _: Message = read_frame(&mut stream).await.unwrap();
    available(&runtime, "test").await;
    until(|| {
        runtime
            .snapshot()
            .plugins
            .iter()
            .find(|p| p.id == "silent")
            .unwrap()
            .last_error
            .as_ref()
            .is_some_and(|e| e.contains("timeout"))
    })
    .await;
    until(|| {
        runtime
            .snapshot()
            .plugins
            .iter()
            .find(|p| p.id == "test")
            .unwrap()
            .last_error
            .as_ref()
            .is_some_and(|e| e.contains("timeout"))
    })
    .await;
    assert!(f.controls.health_calls.load(Ordering::Relaxed) >= 2);
    runtime.shutdown().await;
    f.stop().await;
}

#[tokio::test]
async fn incompatible_hello_result_is_rejected_even_when_peer_does_not_validate() {
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::UnixListener::bind(root.path().join("test.sock")).unwrap();
    registry(
        root.path(),
        vec![registration(root.path(), "test", true, &[])],
    );
    let runtime = start(root.path());
    let (mut stream, _) = listener.accept().await.unwrap();
    let _: Message = read_frame(&mut stream).await.unwrap();
    let mut hello = descriptor("test", &[]).hello();
    hello.kernel_api.min_minor = KERNEL_API.1 + 1;
    write_frame(
        &mut stream,
        &Message::Response {
            id: 1,
            payload: ResponsePayload::Result(serde_json::to_value(hello).unwrap()),
        },
    )
    .await
    .unwrap();
    until(|| {
        runtime.snapshot().plugins[0]
            .last_error
            .as_ref()
            .is_some_and(|e| e.contains("incompatible kernel API"))
    })
    .await;
    assert_ne!(
        runtime.snapshot().plugins[0].state,
        ConnectionState::Available
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn sdk_log_notification_is_authorized_without_fabricating_a_response() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::start(root.path(), "test", &[]);
    registry(
        root.path(),
        vec![registration(root.path(), "test", true, &[])],
    );
    let runtime = start(root.path());
    available(&runtime, "test").await;
    f.kernel
        .log(
            &LogParams {
                level: "info".into(),
                message: "must be denied".into(),
                fields: json!({}),
            },
            TEST_WAIT,
        )
        .await
        .unwrap();
    until(|| {
        runtime.snapshot().plugins[0]
            .last_error
            .as_ref()
            .is_some_and(|e| e.contains(FORBIDDEN))
    })
    .await;
    assert_eq!(
        runtime
            .admin_call("test", admin("echo", json!("still connected")))
            .await
            .unwrap(),
        json!("still connected")
    );
    runtime.shutdown().await;
    f.stop().await;
}

struct TestAccess;
impl gateway_core::plugin::PluginAccess for TestAccess {
    fn id(&self) -> &str {
        "fixture"
    }
    fn groups(&self) -> Vec<String> {
        vec![]
    }
    fn fetch_hosts(&self) -> Vec<String> {
        vec![]
    }
}

#[tokio::test]
async fn uid_mismatch_rejects_before_hello_and_records_health() {
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::UnixListener::bind(root.path().join("test.sock")).unwrap();
    let mut entry = registration(root.path(), "test", true, &[]);
    entry["uid"] = json!(u32::MAX);
    registry(root.path(), vec![entry]);
    let runtime = start(root.path());
    let (mut stream, _) = listener.accept().await.unwrap();
    use tokio::io::AsyncReadExt;
    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    until(|| {
        runtime.snapshot().plugins[0]
            .last_error
            .as_ref()
            .is_some_and(|error| error.contains("UID mismatch"))
    })
    .await;
    assert_ne!(
        runtime.snapshot().plugins[0].state,
        ConnectionState::Available
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn corrupt_or_unreadable_cache_does_not_block_connection() {
    for unreadable in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let f = Fixture::start(root.path(), "test", &["group"]);
        let cache = root.path().join("plugin-ownership.json");
        if unreadable {
            fs::create_dir(&cache).unwrap();
        } else {
            fs::write(&cache, b"broken json").unwrap();
        }
        registry(
            root.path(),
            vec![registration(root.path(), "test", true, &[])],
        );
        let runtime = start(root.path());
        assert!(runtime.snapshot().cache_error.is_some());
        available(&runtime, "test").await;
        assert_eq!(runtime.ownership("group"), owned("test", true));
        assert!(
            runtime.snapshot().plugins[0]
                .last_error
                .as_ref()
                .is_some_and(|error| error.contains("ownership cache"))
        );
        runtime.shutdown().await;
        f.stop().await;
    }
}

#[tokio::test]
async fn owned_and_watched_conflicts_follow_declaration_order_and_cache_watchers() {
    let root = tempfile::tempdir().unwrap();
    let a = Fixture::start(root.path(), "a", &[]);
    let b = Fixture::start(root.path(), "b", &[]);
    registry(
        root.path(),
        vec![
            registration(root.path(), "a", true, &[]),
            registration(root.path(), "b", true, &[]),
        ],
    );
    let runtime = start(root.path());
    available(&runtime, "a").await;
    available(&runtime, "b").await;
    a.kernel
        .set_ownership(vec![], vec!["g".into()])
        .await
        .unwrap();
    until(|| {
        runtime.ownership("g")
            == Ownership::Watched {
                plugins: vec!["a".into()],
            }
    })
    .await;
    b.kernel
        .set_ownership(vec!["g".into()], vec![])
        .await
        .unwrap();
    until(|| runtime.snapshot().plugins[1].last_error.is_some()).await;
    assert_eq!(
        runtime.ownership("g"),
        Ownership::Watched {
            plugins: vec!["a".into()]
        }
    );
    b.kernel
        .set_ownership(vec![], vec!["g".into()])
        .await
        .unwrap();
    until(|| {
        runtime.ownership("g")
            == Ownership::Watched {
                plugins: vec!["a".into(), "b".into()],
            }
    })
    .await;
    a.kernel
        .set_ownership(vec!["g".into()], vec![])
        .await
        .unwrap();
    until(|| runtime.snapshot().plugins[0].last_error.is_some()).await;
    assert_eq!(
        runtime.ownership("g"),
        Ownership::Watched {
            plugins: vec!["b".into()]
        }
    );
    b.kernel
        .set_ownership(vec!["g".into()], vec![])
        .await
        .unwrap();
    until(|| runtime.ownership("g") == owned("b", true)).await;
    a.kernel
        .set_ownership(vec![], vec!["g".into(), "other".into()])
        .await
        .unwrap();
    until(|| {
        runtime.ownership("other")
            == Ownership::Watched {
                plugins: vec!["a".into()],
            }
    })
    .await;
    assert_eq!(runtime.ownership("g"), owned("b", true));
    runtime.shutdown().await;
    a.stop().await;
    b.stop().await;
    let restarted = start(root.path());
    assert_eq!(
        restarted.ownership("other"),
        Ownership::Watched {
            plugins: vec!["a".into()]
        }
    );
    assert_eq!(restarted.ownership("g"), owned("b", false));
    restarted.shutdown().await;
}

#[tokio::test]
async fn all_capabilities_require_request_and_registry_intersection() {
    let methods = [
        "accounts.read",
        "quota.read",
        "groups.move",
        "upstream.probe",
        "http.fetch",
    ];
    for requested in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut desc = descriptor("test", &[]);
        desc.capabilities = if requested {
            methods.iter().map(|method| (*method).into()).collect()
        } else {
            vec![]
        };
        let f = Fixture::with_descriptor(root.path(), "test", desc);
        registry(
            root.path(),
            vec![registration(
                root.path(),
                "test",
                true,
                if requested { &[] } else { &methods },
            )],
        );
        let runtime = start(root.path());
        available(&runtime, "test").await;
        for method in methods {
            let error = runtime
                .admin_call(
                    "test",
                    admin("capability", json!({"method": method, "params": {}})),
                )
                .await
                .unwrap_err();
            assert_eq!(error.code, FORBIDDEN, "{method}");
        }
        runtime.shutdown().await;
        f.stop().await;
    }
}

struct DelayedWarmupAuthorization {
    entered: Notify,
    proceed: Notify,
}
#[async_trait::async_trait]
impl PluginCapabilities for DelayedWarmupAuthorization {
    async fn call(
        &self,
        access: &dyn gateway_core::plugin::PluginAccess,
        method: &str,
        _: Value,
    ) -> RpcResult<Value> {
        gateway_core::plugin::require_warmup(access, method)?;
        self.entered.notify_one();
        self.proceed.notified().await;
        gateway_core::plugin::require_warmup(access, method)?;
        Ok(json!({"accepted":true}))
    }
}

#[tokio::test]
async fn warmup_grant_revocation_rejects_already_queued_maintenance_without_ownership() {
    let root = tempfile::tempdir().unwrap();
    let mut desc = descriptor(ACCOUNT_WARMUP_PLUGIN, &[]);
    desc.capabilities = vec![ACCOUNT_WARMUP_RUN.into()];
    let f = Fixture::with_descriptor(root.path(), ACCOUNT_WARMUP_PLUGIN, desc);
    registry(
        root.path(),
        vec![registration(
            root.path(),
            ACCOUNT_WARMUP_PLUGIN,
            true,
            &[ACCOUNT_WARMUP_RUN],
        )],
    );
    let caps = Arc::new(DelayedWarmupAuthorization {
        entered: Notify::new(),
        proceed: Notify::new(),
    });
    let runtime = PluginRuntime::start(
        PluginRuntimeConfig {
            registry_path: Some(root.path().join("registry.json")),
            ownership_cache_path: Some(root.path().join("plugin-ownership.json")),
        },
        KernelInfo {
            version: "fixture".into(),
            slot: None,
        },
        caps.clone(),
    )
    .unwrap()
    .unwrap();
    available(&runtime, ACCOUNT_WARMUP_PLUGIN).await;
    let request = runtime.admin_call(
        ACCOUNT_WARMUP_PLUGIN,
        admin(
            "capability",
            json!({"method":ACCOUNT_WARMUP_RUN,"params":{}}),
        ),
    );
    tokio::pin!(request);
    tokio::select! {
        result = &mut request => panic!("capability returned before authorization gate: {result:?}"),
        () = caps.entered.notified() => {}
    }
    registry(
        root.path(),
        vec![registration(root.path(), ACCOUNT_WARMUP_PLUGIN, true, &[])],
    );
    until(|| {
        runtime.snapshot().plugins.iter().any(|plugin| {
            plugin.id == ACCOUNT_WARMUP_PLUGIN
                && !plugin
                    .granted_capabilities
                    .iter()
                    .any(|grant| grant == ACCOUNT_WARMUP_RUN)
        })
    })
    .await;
    caps.proceed.notify_one();
    assert!(matches!(
        request.await.unwrap_err().code.as_str(),
        FORBIDDEN | UNAVAILABLE
    ));
    assert_eq!(runtime.ownership("any_group"), Ownership::NotOwned);
    runtime.shutdown().await;
    f.stop().await;
}

#[tokio::test]
async fn configured_slot_is_sent_in_hello() {
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::UnixListener::bind(root.path().join("test.sock")).unwrap();
    registry(
        root.path(),
        vec![registration(root.path(), "test", true, &[])],
    );
    let runtime = start(root.path());
    let (mut stream, _) = listener.accept().await.unwrap();
    let Message::Request { method, params, .. } = read_frame(&mut stream).await.unwrap() else {
        panic!("hello request")
    };
    assert_eq!(method, "hello");
    assert_eq!(params["kernel"]["slot"], "b");
    runtime.shutdown().await;
}

#[test]
fn both_partial_runtime_configurations_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    for registry_only in [false, true] {
        let mut config = PluginRuntimeConfig {
            registry_path: registry_only.then(|| root.path().join("registry.json")),
            ownership_cache_path: (!registry_only).then(|| root.path().join("cache.json")),
        };
        assert!(config.resolve_and_validate(root.path()).is_err());
        assert!(
            PluginRuntime::start(
                config,
                KernelInfo {
                    version: "fixture".into(),
                    slot: None
                },
                Arc::new(LogCapabilities)
            )
            .is_err()
        );
    }
}
