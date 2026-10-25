use async_trait::async_trait;
use futures::{executor::block_on, future::BoxFuture};
use gateway_core::{plugin::*, provider_ports::*};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
const G: &str = "grp_00000000000000000000000000000001";
const H: &str = "grp_00000000000000000000000000000002";
struct Access(Mutex<Vec<String>>);
impl PluginAccess for Access {
    fn id(&self) -> &str {
        "test"
    }
    fn groups(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
    fn fetch_hosts(&self) -> Vec<String> {
        vec!["example.invalid".into(), "other.invalid:8443".into()]
    }
}
struct Accounts(Mutex<Vec<AccountSummary>>);
#[async_trait]
impl PluginAccounts for Accounts {
    async fn read(
        &self,
        ids: Option<&[String]>,
        _model: Option<&str>,
    ) -> Result<Vec<AccountSummary>, RpcError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|account| ids.is_none_or(|ids| ids.contains(&account.id)))
            .cloned()
            .collect())
    }
}
struct Upstream;
#[async_trait]
impl PluginUpstream for Upstream {
    async fn warmup_read(
        &self,
        _: &dyn PluginAccess,
        _: AccountWarmupReadParams,
    ) -> Result<AccountWarmupReadResult, RpcError> {
        Ok(AccountWarmupReadResult { accounts: vec![] })
    }
    async fn warmup_run(
        &self,
        _: &dyn PluginAccess,
        _: AccountWarmupRunParams,
    ) -> Result<AccountWarmupRunResult, RpcError> {
        Ok(AccountWarmupRunResult {
            outcome: AccountWarmupOutcome::Accepted,
            reason_code: None,
            status: Some(200),
            duration_ms: 1,
            send_state: AccountWarmupSendState::Sent,
            short_term: None,
        })
    }
    async fn quota(&self, _: QuotaReadParams) -> Result<QuotaReadResult, RpcError> {
        Ok(QuotaReadResult {
            snapshot: json!({"quota":{"remainingPercent":90},"schedulingSignals":{"remainingRank":9000}}),
        })
    }
    async fn probe(
        &self,
        _: &dyn PluginAccess,
        _: &dyn PluginAccounts,
        _: UpstreamProbeParams,
    ) -> Result<UpstreamProbeResult, RpcError> {
        Ok(UpstreamProbeResult {
            status: Some(200),
            error_kind: None,
            state_returned: true,
            turn_state: Some("fixture".into()),
            set_cookies: vec!["fixture=1".into()],
            rate_limit_headers: vec![],
            retry_after_ms: None,
            duration_ms: 1,
        })
    }
    async fn fetch(&self, _: HttpFetchParams) -> Result<HttpFetchResult, RpcError> {
        Ok(HttpFetchResult {
            status: 200,
            headers: vec![],
            body: "fixture".into(),
            truncated: Some(false),
        })
    }
}
struct Move(Mutex<Vec<AccountGroupMove>>);
impl ProviderGroupMovePort for Move {
    fn move_account_group<'a>(
        &'a self,
        request: &'a AccountGroupMove,
    ) -> BoxFuture<'a, Result<AccountGroupMoveOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let mut requests = self.0.lock().unwrap();
            let outcome = if requests.last() == Some(request) {
                AccountGroupMoveOutcome::Unchanged
            } else {
                AccountGroupMoveOutcome::Moved
            };
            requests.push(request.clone());
            Ok(outcome)
        })
    }
}
struct Log;
#[async_trait]
impl PluginCapabilities for Log {
    async fn call(&self, _: &dyn PluginAccess, _: &str, _: Value) -> Result<Value, RpcError> {
        Ok(Value::Null)
    }
}

struct WarmupAccess {
    id: &'static str,
    granted: std::sync::atomic::AtomicBool,
}
impl PluginAccess for WarmupAccess {
    fn id(&self) -> &str {
        self.id
    }
    fn has_capability(&self, _: &str) -> bool {
        self.granted.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn groups(&self) -> Vec<String> {
        vec![]
    }
    fn fetch_hosts(&self) -> Vec<String> {
        vec![]
    }
}

#[test]
fn warmup_requires_its_own_identity_and_current_grant_without_weakening_groups() {
    block_on(async {
        let service = KernelCapabilities {
            accounts: Arc::new(Accounts(Mutex::new(vec![account("acct_out", G)]))),
            upstream: Arc::new(Upstream),
            groups: Arc::new(Move(Mutex::new(vec![]))),
            log: Arc::new(Log),
        };
        let access = WarmupAccess {
            id: ACCOUNT_WARMUP_PLUGIN,
            granted: true.into(),
        };
        let run =
            json!({"accountId":"acct_out","model":"m","attemptId":"attempt_1","timeoutMs":15000});
        assert_eq!(
            service
                .call(&access, ACCOUNT_WARMUP_RUN, run.clone())
                .await
                .unwrap()["outcome"],
            "accepted"
        );
        assert_eq!(
            service
                .call(&access, "accounts.read", json!({}))
                .await
                .unwrap()["accounts"],
            json!([])
        );
        assert_eq!(
            service
                .call(&access, "quota.read", json!({"accountId":"acct_out"}))
                .await
                .unwrap_err()
                .code,
            FORBIDDEN
        );
        access
            .granted
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            service
                .call(&access, ACCOUNT_WARMUP_RUN, run.clone())
                .await
                .unwrap_err()
                .code,
            FORBIDDEN
        );
        let other = WarmupAccess {
            id: "other",
            granted: true.into(),
        };
        assert_eq!(
            service
                .call(&other, ACCOUNT_WARMUP_READ, json!({}))
                .await
                .unwrap_err()
                .code,
            FORBIDDEN
        );
        access
            .granted
            .store(true, std::sync::atomic::Ordering::Relaxed);
        for change in [
            json!({"timeoutMs":15001}),
            json!({"attemptId":"../bad"}),
            json!({"prompt":"arbitrary"}),
        ] {
            let mut invalid = run.clone();
            invalid
                .as_object_mut()
                .unwrap()
                .extend(change.as_object().unwrap().clone());
            assert_eq!(
                service
                    .call(&access, ACCOUNT_WARMUP_RUN, invalid)
                    .await
                    .unwrap_err()
                    .code,
                INVALID_PARAMS
            );
        }
        for params in [
            json!({"includeModels":true}),
            json!({"includeModels":true,"ids":[]}),
            json!({"includeModels":true,"ids":vec!["acct_out";21]}),
            json!({"ids":vec!["acct_out";501]}),
            json!({"model":"m".repeat(129)}),
        ] {
            assert_eq!(
                service
                    .call(&access, ACCOUNT_WARMUP_READ, params)
                    .await
                    .unwrap_err()
                    .code,
                INVALID_PARAMS
            );
        }
        assert!(
            service
                .call(
                    &access,
                    ACCOUNT_WARMUP_READ,
                    json!({"includeModels":true,"ids":["acct_out"]})
                )
                .await
                .is_ok()
        );
    });
}
fn account(id: &str, group: &str) -> AccountSummary {
    AccountSummary {
        model_allowed: None,
        id: id.into(),
        enabled: true,
        status: "normal".into(),
        authentication_kind: "oauth".into(),
        groups: vec![group.into()],
        upstream_account_id: Some("upstream".into()),
        user_id: None,
    }
}
fn params(method: &str, id: &str) -> Value {
    match method {
        "quota.read" => json!({"accountId":id}),
        "upstream.probe" => {
            json!({"accountId":id,"model":"test","turnState":null,"cookie":null,"timeoutMs":1000})
        }
        "groups.move" => {
            json!({"accountId":id,"removeGroup":G,"addGroup":H,"reason":{"code":"fixture","message":"fixture"}})
        }
        "http.fetch" => {
            json!({"url":"https://example.invalid/","method":"GET","headers":[],"body":null,"egress":{"kind":"account","accountId":id},"timeoutMs":1000})
        }
        _ => unreachable!(),
    }
}
#[test]
fn capabilities_enforce_current_account_and_group_scope_and_return_contracts() {
    block_on(async {
        let access = Access(Mutex::new(vec![G.into(), H.into()]));
        let accounts = Arc::new(Accounts(Mutex::new(vec![
            account("acct_in", G),
            account("acct_out", "outside"),
        ])));
        let mover = Arc::new(Move(Mutex::new(vec![])));
        let service = KernelCapabilities {
            accounts: accounts.clone(),
            upstream: Arc::new(Upstream),
            groups: mover.clone(),
            log: Arc::new(Log),
        };
        for method in ["quota.read", "groups.move", "upstream.probe", "http.fetch"] {
            assert_eq!(
                service
                    .call(&access, method, params(method, "acct_out"))
                    .await
                    .unwrap_err()
                    .code,
                FORBIDDEN,
                "{method}"
            );
            let result = service
                .call(&access, method, params(method, "acct_in"))
                .await
                .unwrap();
            match method {
                "quota.read" => assert_eq!(
                    result["snapshot"]["schedulingSignals"]["remainingRank"],
                    9000
                ),
                "groups.move" => assert_eq!(result["outcome"], "moved"),
                "upstream.probe" => {
                    assert_eq!(result["status"], 200);
                    assert_eq!(result["turnState"], "fixture");
                }
                _ => assert_eq!(result["body"], "fixture"),
            }
        }
        assert_eq!(mover.0.lock().unwrap()[0].reason, "plugin:test:fixture");
        assert_eq!(
            service
                .call(&access, "groups.move", params("groups.move", "acct_in"))
                .await
                .unwrap()["outcome"],
            "unchanged"
        );
        for ids in [
            json!({}),
            json!({"ids":["acct_out","acct_in","acct_missing"]}),
        ] {
            let result = service.call(&access, "accounts.read", ids).await.unwrap();
            assert_eq!(result["accounts"].as_array().unwrap().len(), 1);
            assert_eq!(
                result["accounts"][0],
                serde_json::to_value(account("acct_in", G)).unwrap()
            );
            assert!(!result.to_string().contains("credential"));
        }
        access.0.lock().unwrap().retain(|group| group != H);
        assert_eq!(
            service
                .call(&access, "groups.move", params("groups.move", "acct_in"))
                .await
                .unwrap_err()
                .code,
            FORBIDDEN
        );
        let mut remove = params("groups.move", "acct_in");
        remove["addGroup"] = json!(G);
        remove["removeGroup"] = json!(H);
        assert_eq!(
            service
                .call(&access, "groups.move", remove)
                .await
                .unwrap_err()
                .code,
            FORBIDDEN
        );
        accounts.0.lock().unwrap()[0].groups = vec!["outside".into()];
        assert_eq!(
            service
                .call(&access, "quota.read", params("quota.read", "acct_in"))
                .await
                .unwrap_err()
                .code,
            FORBIDDEN
        );
        accounts.0.lock().unwrap()[0].groups = vec![G.into()];
        access.0.lock().unwrap().clear();
        assert_eq!(
            service
                .call(
                    &access,
                    "upstream.probe",
                    params("upstream.probe", "acct_in")
                )
                .await
                .unwrap_err()
                .code,
            FORBIDDEN
        );
    });
}
#[test]
fn fetch_host_scheme_port_size_and_parameter_validation() {
    block_on(async {
        let access = Access(Mutex::new(vec![G.into()]));
        let service = KernelCapabilities {
            accounts: Arc::new(Accounts(Mutex::new(vec![account("acct_in", G)]))),
            upstream: Arc::new(Upstream),
            groups: Arc::new(Move(Mutex::new(vec![]))),
            log: Arc::new(Log),
        };
        for (url, code) in [
            ("http://example.invalid", INVALID_PARAMS),
            ("https://evil.invalid", FORBIDDEN),
            ("https://example.invalid:8443", FORBIDDEN),
            ("https://user:secret@example.invalid", INVALID_PARAMS),
            ("not a URL", INVALID_PARAMS),
        ] {
            let mut p = params("http.fetch", "acct_in");
            p["url"] = json!(url);
            assert_eq!(
                service
                    .call(&access, "http.fetch", p)
                    .await
                    .unwrap_err()
                    .code,
                code
            );
        }
        let mut p = params("http.fetch", "acct_in");
        p["url"] = json!("https://other.invalid:8443/");
        p["egress"] = json!({"kind":"direct"});
        assert_eq!(
            service
                .call(&access, "http.fetch", p.clone())
                .await
                .unwrap()["status"],
            200
        );
        p["body"] = json!("x".repeat(512 * 1024));
        assert_eq!(
            service
                .call(&access, "http.fetch", p.clone())
                .await
                .unwrap()["status"],
            200
        );
        p["body"] = json!("x".repeat(512 * 1024 + 1));
        assert_eq!(
            service
                .call(&access, "http.fetch", p)
                .await
                .unwrap_err()
                .code,
            INVALID_PARAMS
        );
        for method in ["upstream.probe", "http.fetch"] {
            for ms in [0, 999, 30001] {
                let mut p = params(method, "acct_in");
                p["timeoutMs"] = json!(ms);
                assert_eq!(
                    service.call(&access, method, p).await.unwrap_err().code,
                    INVALID_PARAMS
                );
            }
        }
        let mut same_group = params("groups.move", "acct_in");
        same_group["addGroup"] = json!(G);
        assert_eq!(
            service
                .call(&access, "groups.move", same_group)
                .await
                .unwrap_err()
                .code,
            INVALID_PARAMS
        );
        for method in ["quota.read", "groups.move", "upstream.probe", "http.fetch"] {
            assert_eq!(
                service
                    .call(&access, method, json!({}))
                    .await
                    .unwrap_err()
                    .code,
                INVALID_PARAMS
            );
        }
        assert_eq!(
            service
                .call(&access, "accounts.read", json!({"ids":[""]}))
                .await
                .unwrap_err()
                .code,
            INVALID_PARAMS
        );
    });
}

#[test]
fn watch_rules_cover_all_accounts_and_exclusions_win() {
    let rules = |items: &[&str]| {
        items
            .iter()
            .map(|item| item.to_string())
            .collect::<Vec<_>>()
    };
    assert!(watch_matches(&rules(&[G]), &[G]));
    assert!(!watch_matches(&rules(&[G]), &[H]));
    assert!(watch_matches(&rules(&["*"]), &[H]));
    assert!(watch_matches(&rules(&["*"]), &[] as &[&str]));
    let excluded = format!("!{H}");
    assert!(!watch_matches(&rules(&["*", &excluded]), &[G, H]));
    assert!(!watch_matches(&rules(&[G, &excluded]), &[G, H]));
    assert!(watch_matches(&rules(&["*", &excluded]), &[G]));
}
