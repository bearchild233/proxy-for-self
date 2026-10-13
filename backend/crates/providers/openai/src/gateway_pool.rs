//! 网关池传输桥。插件持久化冷却及配套路由材料；上游账号凭据只留在 Provider。
use crate::{
    credential::{CodexCredentialQuotaService, CodexCredentialRepository},
    transport::{
        CodexBackendClient, CodexRequestContext, protocol::responses::CodexResponsesRequest,
    },
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::{
    account::{ProviderAccount, ProviderAccountId},
    engine::AttemptContext,
};
use reqwest::{Client, redirect::Policy};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct GatewayPoolConfig {
    pub runtime_url: String,
    pub key_file: PathBuf,
    /// 另一个控制面槽的私有入口；控制面 A/B 轮换后无需改网关配置。
    #[serde(default)]
    pub fallback_url: Option<String>,
}
impl GatewayPoolConfig {
    pub(crate) fn validate(&self) -> Result<(), ()> {
        for value in std::iter::once(&self.runtime_url).chain(self.fallback_url.as_ref()) {
            let url = url::Url::parse(value).map_err(|_| ())?;
            if url.scheme() != "http"
                || url
                    .host_str()
                    .is_none_or(|host| host != "127.0.0.1" && host != "[::1]")
                || url.username() != ""
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(());
            }
        }
        if !self.key_file.is_absolute() {
            return Err(());
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Settings {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    group_ids: Vec<String>,
    #[serde(default)]
    pool_account_ids: HashMap<String, String>,
    #[serde(default)]
    pool_url: String,
    #[serde(default)]
    pool_token: String,
    #[serde(default)]
    probe_interval_ms: u64,
    /// 打票用模型；业务请求固定使用由它取得并验证的 S0。
    #[serde(default = "default_probe_model")]
    probe_model: String,
    /// 取网关目录与 Cookie 时走 VPS 直连；打票、业务与额度请求仍走账号出口。
    #[serde(default = "default_pool_direct")]
    pool_direct: bool,
}
fn default_pool_direct() -> bool {
    true
}
fn default_probe_model() -> String {
    "gpt-6.1-sol".into()
}
fn hi_request(model: &str) -> Option<CodexResponsesRequest> {
    Some(CodexResponsesRequest::from_body(json!({"model":model,"input":[{"role":"user","content":[{"type":"input_text","text":"hi"}]}],"instructions":"Do not use external tools.","tools":[],"stream":true,"store":false,"reasoning":{"effort":"low"}}).as_object().cloned()?))
}

#[derive(Clone, Deserialize, Serialize)]
struct Pair {
    gateway: String,
    generation: String,
}
/// 同一组合窗口内其他模型的票：只拿 state，不单独判定，随组合代次失效。
#[derive(Clone)]
struct Ticket {
    state: SecretString,
    cookie: SecretString,
    expires: SystemTime,
}
#[derive(Clone)]
struct Material {
    pair: Pair,
    state: SecretString,
    cookie: SecretString,
    expires: SystemTime,
}

/// UTC 起点用于跨请求合并，耗时只用单调时钟；在 RPC 前固定终态时间。
struct AccessTiming {
    id: String,
    at: f64,
    started: Instant,
    ended: Option<(Duration, &'static str)>,
}
impl AccessTiming {
    fn new(id: String) -> Self {
        Self {
            id,
            at: epoch(SystemTime::now()),
            started: Instant::now(),
            ended: None,
        }
    }
    fn end(&mut self, outcome: &'static str) {
        self.ended
            .get_or_insert_with(|| (self.started.elapsed(), outcome));
    }
    fn value(&self) -> Value {
        let (elapsed, outcome) = self
            .ended
            .unwrap_or_else(|| (self.started.elapsed(), "headers"));
        json!({"id":self.id,"startedAt":self.at,"elapsedMs":elapsed.as_millis(),"ended":self.ended.is_some(),"outcome":outcome})
    }
}

/// 当前 Key 的降智账号集合与可用队列；普通账号由原有选择器处理。
#[derive(Default)]
pub(crate) struct GatewayPoolSelection {
    pub members: Vec<ProviderAccountId>,
    pub preferred: Vec<ProviderAccountId>,
}

pub(crate) struct GatewayPoolService {
    http: Client,
    url: String,
    fallback_url: Option<String>,
    /// 最近一次可用的是备用入口。
    prefer_fallback: std::sync::atomic::AtomicBool,
    key: SecretString,
    repository: CodexCredentialRepository,
    quota: Arc<CodexCredentialQuotaService>,
    client: CodexBackendClient,
    materials: Mutex<HashMap<String, Material>>,
    tickets: Mutex<HashMap<String, Ticket>>,
    /// 按池成员缓存的网关目录，60 秒内复用，避免每次找票都查询池服务。
    catalogs: Mutex<HashMap<String, (Instant, Value)>>,
    /// 近期经过网关池的账号；持续模式只为它们后台轮换，不扫描全部账号。
    active: Mutex<HashMap<String, Instant>>,
    ticket_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    settings: Mutex<Option<(Instant, Settings)>>,
    probes: Arc<Semaphore>,
    #[cfg(feature = "gateway-pool-experiment")]
    direct_pool_experiment: std::sync::atomic::AtomicBool,
}

impl GatewayPoolService {
    pub(crate) async fn new(
        config: &GatewayPoolConfig,
        repository: CodexCredentialRepository,
        client: CodexBackendClient,
        quota: Arc<CodexCredentialQuotaService>,
    ) -> Result<Arc<Self>, ()> {
        config.validate()?;
        let key = tokio::fs::read_to_string(&config.key_file)
            .await
            .map_err(|_| ())?;
        if key.trim().len() != 64 || !key.trim().bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(());
        }
        Ok(Arc::new(Self {
            http: Client::builder()
                .no_proxy()
                .redirect(Policy::none())
                .timeout(Duration::from_secs(3))
                .build()
                .map_err(|_| ())?,
            url: config.runtime_url.clone(),
            fallback_url: config.fallback_url.clone(),
            prefer_fallback: std::sync::atomic::AtomicBool::new(false),
            key: SecretString::from(key.trim().to_owned()),
            repository,
            quota,
            client,
            materials: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
            catalogs: Mutex::new(HashMap::new()),
            ticket_locks: Mutex::new(HashMap::new()),
            settings: Mutex::new(None),
            probes: Arc::new(Semaphore::new(4)),
            #[cfg(feature = "gateway-pool-experiment")]
            direct_pool_experiment: std::sync::atomic::AtomicBool::new(false),
        }))
    }
    async fn call(&self, value: Value) -> Result<Value, ()> {
        use std::sync::atomic::Ordering;
        // 先用最近可用的入口；连接失败（控制面已轮换到另一槽）时改用另一个并记住。
        let fallback = self.fallback_url.as_deref();
        let use_fallback = fallback.is_some() && self.prefer_fallback.load(Ordering::Relaxed);
        let (first, second) = if use_fallback {
            (fallback.unwrap_or(&self.url), Some(self.url.as_str()))
        } else {
            (self.url.as_str(), fallback)
        };
        let send = |url: &str| {
            self.http
                .post(url)
                .bearer_auth(self.key.expose_secret())
                .json(&value)
                .send()
        };
        let response = match send(first).await {
            Ok(response) => response,
            Err(error) if error.is_connect() && second.is_some() => {
                let response = send(second.unwrap_or_default()).await.map_err(|_| ())?;
                self.prefer_fallback.store(!use_fallback, Ordering::Relaxed);
                response
            }
            Err(_) => return Err(()),
        };
        if response.status() != 200 {
            return Err(());
        }
        let bytes = response.bytes().await.map_err(|_| ())?;
        if bytes.len() > 131072 {
            return Err(());
        }
        serde_json::from_slice(&bytes).map_err(|_| ())
    }
    async fn settings(&self) -> Result<Settings, ()> {
        let mut held = self.settings.lock().await;
        if let Some((at, settings)) = &*held
            && at.elapsed() < Duration::from_millis(250)
        {
            return Ok(settings.clone());
        }
        let settings: Settings =
            serde_json::from_value(self.call(json!({"action":"config"})).await?).map_err(|_| ())?;
        *held = Some((Instant::now(), settings.clone()));
        Ok(settings)
    }
    pub(crate) async fn background(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
    ) -> Result<(), ()> {
        let settings = self.settings().await?;
        if !settings.enabled || settings.mode != "continuous" {
            return Ok(());
        }
        let active = {
            let mut active = self.active.lock().await;
            active.retain(|_, seen| seen.elapsed() < Duration::from_secs(1800));
            active
                .keys()
                .cloned()
                .collect::<std::collections::HashSet<_>>()
        };
        let accounts = self
            .repository
            .store()
            .list_accounts()
            .await
            .map_err(|_| ())?
            .into_iter()
            .filter(|account| {
                account.provider().as_str() == "openai"
                    && account.authentication_kind() == "oauth"
                    && account.enabled()
                    && (active.contains(account.id().as_str())
                        || settings
                            .pool_account_ids
                            .contains_key(account.id().as_str()))
            })
            .collect::<Vec<_>>();
        let ordered = self.ordered_accounts(&accounts).await?;
        for id in ordered {
            if let Some(account) = accounts.iter().find(|account| account.id() == &id) {
                self.start_prepare(account.clone(), false, cancellation.clone())
                    .await?;
            }
        }
        Ok(())
    }
    fn pool_client<'a>(
        &'a self,
        account_client: &'a CodexBackendClient,
        settings: &Settings,
    ) -> &'a CodexBackendClient {
        #[cfg(feature = "gateway-pool-experiment")]
        if self
            .direct_pool_experiment
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return &self.client;
        }
        if settings.pool_direct {
            &self.client
        } else {
            account_client
        }
    }
    /// 读取池服务目录，只保留可配对的网关；失败返回空值，控制面退回限定列表或上一份目录。
    async fn catalog(&self, account: &ProviderAccount, settings: &Settings) -> Value {
        let Some(identity) = pool_member_id(settings, account) else {
            return Value::Null;
        };
        if let Some((at, value)) = self.catalogs.lock().await.get(&identity)
            && at.elapsed() < Duration::from_secs(60)
        {
            return value.clone();
        }
        let Ok(client) = self.client.for_account(account) else {
            return Value::Null;
        };
        let Ok(response) = self
            .pool_client(&client, settings)
            .gateway_pool_catalog(&settings.pool_url, &settings.pool_token, &identity)
            .await
        else {
            return Value::Null;
        };
        let entries: Vec<Value> = response["gateways"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| entry["pair_ready"] == true)
            .filter_map(|entry| {
                let gateway = entry["name"].as_str()?;
                let valid = entry["valid_for_s"].as_f64()?;
                let country = entry["datacenter_country"]
                    .as_str()
                    .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()));
                (gateway.strip_prefix("unified-").is_some_and(|n| {
                    !n.is_empty() && n.len() <= 6 && n.bytes().all(|b| b.is_ascii_digit())
                }) && valid.is_finite()
                    && (0.0..=86400.0).contains(&valid))
                .then(|| json!({"gateway":gateway,"validForS":valid,"country":country}))
            })
            .take(512)
            .collect();
        let value = Value::Array(entries);
        let mut catalogs = self.catalogs.lock().await;
        catalogs.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(600));
        catalogs.insert(identity, (Instant::now(), value.clone()));
        value
    }
    /// 按冻结目录中的实际成员身份分类，直接选账号与多分组使用相同规则。
    pub(crate) async fn selection(
        &self,
        context: &AttemptContext,
    ) -> Result<GatewayPoolSelection, ()> {
        let Some(scope) = context.account_scope() else {
            return Ok(GatewayPoolSelection::default());
        };
        let (settings, available) = match self.settings().await {
            Ok(settings) => (settings, true),
            Err(()) => (
                self.settings
                    .lock()
                    .await
                    .as_ref()
                    .map(|(_, settings)| settings.clone())
                    .ok_or(())?,
                false,
            ),
        };
        if !settings.enabled {
            return Ok(GatewayPoolSelection::default());
        }
        let accounts = self
            .repository
            .store()
            .list_accounts()
            .await
            .map_err(|_| ())?
            .into_iter()
            .filter(|account| {
                account.provider().as_str() == "openai"
                    && account.authentication_kind() == "oauth"
                    && scope.allows(account.id())
                    && scope
                        .directory()
                        .account(account.id())
                        .is_some_and(|entry| {
                            entry.group_ids().iter().any(|group| {
                                settings.group_ids.iter().any(|id| id == group.as_str())
                            })
                        })
            })
            .collect::<Vec<_>>();
        let members = accounts
            .iter()
            .map(|account| account.id().clone())
            .collect::<Vec<_>>();
        // 控制面不可用时池账号失败关闭，已确认的普通账号仍能按原策略使用。
        let preferred = if available && !accounts.is_empty() {
            self.ordered_accounts(&accounts)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|id| members.contains(id))
                .collect()
        } else {
            Vec::new()
        };
        Ok(GatewayPoolSelection { members, preferred })
    }
    async fn ordered_accounts(
        &self,
        accounts: &[ProviderAccount],
    ) -> Result<Vec<ProviderAccountId>, ()> {
        self.quota.prepare_scheduling(accounts).await;
        let mut candidates = Vec::new();
        for account in accounts {
            let mut exhausted = account.status_projection(SystemTime::now(), None).status
                == gateway_core::account::AccountStatus::QuotaExhausted;
            if !exhausted
                && self
                    .quota
                    .scheduling_signals(account)
                    .is_some_and(|signals| signals.remaining_rank() == Some(0))
            {
                exhausted = self
                    .quota
                    .read_account(account.id())
                    .await
                    .map_err(|_| ())?
                    .is_some_and(|snapshot| snapshot.quota().is_exhausted());
            }
            candidates.push(json!({"id":account.id().as_str(),"exhausted":exhausted}));
        }
        let value = self
            .call(json!({"action":"active","candidates":candidates}))
            .await?;
        let accounts = value["accounts"]
            .as_array()
            .ok_or(())?
            .iter()
            .filter_map(|id| id.as_str())
            .filter_map(|id| gateway_core::account::ProviderAccountId::new(id).ok())
            .collect();
        Ok(accounts)
    }
    async fn start_prepare(
        self: &Arc<Self>,
        account: ProviderAccount,
        demand: bool,
        cancellation: CancellationToken,
    ) -> Result<(), ()> {
        let Ok(permit) = Arc::clone(&self.probes).try_acquire_owned() else {
            return Ok(());
        };
        let Some(account) = self.eligible_account(&account).await? else {
            return Ok(());
        };
        let settings = self.settings().await?;
        if !settings.enabled {
            return Err(());
        }
        let claim = self
            .call(json!({"action":"claim","accountId":account.id().as_str(),"demand":demand,"catalog":self.catalog(&account, &settings).await}))
            .await?;
        if claim["claimed"] == true {
            let service = Arc::clone(self);
            tokio::spawn(async move {
                let _permit = permit;
                let mut claim = claim;
                let mut settings = settings;
                let queue_turn = claim["queueTurn"].clone();
                loop {
                    if service
                        .prepare(account.clone(), claim, settings.clone(), &cancellation)
                        .await
                        .is_err()
                    {
                        break;
                    }
                    if cancellation.is_cancelled() {
                        break;
                    }
                    // 候选失败后 50ms 即尝试下一组合，不等待下一次 1s Worker 周期。
                    tokio::time::sleep(Duration::from_millis(
                        settings.probe_interval_ms.clamp(1, 1000),
                    ))
                    .await;
                    let Ok(next_settings) = service.settings().await else {
                        break;
                    };
                    if !next_settings.enabled || !demand && next_settings.mode != "continuous" {
                        break;
                    }
                    settings = next_settings;
                    if !matches!(service.eligible_account(&account).await, Ok(Some(_))) {
                        break;
                    }
                    let catalog = service.catalog(&account, &settings).await;
                    let Ok(next) = service.call(json!({"action":"claim","accountId":account.id().as_str(),"demand":demand,"catalog":catalog,"queueTurn":queue_turn})).await else { break; };
                    if next["claimed"] != true {
                        break;
                    }
                    claim = next;
                }
            });
        }
        Ok(())
    }
    pub(crate) async fn route(
        self: &Arc<Self>,
        account: &ProviderAccount,
        deadline: SystemTime,
        cancellation: &CancellationToken,
    ) -> Result<GatewayPoolGuard, ()> {
        let settings = self.settings().await?;
        if !settings.enabled || pool_member_id(&settings, account).is_none() {
            return Err(());
        }
        self.active
            .lock()
            .await
            .insert(account.id().as_str().to_owned(), Instant::now());
        let mut queue_turn = Value::Null;
        loop {
            if self.eligible_account(account).await?.is_none() {
                return Err(());
            }
            if SystemTime::now() >= deadline {
                return Err(());
            }
            let plan = self
                .call(json!({"action":"plan","accountId":account.id().as_str(),"demand":true,"catalog":self.catalog(account, &settings).await}))
                .await?;
            if plan["yieldAccount"] == true
                || (!queue_turn.is_null() && plan["queueTurn"] != queue_turn)
            {
                return Err(());
            }
            queue_turn = plan["queueTurn"].clone();
            if !plan["probe"].is_null() {
                self.start_prepare(account.clone(), true, cancellation.clone())
                    .await?;
            }
            if let Ok(pair) = serde_json::from_value::<Pair>(plan["ready"].clone()) {
                let key = format!("{}:{}", account.id().as_str(), pair.generation);
                let material = self
                    .materials
                    .lock()
                    .await
                    .get(&key)
                    .cloned()
                    .filter(|m| m.expires > SystemTime::now());
                let material = if material.is_some() {
                    material
                } else {
                    // A/B 与重启共用私有材料，不因另一个进程的内存缓存缺失而退役可用组合。
                    let shared = self.call(json!({"action":"material","accountId":account.id().as_str(),"generation":pair.generation})).await?;
                    shared["state"]
                        .as_str()
                        .zip(shared["cookie"].as_str())
                        .and_then(|(state, cookie)| {
                            let expires = SystemTime::UNIX_EPOCH.checked_add(
                                Duration::try_from_secs_f64(shared["expires"].as_f64()?).ok()?,
                            )?;
                            (expires > SystemTime::now()
                                && cookie_gateway(cookie).as_deref() == Some(pair.gateway.as_str()))
                            .then(|| Material {
                                pair: pair.clone(),
                                state: SecretString::from(state.to_owned()),
                                cookie: SecretString::from(cookie.to_owned()),
                                expires,
                            })
                        })
                };
                if let Some(material) = material {
                    let reservation = self.call(json!({"action":"reserve","accountId":account.id().as_str(),"gateway":pair.gateway,"generation":pair.generation,"deadline":epoch(deadline)})).await?;
                    if reservation["reserved"] == true {
                        return Ok(GatewayPoolGuard {
                            service: Arc::clone(self),
                            account: account.id().as_str().to_owned(),
                            lease: reservation["leaseId"].as_str().ok_or(())?.to_owned(),
                            material,
                            model: settings.probe_model.clone(),
                            ticket: None,
                            timing: std::sync::Mutex::new(None),
                            finished: false,
                        });
                    }
                } else {
                    // 重启或 A/B 切换时不能把仅有持久化状态、没有配套材料的组合当成可用。
                    let reservation = self.call(json!({"action":"reserve","accountId":account.id().as_str(),"gateway":pair.gateway,"generation":pair.generation,"deadline":epoch(deadline)})).await?;
                    if let Some(lease) = reservation["leaseId"].as_str() {
                        self.call(json!({"action":"invalidate","accountId":account.id().as_str(),"leaseId":lease})).await?;
                        self.call(json!({"action":"finish","accountId":account.id().as_str(),"leaseId":lease})).await?;
                    }
                }
            } else if plan["probe"].is_null() && plan["preparing"] != true {
                // 所有候选均冷却，不绕过静置；交给原账号选择器尝试其他授权账号。
                return Err(());
            }
            tokio::time::sleep(Duration::from_millis(
                settings.probe_interval_ms.clamp(1, 1000),
            ))
            .await;
        }
    }
    async fn prepare(
        self: &Arc<Self>,
        account: ProviderAccount,
        claim: Value,
        settings: Settings,
        cancellation: &CancellationToken,
    ) -> Result<(), ()> {
        let lease = claim["leaseId"].as_str().ok_or(())?.to_owned();
        let pair: Pair = serde_json::from_value(claim["pair"].clone()).map_err(|_| ())?;
        let started = Instant::now();
        let preparation_started_at = epoch(SystemTime::now());
        let result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            result = tokio::time::timeout(Duration::from_secs(35), self.prepare_material(&account, &pair, &lease, &settings)) => result.ok().and_then(Result::ok),
        };
        let observation = result.as_ref().map(|(material, status, returned)| json!({"status":status,"stateReturned":returned,"paired":material.is_some(),"gatewayMatches":material.is_some()})).unwrap_or_else(|| json!({"status":null,"stateReturned":false,"paired":false,"gatewayMatches":false,"failed":true}));
        let private_material = result.as_ref().and_then(|(material, status, returned)| (*status == 200 && !returned).then_some(material.as_ref()).flatten()).map(|material| json!({"state":material.state.expose_secret(),"cookie":material.cookie.expose_secret(),"expires":epoch(material.expires)}));
        if let Some((Some(material), 200, false)) = result {
            let mut materials = self.materials.lock().await;
            materials.retain(|_, m| m.expires > SystemTime::now());
            if materials.len() >= 512
                && let Some(key) = materials.keys().next().cloned()
            {
                materials.remove(&key);
            }
            materials.insert(
                format!("{}:{}", account.id().as_str(), pair.generation),
                material,
            );
        }
        let report = self.call(json!({"action":"probeResult","accountId":account.id().as_str(),"leaseId":lease,"durationMs":started.elapsed().as_millis(),"preparationStartedAt":preparation_started_at,
            "status":observation["status"],"stateReturned":observation["stateReturned"],"paired":observation["paired"],"gatewayMatches":observation["gatewayMatches"],"failed":observation["failed"],"material":private_material})).await;
        let finish = self
            .call(json!({"action":"finish","accountId":account.id().as_str(),"leaseId":lease}))
            .await;
        report?;
        finish?;
        Ok(())
    }
    async fn prepare_material(
        &self,
        account: &ProviderAccount,
        pair: &Pair,
        lease: &str,
        settings: &Settings,
    ) -> Result<(Option<Material>, u16, bool), ()> {
        let account = self.eligible_account(account).await?.ok_or(())?;
        let account = &account;
        let credential = self
            .repository
            .load_runtime_credential(account)
            .await
            .map_err(|_| ())?;
        if credential.authentication.oauth().is_none() {
            return Err(());
        }
        let client = self.client.for_account(account).map_err(|_| ())?;
        let pool_id = pool_member_id(settings, account).ok_or(())?;
        let pool_id = &pool_id;
        let value = self
            .pool_client(&client, settings)
            .gateway_pool_cookie(
                &settings.pool_url,
                &settings.pool_token,
                pool_id,
                &pair.gateway,
            )
            .await?;
        let cookie = routing_cookie(&value)?;
        // 池服务给出的剩余有效期与 Cookie 自身 exp 取较早者。
        let pool_deadline = value["valid_for_s"]
            .as_f64()
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
            .and_then(|valid| SystemTime::now().checked_add(valid));
        if cookie_gateway(&cookie).as_deref() != Some(&pair.gateway) {
            return Err(());
        }
        let authorization = credential
            .authentication
            .authorization_header()
            .map_err(|_| ())?;
        let id = Uuid::new_v4().to_string();
        let mut context = CodexRequestContext::auxiliary(
            authorization.expose_secret(),
            account.upstream_account_id(),
            &id,
            Some(&credential.installation_id),
        );
        context.cookie_header = Some(&cookie);
        let request = hi_request(&settings.probe_model).ok_or(())?;
        let first = self
            .probe_headers(account, pair, lease, &client, &request, context)
            .await?;
        self.observe_probe_quota(account, &first).await?;
        if first.status != 200 {
            return Ok((None, first.status, false));
        }
        let state = first
            .state
            .filter(|s| !s.is_empty() && s.len() <= 16384)
            .ok_or(())?;
        let paired_cookie = merge_cookie(&cookie, &first.cookies)?;
        if cookie_gateway(&paired_cookie).as_deref() != Some(&pair.gateway) {
            return Ok((None, 200, false));
        }
        context.turn_state = Some(&state);
        context.cookie_header = Some(&paired_cookie);
        if self.eligible_account(account).await?.is_none() {
            return Err(());
        }
        let second = self
            .probe_headers(account, pair, lease, &client, &request, context)
            .await?;
        self.observe_probe_quota(account, &second).await?;
        let resulting_cookie = merge_cookie(&paired_cookie, &second.cookies)?;
        if cookie_gateway(&resulting_cookie).as_deref() != Some(&pair.gateway) {
            return Ok((None, second.status, second.state_returned));
        }
        let expires = cookie_expiry(&resulting_cookie).ok_or(())?;
        let expires = pool_deadline.map_or(expires, |deadline| expires.min(deadline));
        Ok((
            Some(Material {
                pair: pair.clone(),
                state: SecretString::from(state),
                cookie: SecretString::from(resulting_cookie),
                expires,
            }),
            second.status,
            second.state_returned,
        ))
    }
    async fn probe_headers(
        &self,
        account: &ProviderAccount,
        pair: &Pair,
        lease: &str,
        client: &CodexBackendClient,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> Result<crate::transport::probe::ProbeObservation, ()> {
        for attempt in 0..=1 {
            if self.eligible_account(account).await?.is_none() {
                return Err(());
            }
            let access_id = Uuid::new_v4().to_string();
            let sent = self.call(json!({"action":"sent","accountId":account.id().as_str(),"leaseId":lease,"timingVersion":2,"accessId":access_id,"purpose":if context.turn_state.is_some() {"replay"} else {"bootstrap"}}))
                .await?;
            if sent["accepted"] != true {
                return Err(());
            }
            let started = Instant::now();
            let mut timing = AccessTiming::new(access_id);
            let observed = tokio::time::timeout(
                Duration::from_secs(15),
                client.diagnostic_observation(request, context, true),
            )
            .await
            .ok()
            .and_then(Result::ok);
            timing.end(if observed.is_some() {
                "headers"
            } else {
                "failed"
            });
            // 每次访问独立留痕；bootstrap 行仅是材料获取，不参与 21/29 判定。
            self.call(json!({"action":"probeAttempt","accountId":account.id().as_str(),"leaseId":lease,"phase":if context.turn_state.is_some() {"replay"} else {"bootstrap"},"retry":attempt,"status":observed.as_ref().map(|o|o.status),"stateReturned":observed.as_ref().is_some_and(|o|o.state_returned),"durationMs":started.elapsed().as_millis(),"gateway":pair.gateway,"access":timing.value()})).await?;
            if let Some(observation) = observed {
                if observation.status == 200 || attempt == 1 {
                    return Ok(observation);
                }
                self.quota
                    .synchronize_passive_headers(account, &observation.rate_limit_headers)
                    .await
                    .map_err(|_| ())?;
                // Retry-After 超过此次探测预算时不抢跑；交给冷却后的下一次探测。
                if observation
                    .retry_after
                    .is_some_and(|delay| delay > Duration::from_secs(5))
                {
                    return Ok(observation);
                }
                let delay = observation.retry_after.unwrap_or(Duration::from_millis(50));
                tokio::time::sleep(delay).await;
            } else if attempt == 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        Err(())
    }
    async fn eligible_account(
        &self,
        account: &ProviderAccount,
    ) -> Result<Option<ProviderAccount>, ()> {
        let Some(current) = self
            .repository
            .store()
            .get_account(account.id())
            .await
            .map_err(|_| ())?
        else {
            return Ok(None);
        };
        let probe_model = self.settings().await?.probe_model;
        if !current.enabled()
            || current.authentication_kind() != "oauth"
            || !current.model_access().allows(&probe_model)
        {
            return Ok(None);
        }
        let cooldown = self.quota.cooldown(current.id()).await.map_err(|_| ())?;
        let status = current
            .status_projection(SystemTime::now(), cooldown)
            .status;
        if status == gateway_core::account::AccountStatus::QuotaExhausted {
            self.call(json!({"action":"quota","accountId":current.id().as_str(),"exhausted":true}))
                .await?;
        }
        if status != gateway_core::account::AccountStatus::Normal {
            return Ok(None);
        }
        // 没有额度材料时先走既有额度刷新器，使用账号绑定的出口；读取失败不探测。
        let snapshot = match self
            .quota
            .read_account(current.id())
            .await
            .map_err(|_| ())?
        {
            Some(snapshot) => snapshot,
            None => self
                .quota
                .refresh_account(current.id())
                .await
                .map_err(|_| ())?,
        };
        self.call(json!({"action":"quota","accountId":current.id().as_str(),"exhausted":snapshot.quota().is_exhausted()})).await?;
        if snapshot.quota().is_exhausted() {
            return Ok(None);
        }
        Ok(Some(current))
    }
    async fn observe_probe_quota(
        &self,
        account: &ProviderAccount,
        observation: &crate::transport::probe::ProbeObservation,
    ) -> Result<(), ()> {
        self.quota
            .synchronize_passive_headers(account, &observation.rate_limit_headers)
            .await
            .map_err(|_| ())?;
        if observation.status == 429 {
            self.quota
                .apply_rate_limit_429(account, observation.retry_after, SystemTime::now())
                .await
                .map_err(|_| ())?;
        }
        Ok(())
    }
}

pub(crate) struct GatewayPoolGuard {
    service: Arc<GatewayPoolService>,
    account: String,
    lease: String,
    material: Material,
    /// 当前请求使用的模型；初始为打票模型。
    model: String,
    /// 非打票模型的票；为空时使用组合自身的 S0。
    ticket: Option<Ticket>,
    timing: std::sync::Mutex<Option<AccessTiming>>,
    finished: bool,
}
impl GatewayPoolGuard {
    pub(crate) fn state(&self) -> &str {
        self.ticket
            .as_ref()
            .map_or(&self.material.state, |ticket| &ticket.state)
            .expose_secret()
    }
    pub(crate) fn cookie(&self) -> &str {
        self.ticket
            .as_ref()
            .map_or(&self.material.cookie, |ticket| &ticket.cookie)
            .expose_secret()
    }
    /// 切换到请求模型的票。窗口由打票模型验证，其他模型共用窗口，只需各自的 state：
    /// 先读本进程缓存、再读 A/B 共享存储，都没有时用该模型发一次 `hi`，拿到响应头即断开，
    /// 不做第二轮判定。同组合同模型的拿票在进程内合并。
    pub(crate) async fn use_model(
        &mut self,
        model: &str,
        client: &CodexBackendClient,
        account: &ProviderAccount,
        authorization: &str,
        installation_id: &str,
    ) -> Result<(), ()> {
        if model == self.model {
            return Ok(());
        }
        if model.is_empty()
            || model.len() > 64
            || !model
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        {
            return Err(());
        }
        let key = format!("{}:{}:{model}", self.account, self.material.pair.generation);
        let lock = {
            let mut locks = self.service.ticket_locks.lock().await;
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            Arc::clone(locks.entry(key.clone()).or_default())
        };
        let _held = lock.lock().await;
        let cached = self
            .service
            .tickets
            .lock()
            .await
            .get(&key)
            .filter(|ticket| ticket.expires > SystemTime::now())
            .cloned();
        let ticket = match cached {
            Some(ticket) => ticket,
            None => match self.shared_ticket(model).await? {
                Some(ticket) => ticket,
                None => {
                    self.fetch_ticket(model, client, account, authorization, installation_id)
                        .await?
                }
            },
        };
        let mut tickets = self.service.tickets.lock().await;
        tickets.retain(|_, ticket| ticket.expires > SystemTime::now());
        if tickets.len() >= 512
            && let Some(old) = tickets.keys().next().cloned()
        {
            tickets.remove(&old);
        }
        tickets.insert(key, ticket.clone());
        drop(tickets);
        self.model = model.to_owned();
        self.ticket = Some(ticket);
        Ok(())
    }
    async fn shared_ticket(&self, model: &str) -> Result<Option<Ticket>, ()> {
        let shared = self.service.call(json!({"action":"ticket","accountId":self.account,"generation":self.material.pair.generation,"model":model})).await?;
        Ok(shared["state"]
            .as_str()
            .zip(shared["cookie"].as_str())
            .and_then(|(state, cookie)| {
                let expires = SystemTime::UNIX_EPOCH
                    .checked_add(Duration::try_from_secs_f64(shared["expires"].as_f64()?).ok()?)?;
                (expires > SystemTime::now()
                    && cookie_gateway(cookie).as_deref()
                        == Some(self.material.pair.gateway.as_str()))
                .then(|| Ticket {
                    state: SecretString::from(state.to_owned()),
                    cookie: SecretString::from(cookie.to_owned()),
                    expires,
                })
            }))
    }
    async fn fetch_ticket(
        &self,
        model: &str,
        client: &CodexBackendClient,
        account: &ProviderAccount,
        authorization: &str,
        installation_id: &str,
    ) -> Result<Ticket, ()> {
        // 拿票是真实上游访问，记在本业务租约上；组合已退役时不再发送。
        let access_id = Uuid::new_v4().to_string();
        if !self.sent_for("ticket", &access_id).await? {
            return Err(());
        }
        let id = Uuid::new_v4().to_string();
        let base = self.material.cookie.expose_secret();
        let mut context = CodexRequestContext::auxiliary(
            authorization,
            account.upstream_account_id(),
            &id,
            Some(installation_id),
        );
        context.cookie_header = Some(base);
        let request = hi_request(model).ok_or(())?;
        let started = Instant::now();
        let mut timing = AccessTiming::new(access_id);
        let observed = tokio::time::timeout(
            Duration::from_secs(15),
            client.diagnostic_observation(&request, context, true),
        )
        .await
        .ok()
        .and_then(Result::ok);
        timing.end(if observed.is_some() {
            "headers"
        } else {
            "failed"
        });
        if let Some(observation) = &observed {
            let _ = self.service.observe_probe_quota(account, observation).await;
        }
        let ticket = observed
            .as_ref()
            .filter(|observation| observation.status == 200)
            .and_then(|observation| {
                let state = observation
                    .state
                    .clone()
                    .filter(|state| !state.is_empty() && state.len() <= 16384)?;
                let cookie = merge_cookie(base, &observation.cookies).ok()?;
                (cookie_gateway(&cookie).as_deref() == Some(self.material.pair.gateway.as_str()))
                    .then_some(())?;
                let expires = cookie_expiry(&cookie)?.min(self.material.expires);
                Some(Ticket {
                    state: SecretString::from(state),
                    cookie: SecretString::from(cookie),
                    expires,
                })
            });
        let material = ticket.as_ref().map(|ticket| json!({"state":ticket.state.expose_secret(),"cookie":ticket.cookie.expose_secret(),"expires":epoch(ticket.expires)}));
        self.service.call(json!({"action":"ticketResult","accountId":self.account,"leaseId":self.lease,"model":model,"status":observed.as_ref().map(|o| o.status),"stateReturned":observed.as_ref().is_some_and(|o| o.state_returned),"durationMs":started.elapsed().as_millis(),"material":material,"access":timing.value()})).await?;
        ticket.ok_or(())
    }
    pub(crate) async fn sent(&self) -> Result<bool, ()> {
        let access_id = Uuid::new_v4().to_string();
        let accepted = self.sent_for("business", &access_id).await?;
        if accepted && let Ok(mut timing) = self.timing.lock() {
            *timing = Some(AccessTiming::new(access_id));
        }
        Ok(accepted)
    }
    async fn sent_for(&self, purpose: &str, access_id: &str) -> Result<bool, ()> {
        self.service
            .call(json!({"action":"sent","accountId":self.account,"leaseId":self.lease,"purpose":purpose,"timingVersion":2,"accessId":access_id}))
            .await
            .map(|value| value["accepted"] == true)
    }
    pub(crate) async fn observe(
        &self,
        status: u16,
        state_returned: bool,
        cookies: &[String],
        duration: Duration,
    ) -> Result<Option<bool>, ()> {
        if status != 200 || state_returned {
            self.end_access(if status == 200 { "rejected" } else { "failed" });
        }
        let cookie = merge_cookie(self.cookie(), cookies)?;
        let matches = cookie_gateway(&cookie).as_deref() == Some(&self.material.pair.gateway);
        let value = self.service.call(json!({"action":"businessResult","accountId":self.account,"leaseId":self.lease,"model":self.model,"status":status,"stateReturned":state_returned,"paired":true,"gatewayMatches":matches,"durationMs":duration.as_millis(),"access":self.access_value()})).await?;
        Ok(match value["classification"].as_str() {
            Some("21") if matches => Some(true),
            Some("29") if matches => Some(false),
            _ => None,
        })
    }
    pub(crate) fn end_access(&self, outcome: &'static str) {
        if let Ok(mut timing) = self.timing.lock()
            && let Some(timing) = timing.as_mut()
        {
            timing.end(outcome);
        }
    }
    fn access_value(&self) -> Option<Value> {
        self.timing
            .lock()
            .ok()
            .and_then(|timing| timing.as_ref().map(AccessTiming::value))
    }
    fn finish_payload(&self) -> Value {
        self.end_access("interrupted");
        json!({"action":"finish","accountId":self.account,"leaseId":self.lease,"access":self.access_value()})
    }
    pub(crate) async fn finish(mut self) {
        let payload = self.finish_payload();
        if self.service.call(payload).await.is_ok() {
            self.finished = true;
        }
    }
}
impl Drop for GatewayPoolGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let service = Arc::clone(&self.service);
            let payload = self.finish_payload();
            handle.spawn(async move {
                let _ = service.call(payload).await;
            });
        }
    }
}

/// 真实实验仅复用生产的路由、材料、额度和响应头判定，不注册公网接口。
#[cfg(feature = "gateway-pool-experiment")]
#[derive(Clone)]
pub struct GatewayPoolExperiment(Arc<GatewayPoolService>);

#[cfg(feature = "gateway-pool-experiment")]
impl GatewayPoolExperiment {
    pub(crate) fn new(service: Arc<GatewayPoolService>) -> Self {
        Self(service)
    }
    /// 仅显式授权的独立实验启用：池元数据直连，账号上游出口保持不变。
    pub fn allow_direct_pool_metadata(&self) {
        self.0
            .direct_pool_experiment
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub async fn background(&self, cancellation: &CancellationToken) -> bool {
        self.0.background(cancellation).await.is_ok()
    }

    pub async fn refresh_quota(&self, id: &gateway_core::account::ProviderAccountId) -> bool {
        self.0
            .quota
            .refresh_account(id)
            .await
            .is_ok_and(|s| !s.quota().is_exhausted())
    }

    /// 固定 body 与会话的完整 SSE 缓存实验；每个实验通道独立保留已验证材料。
    pub async fn cache_sample(
        &self,
        id: &gateway_core::account::ProviderAccountId,
        body: serde_json::Map<String, Value>,
        session: &str,
        deadline: SystemTime,
        cancellation: &CancellationToken,
    ) -> Value {
        let Ok(Some(account)) = self.0.repository.store().get_account(id).await else {
            return json!({"result":"account_unavailable"});
        };
        let guard = match self.0.route(&account, deadline, cancellation).await {
            Ok(guard) => guard,
            Err(()) => return json!({"result":"route_unavailable"}),
        };
        let result = async {
            // 找票的额度响应会更新账号投影，凭据校验必须使用发送前的最新快照。
            let account = self.0.eligible_account(&account).await.map_err(|_| "account_reload_failed")?.ok_or("account_unavailable_after_prepare")?;
            let credential = self.0.repository.load_runtime_credential(&account).await.map_err(|_| "credential_unavailable")?;
            let authorization = credential.authentication.authorization_header().map_err(|_| "authorization_unavailable")?;
            let client = self.0.client.for_account(&account).map_err(|_| "transport_unavailable")?;
            let mut context = CodexRequestContext::auxiliary(authorization.expose_secret(), account.upstream_account_id(), session, Some(&credential.installation_id));
            context.session_id = Some(session);
            context.thread_id = Some(session);
            context.turn_state = Some(guard.state());
            context.cookie_header = Some(guard.cookie());
            if !guard.sent().await.map_err(|_| "reservation_failed")? { return Err("reservation_retired"); }
            let mut request = CodexResponsesRequest::from_body(body);
            request.force_http_sse = true;
            let started = Instant::now();
            let observation = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err("cancelled"),
                value = tokio::time::timeout(Duration::from_secs(45), client.cache_observation(&request, context)) => value.map_err(|_| "request_timeout")?.map_err(|_| "response_read_failed")?,
            };
            let _ = self.0.observe_probe_quota(&account, &observation).await;
            let classification = guard.observe(observation.status, observation.state_returned, &observation.cookies, started.elapsed()).await.map_err(|_| "observation_failed")?;
            let input = observation.usage.as_ref().and_then(|u| u["input_tokens"].as_u64());
            let cached = observation.usage.as_ref().and_then(|u| u["input_tokens_details"]["cached_tokens"].as_u64());
            Ok::<_, &str>(json!({"result":"cache_sample","requestSent":true,"gateway":guard.material.pair.gateway,"ticketFingerprint":fingerprint(guard.state()),"status":observation.status,"classification":match classification {Some(true)=>Some("21"),Some(false)=>Some("29"),None=>None},"stateReturned":observation.state_returned,"gatewayMatches":classification.is_some(),"inputTokens":input,"cachedTokens":cached,"cacheWriteTokens":observation.usage.as_ref().and_then(|u| u["input_tokens_details"]["cache_write_tokens"].as_u64()),"outputTokens":observation.usage.as_ref().and_then(|u| u["output_tokens"].as_u64()),"usage":observation.usage,"streamFailure":observation.stream_failure,"assistantText":observation.text,"durationMs":started.elapsed().as_millis()}))
        }.await;
        let gateway = guard.material.pair.gateway.clone();
        guard.finish().await;
        result.unwrap_or_else(|stage| json!({"result":"transport_or_observation_failed","stage":stage,"gateway":gateway,"requestSent":matches!(stage,"request_timeout"|"response_read_failed"|"observation_failed")}))
    }

    pub async fn sample(
        &self,
        id: &gateway_core::account::ProviderAccountId,
        deadline: SystemTime,
        cancellation: &CancellationToken,
    ) -> Value {
        self.model_sample(id, "gpt-6-astra", true, deadline, cancellation)
            .await
    }

    /// 在当前已验证的票上发送指定模型的 `hi`，只看响应头。`classify` 为 false 时
    /// 只记录事实，不把结果计入组合判定，避免跨模型观察误伤打票模型的组合。
    pub async fn model_sample(
        &self,
        id: &gateway_core::account::ProviderAccountId,
        model: &str,
        classify: bool,
        deadline: SystemTime,
        cancellation: &CancellationToken,
    ) -> Value {
        let Ok(Some(account)) = self.0.repository.store().get_account(id).await else {
            return json!({"result":"account_unavailable"});
        };
        if !matches!(self.0.eligible_account(&account).await, Ok(Some(_))) {
            return json!({"result":"quota_or_account_unavailable"});
        }
        let guard = match tokio::select! {
            biased;
            _ = cancellation.cancelled() => return json!({"result":"cancelled"}),
            result = self.0.route(&account, deadline, cancellation) => result,
        } {
            Ok(guard) => guard,
            Err(()) => return json!({"result":"route_unavailable"}),
        };
        // 找票的额度响应会更新账号投影，加载凭据前必须改用最新快照。
        let Ok(Some(account)) = self.0.eligible_account(&account).await else {
            guard.finish().await;
            return json!({"result":"account_unavailable_after_prepare","model":model});
        };
        let mut result = self
            .sample_reserved(&account, &guard, model, classify, cancellation)
            .await;
        result["model"] = json!(model);
        result["classified"] = json!(classify);
        guard.finish().await;
        result
    }

    async fn sample_reserved(
        &self,
        account: &ProviderAccount,
        guard: &GatewayPoolGuard,
        model: &str,
        classify: bool,
        cancellation: &CancellationToken,
    ) -> Value {
        let Ok(credential) = self.0.repository.load_runtime_credential(account).await else {
            return json!({"result":"credential_unavailable"});
        };
        let Ok(authorization) = credential.authentication.authorization_header() else {
            return json!({"result":"credential_unavailable"});
        };
        let Ok(client) = self.0.client.for_account(account) else {
            return json!({"result":"transport_unavailable"});
        };
        let session = Uuid::new_v4().to_string();
        let mut context = CodexRequestContext::auxiliary(
            authorization.expose_secret(),
            account.upstream_account_id(),
            &session,
            Some(&credential.installation_id),
        );
        context.turn_state = Some(guard.state());
        context.cookie_header = Some(guard.cookie());
        let Some(request) = hi_request(model) else {
            return json!({"result":"invalid_model"});
        };
        for retry in 0..=1 {
            if !matches!(self.0.eligible_account(account).await, Ok(Some(_))) {
                return json!({"result":"quota_or_account_unavailable"});
            }
            if !matches!(guard.sent().await, Ok(true)) {
                return json!({"result":"reservation_retired"});
            }
            let started = Instant::now();
            let observation = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return json!({"result":"cancelled"}),
                result = tokio::time::timeout(Duration::from_secs(15), client.diagnostic_observation(&request, context, true)) => result.ok().and_then(Result::ok),
            };
            if let Some(observation) = observation {
                let _ = self.0.observe_probe_quota(account, &observation).await;
                let classification = if classify {
                    guard
                        .observe(
                            observation.status,
                            observation.state_returned,
                            &observation.cookies,
                            started.elapsed(),
                        )
                        .await
                } else {
                    Ok(None)
                };
                let gateway_matches = merge_cookie(guard.cookie(), &observation.cookies)
                    .ok()
                    .and_then(|cookie| cookie_gateway(&cookie))
                    .is_some_and(|gateway| gateway == guard.material.pair.gateway);
                let result = json!({"result":"headers","gateway":guard.material.pair.gateway,"status":observation.status,"stateReturned":observation.state_returned,"stateEchoed":observation.state.as_deref().map(|state| state == guard.state()),"stateFingerprint":observation.state.as_deref().map(fingerprint),"ticketFingerprint":fingerprint(guard.state()),"gatewayMatches":gateway_matches,"classification":match classification {Ok(Some(true))=>Some("21"),Ok(Some(false))=>Some("29"),_=>None},"durationMs":started.elapsed().as_millis(),"retry":retry});
                if observation.status == 200 || !classify {
                    return result;
                }
                if retry == 1
                    || observation
                        .retry_after
                        .is_some_and(|d| d > Duration::from_secs(5))
                {
                    let _ = self.0.call(json!({"action":"invalidate","accountId":guard.account,"leaseId":guard.lease})).await;
                    return result;
                }
                tokio::time::sleep(observation.retry_after.unwrap_or(Duration::from_millis(50)))
                    .await;
            } else if retry == 1 && !classify {
                return json!({"result":"transport_failed","gateway":guard.material.pair.gateway});
            } else if retry == 1 {
                let _ = self.0.call(json!({"action":"businessResult","accountId":guard.account,"leaseId":guard.lease,"status":null,"stateReturned":false,"paired":true,"gatewayMatches":true,"failed":true,"durationMs":started.elapsed().as_millis()})).await;
                let _ = self.0.call(json!({"action":"invalidate","accountId":guard.account,"leaseId":guard.lease})).await;
                return json!({"result":"transport_failed","gateway":guard.material.pair.gateway});
            } else {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        json!({"result":"transport_failed"})
    }
}

/// 实验对照用：只输出 SHA-256 前 8 位，便于比较两次 state 是否相同，不暴露原文。
#[cfg(feature = "gateway-pool-experiment")]
fn fingerprint(value: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(value.as_bytes())[..4])
}
/// 池服务成员标识：管理员显式映射优先，否则按上游账号与用户自动生成。
fn pool_member_id(settings: &Settings, account: &ProviderAccount) -> Option<String> {
    settings
        .pool_account_ids
        .get(account.id().as_str())
        .cloned()
        .or_else(|| {
            Some(format!(
                "gwpool-member:{}/{}",
                account.upstream_account_id()?,
                account.upstream_user_id()?
            ))
        })
}
fn epoch(time: SystemTime) -> f64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn claims(cookie: &str) -> Option<Value> {
    let token = cookie
        .split(';')
        .find_map(|part| part.trim().strip_prefix("__oailb="))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(token.split('.').nth(1)?.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}
pub(crate) fn cookie_gateway(cookie: &str) -> Option<String> {
    let value = claims(cookie)?;
    value["host"]
        .as_str()?
        .split('.')
        .find(|s| {
            s.strip_prefix("unified-")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_owned)
}
fn cookie_expiry(cookie: &str) -> Option<SystemTime> {
    let expiry = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(claims(cookie)?["exp"].as_u64()?))?;
    (expiry > SystemTime::now()).then_some(expiry)
}
fn routing_cookie(value: &Value) -> Result<String, ()> {
    if let Some(cookie) = value["cookie"].as_str() {
        return merge_cookie(cookie, &[]);
    }
    let values = value.get("cookies").unwrap_or(value);
    let oailb = values["__oailb"]
        .as_str()
        .or_else(|| values["oailb"].as_str())
        .ok_or(())?;
    let cflb = values["__cflb"]
        .as_str()
        .or_else(|| values["cflb"].as_str())
        .ok_or(())?;
    merge_cookie(&format!("__oailb={oailb}; __cflb={cflb}"), &[])
}
fn merge_cookie(original: &str, updates: &[String]) -> Result<String, ()> {
    let mut values = HashMap::new();
    for value in original.split(';').chain(
        updates
            .iter()
            .map(|s| s.split(';').next().unwrap_or_default()),
    ) {
        let Some((name, value)) = value.trim().split_once('=') else {
            continue;
        };
        if matches!(name, "__oailb" | "__cflb") {
            values.insert(name, value);
        }
    }
    let cookie = format!(
        "__oailb={}; __cflb={}",
        values.get("__oailb").ok_or(())?,
        values.get("__cflb").ok_or(())?
    );
    if cookie.len() > 16384 || reqwest::header::HeaderValue::from_str(&cookie).is_err() {
        return Err(());
    }
    Ok(cookie)
}
