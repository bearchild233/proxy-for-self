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

struct WarmupPolicy;
impl gateway_core::provider_ports::ProviderRuntimePolicyPort for WarmupPolicy {
    fn load_account_selection_policy(
        &self,
    ) -> futures::future::BoxFuture<
        '_,
        Result<
            gateway_core::account::AccountSelectionPolicy,
            gateway_core::provider_ports::ProviderStoreError,
        >,
    > {
        Box::pin(async {
            Ok(gateway_core::account::AccountSelectionPolicy::new(
                gateway_core::account::RotationStrategy::Smart,
                gateway_core::account::AccountConcurrency::new(3),
                Duration::from_millis(17),
            ))
        })
    }
    fn load_refresh_policy(
        &self,
    ) -> futures::future::BoxFuture<
        '_,
        Result<
            gateway_core::provider_ports::ProviderRefreshPolicy,
            gateway_core::provider_ports::ProviderStoreError,
        >,
    > {
        Box::pin(async { runtime_policy().load_refresh_policy().await })
    }
}

struct WarmupAccess(std::sync::atomic::AtomicBool);
impl WarmupAccess {
    fn granted() -> Self {
        Self(true.into())
    }
}
impl PluginAccess for WarmupAccess {
    fn id(&self) -> &str {
        ACCOUNT_WARMUP_PLUGIN
    }
    fn has_capability(&self, _: &str) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn groups(&self) -> Vec<String> {
        vec![]
    }
    fn fetch_hosts(&self) -> Vec<String> {
        vec![]
    }
}

fn warmup(id: &str) -> AccountWarmupRunParams {
    AccountWarmupRunParams {
        account_id: id.into(),
        model: "gpt-test".into(),
        timeout_ms: 15000,
        attempt_id: "warmup_1".into(),
    }
}

fn warmup_models_response() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"models":[{"slug":"gpt-test","display_name":"Test model","supported_reasoning_levels":[{"effort":"minimal"},{"effort":"high"}]}]}))
}

async fn mount_warmup_models(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(warmup_models_response())
        .mount(server)
        .await;
}

#[tokio::test]
async fn warmup_uses_one_hi_normal_lease_and_real_secondary_five_hour_window() {
    let server = MockServer::start().await;
    mount_warmup_models(&server).await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-codex-turn-state", "secret-state")
                .insert_header("set-cookie", "secret=material")
                .insert_header("x-codex-primary-used-percent", "5")
                .insert_header("x-codex-primary-window-minutes", "10080")
                .insert_header("x-codex-primary-reset-after-seconds", "86400")
                .insert_header("x-codex-secondary-used-percent", "1")
                .insert_header("x-codex-secondary-window-minutes", "300")
                .insert_header("x-codex-secondary-reset-after-seconds", "1200"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    let result = f
        .upstream
        .warmup_run(&WarmupAccess::granted(), warmup("acct_a"))
        .await
        .unwrap();
    assert_eq!(result.outcome, AccountWarmupOutcome::Accepted);
    assert_eq!(result.send_state, AccountWarmupSendState::Sent);
    let short = result.short_term.as_ref().unwrap();
    assert_eq!(short.window_seconds, 18000);
    assert_eq!(short.used_percent, Some(1.0));
    assert!(
        chrono::DateTime::parse_from_rfc3339(short.reset_at.as_ref().unwrap()).unwrap()
            > chrono::Utc::now()
    );
    let serialized = serde_json::to_string(&result).unwrap();
    for secret in [
        "secret-state",
        "secret=material",
        "turnState",
        "setCookies",
        "rateLimitHeaders",
    ] {
        assert!(!serialized.contains(secret));
    }
    let requests = server.received_requests().await.unwrap();
    let posts: Vec<_> = requests
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .collect();
    assert_eq!(posts.len(), 1);
    assert!(posts[0].headers.get("cookie").is_none());
    assert!(posts[0].headers.get("x-codex-turn-state").is_none());
    assert!(posts[0].headers.get("session-id").is_none());
    let body = zstd::stream::decode_all(std::io::Cursor::new(&posts[0].body)).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["input"][0]["content"][0]["text"], "hi");
    assert_eq!(body["reasoning"]["effort"], "minimal");
    assert_eq!(body["store"], false);
    assert_eq!(body["tools"], json!([]));
    let leases = f.warmup_leases.requests.lock().unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].account_id().as_str(), "acct_a");
    assert_eq!(leases[0].max_concurrent().get(), 3);
    assert_eq!(leases[0].request_interval(), Duration::from_millis(17));
}

#[tokio::test]
async fn warmup_skips_disabled_unsupported_and_policy_denied_accounts_without_network() {
    let server = MockServer::start().await;
    let f = Fixture::new(server.uri()).await;
    for (id, reason) in [
        ("acct_disabled", "disabled"),
        ("acct_api", "unsupported"),
        ("acct_xai", "unsupported"),
        ("acct_restricted", "model_not_allowed"),
    ] {
        let result = f
            .upstream
            .warmup_run(&WarmupAccess::granted(), warmup(id))
            .await
            .unwrap();
        assert_eq!(result.outcome, AccountWarmupOutcome::Skipped);
        assert_eq!(result.reason_code.as_deref(), Some(reason));
        assert_eq!(result.send_state, AccountWarmupSendState::NotSent);
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(f.warmup_leases.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn warmup_honors_cooldown_and_quota_protection_before_catalog_or_send() {
    let server = MockServer::start().await;
    let f = Fixture::new(server.uri()).await;
    let account = f.store.account("acct_a").unwrap();
    f.quota
        .apply_rate_limit_429(
            &account,
            Some(Duration::from_secs(60)),
            std::time::SystemTime::now(),
        )
        .await
        .unwrap();
    let cooled = f
        .upstream
        .warmup_run(&WarmupAccess::granted(), warmup("acct_a"))
        .await
        .unwrap();
    assert_eq!(cooled.outcome, AccountWarmupOutcome::Skipped);
    assert_eq!(cooled.reason_code.as_deref(), Some("rate_limited"));
    f.store.set_quota_protection(
        "acct_b",
        gateway_core::account::AccountQuotaProtection::new(true, 90).unwrap(),
    );
    let account = f.store.account("acct_b").unwrap();
    let now = std::time::SystemTime::now();
    let reset = (now + Duration::from_secs(600))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    f.store.compare_and_swap_quota(gateway_core::account::QuotaObservation { plan_type: None, account_id: account.id().clone(), expected_revision: account.revision(), quota: gateway_core::account::OpaqueProviderData::new(json!({"rate_limit":{"allowed":true,"primary_window":{"used_percent":90,"limit_window_seconds":18000,"reset_at":reset}}}).as_object().unwrap().clone()), observed_at: now, state: gateway_core::account::QuotaState::allowed(now) }).await.unwrap();
    let protected = f
        .upstream
        .warmup_run(&WarmupAccess::granted(), warmup("acct_b"))
        .await
        .unwrap();
    assert_eq!(protected.reason_code.as_deref(), Some("quota_protection"));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn warmup_busy_lease_or_unknown_model_never_sends_and_missing_reset_is_not_invented() {
    let server = MockServer::start().await;
    mount_warmup_models(&server).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    *f.warmup_leases.busy.lock().unwrap() = true;
    let busy = f
        .upstream
        .warmup_run(&WarmupAccess::granted(), warmup("acct_a"))
        .await
        .unwrap();
    assert_eq!(busy.reason_code.as_deref(), Some("account_busy"));
    *f.warmup_leases.busy.lock().unwrap() = false;
    let mut unsupported = warmup("acct_a");
    unsupported.model = "gpt-missing".into();
    let missing = f
        .upstream
        .warmup_run(&WarmupAccess::granted(), unsupported)
        .await
        .unwrap();
    assert_eq!(missing.reason_code.as_deref(), Some("model_unsupported"));
    let result = f
        .upstream
        .warmup_run(&WarmupAccess::granted(), warmup("acct_a"))
        .await
        .unwrap();
    assert_eq!(result.outcome, AccountWarmupOutcome::Accepted);
    assert_eq!(result.short_term, None);
}

struct RevokeWarmupOnCatalog(Arc<WarmupAccess>);
impl wiremock::Respond for RevokeWarmupOnCatalog {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        self.0.0.store(false, std::sync::atomic::Ordering::Relaxed);
        warmup_models_response()
    }
}

#[tokio::test]
async fn warmup_rechecks_maintenance_grant_after_catalog_and_lease() {
    let server = MockServer::start().await;
    let access = Arc::new(WarmupAccess::granted());
    Mock::given(method("GET"))
        .respond_with(RevokeWarmupOnCatalog(access.clone()))
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    let error = f
        .upstream
        .warmup_run(access.as_ref(), warmup("acct_a"))
        .await
        .unwrap_err();
    assert_eq!(error.code, FORBIDDEN);
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() == "GET")
    );
}

#[tokio::test]
async fn warmup_timeout_is_unknown_and_429_updates_cooldown_without_retry() {
    let server = MockServer::start().await;
    mount_warmup_models(&server).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1300)))
        .expect(1)
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    let mut request = warmup("acct_a");
    request.timeout_ms = 1000;
    let result = f
        .upstream
        .warmup_run(&WarmupAccess::granted(), request)
        .await
        .unwrap();
    assert_eq!(result.outcome, AccountWarmupOutcome::Unknown);
    assert_eq!(result.send_state, AccountWarmupSendState::Ambiguous);
    let limited = MockServer::start().await;
    mount_warmup_models(&limited).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
        .expect(1)
        .mount(&limited)
        .await;
    let f = Fixture::new(limited.uri()).await;
    let result = f
        .upstream
        .warmup_run(&WarmupAccess::granted(), warmup("acct_a"))
        .await
        .unwrap();
    assert_eq!(result.status, Some(429));
    assert_eq!(result.outcome, AccountWarmupOutcome::Failed);
    let again = f
        .upstream
        .warmup_run(&WarmupAccess::granted(), warmup("acct_a"))
        .await
        .unwrap();
    assert_eq!(again.outcome, AccountWarmupOutcome::Skipped);
}

#[tokio::test]
async fn warmup_directory_has_labels_and_no_personal_or_credential_fields() {
    let server = MockServer::start().await;
    mount_warmup_models(&server).await;
    let f = Fixture::new(server.uri()).await;
    let result = f
        .upstream
        .warmup_read(
            &WarmupAccess::granted(),
            AccountWarmupReadParams {
                ids: Some(vec!["acct_a".into(), "acct_xai".into()]),
                model: Some("gpt-test".into()),
                include_models: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result.accounts[0].models.as_ref().unwrap()[0].label,
        "Test model"
    );
    assert!(!result.accounts[1].supported);
    let serialized = serde_json::to_string(&result).unwrap();
    for field in [
        "upstreamAccountId",
        "userId",
        "groups",
        "credential",
        "fixture-token",
        "fixture-upstream",
    ] {
        assert!(!serialized.contains(field));
    }
}

#[tokio::test]
async fn warmup_rejects_authentication_and_official_quota_and_never_defaults_failed_policy_to_unlimited()
 {
    use gateway_core::account::{
        AccountStateChange, CredentialState, QuotaAccessChange, QuotaEvidence, QuotaState,
    };
    let server = MockServer::start().await;
    let f = Fixture::new(server.uri()).await;
    let invalid = f.store.account("acct_a").unwrap();
    f.store
        .apply_state_change(AccountStateChange {
            account_id: invalid.id().clone(),
            expected_revision: invalid.revision(),
            credential_state: CredentialState::Invalid,
            observed_at: std::time::SystemTime::now(),
            error_reason: CredentialState::Invalid.error_reason(),
            message: None,
        })
        .await
        .unwrap();
    assert_eq!(
        f.upstream
            .warmup_run(&WarmupAccess::granted(), warmup("acct_a"))
            .await
            .unwrap()
            .reason_code
            .as_deref(),
        Some("error")
    );
    let exhausted = f.store.account("acct_b").unwrap();
    f.store
        .apply_quota_access(QuotaAccessChange {
            account_id: exhausted.id().clone(),
            expected_revision: exhausted.revision(),
            state: QuotaState::exhausted(
                QuotaEvidence::AccountLimitReached,
                std::time::SystemTime::now(),
                Some(std::time::SystemTime::now() + Duration::from_secs(300)),
            ),
        })
        .await
        .unwrap();
    assert_eq!(
        f.upstream
            .warmup_run(&WarmupAccess::granted(), warmup("acct_b"))
            .await
            .unwrap()
            .reason_code
            .as_deref(),
        Some("quota_exhausted")
    );
    let missing_policy = Fixture::with_warmup_policy(server.uri(), runtime_policy()).await;
    let result = missing_policy
        .upstream
        .warmup_run(&WarmupAccess::granted(), warmup("acct_a"))
        .await
        .unwrap();
    assert_eq!(result.outcome, AccountWarmupOutcome::Skipped);
    assert_eq!(result.send_state, AccountWarmupSendState::NotSent);
    assert_eq!(
        result.reason_code.as_deref(),
        Some("account_selection_policy")
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn warmup_without_new_window_headers_does_not_confirm_a_preexisting_timer() {
    let server = MockServer::start().await;
    mount_warmup_models(&server).await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-codex-primary-used-percent", "1")
                .insert_header("x-codex-primary-window-minutes", "300")
                .insert_header("x-codex-primary-reset-after-seconds", "1200"),
        )
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    let first = f
        .upstream
        .warmup_run(&WarmupAccess::granted(), warmup("acct_a"))
        .await
        .unwrap();
    assert!(first.short_term.is_some());
    server.reset().await;
    mount_warmup_models(&server).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let mut request = warmup("acct_a");
    request.attempt_id = "warmup_2".into();
    let second = f
        .upstream
        .warmup_run(&WarmupAccess::granted(), request)
        .await
        .unwrap();
    assert_eq!(second.outcome, AccountWarmupOutcome::Accepted);
    assert_eq!(second.short_term, None);
    let known = f
        .upstream
        .warmup_read(
            &WarmupAccess::granted(),
            AccountWarmupReadParams {
                ids: Some(vec!["acct_a".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(known.accounts[0].short_term.is_some());
}
#[tokio::test]
async fn quota_refresh_if_missing_uses_account_egress_and_preserves_cached_snapshot() {
    let direct = MockServer::start().await;
    let proxy = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{"used_percent":10,"reset_at":2000000000}}
    }))).mount(&proxy).await;
    let f = Fixture::new(direct.uri()).await;
    f.store.set_egress(
        "acct_a",
        Some(gateway_core::account::OutboundProxy::parse(&proxy.uri()).unwrap()),
        None,
    );
    let old = f
        .upstream
        .quota(QuotaReadParams {
            account_id: "acct_a".into(),
            refresh_if_missing: false,
        })
        .await
        .unwrap();
    assert!(old.snapshot["quota"].is_null());
    assert!(proxy.received_requests().await.unwrap().is_empty());
    for _ in 0..2 {
        let result = f
            .upstream
            .quota(QuotaReadParams {
                account_id: "acct_a".into(),
                refresh_if_missing: true,
            })
            .await
            .unwrap();
        assert!(!result.snapshot["quota"].is_null());
    }
    assert_eq!(proxy.received_requests().await.unwrap().len(), 1);
    assert!(direct.received_requests().await.unwrap().is_empty());
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&direct)
        .await;
    assert_eq!(
        f.upstream
            .quota(QuotaReadParams {
                account_id: "acct_b".into(),
                refresh_if_missing: true
            })
            .await
            .unwrap_err()
            .code,
        UNAVAILABLE
    );
}
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
    async fn read(
        &self,
        ids: Option<&[String]>,
        _model: Option<&str>,
    ) -> Result<Vec<AccountSummary>, RpcError> {
        Ok(ids
            .unwrap()
            .iter()
            .map(|id| AccountSummary {
                model_allowed: None,
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
    warmup_leases: Arc<TestLeaseCoordinator>,
}
impl Fixture {
    async fn new(base: String) -> Self {
        Self::with_warmup_policy(base, Arc::new(WarmupPolicy)).await
    }
    async fn with_warmup_policy(
        base: String,
        warmup_policy: Arc<dyn gateway_core::provider_ports::ProviderRuntimePolicyPort>,
    ) -> Self {
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
        let catalog = Arc::new(
            provider_openai::credential::CodexCredentialCatalogService::new(
                store.repository(),
                profile.clone(),
                http.clone(),
                base.clone(),
                crate::support::catalog_cache(),
            ),
        );
        let warmup_leases = Arc::new(TestLeaseCoordinator::default());
        let upstream = Arc::new(
            OpenAiPluginUpstream::new(
                store.clone(),
                store.repository(),
                quota.clone(),
                CodexBackendClient::new(http, base, profile),
            )
            .with_warmup(warmup_leases.clone(), warmup_policy, catalog),
        );
        Self {
            upstream,
            store,
            cooldown,
            quota,
            warmup_leases,
        }
    }
}
fn probe(id: &str, timeout_ms: u64) -> UpstreamProbeParams {
    UpstreamProbeParams {
        account_id: id.into(),
        model: "gpt-test".into(),
        turn_state: Some("fixture-state".into()),
        cookie: Some("fixture=1".into()),
        include_account_cookies: false,
        timeout_ms,
        request_id: None,
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
async fn probe_uses_distinct_window_limits_and_ignores_expired_and_independent_buckets() {
    for (short_used, weekly_used, expired, blocked) in [
        (94.999, 96.999, false, false),
        (95.0, 96.0, false, true),
        (94.0, 97.0, false, true),
        (99.0, 96.0, true, false),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let f = Fixture::new(server.uri()).await;
        f.store.set_quota_protection(
            "acct_a",
            gateway_core::account::AccountQuotaProtection::new(true, 90)
                .unwrap()
                .with_window_limits(95, 97)
                .unwrap(),
        );
        let account = f.store.account("acct_a").unwrap();
        let now = std::time::SystemTime::now();
        let reset = |time: std::time::SystemTime| {
            time.duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        };
        let short_reset = if expired {
            now - Duration::from_secs(60)
        } else {
            now + Duration::from_secs(3600)
        };
        f.store.compare_and_swap_quota(gateway_core::account::QuotaObservation {
            plan_type: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: gateway_core::account::OpaqueProviderData::new(json!({
                "rate_limit": {"allowed": true,
                    "primary_window": {"used_percent": weekly_used, "limit_window_seconds": 604_800,
                        "reset_at": reset(now + Duration::from_secs(86400))},
                    "secondary_window": {"used_percent": short_used, "limit_window_seconds": 18_000,
                        "reset_at": reset(short_reset)}},
                "code_review_rate_limit": {"primary_window": {"used_percent": 100,
                    "limit_window_seconds": 18_000, "reset_at": reset(now + Duration::from_secs(3600))}}
            }).as_object().unwrap().clone()),
            observed_at: now,
            state: gateway_core::account::QuotaState::allowed(now),
        }).await.unwrap();
        let result = f
            .upstream
            .probe(
                &Access(Mutex::new(true)),
                &Directory,
                probe("acct_a", 30000),
            )
            .await;
        if blocked {
            let error = result.unwrap_err();
            assert_eq!(
                (error.code.as_str(), error.message.as_str()),
                (UNAVAILABLE, "quota_exhausted")
            );
            assert!(server.received_requests().await.unwrap().is_empty());
        } else {
            assert_eq!(result.unwrap().status, Some(200));
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
        assert_eq!(
            f.store.account("acct_a").unwrap().quota().access(),
            gateway_core::account::QuotaAccessState::Allowed
        );
    }
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
    // 与旧网关池探测一致：不带会话头，请求 ID 未指定时由内核生成。
    assert!(request.headers.get("session-id").is_none());
    assert!(
        uuid::Uuid::parse_str(
            request
                .headers
                .get("x-client-request-id")
                .unwrap()
                .to_str()
                .unwrap()
        )
        .is_ok()
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
    // 插件传入的请求 ID 原样作为上游请求 ID，bootstrap 与重放因此属于同一次打票。
    f.upstream
        .probe(
            &Access(Mutex::new(true)),
            &Directory,
            UpstreamProbeParams {
                request_id: Some("probe_fixture-1".into()),
                ..probe("acct_a", 30000)
            },
        )
        .await
        .unwrap();
    let second = &server.received_requests().await.unwrap()[1];
    assert!(second.headers.get("session-id").is_none());
    assert_eq!(
        second.headers.get("x-client-request-id").unwrap(),
        "probe_fixture-1"
    );
    assert!(
        f.upstream
            .probe(
                &Access(Mutex::new(true)),
                &Directory,
                UpstreamProbeParams {
                    request_id: Some("bad id".into()),
                    ..probe("acct_a", 30000)
                },
            )
            .await
            .is_err()
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
            refresh_if_missing: false,
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
                refresh_if_missing: false,
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
    assert_eq!(result.body.len(), 512 * 1024 - 2);
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
async fn fetch_limits_json_escaped_body_with_invalid_utf8_at_character_boundaries() {
    let server = MockServer::start().await;
    let bytes = [b'"', b'\\', 0, 1, 0xff, b'\n'].repeat(100_000);
    Mock::given(path("/escaped"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    let result = f
        .upstream
        .fetch(fetch(format!("{}/escaped", server.uri())))
        .await
        .unwrap();
    assert_eq!(result.truncated, Some(true));
    assert!(result.body.contains('\u{fffd}'));
    let encoded = serde_json::to_vec(&result.body).unwrap();
    assert!(encoded.len() <= 512 * 1024);
    assert!(encoded.len() > 512 * 1024 - 6);
    let frame = Message::Response {
        id: 1,
        payload: ResponsePayload::Result(serde_json::to_value(result).unwrap()),
    };
    assert!(serde_json::to_vec(&frame).unwrap().len() < 1024 * 1024);
}

#[tokio::test]
async fn probe_merges_account_cookies_only_when_requested() {
    use provider_openai::credential::{CodexCredentialAdmin, CodexCredentialCodec};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(""))
        .mount(&server)
        .await;
    let f = Fixture::new(server.uri()).await;
    let mut account = CodexCredentialAdmin
        .prepare_import(ImportCodexOAuthCredential {
            account_id: "acct_cookie".into(),
            name: "fixture".into(),
            secret: secret("fixture"),
            verified_account: profile("fixture"),
            next_refresh_at: None,
            enabled: true,
        })
        .unwrap();
    account.credential = CodexCredentialCodec::encode_new(
        &secret("fixture"),
        &profile("fixture"),
        vec![
            provider_openai::credential::CodexCookie {
                name: "own".into(),
                value: "account".into(),
                domain: "127.0.0.1".into(),
                path: "/".into(),
                host_only: true,
                secure: false,
                expires_at: None,
            },
            provider_openai::credential::CodexCookie {
                name: "fixture".into(),
                value: "old".into(),
                domain: "127.0.0.1".into(),
                path: "/".into(),
                host_only: true,
                secure: false,
                expires_at: None,
            },
        ],
    )
    .unwrap();
    f.store.create_account(account).await.unwrap();
    for include in [false, true] {
        let mut params = probe("acct_cookie", 1000);
        params.include_account_cookies = include;
        f.upstream
            .probe(&Access(Mutex::new(true)), &Directory, params)
            .await
            .unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].headers["cookie"], "fixture=1");
    assert_eq!(requests[1].headers["cookie"], "own=account; fixture=1");
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
