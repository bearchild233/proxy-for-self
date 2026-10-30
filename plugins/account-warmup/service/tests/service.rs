use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use account_warmup::{Account, AccountOverride, Settings as WarmupSettings, Store, WarmupService};
use chrono::Utc;
use plugin_sdk::{
    Context, Descriptor, Kernel, Plugin, RpcResult, Server, Settings, protocol::*,
    testing::MockKernel,
};
use serde_json::{Value, json};
use tokio::{net::UnixStream, sync::oneshot, task::JoinHandle};

const BUDGET: Duration = Duration::from_secs(5);
static NEXT: AtomicU64 = AtomicU64::new(1);

fn directory() -> Value {
    json!({"accounts":[{"id":"a","provider":"openai","authenticationKind":"oauth","enabled":true,"status":"active","supported":true,"reasonCode":null,"modelAllowed":true,"models":null,"shortTerm":null},{"id":"b","provider":"xai","authenticationKind":"oauth","enabled":true,"status":"active","supported":false,"reasonCode":"unsupported_provider","modelAllowed":null,"models":null,"shortTerm":null}]})
}
fn descriptor() -> Descriptor {
    Descriptor {
        plugin: PluginInfo {
            id: "account-warmup".into(),
            version: "test".into(),
        },
        kernel_api: PluginApi {
            major: 0,
            min_minor: 8,
        },
        hooks: vec!["admin.call".into()],
        capabilities: vec!["accounts.warmup.read".into(), "accounts.warmup.run".into()],
        owned_groups: vec![],
        watched_groups: vec![],
        shutdown_timeout: BUDGET,
    }
}
fn hello() -> HelloParams {
    HelloParams {
        kernel_api: KernelApi::default(),
        kernel: KernelInfo {
            version: "test".into(),
            slot: Some(KernelSlot::A),
        },
        role: ConnectionRole::Kernel,
    }
}
fn path() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "warmup-svc-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    path
}
struct Manual(WarmupService);
impl Plugin for Manual {
    async fn admin_call(&self, context: Context, params: AdminCallParams) -> RpcResult<Value> {
        self.0.admin_call(context, params).await
    }
}
struct Fixture {
    path: PathBuf,
    service: WarmupService,
    kernel: Kernel,
    peer: Arc<MockKernel>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<std::io::Result<()>>,
}
impl Fixture {
    async fn start() -> Self {
        let path = path();
        let store = Store::open(&path.join("data")).unwrap();
        let settings = WarmupSettings {
            jitter_min_seconds: 0,
            jitter_max_seconds: 0,
            ..WarmupSettings::default()
        };
        store.configure(0, &settings).unwrap();
        let service = WarmupService::new(store);
        let socket = path.join("service.sock");
        let server = Server::bind(
            Settings {
                socket: socket.clone(),
                data: path.join("data"),
            },
            descriptor(),
            Manual(service.clone()),
        )
        .unwrap();
        let kernel = server.kernel();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(server.run_until(async {
            let _ = stopped.await;
        }));
        let peer = Arc::new(MockKernel::connect(&socket, hello(), BUDGET).await.unwrap());
        Self {
            path,
            service,
            kernel,
            peer,
            stop,
            task,
        }
    }
    async fn finish(self) {
        let _ = self.stop.send(());
        self.task.await.unwrap().unwrap();
        drop(self.peer);
        drop(self.service);
        let _ = std::fs::remove_dir_all(self.path);
    }
}

#[tokio::test]
async fn manual_all_is_resolved_in_service_and_unsupported_is_visible() {
    let fixture = Fixture::start().await;
    let sends = Arc::new(AtomicU64::new(0));
    let sent = sends.clone();
    fixture.peer.set_handler(move |method, params| match method {
        "accounts.warmup.read" => Some(Ok(directory())),
        "accounts.warmup.run" => {
            assert_eq!(params["accountId"], "a");
            assert_eq!(params["timeoutMs"], 15000);
            assert_eq!(params["prompt"], "hi");
            assert_eq!(params["reasoningEffort"], "auto");
            sent.fetch_add(1, Ordering::SeqCst);
            Some(Ok(json!({"outcome":"accepted","reasonCode":null,"status":200,"durationMs":3,"sendState":"sent","shortTerm":null})))
        }
        _ => None,
    });
    let run = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "run",
            json!({"submissionId":"all-one","scope":"all"}),
        )
        .await
        .unwrap();
    assert_eq!(run["run"]["total"], 2);
    assert_eq!(run["run"]["skipped"], 1);
    fixture
        .service
        .tick(&fixture.kernel, Utc::now())
        .await
        .unwrap();
    let again = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "run",
            json!({"submissionId":"all-one","scope":"all"}),
        )
        .await
        .unwrap();
    assert_eq!(again["run"]["id"], run["run"]["id"]);
    assert_eq!(again["run"]["accepted"], 1);
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    let history = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "history",
            json!({"runId":run["run"]["id"]}),
        )
        .await
        .unwrap();
    assert!(history["items"][0]["scheduledAt"].is_string());
    assert!(!serde_json::to_string(&history).unwrap().contains("Cookie"));
    fixture.finish().await;
}

#[tokio::test]
async fn interrupted_run_is_unknown_and_same_submission_never_resends() {
    let fixture = Fixture::start().await;
    let sends = Arc::new(AtomicU64::new(0));
    let sent = sends.clone();
    fixture.peer.set_handler(move |method, _| match method {
        "accounts.warmup.read" => Some(Ok(directory())),
        "accounts.warmup.run" => {
            sent.fetch_add(1, Ordering::SeqCst);
            Some(Err(RpcError::new(UNAVAILABLE, "synthetic disconnect")))
        }
        _ => None,
    });
    fixture
        .service
        .dispatch(
            &fixture.kernel,
            "run",
            json!({"submissionId":"uncertain","scope":"selected","accountIds":["a"]}),
        )
        .await
        .unwrap();
    fixture
        .service
        .tick(&fixture.kernel, Utc::now())
        .await
        .unwrap();
    let result = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "run",
            json!({"submissionId":"uncertain","scope":"selected","accountIds":["a"]}),
        )
        .await
        .unwrap();
    assert_eq!(result["run"]["unknown"], 1);
    let read = fixture
        .service
        .dispatch(&fixture.kernel, "read", json!({}))
        .await
        .unwrap();
    assert_eq!(read["accountStates"]["a"]["status"], "unknown");
    assert!(read["accountStates"]["a"].get("prompt").is_none());
    fixture
        .service
        .tick(&fixture.kernel, Utc::now())
        .await
        .unwrap();
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn manual_overrides_replace_each_target_snapshot_and_leave_automatic_settings_unchanged() {
    let fixture = Fixture::start().await;
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = calls.clone();
    fixture.peer.set_handler(move |method, params| match method {
        "accounts.warmup.read" => Some(Ok(directory())),
        "accounts.warmup.run" => {
            captured.lock().unwrap().push(params.clone());
            Some(Ok(json!({"outcome":"accepted","reasonCode":null,"status":200,"durationMs":3,"sendState":"sent","shortTerm":null})))
        }
        _ => None,
    });
    let automatic = WarmupSettings {
        prompt: "automatic prompt".into(),
        reasoning_effort: "low".into(),
        jitter_min_seconds: 0,
        jitter_max_seconds: 0,
        account_overrides: vec![AccountOverride {
            account_id: "a".into(),
            enabled: true,
            daily_time: None,
            model: Some("account-model".into()),
            reasoning_effort: Some("medium".into()),
            reset_delay_seconds: None,
        }],
        ..WarmupSettings::default()
    };
    fixture
        .service
        .dispatch(
            &fixture.kernel,
            "configure",
            json!({"expectedRevision":1,"settings":automatic}),
        )
        .await
        .unwrap();
    let run = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "run",
            json!({
                "submissionId":"explicit-settings", "scope":"selected", "accountIds":["a"],
                "model":"manual-model", "prompt":"  manual\n\tprompt  ", "reasoningEffort":"high"
            }),
        )
        .await
        .unwrap();
    let read = fixture
        .service
        .dispatch(&fixture.kernel, "read", json!({}))
        .await
        .unwrap();
    assert_eq!(read["settings"], serde_json::to_value(&automatic).unwrap());
    assert_eq!(read["revision"], 2);
    assert_eq!(read["accountStates"]["a"]["status"], "queued");
    let mut changed = automatic;
    changed.prompt = "new automatic prompt".into();
    changed.reasoning_effort = "none".into();
    fixture
        .service
        .dispatch(
            &fixture.kernel,
            "configure",
            json!({"expectedRevision":2,"settings":changed}),
        )
        .await
        .unwrap();
    fixture
        .service
        .tick(&fixture.kernel, Utc::now())
        .await
        .unwrap();
    let sent = calls.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["model"], "manual-model");
    assert_eq!(sent[0]["prompt"], "  manual\n\tprompt  ");
    assert_eq!(sent[0]["reasoningEffort"], "high");
    let history = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "history",
            json!({"runId":run["run"]["id"],"source":"manual"}),
        )
        .await
        .unwrap();
    assert_eq!(history["items"][0]["prompt"], sent[0]["prompt"]);
    assert!(
        fixture
            .service
            .dispatch(
                &fixture.kernel,
                "history",
                json!({"runId":run["run"]["id"],"source":"automatic"})
            )
            .await
            .is_err()
    );
    let read = fixture
        .service
        .dispatch(&fixture.kernel, "read", json!({}))
        .await
        .unwrap();
    assert_eq!(read["settings"], serde_json::to_value(&changed).unwrap());
    fixture
        .service
        .dispatch(
            &fixture.kernel,
            "run",
            json!({"submissionId":"inherited-settings","scope":"selected","accountIds":["a"]}),
        )
        .await
        .unwrap();
    fixture
        .service
        .tick(&fixture.kernel, Utc::now())
        .await
        .unwrap();
    let sent = calls.lock().unwrap().clone();
    assert_eq!(sent[1]["model"], "account-model");
    assert_eq!(sent[1]["prompt"], "new automatic prompt");
    assert_eq!(sent[1]["reasoningEffort"], "medium");
    fixture.finish().await;
}

#[tokio::test]
async fn invalid_manual_prompt_effort_and_history_source_fail_before_kernel_calls() {
    let fixture = Fixture::start().await;
    fixture
        .peer
        .set_handler(|_, _| panic!("invalid parameters must not call the kernel"));
    for patch in [
        json!({"prompt":" \n\t"}),
        json!({"prompt":"x\u{0001}"}),
        json!({"prompt":"中".repeat(1366)}),
        json!({"reasoningEffort":"max"}),
        json!({"model":""}),
    ] {
        let mut params = json!({"submissionId":"invalid","scope":"all"});
        params
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert_eq!(
            fixture
                .service
                .dispatch(&fixture.kernel, "run", params)
                .await
                .unwrap_err()
                .code,
            INVALID_PARAMS
        );
    }
    assert!(
        fixture
            .service
            .dispatch(&fixture.kernel, "history", json!({"source":"daily"}))
            .await
            .is_err()
    );
    fixture.finish().await;
}

#[tokio::test]
async fn accepted_without_a_sent_boundary_is_recorded_as_unknown() {
    let fixture = Fixture::start().await;
    fixture.peer.set_handler(|method, _| match method {
        "accounts.warmup.read" => Some(Ok(directory())),
        "accounts.warmup.run" => Some(Ok(json!({
            "outcome":"accepted",
            "reasonCode":null,
            "status":200,
            "durationMs":3,
            "sendState":"not_sent",
            "shortTerm":null
        }))),
        _ => None,
    });
    let run = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "run",
            json!({"submissionId":"accepted-not-sent","scope":"selected","accountIds":["a"]}),
        )
        .await
        .unwrap();
    fixture
        .service
        .tick(&fixture.kernel, Utc::now())
        .await
        .unwrap();
    let history = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "history",
            json!({"runId":run["run"]["id"]}),
        )
        .await
        .unwrap();
    assert_eq!(history["items"][0]["status"], "unknown");
    assert_eq!(
        history["items"][0]["reasonCode"],
        "accepted_without_sent_result"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn lazy_models_translate_account_ids_and_reject_invalid_admin_input() {
    let fixture = Fixture::start().await;
    fixture.peer.set_handler(|method, params| {
        assert_eq!(method, "accounts.warmup.read");
        assert_eq!(params["ids"], json!(["a"]));
        assert_eq!(params["includeModels"], true);
        assert!(params.get("accountIds").is_none());
        Some(Ok(directory()))
    });
    let read = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "read",
            json!({"accountIds":["a"],"includeModels":true}),
        )
        .await
        .unwrap();
    assert_eq!(read["available"], true);
    assert!(
        fixture
            .service
            .dispatch(
                &fixture.kernel,
                "run",
                json!({"submissionId":"bad","scope":"selected","accountIds":[]})
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .dispatch(
                &fixture.kernel,
                "run",
                json!({"submissionId":"bad","scope":"all","accountIds":["a"]})
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .dispatch(
                &fixture.kernel,
                "run",
                json!({"submissionId":"bad","scope":"all","prompt":"\u{0001}"})
            )
            .await
            .is_err()
    );
    let denied: RpcResult<Value> = fixture
        .peer
        .call(
            "admin.call",
            &AdminCallParams {
                method: "read".into(),
                params: json!({}),
                actor: Actor { role: "key".into() },
            },
            BUDGET,
        )
        .await;
    assert_eq!(denied.unwrap_err().code, FORBIDDEN);
    fixture.finish().await;
}

#[tokio::test]
async fn read_preserves_model_reasoning_efforts_and_accepts_older_directory_entries() {
    let fixture = Fixture::start().await;
    fixture.peer.set_handler(|method, params| {
        assert_eq!(method, "accounts.warmup.read");
        assert_eq!(params["includeModels"], true);
        let mut response = directory();
        response["accounts"][0]["models"] = json!([
            {"id":"supported-model","label":"Supported","reasoningEfforts":["none","low","high"]},
            {"id":"legacy-model","label":"Legacy"}
        ]);
        Some(Ok(response))
    });
    let read = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "read",
            json!({"accountIds":["a"],"includeModels":true}),
        )
        .await
        .unwrap();
    assert_eq!(
        read["accounts"][0]["models"][0]["reasoningEfforts"],
        json!(["none", "low", "high"])
    );
    assert_eq!(
        read["accounts"][0]["models"][1]["reasoningEfforts"],
        json!([])
    );
    assert_eq!(read["settings"]["prompt"], "hi");
    assert_eq!(read["settings"]["reasoningEffort"], "auto");
    assert_eq!(read["groupTargetsSupported"], false);
    assert_eq!(read["accounts"][0]["groups"], json!([]));
    assert!(read["runtime"].get("nextAutomaticRunAt").is_some());
    assert_eq!(
        read["accountStates"]["a"],
        json!({"status":"pending","taskId":null,"updatedAt":null,"resetAt":null,"reasonCode":null})
    );
    assert_eq!(read["accountStates"]["b"]["status"], "unsupported");
    fixture.finish().await;
}

#[tokio::test]
async fn stopping_waits_for_inflight_background_reply_before_closing_kernel_connection() {
    let path = path();
    let data = path.join("data");
    let store = Store::open(&data).unwrap();
    let settings = WarmupSettings {
        jitter_min_seconds: 0,
        jitter_max_seconds: 0,
        ..WarmupSettings::default()
    };
    let account: Account = serde_json::from_value(directory()["accounts"][0].clone()).unwrap();
    let run = store
        .enqueue_manual("draining", &[account], &settings, Utc::now())
        .unwrap();
    let service = WarmupService::new(store);
    let socket = path.join("service.sock");
    let server = Server::bind(
        Settings {
            socket: socket.clone(),
            data: data.clone(),
        },
        descriptor(),
        service.clone(),
    )
    .unwrap();
    let api = server.kernel();
    let (stop, stopped) = oneshot::channel();
    let (stopping, stop_started) = oneshot::channel();
    let closing = service.clone();
    let task = tokio::spawn(server.run_until(async move {
        stopped.await.unwrap();
        closing.stop_background();
        let _ = stopping.send(());
        closing.join_background(BUDGET).await.unwrap();
    }));
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    write_frame(
        &mut stream,
        &Message::Request {
            id: 1,
            method: "hello".into(),
            params: serde_json::to_value(hello()).unwrap(),
        },
    )
    .await
    .unwrap();
    let _: Message = read_frame(&mut stream).await.unwrap();
    let (entered, sent) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let kernel = tokio::spawn(async move {
        let mut entered = Some(entered);
        let mut released = Some(released);
        loop {
            let message: Message = match read_frame(&mut stream).await {
                Ok(message) => message,
                Err(_) => break,
            };
            let Message::Request { id, method, .. } = message else {
                continue;
            };
            let result = if method == "accounts.warmup.read" {
                directory()
            } else {
                assert_eq!(method, "accounts.warmup.run");
                entered.take().unwrap().send(()).unwrap();
                released.take().unwrap().await.unwrap();
                json!({"outcome":"accepted","reasonCode":null,"status":200,"durationMs":3,"sendState":"sent","shortTerm":null})
            };
            if write_frame(
                &mut stream,
                &Message::Response {
                    id,
                    payload: ResponsePayload::Result(result),
                },
            )
            .await
            .is_err()
            {
                break;
            }
        }
    });
    service
        .dispatch(
            &api,
            "run",
            json!({"submissionId":"draining","scope":"all"}),
        )
        .await
        .unwrap();
    tokio::time::timeout(BUDGET, sent).await.unwrap().unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(BUDGET, stop_started)
        .await
        .unwrap()
        .unwrap();
    assert!(!task.is_finished());
    release.send(()).unwrap();
    tokio::time::timeout(BUDGET, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(BUDGET, kernel).await.unwrap().unwrap();
    assert_eq!(
        Store::open(&data)
            .unwrap()
            .history(20, Some(&run.id))
            .unwrap()["runs"][0]["accepted"],
        1
    );
    drop(service);
    std::fs::remove_dir_all(path).unwrap();
}

fn groups_directory(accounts: &[(&str, &[&str])]) -> Value {
    let accounts: Vec<Value> = accounts
        .iter()
        .map(|(id, groups)| {
            let mut account = directory()["accounts"][0].clone();
            account["id"] = json!(id);
            account["groups"] = json!(groups);
            account
        })
        .collect();
    json!({"accounts":accounts,"groupTargetsSupported":true})
}

fn groups_settings(now: chrono::DateTime<Utc>) -> WarmupSettings {
    WarmupSettings {
        enabled: true,
        daily_enabled: true,
        reset_enabled: false,
        timezone: "UTC".into(),
        daily_time: now.format("%H:%M").to_string(),
        jitter_min_seconds: 0,
        jitter_max_seconds: 0,
        target_mode: "groups".into(),
        group_ids: vec!["first".into(), "second".into()],
        ..WarmupSettings::default()
    }
}

#[tokio::test]
async fn group_configure_requires_a_fresh_capable_directory_and_does_not_save_on_failure() {
    let fixture = Fixture::start().await;
    let current = Arc::new(std::sync::Mutex::new(directory()));
    let response = current.clone();
    let reads = Arc::new(AtomicU64::new(0));
    let called = reads.clone();
    fixture.peer.set_handler(move |method, _| {
        assert_eq!(method, "accounts.warmup.read");
        called.fetch_add(1, Ordering::SeqCst);
        Some(Ok(response.lock().unwrap().clone()))
    });
    let config = groups_settings(Utc::now());
    let params = json!({"expectedRevision":1,"settings":config});
    let error = fixture
        .service
        .dispatch(&fixture.kernel, "configure", params.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, NOT_SUPPORTED);
    assert_eq!(error.message, "group_targets_unavailable");
    assert_eq!(
        Store::open(&fixture.path.join("data"))
            .unwrap()
            .settings()
            .unwrap()
            .0,
        1
    );
    *current.lock().unwrap() = groups_directory(&[("a", &["first"])]);
    fixture
        .service
        .dispatch(&fixture.kernel, "configure", params)
        .await
        .unwrap();
    *current.lock().unwrap() = directory();
    let error = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "configure",
            json!({"expectedRevision":2,"settings":config}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, NOT_SUPPORTED);
    assert_eq!(reads.load(Ordering::SeqCst), 4);
    let mut invalid = config;
    invalid.group_ids.clear();
    assert_eq!(
        fixture
            .service
            .dispatch(
                &fixture.kernel,
                "configure",
                json!({"expectedRevision":2,"settings":invalid})
            )
            .await
            .unwrap_err()
            .code,
        INVALID_PARAMS
    );
    assert_eq!(reads.load(Ordering::SeqCst), 4);
    fixture.finish().await;
}

#[tokio::test]
async fn group_tick_forces_latest_membership_unions_groups_and_keeps_original_task_parameters() {
    let fixture = Fixture::start().await;
    let now = Utc::now();
    let current = Arc::new(std::sync::Mutex::new(groups_directory(&[
        ("leaving", &["first", "second"]),
        ("joining", &[]),
    ])));
    let response = current.clone();
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = calls.clone();
    fixture.peer.set_handler(move |method, params| match method {
        "accounts.warmup.read" => Some(Ok(response.lock().unwrap().clone())),
        "accounts.warmup.run" => {
            captured.lock().unwrap().push(params.clone());
            Some(Ok(json!({"outcome":"accepted","reasonCode":null,"status":200,"durationMs":1,"sendState":"sent","shortTerm":null})))
        }
        _ => None,
    });
    let mut config = groups_settings(now);
    config.prompt = "frozen".into();
    config.reasoning_effort = "low".into();
    fixture
        .service
        .dispatch(
            &fixture.kernel,
            "configure",
            json!({"expectedRevision":1,"settings":config}),
        )
        .await
        .unwrap();
    let old_accounts: Vec<Account> =
        serde_json::from_value(current.lock().unwrap()["accounts"].clone()).unwrap();
    let store = Store::open(&fixture.path.join("data")).unwrap();
    store.schedule(&old_accounts, &config, now).unwrap();
    assert_eq!(
        store.history(20, None).unwrap()["runs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    *current.lock().unwrap() =
        groups_directory(&[("leaving", &[]), ("joining", &["first", "second", "first"])]);
    fixture.service.tick(&fixture.kernel, now).await.unwrap();
    fixture.service.tick(&fixture.kernel, now).await.unwrap();
    let sent = calls.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["accountId"], "joining");
    assert_eq!(sent[0]["prompt"], "frozen");
    let history = store.history(20, None).unwrap();
    let leaving = history["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|run| {
            let history = store.history(20, run["id"].as_str()).unwrap();
            (history["items"][0]["accountId"] == "leaving").then(|| history["items"][0].clone())
        })
        .unwrap();
    assert_eq!(leaving["status"], "skipped");
    assert_eq!(leaving["reasonCode"], "group_membership_changed");
    // read 不等 TTL，成员离组后立即从预测移除。
    *current.lock().unwrap() = groups_directory(&[("leaving", &[]), ("joining", &[])]);
    let read = fixture
        .service
        .dispatch(&fixture.kernel, "read", json!({}))
        .await
        .unwrap();
    assert_eq!(read["groupTargetsSupported"], true);
    assert_eq!(read["runtime"]["nextAutomaticRunAt"], Value::Null);
    drop(store);
    fixture.finish().await;
}

#[tokio::test]
async fn unknown_or_incapable_group_directory_never_sends_automatic_but_does_not_block_manual() {
    let fixture = Fixture::start().await;
    let now = Utc::now();
    let current = Arc::new(std::sync::Mutex::new(groups_directory(&[
        ("automatic", &["first"]),
        ("explicit", &[]),
    ])));
    let response = current.clone();
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = calls.clone();
    fixture.peer.set_handler(move |method, params| match method {
        "accounts.warmup.read" => {
            let value = response.lock().unwrap().clone();
            Some(if value.is_null() { Err(RpcError::new(UNAVAILABLE, "directory unavailable")) } else { Ok(value) })
        }
        "accounts.warmup.run" => {
            captured.lock().unwrap().push(params["accountId"].as_str().unwrap().to_string());
            Some(Ok(json!({"outcome":"accepted","reasonCode":null,"status":200,"durationMs":1,"sendState":"sent","shortTerm":null})))
        }
        _ => None,
    });
    let config = groups_settings(now);
    fixture
        .service
        .dispatch(
            &fixture.kernel,
            "configure",
            json!({"expectedRevision":1,"settings":config}),
        )
        .await
        .unwrap();
    let accounts: Vec<Account> =
        serde_json::from_value(current.lock().unwrap()["accounts"].clone()).unwrap();
    let store = Store::open(&fixture.path.join("data")).unwrap();
    store.schedule(&accounts, &config, now).unwrap();
    fixture
        .service
        .dispatch(
            &fixture.kernel,
            "run",
            json!({"submissionId":"explicit","scope":"selected","accountIds":["explicit"]}),
        )
        .await
        .unwrap();
    *current.lock().unwrap() = Value::Null;
    assert_eq!(
        fixture
            .service
            .tick(&fixture.kernel, now + chrono::Duration::seconds(2))
            .await
            .unwrap_err()
            .code,
        UNAVAILABLE
    );
    assert_eq!(*calls.lock().unwrap(), vec!["explicit"]);
    assert!(
        fixture
            .service
            .tick(&fixture.kernel, now + chrono::Duration::seconds(3))
            .await
            .is_err()
    );
    assert_eq!(*calls.lock().unwrap(), vec!["explicit"]);
    let mut incapable = groups_directory(&[("automatic", &["first"]), ("explicit", &[])]);
    incapable["groupTargetsSupported"] = json!(false);
    *current.lock().unwrap() = incapable;
    assert_eq!(
        fixture
            .service
            .tick(&fixture.kernel, now + chrono::Duration::seconds(4))
            .await
            .unwrap_err()
            .code,
        NOT_SUPPORTED
    );
    assert_eq!(*calls.lock().unwrap(), vec!["explicit"]);
    fixture
        .service
        .dispatch(
            &fixture.kernel,
            "run",
            json!({"submissionId":"merged-explicit","scope":"selected","accountIds":["automatic"]}),
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .service
            .tick(&fixture.kernel, now + chrono::Duration::seconds(5))
            .await
            .unwrap_err()
            .code,
        NOT_SUPPORTED
    );
    assert_eq!(*calls.lock().unwrap(), vec!["explicit", "automatic"]);
    drop(store);
    fixture.finish().await;
}

#[tokio::test]
async fn empty_group_queue_discovers_plans_with_ttl_without_reading_directory_each_tick() {
    let fixture = Fixture::start().await;
    let reads = Arc::new(AtomicU64::new(0));
    let called = reads.clone();
    fixture.peer.set_handler(move |method, _| {
        assert_eq!(method, "accounts.warmup.read");
        called.fetch_add(1, Ordering::SeqCst);
        Some(Ok(groups_directory(&[])))
    });
    let now = Utc::now();
    fixture
        .service
        .dispatch(
            &fixture.kernel,
            "configure",
            json!({"expectedRevision":1,"settings":groups_settings(now)}),
        )
        .await
        .unwrap();
    let configured_reads = reads.load(Ordering::SeqCst);
    for _ in 0..5 {
        fixture.service.tick(&fixture.kernel, now).await.unwrap();
    }
    assert_eq!(reads.load(Ordering::SeqCst), configured_reads);
    fixture.finish().await;
}

#[tokio::test]
async fn read_invalidates_group_runtime_when_configuration_switches_during_a_filtered_directory_read()
 {
    let fixture = Fixture::start().await;
    let config = groups_settings(Utc::now());
    let data = fixture.path.join("data");
    let reads = Arc::new(AtomicU64::new(0));
    let called = reads.clone();
    fixture.peer.set_handler(move |method, _| {
        assert_eq!(method, "accounts.warmup.read");
        if called.fetch_add(1, Ordering::SeqCst) == 0 {
            Store::open(&data).unwrap().configure(1, &config).unwrap();
        }
        Some(Ok(groups_directory(&[("a", &["first"])])))
    });
    let read = fixture
        .service
        .dispatch(
            &fixture.kernel,
            "read",
            json!({"accountIds":["a"],"includeModels":true}),
        )
        .await
        .unwrap();
    assert_eq!(read["settings"]["targetMode"], "groups");
    assert_eq!(read["groupTargetsSupported"], false);
    assert_eq!(read["runtime"]["ready"], false);
    assert_eq!(read["runtime"]["nextAutomaticRunAt"], Value::Null);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let read = fixture
        .service
        .dispatch(&fixture.kernel, "read", json!({}))
        .await
        .unwrap();
    assert_eq!(read["groupTargetsSupported"], true);
    assert_eq!(read["runtime"]["ready"], true);
    assert!(read["runtime"]["nextAutomaticRunAt"].is_string());
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    fixture.finish().await;
}

#[tokio::test]
async fn persisted_group_configuration_is_not_ready_on_an_older_incapable_directory() {
    let fixture = Fixture::start().await;
    let store = Store::open(&fixture.path.join("data")).unwrap();
    store.configure(1, &groups_settings(Utc::now())).unwrap();
    fixture.peer.set_handler(|method, _| {
        assert_eq!(method, "accounts.warmup.read");
        Some(Ok(directory()))
    });
    let read = fixture
        .service
        .dispatch(&fixture.kernel, "read", json!({}))
        .await
        .unwrap();
    assert_eq!(read["settings"]["enabled"], true);
    assert_eq!(read["groupTargetsSupported"], false);
    assert_eq!(read["runtime"]["ready"], false);
    assert_eq!(read["runtime"]["nextAutomaticRunAt"], Value::Null);
    drop(store);
    fixture.finish().await;
}
