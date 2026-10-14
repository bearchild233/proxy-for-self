//! 请求路径编排：把旧网关里 `gateway_pool.rs` 的选号（`selection`）、路由（`route`）、打票（`start_prepare`、
//! `prepare`、`prepare_material`、`probe_headers`）、拿票（`use_model`）与结果观察（`GatewayPoolGuard`）
//! 迁到插件钩子上。状态机本体仍在 `Store::runtime`；这里负责资格判断、访问内核能力和内存缓存。
//!
//! 钩子与旧路径的对应关系：
//! - `route.select` ← `selection` + `ordered_accounts` + 续接去留规则（`select.rs`）
//! - `route.prepare` ← `route` 循环的一步（`prepare.rs`），打票在后台任务里执行（`probe.rs`），拿票见 `ticket.rs`
//! - `response.head` ← `GatewayPoolGuard::observe` 与 `GatewayPoolAttempt::send` 的判定（`head.rs`）
//! - `request.finish` ← `GatewayPoolGuard::finish`（`head.rs`）
//!
//! 普通账号观察与移组见 `observe`，恢复检测与持续模式见 `background`。

mod access;
mod background;
mod head;
mod observe;
mod persistence;
mod prepare;
mod probe;
mod select;
mod ticket;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};

use plugin_sdk::protocol::Reason;
use serde_json::{Value, json};
use tokio::{sync::Semaphore, task::JoinHandle};

use crate::{error::Failure, store::Store};

/// 设置缓存时间（与旧实现一致：250 毫秒）。
const SETTINGS_TTL: Duration = Duration::from_millis(250);
/// 网关目录缓存 60 秒，保留 600 秒（与旧实现一致）。
const CATALOG_TTL: Duration = Duration::from_secs(60);
const CATALOG_KEEP: Duration = Duration::from_secs(600);
/// 材料与票的内存缓存条数上限（与旧实现一致）。
const CACHE_LIMIT: usize = 512;
/// 同时进行的打票任务上限（与旧实现一致）。
const PROBE_PERMITS: usize = 4;
/// 一次打票（取 Cookie、bootstrap、重放）的整体超时（与旧实现一致：35 秒）。
const PREPARE_TIMEOUT: Duration = Duration::from_secs(35);
/// 单次探测请求的超时（与旧实现一致：15 秒）。
const PROBE_ATTEMPT_MS: u64 = 15_000;
/// 内核钩子的调用预算是 2 秒（`gateway-host` 的 `PluginRuntime`）；钩子内部的等待不超过其中的这一段，
/// 超出时返回 `wait` 让内核稍后再来。
// 011 已确认：钩子内部等待上限为 1.5 秒。
const HOOK_BUDGET: Duration = Duration::from_millis(1500);
/// `route.select` 记录的请求状态在没有截止时间可用时的保留时间。
// 011 已确认：未收到请求截止时间前保留 600 秒。
const SELECT_KEEP_SECONDS: f64 = 600.0;

/// 池设置中请求路径用到的字段（`config` 动作的结果，含池服务凭证）。
#[derive(Clone)]
pub struct PoolSettings {
    pub dynamic_groups: bool,
    pub normal_group_id: String,
    pub degraded_group_id: String,
    pub mode: String,
    pub pool_account_ids: HashMap<String, String>,
    pub pool_url: String,
    pub pool_token: String,
    pub probe_interval_ms: u64,
    /// 打票用模型；业务请求固定使用由它取得并验证的 S0。
    pub probe_model: String,
    /// 取网关目录与 Cookie 时走直连；否则走该账号的出口。
    pub pool_direct: bool,
}

impl PoolSettings {
    fn from_config(value: &Value) -> Self {
        let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
        Self {
            dynamic_groups: value["dynamicGroups"] == true,
            normal_group_id: text("normalGroupId"),
            degraded_group_id: text("degradedGroupId"),
            mode: text("mode"),
            pool_account_ids: value["poolAccountIds"]
                .as_object()
                .map(|map| {
                    map.iter()
                        .filter_map(|(key, identity)| {
                            Some((key.clone(), identity.as_str()?.to_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            pool_url: text("poolUrl"),
            pool_token: text("poolToken"),
            probe_interval_ms: value["probeIntervalMs"]
                .as_f64()
                .map_or(0, |ms| ms as u64)
                .clamp(1, 1000),
            probe_model: value["probeModel"]
                .as_str()
                .unwrap_or("gpt-6.1-sol")
                .to_owned(),
            pool_direct: value["poolDirect"].as_bool().unwrap_or(true),
        }
    }

    pub fn dynamic_groups_ready(&self) -> bool {
        self.dynamic_groups
            && !self.normal_group_id.is_empty()
            && !self.degraded_group_id.is_empty()
            && self.normal_group_id != self.degraded_group_id
    }

    /// 池服务成员标识：管理员显式映射优先，否则按上游账号与用户自动生成。
    pub fn member_id(
        &self,
        account_id: &str,
        upstream_account_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Option<String> {
        self.pool_account_ids.get(account_id).cloned().or_else(|| {
            Some(format!(
                "gwpool-member:{}/{}",
                upstream_account_id?, user_id?
            ))
        })
    }
}

/// 网关池拒绝或无法完成路由的原因；原因码与中文说明沿用旧路径，写入错误诊断。
#[derive(Debug, Clone)]
pub(crate) struct PoolFailure {
    pub code: &'static str,
    pub message: String,
}

impl PoolFailure {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn control_plane() -> Self {
        Self::new(
            "control_plane_unavailable",
            "网关池控制面暂时不可用，读不到设置或调度状态，请检查插件服务是否在运行",
        )
    }

    /// 内核能力（账号、额度）读取失败。旧路径里这些读取在进程内，没有对应原因码。
    // 011 已确认采用此原因码与文案。
    pub fn kernel() -> Self {
        Self::new(
            "kernel_unavailable",
            "无法从内核读取账号或额度状态，网关池暂不检测该账号",
        )
    }

    /// 运行状态机返回的阻塞原因，原因码只接受已知的几种。
    pub fn from_block(value: &Value) -> Option<Self> {
        let code = match value["code"].as_str()? {
            "queue_disabled" => "queue_disabled",
            "quota_exhausted" => "quota_exhausted",
            "catalog_empty" => "catalog_empty",
            "all_resting" => "all_resting",
            _ => "pool_blocked",
        };
        let message = value["message"]
            .as_str()
            .unwrap_or("网关池暂时不能调度该账号");
        Some(Self::new(code, message))
    }

    pub fn reason(&self) -> Reason {
        Reason {
            code: self.code.to_owned(),
            message: self.message.clone(),
        }
    }
}

impl From<Failure> for PoolFailure {
    fn from(_: Failure) -> Self {
        Self::control_plane()
    }
}

/// 池窗口：网关与代次。
#[derive(Clone)]
pub(super) struct Pair {
    pub gateway: String,
    pub generation: String,
}

impl Pair {
    pub fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            gateway: value["gateway"].as_str()?.to_owned(),
            generation: value["generation"].as_str()?.to_owned(),
        })
    }
}

/// 窗口配套材料：打票得到的 state 与路由 Cookie，随代次失效。
#[derive(Clone)]
pub(super) struct Material {
    pub pair: Pair,
    pub state: String,
    pub cookie: String,
    /// Unix 秒。
    pub expires: f64,
}

/// 同一窗口内其他模型的票：只拿 state，不单独判定。
#[derive(Clone)]
pub(super) struct Ticket {
    pub state: String,
    pub cookie: String,
    pub expires: f64,
}

/// 单次访问的计时：UTC 起点用于跨请求合并，耗时只用单调时钟；终态一经固定不再改变。
pub(super) struct AccessTiming {
    pub id: String,
    at: f64,
    started: Instant,
    elapsed_before: Duration,
    ended: Option<(Duration, &'static str)>,
}

impl AccessTiming {
    pub fn new(id: String, at: f64) -> Self {
        Self {
            id,
            at,
            started: Instant::now(),
            elapsed_before: Duration::ZERO,
            ended: None,
        }
    }

    pub fn end(&mut self, outcome: &'static str) {
        self.ended
            .get_or_insert_with(|| (self.elapsed_before + self.started.elapsed(), outcome));
    }

    pub fn value(&self) -> Value {
        let (elapsed, outcome) = self
            .ended
            .unwrap_or_else(|| (self.elapsed_before + self.started.elapsed(), "headers"));
        json!({"id": self.id, "startedAt": self.at, "elapsedMs": elapsed.as_millis(), "ended": self.ended.is_some(), "outcome": outcome})
    }

    /// 如果此刻以 `outcome` 结束，`value` 会给出的样子；不改变计时本身。
    /// 业务结果与租约归还同事务提交时，归还用的访问值要在知道是否归还之前备好。
    pub fn closed_value(&self, outcome: &'static str) -> Value {
        let (elapsed, outcome) = self
            .ended
            .unwrap_or_else(|| (self.elapsed_before + self.started.elapsed(), outcome));
        json!({"id": self.id, "startedAt": self.at, "elapsedMs": elapsed.as_millis(), "ended": true, "outcome": outcome})
    }
}

/// 已预约、尚未交给内核的窗口：等待其他模型的票时在多次 `route.prepare` 之间保留。
pub(super) struct Held {
    pub lease: String,
    pub material: Material,
    pub ticket: Option<Ticket>,
    pub task: Option<JoinHandle<Result<Ticket, ()>>>,
}

/// 同一请求对某个账号的路由进度（对应旧 `route` 循环里的局部变量）。
pub(super) struct Attempt {
    pub waiting: String,
    pub queue_turn: Value,
    pub held: Option<Held>,
}

impl Default for Attempt {
    fn default() -> Self {
        Self {
            waiting: "尚未收到调度状态".to_owned(),
            queue_turn: Value::Null,
            held: None,
        }
    }
}

/// 按请求标识保存：`route.select` 得到的候选与普通账号退路，以及各账号的路由进度。
pub(super) struct RequestState {
    /// Unix 秒；过期后清除。
    pub expires: f64,
    /// 本 Key 范围内可替代的降智账号（可调度的，顺序同选择结果）。
    pub alternatives: Vec<String>,
    /// 是否还有降智组之外的普通账号可作退路。
    pub ordinary: bool,
    pub attempts: HashMap<String, Attempt>,
}

/// 已交给内核的租约：响应头判定与请求结束时需要的事实。
pub(super) struct LeaseState {
    pub account: String,
    pub model: String,
    pub gateway: String,
    pub generation: String,
    pub state: String,
    /// 本次发送使用的 Cookie（票的 Cookie 或窗口 Cookie）。
    pub cookie: String,
    pub timing: Option<AccessTiming>,
    /// Unix 秒；过期后清除。
    pub expires: f64,
}

pub struct Pool {
    store: Arc<Store>,
    settings: Mutex<Option<(Instant, Arc<PoolSettings>)>>,
    materials: Mutex<HashMap<String, Material>>,
    tickets: Mutex<HashMap<String, Ticket>>,
    ticket_inflight: Mutex<HashSet<String>>,
    /// 按池成员缓存的网关目录，60 秒内复用，避免每次找票都查询池服务。
    catalogs: Mutex<HashMap<String, (Instant, Value)>>,
    catalog_inflight: Mutex<HashSet<String>>,
    probes: Arc<Semaphore>,
    requests: Mutex<HashMap<String, RequestState>>,
    leases: Mutex<HashMap<String, LeaseState>>,
    active: Mutex<HashMap<String, f64>>,
    recoverable: Mutex<HashMap<String, f64>>,
    recovering: Mutex<HashSet<String>>,
    confirmations: Mutex<HashMap<String, f64>>,
    stopped: AtomicBool,
    background: Mutex<Option<JoinHandle<()>>>,
}

impl Pool {
    pub(crate) fn invalidate_settings(&self) {
        *self.settings.lock().expect("settings lock") = None;
    }

    pub fn new(store: Arc<Store>) -> Arc<Self> {
        Arc::new(Self {
            store,
            settings: Mutex::new(None),
            materials: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
            ticket_inflight: Mutex::new(HashSet::new()),
            catalogs: Mutex::new(HashMap::new()),
            catalog_inflight: Mutex::new(HashSet::new()),
            probes: Arc::new(Semaphore::new(PROBE_PERMITS)),
            requests: Mutex::new(HashMap::new()),
            leases: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
            recoverable: Mutex::new(HashMap::new()),
            recovering: Mutex::new(HashSet::new()),
            confirmations: Mutex::new(HashMap::new()),
            stopped: AtomicBool::new(false),
            background: Mutex::new(None),
        })
    }

    /// 当前时间（Unix 秒），与运行状态机用同一个时钟。
    fn now(&self) -> f64 {
        (self.store.clock)()
    }

    fn new_id(&self) -> String {
        (self.store.ids)()
    }

    /// 运行状态机是同步的 SQLite 事务，放到阻塞线程池，避免占用插件的异步运行时。
    pub(super) async fn runtime(&self, message: Value) -> Result<Value, Failure> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.runtime(&message))
            .await
            .map_err(|error| Failure::Internal(error.to_string()))?
    }

    /// 打票结果与租约归还在同一事务内完成，见 `Store::runtime_then_finish`。
    pub(super) async fn runtime_then_finish(
        &self,
        report: Value,
        finish: Value,
    ) -> (Result<Value, Failure>, Result<Value, Failure>) {
        let store = Arc::clone(&self.store);
        let joined =
            tokio::task::spawn_blocking(move || store.runtime_then_finish(&report, &finish)).await;
        match joined {
            Ok(Ok(results)) => results,
            Ok(Err(error)) => (
                Err(Failure::Internal(error.to_string())),
                Err(Failure::Internal(error.to_string())),
            ),
            Err(error) => (
                Err(Failure::Internal(error.to_string())),
                Err(Failure::Internal(error.to_string())),
            ),
        }
    }

    /// 业务结果与租约归还在同一事务内完成，见 `Store::runtime_then_finish_if`。
    /// `needed` 在事务内按业务结果的返回值判断是否归还；归还成功时同时清掉租约的接管事实。
    pub(super) async fn business_result_then_finish(
        &self,
        report: Value,
        finish: Value,
        lease: &str,
        needed: impl Fn(&Result<Value, Failure>) -> bool + Send + 'static,
    ) -> (Result<Value, Failure>, Option<Result<Value, Failure>>) {
        let store = Arc::clone(&self.store);
        let joined = tokio::task::spawn_blocking(move || {
            store.runtime_then_finish_if(&report, &finish, &needed)
        })
        .await;
        let (first, second) = match joined {
            Ok(Ok(results)) => results,
            Ok(Err(error)) => (
                Err(Failure::Internal(error.to_string())),
                Some(Err(Failure::Internal(error.to_string()))),
            ),
            Err(error) => (
                Err(Failure::Internal(error.to_string())),
                Some(Err(Failure::Internal(error.to_string()))),
            ),
        };
        if matches!(second, Some(Ok(_))) {
            let store = self.store.clone();
            let lease = lease.to_owned();
            let _ = tokio::task::spawn_blocking(move || store.delete_hook_lease(&lease)).await;
        }
        (first, second)
    }

    pub(super) async fn settings(&self) -> Result<Arc<PoolSettings>, Failure> {
        if let Some((at, settings)) = &*self.settings.lock().expect("settings lock")
            && at.elapsed() < SETTINGS_TTL
        {
            return Ok(Arc::clone(settings));
        }
        let config = self.runtime(json!({"action": "config"})).await?;
        let settings = Arc::new(PoolSettings::from_config(&config));
        *self.settings.lock().expect("settings lock") =
            Some((Instant::now(), Arc::clone(&settings)));
        Ok(settings)
    }

    /// 归还租约；失败时由运行状态机的租约超时回收兜底。
    pub(super) async fn release(&self, account: &str, lease: &str, access: Option<Value>) -> bool {
        let released = self.runtime(
            json!({"action": "finish", "accountId": account, "leaseId": lease, "access": access}),
        )
        .await
        .is_ok();
        if released {
            let store = self.store.clone();
            let lease = lease.to_owned();
            let _ = tokio::task::spawn_blocking(move || store.delete_hook_lease(&lease)).await;
        }
        released
    }

    /// 清除过期的请求与租约状态；已预约却没有交给内核的窗口在这里归还。
    pub(super) fn sweep(self: &Arc<Self>) {
        let now = self.now();
        let mut orphans = Vec::new();
        self.requests
            .lock()
            .expect("requests lock")
            .retain(|_, state| {
                if state.expires > now {
                    return true;
                }
                for (account, attempt) in &mut state.attempts {
                    if let Some(held) = attempt.held.take() {
                        orphans.push((account.clone(), held.lease));
                    }
                }
                false
            });
        self.leases
            .lock()
            .expect("leases lock")
            .retain(|_, lease| lease.expires > now);
        if !orphans.is_empty() {
            let pool = Arc::clone(self);
            tokio::spawn(async move {
                for (account, lease) in orphans {
                    pool.release(&account, &lease, None).await;
                }
            });
        }
    }

    fn insert_bounded<V>(map: &mut HashMap<String, V>, key: String, value: V) {
        if map.len() >= CACHE_LIMIT
            && let Some(old) = map.keys().next().cloned()
        {
            map.remove(&old);
        }
        map.insert(key, value);
    }
}
