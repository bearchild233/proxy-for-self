//! SDK 插件通过真实 Unix socket / Host 运行时接管 Provider，所有账号与上游均为测试替身。
use super::*;
use gateway_core::plugin::*;
use gateway_host::plugin::{LogCapabilities, PluginRuntime, PluginRuntimeConfig};
use plugin_sdk::{Context, Descriptor, Plugin, RpcResult, Server, Settings};
use std::collections::VecDeque;

const BUDGET: Duration = Duration::from_secs(10);
const A: &str = "acct_provider_contract";
const B: &str = "acct_scope_new";
const C: &str = "acct_scope_old";
const BODY: &str = "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_plugin\",\"status\":\"completed\",\"output\":[]}}\n\n";

#[derive(Default)]
struct Controls {
    select: Mutex<Vec<RouteSelectParams>>,
    preferred: Mutex<Vec<String>>,
    blocked: Mutex<Vec<String>>,
    ordinary: Mutex<Vec<String>>,
    pinned: Mutex<Option<PinnedDecision>>,
    prepare: Mutex<Vec<RoutePrepareParams>>,
    prepares: Mutex<VecDeque<RoutePrepareResult>>,
    heads: Mutex<Vec<ResponseHeadParams>>,
    actions: Mutex<VecDeque<ResponseHeadResult>>,
    finishes: Mutex<Vec<RequestFinishParams>>,
    observed: Mutex<Vec<ResponseObserveParams>>,
    switch_account: std::sync::atomic::AtomicBool,
    select_error: std::sync::atomic::AtomicBool,
    observe_error: std::sync::atomic::AtomicBool,
    head_error: std::sync::atomic::AtomicBool,
    select_pending: std::sync::atomic::AtomicBool,
}
struct TestPlugin(Arc<Controls>);
impl Plugin for TestPlugin {
    async fn route_select(
        &self,
        _: Context,
        params: RouteSelectParams,
    ) -> RpcResult<RouteSelectResult> {
        self.0.select.lock().unwrap().push(params.clone());
        if self
            .0
            .select_pending
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            std::future::pending::<()>().await;
        }
        if self
            .0
            .select_error
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(RpcError::new(UNAVAILABLE, "fixture"));
        }
        let preferred = self.0.preferred.lock().unwrap().clone();
        Ok(RouteSelectResult {
            preferred: if preferred.is_empty() {
                params.accounts
            } else {
                preferred
            },
            live: vec![],
            blocked: self
                .0
                .blocked
                .lock()
                .unwrap()
                .iter()
                .map(|id| BlockedAccount {
                    account_id: id.clone(),
                    reason: reason(),
                })
                .collect(),
            pinned_decision: params.pinned.map(|_| {
                self.0
                    .pinned
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or(PinnedDecision::Stay)
            }),
            ordinary: self.0.ordinary.lock().unwrap().clone(),
        })
    }
    async fn route_prepare(
        &self,
        _: Context,
        params: RoutePrepareParams,
    ) -> RpcResult<RoutePrepareResult> {
        self.0.prepare.lock().unwrap().push(params.clone());
        Ok(self
            .0
            .prepares
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(RoutePrepareResult::Use {
                account_id: params.account_id,
                lease_id: format!("lease-{}", params.attempt),
                upstream: Upstream {
                    cookie: Some("fixture=plugin".into()),
                    turn_state: Some(format!("state-{}", params.attempt)),
                },
            }))
    }
    async fn response_head(
        &self,
        _: Context,
        params: ResponseHeadParams,
    ) -> RpcResult<ResponseHeadResult> {
        self.0.heads.lock().unwrap().push(params);
        if self.0.head_error.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(RpcError::new(UNAVAILABLE, "fixture head unavailable"));
        }
        Ok(self
            .0
            .actions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(ResponseHeadResult::Accept))
    }
    async fn response_observe(
        &self,
        _: Context,
        params: ResponseObserveParams,
    ) -> RpcResult<ResponseObserveResult> {
        self.0.observed.lock().unwrap().push(params);
        if self
            .0
            .observe_error
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(RpcError::new(UNAVAILABLE, "fixture"));
        }
        Ok(
            if self
                .0
                .switch_account
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                ResponseObserveResult::SwitchAccount { reason: reason() }
            } else {
                ResponseObserveResult::Accept
            },
        )
    }
    async fn request_finish(&self, _: Context, params: RequestFinishParams) -> RpcResult<()> {
        self.0.finishes.lock().unwrap().push(params);
        Ok(())
    }
}
fn reason() -> Reason {
    Reason {
        code: "fixture_reason".into(),
        message: "fixture".into(),
    }
}

struct Fixture {
    store: Arc<MemoryAccountStore>,
    root: tempfile::TempDir,
    runtime: Arc<PluginRuntime>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    controls: Arc<Controls>,
    provider: CodexProvider,
    server: MockServer,
    groups: Vec<(&'static str, &'static str)>,
}
impl Fixture {
    async fn new(groups: Vec<(&'static str, &'static str)>, owned: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let controls = Arc::new(Controls::default());
        let descriptor = Descriptor {
            plugin: PluginInfo {
                id: "fixture".into(),
                version: "test".into(),
            },
            kernel_api: PluginApi {
                major: 0,
                min_minor: 4,
            },
            hooks: [
                "route.select",
                "route.prepare",
                "response.head",
                "response.observe",
                "request.finish",
            ]
            .map(str::to_owned)
            .to_vec(),
            capabilities: vec![],
            owned_groups: if owned {
                vec!["grp_00000000000000000000000000000001".into()]
            } else {
                vec![]
            },
            watched_groups: if owned {
                vec![]
            } else {
                vec!["grp_00000000000000000000000000000001".into()]
            },
            shutdown_timeout: BUDGET,
        };
        let server = Server::bind(
            Settings {
                socket: root.path().join("fixture.sock"),
                data: root.path().join("data"),
            },
            descriptor,
            TestPlugin(controls.clone()),
        )
        .unwrap();
        let task = tokio::spawn(server.run_until(std::future::pending()));
        write_registry(root.path(), true);
        let runtime = Arc::new(
            PluginRuntime::start(
                PluginRuntimeConfig {
                    registry_path: Some(root.path().join("registry.json")),
                    ownership_cache_path: Some(root.path().join("ownership.json")),
                },
                KernelInfo {
                    version: "test".into(),
                    slot: None,
                },
                Arc::new(LogCapabilities),
            )
            .unwrap()
            .unwrap(),
        );
        until(|| {
            runtime
                .snapshot()
                .plugins
                .iter()
                .any(|entry| entry.state == gateway_host::plugin::ConnectionState::Available)
        })
        .await;
        let store = Arc::new(MemoryAccountStore::default());
        for (id, _) in &groups {
            create_account(&store, id).await;
        }
        let server = MockServer::start().await;
        let provider = provider_with_base_url(&store, server.uri()).with_plugin_routing(true);
        provider.set_plugin_hooks(runtime.clone());
        Self {
            store,
            root,
            runtime,
            task,
            controls,
            provider,
            server,
            groups,
        }
    }
    async fn upstream(&self, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(response)
            .mount(&self.server)
            .await;
    }

    async fn protect(&self, id: &str) {
        self.store.set_quota_protection(
            id,
            gateway_core::account::AccountQuotaProtection::new(true, 90).unwrap(),
        );
        let account = self.store.account(id).unwrap();
        let now = SystemTime::now();
        let reset = (now + BUDGET + Duration::from_secs(3600))
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.store
            .compare_and_swap_quota(QuotaObservation {
                plan_type: None,
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: OpaqueProviderData::new(
                    json!({"rate_limit": {
                        "allowed": true, "primary_window": {"used_percent": 95, "reset_at": reset}
                    }})
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
                observed_at: now,
                state: QuotaState::allowed(now),
            })
            .await
            .unwrap();
    }
    fn context(
        &self,
        id: &str,
        cancellation: CancellationToken,
        required: Option<&str>,
        pinned: Option<&str>,
        budget: PluginAttemptBudget,
    ) -> AttemptContext {
        let provider = ProviderKind::new("openai").unwrap();
        let scope = Arc::new(FrozenAccountScope::new(
            Arc::new(RuntimeAccountDirectory::new(
                self.groups
                    .iter()
                    .map(|(id, group)| {
                        (
                            ProviderAccountId::new(*id).unwrap(),
                            RuntimeAccount::new(
                                provider.clone(),
                                BTreeSet::from([
                                    gateway_core::account::scope::AccountGroupId::new(*group)
                                        .unwrap(),
                                ]),
                            ),
                        )
                    })
                    .collect(),
            )),
            ClientRoutingScope::all_accounts(),
        ));
        let continuation = pinned.map(|id| {
            ContinuationBinding::Pinned(
                NativeContinuationPin::new(
                    PreviousResponseId::new("resp_previous"),
                    PreviousResponseId::new("resp_previous"),
                    ClientApiKeyId::new("key_plugin").unwrap(),
                    provider.clone(),
                    ProviderAccountId::new(id).unwrap(),
                )
                .with_scope(gateway_core::engine::continuation::NativeContinuationScope::Persisted),
            )
        });
        AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new(id).unwrap(),
                ClientApiKeyId::new("key_plugin").unwrap(),
            )
            .with_plugin_attempt_budget(budget),
            NonZeroU32::new(1).unwrap(),
            SystemTime::now() + BUDGET,
            account_policy(),
            AccountAttemptContext::new(
                BTreeSet::new(),
                required.map(|id| ProviderAccountId::new(id).unwrap()),
                pinned.map(|id| {
                    ProviderAccountStateOwner::new(
                        provider.clone(),
                        ProviderAccountId::new(id).unwrap(),
                    )
                }),
            )
            .with_account_scope(scope),
            continuation,
            cancellation,
        )
    }
    async fn execute(
        &self,
        context: AttemptContext,
        unsafe_replay: bool,
    ) -> Result<(String, Vec<gateway_core::event::ProviderEvent>), gateway_core::error::ProviderError>
    {
        let mut stream = self
            .provider
            .execute(planned_request("openai", operation(unsafe_replay)), context)
            .await?;
        let account = stream.metadata().provider_account_id().as_str().to_owned();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event?);
        }
        Ok((account, events))
    }
    fn ordinary_context(&self) -> AttemptContext {
        self.context(
            "req_plugin",
            CancellationToken::new(),
            None,
            None,
            Default::default(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn operation(unsafe_replay: bool) -> Operation {
    let mut body = json!({"model":"gpt-5.4","input":"hello","client_metadata":{"x-codex-turn-state":"old","keep":"yes"}});
    if unsafe_replay {
        body["tools"] = json!([{"type":"web_search"}]);
    }
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_original_json(Bytes::from(body.to_string()))
            .with_context(Map::from_iter([("use_websocket".into(), json!(false))])),
    ))
}
fn successful() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(BODY)
}
fn write_registry(root: &std::path::Path, enabled: bool) {
    std::fs::write(root.join("registry.tmp"), serde_json::to_vec(&json!({"plugins":[{"id":"fixture","enabled":enabled,"socket":root.join("fixture.sock"),"grants":[]}]})).unwrap()).unwrap();
    std::fs::rename(root.join("registry.tmp"), root.join("registry.json")).unwrap();
}
async fn until(mut predicate: impl FnMut() -> bool) {
    timeout(BUDGET, async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn select_order_blocked_prepare_materials_and_finish() {
    let f = Fixture::new(
        vec![
            (A, "grp_00000000000000000000000000000001"),
            (B, "grp_00000000000000000000000000000001"),
        ],
        true,
    )
    .await;
    *f.controls.preferred.lock().unwrap() = vec![B.into(), A.into()];
    *f.controls.blocked.lock().unwrap() = vec![B.into()];
    f.upstream(successful()).await;
    let result = f.execute(f.ordinary_context(), false).await.unwrap();
    assert_eq!(result.0, A);
    until(|| !f.controls.finishes.lock().unwrap().is_empty()).await;
    let requests = f.server.received_requests().await.unwrap();
    assert_eq!(requests[0].headers["cookie"], "fixture=plugin");
    assert_eq!(requests[0].headers["x-codex-turn-state"], "state-1");
    assert_eq!(
        captured_request_body(&requests[0])["client_metadata"]["x-codex-turn-state"],
        "state-1"
    );
    let finish = f.controls.finishes.lock().unwrap()[0].clone();
    assert_eq!(finish.outcome, RequestOutcome::Completed);
    assert!(finish.first_byte_ms.is_some());
    f.controls.blocked.lock().unwrap().clear();
    assert_eq!(f.execute(f.ordinary_context(), false).await.unwrap().0, B);
}

#[tokio::test]
async fn quota_protection_reports_exhaustion_before_any_owned_or_watched_upstream_request() {
    for owned in [true, false] {
        let f = Fixture::new(vec![(A, "grp_00000000000000000000000000000001")], owned).await;
        f.protect(A).await;
        let error = f.execute(f.ordinary_context(), false).await.err().unwrap();
        assert_eq!(
            error.kind(),
            ProviderErrorKind::QuotaExhausted,
            "owned {owned}"
        );
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        assert!(f.controls.prepare.lock().unwrap().is_empty());
        assert!(f.controls.observed.lock().unwrap().is_empty());
        assert!(f.server.received_requests().await.unwrap().is_empty());
        assert_eq!(
            f.store.account(A).unwrap().quota().access(),
            QuotaAccessState::Allowed
        );
    }
}

#[tokio::test]
async fn quota_protection_skips_owned_account_and_preserves_required_account_semantics() {
    let f = Fixture::new(
        vec![
            (A, "grp_00000000000000000000000000000001"),
            (B, "grp_00000000000000000000000000000002"),
        ],
        true,
    )
    .await;
    f.protect(A).await;
    f.upstream(successful()).await;
    assert_eq!(f.execute(f.ordinary_context(), false).await.unwrap().0, B);
    assert!(f.controls.prepare.lock().unwrap().is_empty());
    let requests_before = f.server.received_requests().await.unwrap().len();
    let required = f.context(
        "req_protection_required",
        CancellationToken::new(),
        Some(A),
        None,
        Default::default(),
    );
    let error = f.execute(required, false).await.err().unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
    assert_eq!(
        f.server.received_requests().await.unwrap().len(),
        requests_before
    );
    assert!(f.controls.prepare.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ordinary_member_skips_prepare_and_uses_plain_upstream() {
    let f = Fixture::new(
        vec![
            (A, "grp_00000000000000000000000000000001"),
            (B, "grp_00000000000000000000000000000001"),
        ],
        true,
    )
    .await;
    *f.controls.blocked.lock().unwrap() = vec![B.into()];
    *f.controls.ordinary.lock().unwrap() = vec![A.into()];
    f.upstream(successful()).await;
    assert_eq!(f.execute(f.ordinary_context(), false).await.unwrap().0, A);
    assert!(f.controls.prepare.lock().unwrap().is_empty());
    let requests = f.server.received_requests().await.unwrap();
    assert!(
        requests[0]
            .headers
            .get("cookie")
            .is_none_or(|cookie| cookie != "fixture=plugin")
    );
}

#[tokio::test]
async fn prepare_wait_fail_and_head_retry_switch_fail_replay_limit() {
    for (actions, unsafe_replay, count, success, prepares) in [
        (
            vec![ResponseHeadResult::Retry { delay_ms: 1 }],
            false,
            2,
            true,
            1,
        ),
        (vec![ResponseHeadResult::Switch], false, 2, true, 2),
        (
            vec![ResponseHeadResult::Fail { reason: reason() }],
            false,
            1,
            false,
            1,
        ),
        (
            vec![ResponseHeadResult::Retry { delay_ms: 1 }],
            true,
            1,
            false,
            1,
        ),
        (vec![ResponseHeadResult::Switch; 5], false, 4, false, 4),
        (vec![ResponseHeadResult::Switch; 3], false, 4, true, 4),
        (vec![ResponseHeadResult::Switch], true, 1, false, 1),
    ] {
        let f = Fixture::new(vec![(A, "grp_00000000000000000000000000000001")], true).await;
        f.upstream(successful()).await;
        f.controls.actions.lock().unwrap().extend(actions);
        let result = f.execute(f.ordinary_context(), unsafe_replay).await;
        assert_eq!(result.is_ok(), success);
        assert_eq!(f.server.received_requests().await.unwrap().len(), count);
        assert_eq!(f.controls.prepare.lock().unwrap().len(), prepares);
        until(|| !f.controls.finishes.lock().unwrap().is_empty()).await;
        assert_eq!(
            f.controls.finishes.lock().unwrap()[0].outcome,
            if success {
                RequestOutcome::Completed
            } else {
                RequestOutcome::Failed
            }
        );
        if count == 2 {
            let requests = f.server.received_requests().await.unwrap();
            assert_eq!(
                requests[1].headers["x-codex-turn-state"],
                if prepares == 1 { "state-1" } else { "state-2" }
            );
        }
    }
    let f = Fixture::new(
        vec![
            (A, "grp_00000000000000000000000000000001"),
            (C, "grp_00000000000000000000000000000002"),
        ],
        true,
    )
    .await;
    f.upstream(successful()).await;
    f.controls
        .prepares
        .lock()
        .unwrap()
        .push_back(RoutePrepareResult::Wait {
            retry_after_ms: 1,
            reason: reason(),
        });
    assert!(f.execute(f.ordinary_context(), false).await.is_ok());
    assert_eq!(f.controls.prepare.lock().unwrap().len(), 2);
    f.controls
        .prepares
        .lock()
        .unwrap()
        .push_back(RoutePrepareResult::Fail { reason: reason() });
    let error = f.execute(f.ordinary_context(), false).await.err().unwrap();
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    let mut context = f.ordinary_context();
    context.exclude_accounts([ProviderAccountId::new(A).unwrap()]);
    assert_eq!(f.execute(context, false).await.unwrap().0, C);
}

#[tokio::test]
async fn pinned_stay_escape_and_unavailable_disabled_default() {
    let f = Fixture::new(
        vec![
            (A, "grp_00000000000000000000000000000001"),
            (C, "grp_00000000000000000000000000000002"),
        ],
        true,
    )
    .await;
    f.upstream(successful()).await;
    let pinned = || {
        f.context(
            "req_pinned",
            CancellationToken::new(),
            None,
            Some(A),
            Default::default(),
        )
    };
    assert_eq!(f.execute(pinned(), false).await.unwrap().0, A);
    *f.controls.pinned.lock().unwrap() = Some(PinnedDecision::Escape);
    assert_eq!(
        f.execute(pinned(), false).await.err().unwrap().kind(),
        ProviderErrorKind::NoEligibleAccount
    );
    f.controls
        .select_error
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(f.execute(f.ordinary_context(), false).await.unwrap().0, C);
    f.task.abort();
    until(|| {
        matches!(
            f.runtime.ownership("grp_00000000000000000000000000000001"),
            Ownership::Owned {
                available: false,
                ..
            }
        )
    })
    .await;
    assert_eq!(f.execute(f.ordinary_context(), false).await.unwrap().0, C);
    let mut context = f.ordinary_context();
    context.exclude_accounts([ProviderAccountId::new(C).unwrap()]);
    let error = f.execute(context, false).await.err().unwrap();
    assert_eq!(
        error.diagnostic().unwrap().code(),
        Some("plugin_unavailable")
    );
    write_registry(f.root.path(), false);
    until(|| f.runtime.ownership("grp_00000000000000000000000000000001") == Ownership::NotOwned)
        .await;
    let context = f.context(
        "req_disabled",
        CancellationToken::new(),
        Some(A),
        None,
        Default::default(),
    );
    assert_eq!(f.execute(context, false).await.unwrap().0, A);
}

#[tokio::test]
async fn observe_switch_guard_failure_open_and_shared_budget() {
    let f = Fixture::new(
        vec![
            (A, "grp_00000000000000000000000000000001"),
            (C, "grp_00000000000000000000000000000002"),
        ],
        false,
    )
    .await;
    f.upstream(successful()).await;
    f.controls
        .switch_account
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let budget = PluginAttemptBudget::default();
    let context = f.context(
        "req_observe",
        CancellationToken::new(),
        None,
        None,
        budget.clone(),
    );
    let error = f.execute(context, false).await.err().unwrap();
    assert!(f.controls.observed.lock().unwrap()[0].sse);
    assert_eq!(
        error.pre_delivery_retry(),
        Some(PreDeliveryRetry::AccountRotation)
    );
    let mut context = f.context(
        "req_observe",
        CancellationToken::new(),
        None,
        None,
        budget.clone(),
    );
    context.exclude_accounts([ProviderAccountId::new(A).unwrap()]);
    assert_eq!(f.execute(context, false).await.unwrap().0, C);
    let context = f.context(
        "req_fixed",
        CancellationToken::new(),
        Some(A),
        None,
        budget.clone(),
    );
    assert!(f.execute(context, false).await.is_ok());
    assert!(
        !f.controls
            .observed
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .can_switch
    );
    f.controls
        .observe_error
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(
        f.execute(
            f.context(
                "req_four",
                CancellationToken::new(),
                Some(A),
                None,
                budget.clone()
            ),
            false
        )
        .await
        .is_ok()
    );
    assert!(
        f.execute(
            f.context("req_five", CancellationToken::new(), Some(A), None, budget),
            false
        )
        .await
        .is_err()
    );
    assert_eq!(f.server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn finish_cancel_and_drop() {
    for cancel in [true, false] {
        let f = Fixture::new(vec![(A, "grp_00000000000000000000000000000001")], true).await;
        f.upstream(successful().set_delay(Duration::from_secs(2)))
            .await;
        let cancellation = CancellationToken::new();
        let mut stream = f
            .provider
            .execute(
                planned_request("openai", operation(false)),
                f.context(
                    "req_cancel",
                    cancellation.clone(),
                    None,
                    None,
                    Default::default(),
                ),
            )
            .await
            .unwrap();
        {
            let next = stream.next();
            tokio::pin!(next);
            tokio::select! {
                result = &mut next => panic!("unexpected {result:?}"),
                _ = wait_request(&f.server) => {}
            }
            if cancel {
                cancellation.cancel();
                assert!(next.await.unwrap().is_err());
            }
        }
        drop(stream);
        until(|| !f.controls.finishes.lock().unwrap().is_empty()).await;
        assert_eq!(
            f.controls.finishes.lock().unwrap()[0].outcome,
            if cancel {
                RequestOutcome::Cancelled
            } else {
                RequestOutcome::Interrupted
            }
        );
    }
}
async fn wait_request(server: &MockServer) {
    timeout(BUDGET, async {
        while server.received_requests().await.unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn absent_runtime_json_observation_and_hook_failures() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, A).await;
    let f = Fixture::new(vec![(A, "grp_00000000000000000000000000000001")], false).await;
    f.upstream(
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_json(json!({"id":"resp_json","status":"completed","output":[]})),
    )
    .await;
    let provider = provider_with_base_url(&store, f.server.uri()).with_plugin_routing(true);
    let mut stream = provider
        .execute(
            planned_request("openai", operation(false)),
            f.ordinary_context(),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    assert!(f.controls.observed.lock().unwrap().is_empty());
    assert!(f.execute(f.ordinary_context(), false).await.is_ok());
    assert_eq!(f.controls.observed.lock().unwrap().len(), 1);
    assert!(!f.controls.observed.lock().unwrap()[0].sse);

    let f = Fixture::new(vec![(A, "grp_00000000000000000000000000000001")], true).await;
    f.upstream(successful()).await;
    f.controls
        .head_error
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let error = f.execute(f.ordinary_context(), false).await.err().unwrap();
    assert_eq!(
        error.diagnostic().unwrap().code(),
        Some("plugin_unavailable")
    );
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    f.controls
        .select_pending
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let error = f.execute(f.ordinary_context(), false).await.err().unwrap();
    assert_eq!(
        error.diagnostic().unwrap().code(),
        Some("plugin_unavailable")
    );
}

#[tokio::test]
async fn cancel_prepare_wait_finishes_without_sending() {
    let f = Fixture::new(vec![(A, "grp_00000000000000000000000000000001")], true).await;
    f.controls
        .prepares
        .lock()
        .unwrap()
        .push_back(RoutePrepareResult::Wait {
            retry_after_ms: 10_000,
            reason: reason(),
        });
    let cancellation = CancellationToken::new();
    let request = f.execute(
        f.context(
            "req_wait_cancel",
            cancellation.clone(),
            None,
            None,
            Default::default(),
        ),
        false,
    );
    tokio::pin!(request);
    tokio::select! {
        _ = &mut request => panic!("wait returned before cancellation"),
        _ = until(|| !f.controls.prepare.lock().unwrap().is_empty()) => {}
    }
    cancellation.cancel();
    assert_eq!(
        request.await.err().unwrap().kind(),
        ProviderErrorKind::Cancelled
    );
    // 没有取得租约的请求不发送 request.finish；留出投递时间后确认插件没有收到通知。
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(f.controls.finishes.lock().unwrap().is_empty());
    assert!(f.server.received_requests().await.unwrap().is_empty());
}

const G1: &str = "grp_00000000000000000000000000000001";
const G2: &str = "grp_00000000000000000000000000000002";

/// 两个不同插件分别负责 G1 与 G2；同一账号同时属于两个分组时必须失败关闭。
struct ConflictHooks;
#[async_trait::async_trait]
impl PluginHooks for ConflictHooks {
    fn ownership(&self, group: &str) -> Ownership {
        let plugin = if group == G1 { "plugin-a" } else { "plugin-b" };
        Ownership::Owned {
            plugin: plugin.into(),
            available: true,
        }
    }
    async fn route_select(
        &self,
        _: &str,
        _: RouteSelectParams,
    ) -> Result<RouteSelectResult, RpcError> {
        Err(RpcError::new(
            UNAVAILABLE,
            "conflict fixture must not select",
        ))
    }
    async fn response_observe(
        &self,
        _: &str,
        _: ResponseObserveParams,
    ) -> Result<ResponseObserveResult, RpcError> {
        Err(RpcError::new(
            UNAVAILABLE,
            "conflict fixture must not observe",
        ))
    }
    async fn route_prepare(
        &self,
        _: &str,
        _: RoutePrepareParams,
    ) -> Result<RoutePrepareResult, RpcError> {
        Err(RpcError::new(
            UNAVAILABLE,
            "conflict fixture must not prepare",
        ))
    }
    async fn response_head(
        &self,
        _: &str,
        _: ResponseHeadParams,
    ) -> Result<ResponseHeadResult, RpcError> {
        Err(RpcError::new(
            UNAVAILABLE,
            "conflict fixture must not receive heads",
        ))
    }
    async fn request_finish(&self, _: &str, _: RequestFinishParams) {}
}

/// 按账号列出分组的请求上下文；用于构造单个账号同属多个分组的场景。
fn scoped_context(id: &str, accounts: &[(&str, &[&str])]) -> AttemptContext {
    let provider = ProviderKind::new("openai").unwrap();
    let directory = accounts
        .iter()
        .map(|(account, groups)| {
            (
                ProviderAccountId::new(*account).unwrap(),
                RuntimeAccount::new(
                    provider.clone(),
                    groups
                        .iter()
                        .map(|group| {
                            gateway_core::account::scope::AccountGroupId::new(*group).unwrap()
                        })
                        .collect(),
                ),
            )
        })
        .collect();
    let scope = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(directory)),
        ClientRoutingScope::all_accounts(),
    ));
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(id).unwrap(),
            ClientApiKeyId::new("key_plugin").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + BUDGET,
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
        None,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn ownership_conflict_fails_closed_and_names_both_plugins() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, A).await;
    let server = MockServer::start().await;
    let provider = provider_with_base_url(&store, server.uri()).with_plugin_routing(true);
    provider.set_plugin_hooks(Arc::new(ConflictHooks));
    let context = scoped_context("req_conflict", &[(A, &[G1, G2])]);
    let error = provider
        .execute(planned_request("openai", operation(false)), context)
        .await
        .err()
        .unwrap();
    let diagnostic = error.diagnostic().unwrap();
    assert_eq!(diagnostic.code(), Some("plugin_ownership_conflict"));
    assert_eq!(
        diagnostic.as_str(),
        "插件 plugin-a, plugin-b：账号的分组归属多个插件，已失败关闭"
    );
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn refused_retry_and_switch_report_specific_reason_codes() {
    for (actions, unsafe_replay, code, requests, text) in [
        (
            vec![ResponseHeadResult::Retry { delay_ms: 1 }],
            true,
            "plugin_retry_unsafe",
            1,
            "retry",
        ),
        (
            vec![ResponseHeadResult::Switch],
            true,
            "plugin_retry_unsafe",
            1,
            "switch",
        ),
        (
            vec![ResponseHeadResult::Switch; 5],
            false,
            "plugin_attempt_limit",
            4,
            "上限",
        ),
    ] {
        let f = Fixture::new(vec![(A, G1)], true).await;
        f.upstream(successful()).await;
        f.controls.actions.lock().unwrap().extend(actions);
        let error = f
            .execute(f.ordinary_context(), unsafe_replay)
            .await
            .err()
            .unwrap();
        let diagnostic = error.diagnostic().unwrap();
        assert_eq!(diagnostic.code(), Some(code));
        assert!(
            diagnostic.as_str().contains(text),
            "{}",
            diagnostic.as_str()
        );
        assert_eq!(f.server.received_requests().await.unwrap().len(), requests);
    }
}

#[tokio::test]
async fn response_head_observes_http_error_before_account_cooldown() {
    let f = Fixture::new(vec![(A, "grp_00000000000000000000000000000001")], true).await;
    f.upstream(
        ResponseTemplate::new(429)
            .insert_header("retry-after", "30")
            .insert_header("set-cookie", "fixture=response")
            .set_body_json(json!({"error":{"code":"rate_limit_exceeded","message":"fixture"}})),
    )
    .await;
    f.controls
        .actions
        .lock()
        .unwrap()
        .push_back(ResponseHeadResult::Fail { reason: reason() });
    let error = f.execute(f.ordinary_context(), false).await.err().unwrap();
    assert_eq!(error.diagnostic().unwrap().code(), Some("fixture_reason"));
    let heads = f.controls.heads.lock().unwrap().clone();
    assert_eq!(heads[0].status, Some(429));
    assert_eq!(heads[0].error_kind, None);
    assert_eq!(heads[0].set_cookies, vec!["fixture=response"]);
    assert!(f.execute(f.ordinary_context(), false).await.is_err());
    assert_eq!(f.server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn response_head_accept_preserves_original_non_200_error() {
    let f = Fixture::new(vec![(A, "grp_00000000000000000000000000000001")], true).await;
    let body = r#"{"error":{"code":"upstream_original","message":"original body"}}"#;
    f.upstream(
        ResponseTemplate::new(422)
            .insert_header("content-type", "application/json")
            .set_body_string(body),
    )
    .await;
    let error = f.execute(f.ordinary_context(), false).await.err().unwrap();
    assert_eq!(error.upstream_status(), Some(422));
    assert_eq!(
        error
            .client_visible_upstream_response()
            .unwrap()
            .body()
            .as_ref(),
        body.as_bytes()
    );
    assert_eq!(f.controls.heads.lock().unwrap()[0].status, Some(422));
}

#[tokio::test]
#[ignore = "manual 1000-request scheduling comparison"]
async fn scheduling_overhead_1000() {
    let f = Fixture::new(vec![(A, "grp_00000000000000000000000000000001")], true).await;
    f.upstream(successful()).await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, A).await;
    let ordinary = provider_with_base_url(&store, f.server.uri());
    for enabled in [false, true] {
        let started = std::time::Instant::now();
        for index in 0..1000 {
            let provider = if enabled { &f.provider } else { &ordinary };
            let mut stream = provider
                .execute(
                    planned_request("openai", operation(false)),
                    f.context(
                        &format!("req_bench_{enabled}_{index}"),
                        CancellationToken::new(),
                        None,
                        None,
                        Default::default(),
                    ),
                )
                .await
                .unwrap();
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
        }
        println!(
            "plugin_routing={enabled} requests=1000 elapsed_us={} mean_us={}",
            started.elapsed().as_micros(),
            started.elapsed().as_micros() / 1000
        );
    }
}
