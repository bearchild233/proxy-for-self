//! 请求路径钩子测试的共用装置：真实的插件服务（Unix socket）+ 模拟内核。
//! 内核能力（`accounts.read`、`quota.read`、`upstream.probe`、`http.fetch`、`log`）由 `Fake` 即时应答，
//! 并记录每次调用，供测试核对池服务请求与探测参数。

#![allow(dead_code)]

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gateway_pool::{CAPABILITIES, GatewayPoolService, HOOKS, Store, dispatch, system_clock};
use plugin_sdk::{Descriptor, RpcResult, Server, Settings, protocol::*, testing::MockKernel};
use rusqlite::{Connection, types::ValueRef};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::common::{TestDir, object_of};

pub const GROUP: &str = "grp_00000000000000000000000000000001";
pub const ACCOUNT: &str = "acct_gateway_pool";
pub const POOL_URL: &str = "https://pool.example.invalid";
pub const POOL_TOKEN: &str = "demo-pool-token";

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs_f64()
}

fn encode_base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(TABLE[((value >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

fn decode_base64url(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut buffer, mut bits) = (0_u32, 0_u32);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            _ => 63,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    out
}

/// 路由 Cookie：`__oailb` 的第二段是带 `host` 与 `exp` 的 JSON。
pub fn cookie_for(gateway: &str, exp: f64) -> String {
    let payload =
        json!({"host": format!("{gateway}.example.invalid"), "exp": exp as u64}).to_string();
    format!(
        "__oailb=h.{}.s; __cflb=demo",
        encode_base64url(payload.as_bytes())
    )
}

/// 从 Cookie 里读出网关名。
pub fn gateway_of(cookie: &str) -> Option<String> {
    let token = cookie
        .split(';')
        .find_map(|part| part.trim().strip_prefix("__oailb="))?;
    let claims: Value = serde_json::from_slice(&decode_base64url(token.split('.').nth(1)?)).ok()?;
    claims["host"]
        .as_str()?
        .split('.')
        .next()
        .map(str::to_owned)
}

#[derive(Clone)]
pub struct FakeAccount {
    pub enabled: bool,
    pub status: String,
    pub kind: String,
    pub upstream: Option<String>,
    pub user: Option<String>,
}

impl FakeAccount {
    pub fn normal() -> Self {
        Self {
            enabled: true,
            status: "normal".into(),
            kind: "oauth".into(),
            upstream: Some("demo-upstream".into()),
            user: Some("demo-user".into()),
        }
    }
}

#[derive(Clone)]
pub struct FakeQuota {
    pub present: bool,
    pub exhausted: bool,
    pub rank: Option<u64>,
}

type ProbeBehavior = Box<dyn Fn(&Value) -> Value + Send + Sync>;

/// 模拟内核侧：账号、额度、上游探测和池服务。
pub struct Fake {
    pub accounts: Mutex<BTreeMap<String, FakeAccount>>,
    pub quota: Mutex<BTreeMap<String, FakeQuota>>,
    pub gateways: Mutex<Vec<String>>,
    pub calls: Mutex<Vec<(String, Value)>>,
    pub failing: Mutex<HashSet<String>>,
    probe: Mutex<ProbeBehavior>,
    catalog_body: Mutex<Option<Value>>,
    catalog_delay: Mutex<Duration>,
}

impl Fake {
    fn new() -> Self {
        Self {
            accounts: Mutex::new(BTreeMap::new()),
            quota: Mutex::new(BTreeMap::new()),
            gateways: Mutex::new(vec!["unified-142".into(), "unified-143".into()]),
            calls: Mutex::new(Vec::new()),
            failing: Mutex::new(HashSet::new()),
            probe: Mutex::new(Box::new(default_probe)),
            catalog_body: Mutex::new(None),
            catalog_delay: Mutex::new(Duration::ZERO),
        }
    }

    /// 池服务目录改为固定的响应体。
    pub fn set_catalog_body(&self, body: Value) {
        *self.catalog_body.lock().expect("catalog") = Some(body);
    }

    /// 池服务读取目录前阻塞这么久（模拟慢响应）。
    pub fn set_catalog_delay(&self, delay: Duration) {
        *self.catalog_delay.lock().expect("catalog delay") = delay;
    }

    pub fn set_probe(&self, behavior: impl Fn(&Value) -> Value + Send + Sync + 'static) {
        *self.probe.lock().expect("probe") = Box::new(behavior);
    }

    pub fn add_account(&self, id: &str, account: FakeAccount) {
        self.accounts
            .lock()
            .expect("accounts")
            .insert(id.to_owned(), account);
        self.quota.lock().expect("quota").insert(
            id.to_owned(),
            FakeQuota {
                present: true,
                exhausted: false,
                rank: Some(5),
            },
        );
    }

    pub fn account_mut(&self, id: &str, change: impl FnOnce(&mut FakeAccount)) {
        change(
            self.accounts
                .lock()
                .expect("accounts")
                .get_mut(id)
                .expect("account"),
        );
    }

    pub fn quota_mut(&self, id: &str, change: impl FnOnce(&mut FakeQuota)) {
        change(
            self.quota
                .lock()
                .expect("quota")
                .get_mut(id)
                .expect("quota"),
        );
    }

    /// 某个方法的全部调用参数。
    pub fn calls_to(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .filter(|(name, _)| name == method)
            .map(|(_, params)| params.clone())
            .collect()
    }

    /// 池服务请求（`http.fetch`）：按路径过滤。
    pub fn fetches(&self, path: &str) -> Vec<Value> {
        self.calls_to("http.fetch")
            .into_iter()
            .filter(|params| {
                params["url"].as_str().is_some_and(|url| {
                    url.split('?')
                        .next()
                        .is_some_and(|base| base.ends_with(path))
                })
            })
            .collect()
    }

    fn answer(&self, method: &str, params: &Value) -> Option<RpcResult<Value>> {
        self.calls
            .lock()
            .expect("calls")
            .push((method.to_owned(), params.clone()));
        if self.failing.lock().expect("failing").contains(method) {
            return Some(Err(RpcError::new(UNAVAILABLE, method)));
        }
        Some(Ok(match method {
            "accounts.read" => {
                let accounts = self.accounts.lock().expect("accounts");
                let ids: Vec<String> = match params["ids"].as_array() {
                    Some(ids) => ids
                        .iter()
                        .filter_map(|id| id.as_str().map(str::to_owned))
                        .collect(),
                    None => accounts.keys().cloned().collect(),
                };
                let rows: Vec<Value> = ids
                    .iter()
                    .filter_map(|id| {
                        let account = accounts.get(id)?;
                        Some(
                            json!({"id": id, "enabled": account.enabled, "status": account.status,
                            "authenticationKind": account.kind, "groups": [GROUP],
                            "upstreamAccountId": account.upstream, "userId": account.user}),
                        )
                    })
                    .collect();
                json!({"accounts": rows})
            }
            "quota.read" => {
                let quota = self.quota.lock().expect("quota");
                let Some(item) = params["accountId"].as_str().and_then(|id| quota.get(id)) else {
                    return Some(Err(RpcError::new(
                        FORBIDDEN,
                        "account outside plugin groups",
                    )));
                };
                let snapshot = if item.present {
                    json!({"quota": {"access": if item.exhausted {"exhausted"} else {"allowed"}}})
                } else {
                    Value::Null
                };
                json!({"snapshot": {"quota": snapshot,
                    "schedulingSignals": item.rank.map(|rank| json!({"remainingRank": rank}))}})
            }
            "upstream.probe" => (self.probe.lock().expect("probe"))(params),
            "http.fetch" => self.fetch(params),
            "log" => Value::Null,
            other => return Some(Err(RpcError::new(NOT_SUPPORTED, other))),
        }))
    }

    /// 池服务：`/gateways` 返回目录，`/cookie` 返回指定网关的路由 Cookie。
    fn fetch(&self, params: &Value) -> Value {
        let url = params["url"].as_str().unwrap_or_default();
        let (path, query) = url.split_once('?').unwrap_or((url, ""));
        let body = if path.ends_with("/gateways") {
            std::thread::sleep(*self.catalog_delay.lock().expect("catalog delay"));
            if let Some(body) = self.catalog_body.lock().expect("catalog").clone() {
                return json!({"status": 200, "headers": [], "body": body.to_string(), "truncated": false});
            }
            let entries: Vec<Value> = self
                .gateways
                .lock()
                .expect("gateways")
                .iter()
                .map(|name| json!({"name": name, "pair_ready": true, "valid_for_s": 3600, "datacenter_country": "US"}))
                .collect();
            json!({"gateways": entries})
        } else {
            let gateway = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("gateway="))
                .unwrap_or("unified-142");
            json!({"cookie": cookie_for(gateway, now_secs() + 600.0), "valid_for_s": 600})
        };
        json!({"status": 200, "headers": [], "body": body.to_string(), "truncated": false})
    }
}

/// 探测的默认行为：没带 state 的 bootstrap 返回 `state-<网关>`；带 state 的重放不再返回 state（21）。
pub fn probe_result(status: Option<u16>, turn_state: Option<&str>) -> Value {
    json!({"status": status, "errorKind": if status.is_none() {json!("connect")} else {Value::Null},
        "stateReturned": turn_state.is_some(), "turnState": turn_state, "setCookies": [],
        "rateLimitHeaders": [], "retryAfterMs": null, "durationMs": 1})
}

fn default_probe(params: &Value) -> Value {
    if params["turnState"].is_null() {
        let gateway = params["cookie"]
            .as_str()
            .and_then(gateway_of)
            .unwrap_or_default();
        probe_result(Some(200), Some(&format!("state-{gateway}")))
    } else {
        probe_result(Some(200), None)
    }
}

pub struct Rig {
    pub dir: TestDir,
    /// 与服务共用同一数据目录的另一个句柄：用于配置、播种和核对状态。
    pub store: Store,
    pub kernel: MockKernel,
    pub fake: Arc<Fake>,
    task: JoinHandle<std::io::Result<()>>,
}

impl Rig {
    /// 启动服务并连接模拟内核。`configure` 在启动前写入设置。
    pub async fn start(label: &str, configure: Value) -> Self {
        let dir = TestDir::new(label);
        let data = dir.path().join("data");
        let store = Store::open(&data, system_clock()).expect("open store");
        let mut settings = json!({
            "groupIds": [GROUP], "poolUrl": POOL_URL, "poolToken": POOL_TOKEN,
            "probeIntervalMs": 10, "probeModel": "gpt-6-astra",
        });
        for (key, value) in object_of(configure) {
            settings[key] = value;
        }
        dispatch(&store, "configure", &object_of(settings)).expect("configure");
        let socket = dir.path().join("pool.sock");
        let descriptor = Descriptor {
            plugin: PluginInfo {
                id: "gateway-pool".into(),
                version: "test".into(),
            },
            kernel_api: PluginApi {
                major: 0,
                min_minor: 5,
            },
            hooks: HOOKS.map(str::to_owned).to_vec(),
            capabilities: CAPABILITIES.map(str::to_owned).to_vec(),
            owned_groups: vec![GROUP.into()],
            watched_groups: vec![],
            shutdown_timeout: Duration::from_secs(3),
        };
        let service =
            GatewayPoolService::new(Store::open(&data, system_clock()).expect("service store"));
        let server = Server::bind(
            Settings {
                socket: socket.clone(),
                data,
            },
            descriptor,
            service,
        )
        .expect("bind");
        let task = tokio::spawn(server.run_until(std::future::pending()));
        let kernel = MockKernel::connect(
            &socket,
            HelloParams {
                kernel_api: KernelApi::default(),
                kernel: KernelInfo {
                    version: "test".into(),
                    slot: None,
                },
                role: ConnectionRole::Kernel,
            },
            Duration::from_secs(3),
        )
        .await
        .expect("connect");
        let fake = Arc::new(Fake::new());
        fake.add_account(ACCOUNT, FakeAccount::normal());
        let handler = Arc::clone(&fake);
        kernel.set_handler(move |method, params| handler.answer(method, params));
        Self {
            dir,
            store,
            kernel,
            fake,
            task,
        }
    }

    // ---- 钩子调用 ----

    pub async fn select(
        &self,
        request: &str,
        accounts: &[&str],
        ordinary: bool,
        pinned: Option<&str>,
    ) -> RouteSelectResult {
        self.kernel
            .call(
                "route.select",
                &RouteSelectParams {
                    request_id: request.into(),
                    model: "gpt-6-astra".into(),
                    accounts: accounts.iter().map(|id| (*id).to_owned()).collect(),
                    ordinary_available: ordinary,
                    pinned: pinned.map(str::to_owned),
                },
                Duration::from_secs(5),
            )
            .await
            .expect("route.select")
    }

    pub async fn prepare_once(
        &self,
        request: &str,
        account: &str,
        model: &str,
        deadline_ms: u64,
    ) -> RoutePrepareResult {
        self.kernel
            .call(
                "route.prepare",
                &RoutePrepareParams {
                    request_id: request.into(),
                    account_id: account.into(),
                    attempt: 1,
                    model: model.into(),
                    deadline_unix_ms: deadline_ms,
                    replay_safe: true,
                },
                Duration::from_secs(5),
            )
            .await
            .expect("route.prepare")
    }

    /// 像内核一样按 `wait` 的间隔重复调用，直到得到 `use` 或 `fail`。
    pub async fn drive(
        &self,
        request: &str,
        account: &str,
        model: &str,
        seconds: u64,
    ) -> RoutePrepareResult {
        let deadline_ms = (now_secs() * 1000.0) as u64 + seconds * 1000;
        loop {
            match self
                .prepare_once(request, account, model, deadline_ms)
                .await
            {
                RoutePrepareResult::Wait { retry_after_ms, .. } => {
                    tokio::time::sleep(Duration::from_millis(retry_after_ms)).await;
                }
                other => return other,
            }
            assert!(
                now_secs() * 1000.0 < (deadline_ms + 5000) as f64,
                "route.prepare never finished"
            );
        }
    }

    pub async fn head(
        &self,
        request: &str,
        lease: &str,
        status: Option<u16>,
        state_returned: bool,
        set_cookies: Vec<String>,
    ) -> ResponseHeadResult {
        self.kernel
            .call(
                "response.head",
                &ResponseHeadParams {
                    request_id: request.into(),
                    lease_id: lease.into(),
                    attempt: 1,
                    status,
                    error_kind: if status.is_none() {
                        Some("connect".into())
                    } else {
                        None
                    },
                    state_returned,
                    turn_state: None,
                    set_cookies,
                    duration_ms: 7,
                    replay_safe: true,
                },
                Duration::from_secs(5),
            )
            .await
            .expect("response.head")
    }

    /// `request.finish` 是通知；返回前等待租约被归还。
    pub async fn finish(&self, request: &str, lease: &str, outcome: RequestOutcome) {
        self.kernel
            .notify(
                "request.finish",
                &RequestFinishParams {
                    request_id: request.into(),
                    lease_id: lease.into(),
                    outcome,
                    duration_ms: 50,
                    first_byte_ms: Some(20),
                    usage: None,
                },
            )
            .await
            .expect("request.finish");
        self.until(|| self.lease_count(lease) == 0).await;
    }

    pub async fn until(&self, mut done: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !done() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("condition not reached in time");
    }

    // ---- 状态核对 ----

    pub fn connection(&self) -> Connection {
        Connection::open(self.dir.path().join("data").join(gateway_pool::DB_FILE))
            .expect("open database")
    }

    pub fn query(&self, sql: &str) -> Vec<Vec<Value>> {
        let connection = self.connection();
        let mut statement = connection.prepare(sql).expect("prepare");
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns)
                    .map(|i| {
                        Ok(match row.get_ref(i)? {
                            ValueRef::Null | ValueRef::Blob(_) => Value::Null,
                            ValueRef::Integer(v) => json!(v),
                            ValueRef::Real(v) => json!(v),
                            ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                        })
                    })
                    .collect()
            })
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows")
    }

    pub fn lease_count(&self, lease: &str) -> i64 {
        self.query(&format!("SELECT COUNT(*) FROM leases WHERE id='{lease}'"))[0][0]
            .as_i64()
            .expect("count")
    }

    /// 观察记录（`observations.data`），按先后顺序。
    pub fn observations(&self) -> Vec<Value> {
        self.query("SELECT data FROM observations ORDER BY id")
            .into_iter()
            .map(|row| serde_json::from_str(row[0].as_str().expect("data")).expect("json"))
            .collect()
    }

    pub fn pair(&self, account: &str, gateway: &str) -> Value {
        self.query(&format!(
            "SELECT data FROM pairs WHERE account='{account}' AND gateway='{gateway}'"
        ))
        .first()
        .map_or(Value::Null, |row| {
            serde_json::from_str(row[0].as_str().expect("data")).expect("json")
        })
    }

    /// 直接写入一条组合事实（同 `Harness::seed_pair`）。
    pub fn seed_pair(&self, account: &str, gateway: &str, data: Value) {
        self.connection()
            .execute(
                "INSERT OR REPLACE INTO pairs VALUES (?,?,?)",
                rusqlite::params![account, gateway, data.to_string()],
            )
            .expect("seed pair");
    }

    /// 播种一个持有窗口且未到期的组合（含配套材料），返回代次。
    pub fn seed_ready(&self, account: &str, gateway: &str, generation: &str) {
        let now = now_secs();
        self.seed_pair(
            account,
            gateway,
            json!({"accountId": account, "gateway": gateway, "state": "ready", "generation": generation,
                "firstContactAt": now, "lastContactAt": now, "verifiedAt": now, "inFlight": 0}),
        );
        self.connection()
            .execute(
                "INSERT OR REPLACE INTO materials VALUES (?,?,?,?)",
                rusqlite::params![
                    account,
                    generation,
                    now + 600.0,
                    json!({"state": format!("state-{generation}"), "cookie": cookie_for(gateway, now + 600.0), "expires": now + 600.0}).to_string()
                ],
            )
            .expect("seed material");
    }

    /// 播种一个静置中的组合。
    pub fn seed_cooling(&self, account: &str, gateway: &str, seconds: f64) {
        let now = now_secs();
        self.seed_pair(
            account,
            gateway,
            json!({"accountId": account, "gateway": gateway, "state": "cooling", "restSeconds": seconds,
                "restUntil": now + seconds, "lastContactAt": now, "inFlight": 0}),
        );
    }

    pub fn queue_revision(&self) -> i64 {
        dispatch(&self.store, "read", &object_of(json!({})))
            .expect("read")
            .0["accountQueue"]["revision"]
            .as_i64()
            .expect("revision")
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.task.abort();
    }
}
