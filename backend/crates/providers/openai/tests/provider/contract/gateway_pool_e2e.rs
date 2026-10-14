//! 真实的网关池插件进程（`plugins/gateway-pool`）通过 Unix socket 接管 OpenAI Provider 的请求路径：
//! 账号、额度与上游探测经内核能力（真实 `KernelCapabilities` + `OpenAiPluginUpstream`），
//! 上游与池服务是测试替身。主路径：打票成功 → 业务请求带上插件给的 Cookie 与 state →
//! 29 换网关 → 请求结束。
//!
//! 需要先构建插件二进制（`scripts/build-plugins-local.sh <仓库> 4096 3 build gateway-pool`），
//! 所以默认忽略（CI 的 `cargo test --test main` 不构建插件），运行时加 `-- --ignored`。
//! 路径取环境变量 `GATEWAY_POOL_PLUGIN_BIN`，缺省为 `plugins-target/debug/gateway-pool`
//! （本地构建镜像）或 `plugins/target/debug/gateway-pool`（仓库内默认 target）。
use super::*;
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use gateway_core::plugin::*;
use gateway_host::plugin::{LogCapabilities, PluginRuntime, PluginRuntimeConfig};
use provider_openai::{OpenAiPluginUpstream, transport::CodexBackendClient};
use std::path::{Path, PathBuf};

const BUDGET: Duration = Duration::from_secs(15);
const ACCOUNT: &str = "acct_gateway_pool";
const POOL_GROUP: &str = "grp_00000000000000000000000000000001";
const PROBE_MODEL: &str = "gpt-5.4";
const GOOD_BODY: &str = "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"item\",\"output_index\":0,\"content_index\":0,\"delta\":\"complete good answer\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_pool\",\"status\":\"completed\",\"output\":[]}}\n\n";
const BAD_BODY: &str = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"BAD ANSWER MUST NOT BE DELIVERED\"}\n\n";

fn plugin_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("GATEWAY_POOL_PLUGIN_BIN") {
        return path.into();
    }
    // <根>/backend/crates/providers/openai：向上四级是仓库根（本地构建镜像里同样成立）。
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("repository root");
    ["plugins-target/debug/gateway-pool", "plugins/target/debug/gateway-pool"]
        .iter()
        .map(|relative| root.join(relative))
        .find(|path| path.exists())
        .unwrap_or_else(|| {
            panic!(
                "网关池插件二进制不存在：先构建（build-plugins-local.sh ... build gateway-pool），或设置 GATEWAY_POOL_PLUGIN_BIN"
            )
        })
}

/// 子进程随测试结束被终止。
struct PluginProcess(std::process::Child);
impl Drop for PluginProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn routing_cookie(gateway: &str) -> String {
    let payload = URL_SAFE_NO_PAD.encode(
        json!({"host": format!("{gateway}.example.invalid"), "exp": Utc::now().timestamp() + 600})
            .to_string(),
    );
    format!("__oailb=h.{payload}.s; __cflb=demo")
}

fn gateway_of(cookie: &str) -> Option<String> {
    let token = cookie
        .split(';')
        .find_map(|part| part.trim().strip_prefix("__oailb="))?;
    let claims: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token.split('.').nth(1)?).ok()?).ok()?;
    claims["host"]
        .as_str()?
        .split('.')
        .next()
        .map(str::to_owned)
}

/// 账号目录：读取测试账号库，分组固定为网关池分组。
struct Directory(Arc<MemoryAccountStore>);
#[async_trait]
impl PluginAccounts for Directory {
    async fn read(
        &self,
        ids: Option<&[String]>,
        model: Option<&str>,
    ) -> Result<Vec<AccountSummary>, RpcError> {
        Ok(ids
            .unwrap_or_default()
            .iter()
            .filter_map(|id| {
                let account = self.0.account(id)?;
                Some(AccountSummary {
                    model_allowed: model.map(|model| account.model_access().allows(model)),
                    id: id.clone(),
                    enabled: account.enabled(),
                    status: "normal".into(),
                    authentication_kind: account.authentication_kind().to_owned(),
                    groups: vec![POOL_GROUP.into()],
                    upstream_account_id: account.upstream_account_id().map(str::to_owned),
                    user_id: account.upstream_user_id().map(str::to_owned),
                })
            })
            .collect())
    }
}

struct NoGroupMoves;
impl gateway_core::provider_ports::ProviderGroupMovePort for NoGroupMoves {
    fn move_account_group<'a>(
        &'a self,
        _: &'a gateway_core::provider_ports::AccountGroupMove,
    ) -> futures::future::BoxFuture<
        'a,
        Result<
            gateway_core::provider_ports::AccountGroupMoveOutcome,
            gateway_core::provider_ports::ProviderStoreError,
        >,
    > {
        Box::pin(async { Ok(gateway_core::provider_ports::AccountGroupMoveOutcome::Unchanged) })
    }
}

/// 真实的内核能力，只把 `http.fetch`（池服务）换成替身：测试地址是 http，内核对外访问要求 https 白名单。
struct Capabilities {
    inner: KernelCapabilities,
    fetches: Mutex<Vec<Value>>,
}
#[async_trait]
impl PluginCapabilities for Capabilities {
    async fn call(
        &self,
        access: &dyn PluginAccess,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        if method != "http.fetch" {
            return self.inner.call(access, method, params).await;
        }
        let url = params["url"].as_str().unwrap_or_default().to_owned();
        self.fetches.lock().unwrap().push(params);
        let (path, query) = url.split_once('?').unwrap_or((&url, ""));
        let body = if path.ends_with("/gateways") {
            let gateways = ["unified-142", "unified-143"].map(|name| {
                json!({"name": name, "pair_ready": true, "valid_for_s": 3600, "datacenter_country": "US"})
            });
            json!({ "gateways": gateways })
        } else {
            let gateway = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("gateway="))
                .unwrap_or("unified-142");
            json!({"cookie": routing_cookie(gateway), "valid_for_s": 600})
        };
        Ok(json!({"status": 200, "headers": [], "body": body.to_string(), "truncated": false}))
    }
}

async fn until(mut predicate: impl FnMut() -> bool) {
    timeout(BUDGET, async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("condition not reached in time");
}

async fn admin(runtime: &PluginRuntime, method: &str, params: Value) -> Value {
    runtime
        .admin_call(
            "gateway-pool",
            AdminCallParams {
                method: method.into(),
                params,
                actor: Actor {
                    role: "admin".into(),
                },
            },
        )
        .await
        .unwrap_or_else(|error| panic!("{method}: {error}"))
}

#[tokio::test]
#[ignore = "需要先构建 gateway-pool 插件二进制（见文件头说明），用 -- --ignored 运行"]
async fn gateway_pool_plugin_routes_a_request_with_ticket_switch_and_finish() {
    // ---- 上游替身：探测与业务请求都走 /codex/responses，按 Cookie 里的网关区分 ----
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(|request: &wiremock::Request| {
            let body = captured_request_body(request);
            let gateway = request
                .headers
                .get("cookie")
                .and_then(|value| value.to_str().ok())
                .and_then(gateway_of)
                .expect("every request carries the routing cookie");
            let state = request
                .headers
                .get("x-codex-turn-state")
                .and_then(|value| value.to_str().ok());
            if body["input"][0]["content"][0]["text"] == "hi" {
                // 打票：bootstrap 返回 state，重放不再返回（21）。
                let template = ResponseTemplate::new(200).set_body_string("not read");
                return match state {
                    None => template.insert_header("x-codex-turn-state", format!("boot-{gateway}")),
                    Some(state) => {
                        assert_eq!(state, format!("boot-{gateway}"));
                        template
                    }
                };
            }
            // 业务请求必须带插件给的 s0；unified-142 上被判降智（又返回 state），unified-143 满血。
            assert_eq!(state, Some(format!("boot-{gateway}").as_str()));
            let template =
                ResponseTemplate::new(200).insert_header("content-type", "text/event-stream");
            if gateway == "unified-142" {
                template
                    .insert_header(
                        "x-codex-turn-state",
                        "returned-state-that-must-not-replace-s0",
                    )
                    .set_body_string(BAD_BODY)
            } else {
                template.set_body_string(GOOD_BODY)
            }
        })
        .mount(&upstream)
        .await;

    // ---- 内核侧：账号、额度、Provider、插件运行时 ----
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let account = store.account(ACCOUNT).unwrap();
    let now = SystemTime::now();
    store
        .compare_and_swap_quota(QuotaObservation {
            plan_type: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            observed_at: now,
            state: QuotaState::allowed(now),
            quota: OpaqueProviderData::new(
                json!({"rate_limit": {"allowed": true, "limit_reached": false,
                    "primary_window": {"used_percent": 10, "reset_at": Utc::now().timestamp() + 18000}}})
                .as_object()
                .unwrap()
                .clone(),
            ),
        })
        .await
        .unwrap();
    let (provider, quota) = provider_and_quota_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        upstream.uri(),
        Arc::new(TestLeaseCoordinator::default()),
        u32::try_from(DEFAULT_STREAM_MAX_RETRIES).unwrap(),
    );
    let provider = provider.with_plugin_routing(true);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let capabilities = Arc::new(Capabilities {
        inner: KernelCapabilities {
            accounts: Arc::new(Directory(store.clone())),
            upstream: Arc::new(OpenAiPluginUpstream::new(
                store.clone(),
                store.repository(),
                quota,
                CodexBackendClient::new(http, upstream.uri(), wire_profile()),
            )),
            groups: Arc::new(NoGroupMoves),
            log: Arc::new(LogCapabilities),
        },
        fetches: Mutex::new(Vec::new()),
    });

    // ---- 真实的插件进程 ----
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("gateway-pool.sock");
    let _process = PluginProcess(
        std::process::Command::new(plugin_binary())
            .env("CPR_PLUGIN_SOCKET", &socket)
            .env("CPR_PLUGIN_DATA", root.path().join("data"))
            .env("CPR_PLUGIN_SHUTDOWN_MS", "3000")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn gateway-pool plugin"),
    );
    until(|| socket.exists()).await;
    std::fs::write(
        root.path().join("registry.json"),
        serde_json::to_vec(
            &json!({"plugins": [{"id": "gateway-pool", "enabled": true, "socket": socket,
            "grants": ["accounts.read", "quota.read", "upstream.probe", "http.fetch", "log"]}]}),
        )
        .unwrap(),
    )
    .unwrap();
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
            capabilities.clone(),
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
    // 管理页面的配置入口：保存设置后插件广播归属，内核据此把分组里的账号交给插件。
    admin(
        &runtime,
        "configure",
        json!({"groupIds": [POOL_GROUP], "poolUrl": "https://pool.example.invalid", "poolToken": "demo-pool-token",
            "probeIntervalMs": 10, "probeModel": PROBE_MODEL}),
    )
    .await;
    until(|| {
        runtime.ownership(POOL_GROUP)
            == Ownership::Owned {
                plugin: "gateway-pool".into(),
                available: true,
            }
    })
    .await;
    provider.set_plugin_hooks(runtime.clone());

    // ---- 一次业务请求 ----
    let provider_kind = ProviderKind::new("openai").unwrap();
    let scope = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(
            [(
                ProviderAccountId::new(ACCOUNT).unwrap(),
                RuntimeAccount::new(
                    provider_kind,
                    BTreeSet::from([
                        gateway_core::account::scope::AccountGroupId::new(POOL_GROUP).unwrap(),
                    ]),
                ),
            )]
            .into_iter()
            .collect(),
        )),
        ClientRoutingScope::all_accounts(),
    ));
    let context = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_gateway_pool_e2e").unwrap(),
            ClientApiKeyId::new("key_gateway_pool").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + BUDGET,
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
        None,
        CancellationToken::new(),
    );
    let body = json!({"model": PROBE_MODEL, "input": "hello", "client_metadata": {"x-codex-turn-state": "client-state-must-not-win", "keep": "yes"}});
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_original_json(Bytes::from(body.to_string()))
            .with_context(Map::from_iter([("use_websocket".into(), json!(false))])),
    ));
    let mut stream = provider
        .execute(planned_request("openai", operation), context)
        .await
        .expect("plugin routes the request");
    assert_eq!(stream.metadata().provider_account_id().as_str(), ACCOUNT);
    let mut answer = String::new();
    while let Some(event) = stream.next().await {
        if let Some(wire) = event.expect("stream event").wire_event() {
            answer.push_str(&wire.data().to_string());
        }
    }
    drop(stream);
    assert!(answer.contains("complete good answer"), "{answer}");
    assert!(!answer.contains("BAD ANSWER"), "{answer}");

    // ---- 上游看到的请求：每个网关各一次 bootstrap、重放和业务请求；业务请求带插件给的 Cookie 与 s0 ----
    let requests = upstream.received_requests().await.unwrap();
    let summary: Vec<(String, bool)> = requests
        .iter()
        .map(|request| {
            let gateway = gateway_of(request.headers["cookie"].to_str().unwrap()).unwrap();
            let probe = captured_request_body(request)["input"][0]["content"][0]["text"] == "hi";
            (gateway, probe)
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("unified-142".to_owned(), true),
            ("unified-142".to_owned(), true),
            ("unified-142".to_owned(), false),
            ("unified-143".to_owned(), true),
            ("unified-143".to_owned(), true),
            ("unified-143".to_owned(), false),
        ]
    );
    for request in requests
        .iter()
        .filter(|r| captured_request_body(r)["input"][0]["content"][0]["text"] != "hi")
    {
        let gateway = gateway_of(request.headers["cookie"].to_str().unwrap()).unwrap();
        assert_eq!(
            request.headers["x-codex-turn-state"],
            format!("boot-{gateway}").as_str()
        );
        let sent = captured_request_body(request);
        assert_eq!(sent["client_metadata"]["keep"], "yes");
        if let Some(state) = sent["client_metadata"].get("x-codex-turn-state") {
            assert_eq!(state, &json!(format!("boot-{gateway}")));
        }
    }
    // 池服务只被访问一次目录，每个网关取一次 Cookie，带池凭证。
    let fetches = capabilities.fetches.lock().unwrap().clone();
    assert_eq!(
        fetches
            .iter()
            .filter(|f| f["url"].as_str().unwrap().contains("/gateways?"))
            .count(),
        1
    );
    assert_eq!(
        fetches
            .iter()
            .filter(|f| f["url"].as_str().unwrap().contains("/cookie?"))
            .count(),
        2
    );
    assert!(
        fetches
            .iter()
            .all(|f| f["headers"].to_string().contains("Bearer demo-pool-token"))
    );

    // ---- 插件侧状态：29 的网关静置、新窗口就绪、两次业务访问都已记录，请求结束后租约全部归还 ----
    until_idle(&runtime).await;
    let view = admin(&runtime, "read", json!({"limit": 100})).await;
    let pairs = view["pairs"].as_array().unwrap();
    let state_of = |gateway: &str| {
        pairs
            .iter()
            .find(|pair| pair["gateway"] == gateway)
            .map(|pair| pair["state"].clone())
    };
    assert_eq!(state_of("unified-142"), Some(json!("cooling")));
    assert_eq!(state_of("unified-143"), Some(json!("ready")));
    let classifications: Vec<Value> = view["observations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["source"] == "business")
        .map(|item| item["classification"].clone())
        .collect();
    assert_eq!(classifications.len(), 2);
    assert!(classifications.contains(&json!("29")) && classifications.contains(&json!("21")));
}

/// 请求结束通知是异步投递的：等所有窗口的在途数归零。
async fn until_idle(runtime: &PluginRuntime) {
    timeout(BUDGET, async {
        loop {
            let view = admin(runtime, "read", json!({"limit": 1})).await;
            if view["pairs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|pair| pair["inFlight"] == 0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("leases were not returned");
}

/// 真实 PostgreSQL 成员关系供能力授权现读，移组由生产 PgProviderAccountRepository 执行。
struct ObservedDirectory {
    accounts: Arc<MemoryAccountStore>,
    pool: sqlx::PgPool,
}
#[async_trait]
impl PluginAccounts for ObservedDirectory {
    async fn read(
        &self,
        ids: Option<&[String]>,
        model: Option<&str>,
    ) -> Result<Vec<AccountSummary>, RpcError> {
        let mut rows = Vec::new();
        for id in ids.unwrap_or_default() {
            let Some(account) = self.accounts.account(id) else {
                continue;
            };
            let groups = sqlx::query_scalar::<_, String>(
                "SELECT account_group_id FROM account_group_accounts WHERE provider_account_id=$1",
            )
            .bind(id)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| RpcError::new(UNAVAILABLE, "directory"))?;
            rows.push(AccountSummary {
                id: id.clone(),
                enabled: account.enabled(),
                status: "normal".into(),
                authentication_kind: "oauth".into(),
                groups,
                upstream_account_id: account.upstream_account_id().map(str::to_owned),
                user_id: account.upstream_user_id().map(str::to_owned),
                model_allowed: model.map(|m| account.model_access().allows(m)),
            });
        }
        Ok(rows)
    }
}

#[tokio::test]
#[ignore = "需要 gateway-pool 插件二进制和隔离 CPR_TEST_DATABASE_URL，阶段验收必跑"]
async fn gateway_pool_plugin_observes_degradation_moves_group_and_delivers_from_other_account() {
    const NORMAL: &str = "grp_00000000000000000000000000000050";
    const SPARE: &str = "acct_normal_spare";
    let database =
        std::env::var("CPR_TEST_DATABASE_URL").expect("isolated test PostgreSQL required");
    let admin_db = sqlx::PgPool::connect(&database).await.unwrap();
    let schema = format!("cpr_pool_e2e_{}", uuid::Uuid::new_v4().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin_db)
        .await
        .unwrap();
    let search = schema.clone();
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .after_connect(move |conn, _| {
            let search = search.clone();
            Box::pin(async move {
                sqlx::query("SELECT set_config('search_path',$1,false)")
                    .bind(search)
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&database)
        .await
        .unwrap();
    sqlx::migrate!("../../../migrations")
        .run(&db)
        .await
        .unwrap();
    let store = Arc::new(MemoryAccountStore::default());
    for id in [ACCOUNT, SPARE] {
        store
            .seed_oauth_credential(ImportCodexOAuthCredential {
                account_id: id.into(),
                name: id.into(),
                secret: secret(id),
                verified_account: profile(id),
                next_refresh_at: None,
                enabled: true,
            })
            .await;
        sqlx::query("INSERT INTO provider_accounts (id,provider_kind,name,upstream_user_id,upstream_account_id,authentication_kind,provider_credentials_json,credential_revision,has_refresh_token,enabled,credential_state,credential_observed_at,created_at,updated_at) VALUES ($1,'openai',$1,$1,$1,'oauth','{}'::jsonb,1,false,true,'ready',now(),now(),now())")
            .bind(id).execute(&db).await.unwrap();
    }
    store.set_scheduling(ACCOUNT, None, AccountWeight::new(90).unwrap());
    store.set_scheduling(SPARE, None, AccountWeight::new(1).unwrap());
    for group in [NORMAL, POOL_GROUP] {
        sqlx::query("INSERT INTO account_groups (id,name,color,created_at,updated_at) VALUES ($1,$1,'#2563EBFF',now(),now())").bind(group).execute(&db).await.unwrap();
    }
    for id in [ACCOUNT, SPARE] {
        sqlx::query("INSERT INTO account_group_accounts (account_group_id,provider_account_id,created_at) VALUES ($1,$2,now())").bind(NORMAL).bind(id).execute(&db).await.unwrap();
    }
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(|r: &wiremock::Request| {
            let bad = r.headers["authorization"]
                .to_str()
                .unwrap()
                .ends_with(ACCOUNT);
            let probe = captured_request_body(r)["input"][0]["content"][0]["text"] == "hi";
            if probe {
                assert!(bad);
                assert_eq!(r.headers["x-codex-turn-state"], "returned-state-a");
                assert_eq!(r.headers["cookie"], "route=new");
            }
            let response =
                ResponseTemplate::new(200).insert_header("content-type", "text/event-stream");
            if bad {
                response
                    .insert_header("x-codex-turn-state", "returned-state-a")
                    .insert_header("set-cookie", "route=new; Path=/")
                    .set_body_string(BAD_BODY)
            } else {
                response.set_body_string(GOOD_BODY)
            }
        })
        .mount(&upstream)
        .await;
    let (provider, quota) = provider_and_quota_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        upstream.uri(),
        Arc::new(TestLeaseCoordinator::default()),
        u32::try_from(DEFAULT_STREAM_MAX_RETRIES).unwrap(),
    );
    let provider = provider.with_plugin_routing(true);
    let capabilities = Arc::new(KernelCapabilities {
        accounts: Arc::new(ObservedDirectory {
            accounts: store.clone(),
            pool: db.clone(),
        }),
        upstream: Arc::new(OpenAiPluginUpstream::new(
            store.clone(),
            store.repository(),
            quota,
            CodexBackendClient::new(
                reqwest::Client::builder().no_proxy().build().unwrap(),
                upstream.uri(),
                wire_profile(),
            ),
        )),
        groups: Arc::new(gateway_store::postgres::PgProviderAccountRepository::new(
            db.clone(),
        )),
        log: Arc::new(LogCapabilities),
    });
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("pool.sock");
    let process = PluginProcess(
        std::process::Command::new(plugin_binary())
            .env("CPR_PLUGIN_SOCKET", &socket)
            .env("CPR_PLUGIN_DATA", root.path().join("data"))
            .env("CPR_PLUGIN_SHUTDOWN_MS", "3000")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    until(|| socket.exists()).await;
    std::fs::write(root.path().join("registry.json"),serde_json::to_vec(&json!({"plugins":[{"id":"gateway-pool","enabled":true,"socket":socket,"grants":["accounts.read","quota.read","upstream.probe","groups.move","http.fetch","log"]}]})).unwrap()).unwrap();
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
            capabilities,
        )
        .unwrap()
        .unwrap(),
    );
    until(|| {
        runtime
            .snapshot()
            .plugins
            .iter()
            .any(|p| p.state == gateway_host::plugin::ConnectionState::Available)
    })
    .await;
    admin(&runtime,"configure",json!({"groupIds":[POOL_GROUP],"dynamicGroups":true,"normalGroupId":NORMAL,"degradedGroupId":POOL_GROUP,"probeModel":PROBE_MODEL})).await;
    until(|| matches!(runtime.ownership(NORMAL), Ownership::Watched { .. })).await;
    provider.set_plugin_hooks(runtime.clone());
    let budget = PluginAttemptBudget::default();
    let context = || {
        let kind = ProviderKind::new("openai").unwrap();
        let scope = Arc::new(FrozenAccountScope::new(
            Arc::new(RuntimeAccountDirectory::new(
                [ACCOUNT, SPARE]
                    .into_iter()
                    .map(|id| {
                        (
                            ProviderAccountId::new(id).unwrap(),
                            RuntimeAccount::new(
                                kind.clone(),
                                BTreeSet::from([
                                    gateway_core::account::scope::AccountGroupId::new(NORMAL)
                                        .unwrap(),
                                ]),
                            ),
                        )
                    })
                    .collect(),
            )),
            ClientRoutingScope::all_accounts(),
        ));
        AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new("req_observe_e2e").unwrap(),
                ClientApiKeyId::new("key_observe").unwrap(),
            )
            .with_plugin_attempt_budget(budget.clone()),
            NonZeroU32::new(1).unwrap(),
            SystemTime::now() + BUDGET,
            account_policy(),
            AccountAttemptContext::new(
                BTreeSet::new(),
                None,
                Some(gateway_core::engine::ProviderAccountStateOwner::new(
                    ProviderKind::new("openai").unwrap(),
                    ProviderAccountId::new(ACCOUNT).unwrap(),
                )),
            )
            .with_account_scope(scope),
            None,
            CancellationToken::new(),
        )
    };
    let operation = || {
        let body = json!({"model":PROBE_MODEL,"input":"hello","client_metadata":{"x-codex-turn-state":"original"}});
        Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
                .unwrap()
                .with_original_json(Bytes::from(body.to_string()))
                .with_context(Map::from_iter([("use_websocket".into(), json!(false))])),
        ))
    };
    let mut first = provider
        .execute(planned_request("openai", operation()), context())
        .await
        .unwrap();
    assert_eq!(first.metadata().provider_account_id().as_str(), ACCOUNT);
    let mut delivered = String::new();
    let error = loop {
        match first.next().await {
            Some(Err(error)) => break error,
            Some(Ok(e)) => {
                if let Some(w) = e.wire_event() {
                    delivered.push_str(&w.data().to_string());
                }
            }
            None => panic!("expected switchAccount"),
        }
    };
    assert!(delivered.is_empty());
    assert_eq!(
        error.pre_delivery_retry(),
        Some(PreDeliveryRetry::AccountRotation)
    );
    assert_eq!(error.diagnostic().unwrap().code(), Some("account_degraded"));
    drop(first);
    // 按 Core 的 AccountRotation 标记排除首账号，同一请求预算与冻结范围交给下一 attempt。
    let mut next = context();
    next.exclude_accounts([ProviderAccountId::new(ACCOUNT).unwrap()]);
    let mut stream = provider
        .execute(planned_request("openai", operation()), next)
        .await
        .unwrap();
    assert_eq!(stream.metadata().provider_account_id().as_str(), SPARE);
    while let Some(e) = stream.next().await {
        if let Some(w) = e.unwrap().wire_event() {
            delivered.push_str(&w.data().to_string());
        }
    }
    assert!(delivered.contains("complete good answer"));
    assert!(!delivered.contains("BAD ANSWER"));
    timeout(BUDGET, async {
        loop {
            let view = admin(&runtime, "read", json!({})).await;
            if view["groupEvents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["status"] == "moved")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let groups = sqlx::query_scalar::<_, String>(
        "SELECT account_group_id FROM account_group_accounts WHERE provider_account_id=$1",
    )
    .bind(ACCOUNT)
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(groups, vec![POOL_GROUP]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM admin_audit_events WHERE entity_ref=$1 AND action='regroup'"
        )
        .bind(ACCOUNT)
        .fetch_one(&db)
        .await
        .unwrap(),
        1
    );
    assert_eq!(upstream.received_requests().await.unwrap().len(), 3);
    runtime.shutdown().await;
    drop(process);
    drop(provider);
    drop(runtime);
    db.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin_db)
        .await
        .unwrap();
    admin_db.close().await;
}
