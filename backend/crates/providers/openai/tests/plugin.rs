use crate::support::{
    MemoryAccountStore, MemoryCooldownPort, TestLeaseCoordinator, profile, runtime_policy, secret,
};
use async_trait::async_trait;
use gateway_core::{
    account::{ProviderAccountId, ProviderAccountStore},
    plugin::*,
};
use provider_openai::{
    OpenAiPluginUpstream,
    credential::{CodexCredentialQuotaService, ImportCodexOAuthCredential},
    transport::{
        CodexBackendClient,
        profile::{CodexWireProfile, CodexWireProfileState},
    },
};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};
struct Access(Mutex<bool>);
impl PluginAccess for Access {
    fn id(&self) -> &str {
        "fixture"
    }
    fn groups(&self) -> Vec<String> {
        if *self.0.lock().unwrap() {
            vec!["g".into()]
        } else {
            vec![]
        }
    }
    fn fetch_hosts(&self) -> Vec<String> {
        vec![]
    }
}
struct Directory;
#[async_trait]
impl PluginAccounts for Directory {
    async fn read(&self, ids: Option<&[String]>) -> Result<Vec<AccountSummary>, RpcError> {
        Ok(ids
            .unwrap()
            .iter()
            .map(|id| AccountSummary {
                id: id.clone(),
                enabled: true,
                status: "normal".into(),
                authentication_kind: "oauth".into(),
                groups: vec!["g".into()],
                upstream_account_id: None,
                user_id: None,
            })
            .collect())
    }
}
struct Fixture {
    upstream: Arc<OpenAiPluginUpstream>,
    store: Arc<MemoryAccountStore>,
    cooldown: Arc<MemoryCooldownPort>,
    quota: Arc<CodexCredentialQuotaService>,
}
impl Fixture {
    async fn new(base: String) -> Self {
        let store = Arc::new(MemoryAccountStore::default());
        for id in [
            "acct_a",
            "acct_b",
            "acct_c",
            "acct_d",
            "acct_e",
            "acct_disabled",
        ] {
            store
                .seed_oauth_credential(ImportCodexOAuthCredential {
                    account_id: id.into(),
                    name: id.into(),
                    secret: secret("fixture-token"),
                    verified_account: profile("fixture-upstream"),
                    next_refresh_at: None,
                    enabled: id != "acct_disabled",
                })
                .await;
        }
        let mut restricted = provider_openai::credential::CodexCredentialAdmin
            .prepare_import(ImportCodexOAuthCredential {
                account_id: "acct_restricted".into(),
                name: "restricted".into(),
                secret: secret("fixture-token"),
                verified_account: profile("fixture-upstream"),
                next_refresh_at: None,
                enabled: true,
            })
            .unwrap();
        restricted.account = restricted.account.with_model_access(
            gateway_core::account::AccountModelAccess::new(
                gateway_core::account::AccountModelAccessMode::Denylist,
                vec!["gpt-test".into()],
            )
            .unwrap(),
        );
        store.create_account(restricted).await.unwrap();
        store
            .create_account(gateway_core::account::NewProviderAccount {
                account: gateway_core::account::ProviderAccount::new(
                    ProviderAccountId::new("acct_xai").unwrap(),
                    gateway_core::routing::ProviderKind::new("xai").unwrap(),
                    "xai".into(),
                    None,
                    "oauth".into(),
                    gateway_core::account::CredentialRevision::new(1).unwrap(),
                    None,
                ),
                credential: gateway_core::account::PlaintextCredential::new(
                    json!({}).as_object().unwrap().clone(),
                ),
                model_access: None,
            })
            .await
            .unwrap();
        store
            .seed_api_key(
                "acct_api",
                base.clone(),
                provider_openai::credential::ApiKeyTransport::Http,
            )
            .await;
        let profile = CodexWireProfileState::new(CodexWireProfile {
            codex_version: "1.2.3".into(),
            desktop_version: "1.2.3".into(),
            desktop_build: "1".into(),
            ..CodexWireProfile::default()
        });
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let cooldown = Arc::new(MemoryCooldownPort::new());
        let quota = Arc::new(CodexCredentialQuotaService::new(
            store.repository(),
            profile.clone(),
            http.clone(),
            base.clone(),
            cooldown.clone(),
            Arc::new(TestLeaseCoordinator::default()),
            runtime_policy(),
        ));
        let upstream = Arc::new(OpenAiPluginUpstream::new(
            store.clone(),
            store.repository(),
            quota.clone(),
            CodexBackendClient::new(http, base, profile),
        ));
        Self {
            upstream,
            store,
            cooldown,
            quota,
        }
    }
}
fn probe(id: &str, timeout_ms: u64) -> UpstreamProbeParams {
    UpstreamProbeParams {
        account_id: id.into(),
        model: "gpt-test".into(),
        turn_state: Some("fixture-state".into()),
        cookie: Some("fixture=1".into()),
        timeout_ms,
    }
}
async fn wait_requests(server: &MockServer, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.received_requests().await.unwrap().len() < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn probe_success_maps_headers_uses_hi_and_persists_passive_quota() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-codex-turn-state", "new-state")
                .insert_header("set-cookie", "session=test")
                .insert_header("x-codex-primary-used-percent", "25")
                .insert_header("x-codex-primary-window-minutes", "300")
                .insert_header("x-codex-primary-reset-after-seconds", "600"),
        )
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    let result = f
        .upstream
        .probe(
            &Access(Mutex::new(true)),
            &Directory,
            probe("acct_a", 30000),
        )
        .await
        .unwrap();
    assert_eq!(result.status, Some(200));
    assert_eq!(result.error_kind, None);
    assert!(result.state_returned);
    assert_eq!(result.turn_state.as_deref(), Some("new-state"));
    assert_eq!(result.set_cookies, vec!["session=test"]);
    assert!(!result.rate_limit_headers.is_empty());
    assert_eq!(result.retry_after_ms, None);
    let requests = server.received_requests().await.unwrap();
    let request = &requests[0];
    assert!(
        uuid::Uuid::parse_str(request.headers.get("session-id").unwrap().to_str().unwrap()).is_ok()
    );
    assert_eq!(
        request.headers.get("chatgpt-account-id").unwrap(),
        "fixture-upstream"
    );
    assert_eq!(request.headers.get("cookie").unwrap(), "fixture=1");
    assert_eq!(
        request.headers.get("x-codex-turn-state").unwrap(),
        "fixture-state"
    );
    let body = zstd::stream::decode_all(std::io::Cursor::new(&request.body)).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["input"][0]["content"][0]["text"], "hi");
    let credential = f
        .store
        .repository()
        .load_runtime_credential(&f.store.account("acct_a").unwrap())
        .await
        .unwrap();
    assert_eq!(
        body["client_metadata"]["x-codex-installation-id"],
        credential.installation_id
    );
    let first_session = request.headers.get("session-id").unwrap().clone();
    f.upstream
        .probe(
            &Access(Mutex::new(true)),
            &Directory,
            probe("acct_a", 30000),
        )
        .await
        .unwrap();
    assert_ne!(
        server.received_requests().await.unwrap()[1]
            .headers
            .get("session-id")
            .unwrap(),
        &first_session
    );

    assert!(f.store.has_quota("acct_a"));
    let expected = f
        .quota
        .read_account(&ProviderAccountId::new("acct_a").unwrap())
        .await
        .unwrap()
        .unwrap();
    let result = f
        .upstream
        .quota(QuotaReadParams {
            account_id: "acct_a".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        result.snapshot["quota"]["remainingPercent"],
        expected.fact().remaining_percent().unwrap()
    );
    assert_eq!(
        result.snapshot["schedulingSignals"]["remainingRank"],
        f.quota
            .scheduling_signals(&f.store.account("acct_a").unwrap())
            .unwrap()
            .remaining_rank()
            .unwrap()
    );
}
#[tokio::test]
async fn probe_429_applies_retry_after_cooldown() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    let result = f
        .upstream
        .probe(
            &Access(Mutex::new(true)),
            &Directory,
            probe("acct_a", 30000),
        )
        .await
        .unwrap();
    assert_eq!(result.status, Some(429));
    assert_eq!(result.retry_after_ms, Some(7000));
    assert!(
        f.cooldown
            .cooldowns
            .lock()
            .unwrap()
            .contains_key(&ProviderAccountId::new("acct_a").unwrap())
    );
}
#[tokio::test]
async fn probe_no_response_and_account_eligibility() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let f = Fixture::new(base).await;
    let result = f
        .upstream
        .probe(&Access(Mutex::new(true)), &Directory, probe("acct_a", 1000))
        .await
        .unwrap();
    assert_eq!(result.status, None);
    assert_eq!(result.error_kind.as_deref(), Some("connect"));
    assert!(!result.state_returned);
    assert!(result.set_cookies.is_empty());
    for id in ["acct_disabled", "acct_api", "acct_restricted"] {
        assert_eq!(
            f.upstream
                .probe(&Access(Mutex::new(true)), &Directory, probe(id, 1000))
                .await
                .unwrap_err()
                .code,
            FORBIDDEN
        );
    }
    assert_eq!(
        f.upstream
            .probe(
                &Access(Mutex::new(true)),
                &Directory,
                probe("acct_xai", 1000)
            )
            .await
            .unwrap_err()
            .code,
        NOT_SUPPORTED
    );
    assert_eq!(
        f.upstream
            .quota(QuotaReadParams {
                account_id: "acct_xai".into()
            })
            .await
            .unwrap_err()
            .code,
        NOT_SUPPORTED
    );
    assert_eq!(
        f.upstream
            .probe(
                &Access(Mutex::new(false)),
                &Directory,
                probe("acct_a", 1000)
            )
            .await
            .unwrap_err()
            .code,
        FORBIDDEN
    );
}
#[tokio::test]
async fn probe_account_queue_times_out_or_resumes_without_exceeding_two() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let upstream = f.upstream.clone();
        tasks.push(tokio::spawn(async move {
            upstream
                .probe(
                    &Access(Mutex::new(true)),
                    &Directory,
                    probe("acct_a", 10000),
                )
                .await
                .unwrap()
        }));
    }
    wait_requests(&server, 2).await;
    let error = f
        .upstream
        .probe(&Access(Mutex::new(true)), &Directory, probe("acct_a", 1000))
        .await
        .unwrap_err();
    assert_eq!(
        (error.code.as_str(), error.message.as_str()),
        (UNAVAILABLE, "probe_busy")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    let queued = f
        .upstream
        .probe(
            &Access(Mutex::new(true)),
            &Directory,
            probe("acct_a", 10000),
        )
        .await
        .unwrap();
    assert_eq!(queued.status, Some(200));
    assert!(queued.duration_ms >= 4000);
    for task in tasks {
        assert_eq!(task.await.unwrap().status, Some(200));
    }
}
#[tokio::test]
async fn probe_global_limit_is_eight_and_waiters_recheck_scope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    let mut tasks = Vec::new();
    for id in ["acct_a", "acct_b", "acct_c", "acct_d"] {
        for _ in 0..2 {
            let upstream = f.upstream.clone();
            tasks.push(tokio::spawn(async move {
                upstream
                    .probe(&Access(Mutex::new(true)), &Directory, probe(id, 10000))
                    .await
                    .unwrap()
            }));
        }
    }
    wait_requests(&server, 8).await;
    let error = f
        .upstream
        .probe(&Access(Mutex::new(true)), &Directory, probe("acct_e", 1000))
        .await
        .unwrap_err();
    assert_eq!(error.message, "probe_busy");
    assert_eq!(server.received_requests().await.unwrap().len(), 8);
    let access = Arc::new(Access(Mutex::new(true)));
    let waiting_access = access.clone();
    let upstream = f.upstream.clone();
    let waiter = tokio::spawn(async move {
        upstream
            .probe(waiting_access.as_ref(), &Directory, probe("acct_e", 10000))
            .await
    });
    *access.0.lock().unwrap() = false;
    assert_eq!(waiter.await.unwrap().unwrap_err().code, FORBIDDEN);
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 8);
}
#[tokio::test]
async fn probe_network_timeout_is_reported_as_no_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    let result = f
        .upstream
        .probe(&Access(Mutex::new(true)), &Directory, probe("acct_a", 1000))
        .await
        .unwrap();
    assert_eq!(result.status, None);
    assert_eq!(result.error_kind.as_deref(), Some("timeout"));
}
fn fetch(url: String) -> HttpFetchParams {
    HttpFetchParams {
        url,
        method: HttpMethod::Get,
        headers: vec![("Authorization".into(), "Bearer plugin-fixture".into())],
        body: None,
        egress: Egress::Direct,
        timeout_ms: 30000,
    }
}
#[tokio::test]
async fn fetch_truncates_body_preserves_plugin_headers_and_does_not_follow_redirects() {
    let server = MockServer::start().await;
    Mock::given(path("/large"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(512 * 1024 + 100)))
        .mount(&server)
        .await;
    Mock::given(path("/redirect"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/target", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(path("/target"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    // 仅在测试中直接调用 HTTP 适配器；生产 RPC 必须经过 Core 的 HTTPS/主机授权检查。
    let result = f
        .upstream
        .fetch(fetch(format!("{}/large", server.uri())))
        .await
        .unwrap();
    assert_eq!(result.status, 200);
    assert_eq!(result.body.len(), 512 * 1024);
    assert_eq!(result.truncated, Some(true));
    let result = f
        .upstream
        .fetch(fetch(format!("{}/redirect", server.uri())))
        .await
        .unwrap();
    assert_eq!(result.status, 302);
    assert_eq!(result.truncated, Some(false));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer plugin-fixture"
    );
}

#[tokio::test]
async fn probe_protocol_failure_is_reported_without_sensitive_details() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 8192];
        assert!(stream.read(&mut buffer).await.unwrap() > 0);
        stream.write_all(b"not-http-at-all\r\n\r\n").await.unwrap();
    });
    let f = Fixture::new(base).await;
    let result = f
        .upstream
        .probe(&Access(Mutex::new(true)), &Directory, probe("acct_a", 1000))
        .await
        .unwrap();
    assert_eq!(result.status, None);
    assert_eq!(result.error_kind.as_deref(), Some("protocol"));
    server.await.unwrap();
}

#[tokio::test]
async fn fetch_account_egress_uses_proxy_without_account_credentials() {
    let direct = MockServer::start().await;
    let proxy = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("proxied"))
        .mount(&proxy)
        .await;
    let f = Fixture::new(direct.uri()).await;
    f.store.set_egress(
        "acct_a",
        Some(gateway_core::account::OutboundProxy::parse(&proxy.uri()).unwrap()),
        None,
    );
    let mut request = fetch(direct.uri());
    request.headers.clear();
    request.egress = Egress::Account {
        account_id: "acct_a".into(),
    };
    let result = f.upstream.fetch(request).await.unwrap();
    assert_eq!(result.body, "proxied");
    assert!(direct.received_requests().await.unwrap().is_empty());
    let requests = proxy.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key("authorization"));
    assert!(!requests[0].headers.contains_key("chatgpt-account-id"));
}
