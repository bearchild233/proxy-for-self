use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use plugin_sdk::{
    Context, Kernel, Plugin, RpcResult,
    protocol::{AdminCallParams, FORBIDDEN, HealthResult, INTERNAL, NOT_SUPPORTED, RpcError},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::{sync::Notify, task::JoinHandle};

use crate::{
    model::{
        Account, CancelParams, ConfigureParams, Directory, HistoryParams, Outcome, ReadParams,
        RunParams,
    },
    schedule,
    store::{Store, new_id},
};

const READ_BUDGET: Duration = Duration::from_secs(8);
const EXECUTION_BUDGET: Duration = Duration::from_secs(18);
const DIRECTORY_TTL: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Cache {
    accounts: Vec<Account>,
    refreshed: Option<Instant>,
    ready: bool,
}

struct Inner {
    store: Arc<Store>,
    owner: String,
    cache: Mutex<Cache>,
    stopped: AtomicBool,
    wake: Notify,
    background: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct WarmupService(Arc<Inner>);

impl WarmupService {
    pub fn new(store: Store) -> Self {
        Self(Arc::new(Inner {
            store: Arc::new(store),
            owner: new_id(),
            cache: Mutex::new(Cache::default()),
            stopped: AtomicBool::new(false),
            wake: Notify::new(),
            background: Mutex::new(None),
        }))
    }

    async fn database<T: Send + 'static>(
        &self,
        action: impl FnOnce(&Store) -> RpcResult<T> + Send + 'static,
    ) -> RpcResult<T> {
        let store = self.0.store.clone();
        tokio::task::spawn_blocking(move || action(&store))
            .await
            .map_err(|_| RpcError::new(INTERNAL, "warmup storage task failed"))?
    }

    async fn directory(
        &self,
        kernel: &Kernel,
        ids: Option<Vec<String>>,
        model: Option<String>,
        include_models: bool,
    ) -> RpcResult<Vec<Account>> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Params {
            #[serde(skip_serializing_if = "Option::is_none")]
            ids: Option<Vec<String>>,
            #[serde(skip_serializing_if = "Option::is_none")]
            model: Option<String>,
            include_models: bool,
        }
        let all = ids.is_none() && model.is_none() && !include_models;
        let result: Directory = kernel
            .call(
                "accounts.warmup.read",
                &Params {
                    ids,
                    model,
                    include_models,
                },
                READ_BUDGET,
            )
            .await?;
        let observed = result.accounts.clone();
        self.database(move |store| store.observe(&observed)).await?;
        let mut cache = self
            .0
            .cache
            .lock()
            .map_err(|_| RpcError::new(INTERNAL, "warmup cache"))?;
        if all {
            cache.accounts = result.accounts.clone();
            cache.refreshed = Some(Instant::now());
        } else {
            for account in &result.accounts {
                if let Some(current) = cache.accounts.iter_mut().find(|item| item.id == account.id)
                {
                    *current = account.clone();
                } else {
                    cache.accounts.push(account.clone());
                }
            }
        }
        cache.ready = true;
        Ok(result.accounts)
    }

    fn cached(&self) -> Vec<Account> {
        self.0.cache.lock().expect("warmup cache").accounts.clone()
    }

    async fn refresh(&self, kernel: &Kernel, force: bool) -> RpcResult<Vec<Account>> {
        let needed = force
            || self
                .0
                .cache
                .lock()
                .expect("warmup cache")
                .refreshed
                .is_none_or(|at| at.elapsed() >= DIRECTORY_TTL);
        if needed && let Err(error) = self.directory(kernel, None, None, false).await {
            self.0.cache.lock().expect("warmup cache").ready = false;
            return Err(error);
        }
        Ok(self.cached())
    }

    async fn read(&self, kernel: &Kernel, params: ReadParams) -> RpcResult<Value> {
        if let Some(ids) = &params.account_ids {
            schedule::ids(ids)?;
        }
        if params
            .model
            .as_deref()
            .is_some_and(|value| !schedule::identifier(value))
        {
            return Err(schedule::invalid("invalid model"));
        }
        let filtered = params.account_ids.is_some()
            || params.include_models == Some(true)
            || params.model.is_some();
        let accounts = if filtered {
            self.directory(
                kernel,
                params.account_ids,
                params.model,
                params.include_models.unwrap_or(false),
            )
            .await?
        } else {
            self.refresh(kernel, false)
                .await
                .unwrap_or_else(|_| self.cached())
        };
        let ready = self.0.cache.lock().expect("warmup cache").ready;
        let runtime_accounts = self.cached();
        let now = Utc::now();
        self.database(move |store| {
            let (revision, settings) = store.settings()?;
            let mut runtime = store.runtime(now, &runtime_accounts, &settings)?;
            runtime["ready"] = json!(ready);
            let account_states = store.account_states(now, &accounts)?;
            Ok(json!({"available":true,"revision":revision,"settings":settings,"accounts":accounts,"accountStates":account_states,"runtime":runtime}))
        }).await
    }

    pub async fn dispatch(&self, kernel: &Kernel, method: &str, params: Value) -> RpcResult<Value> {
        fn decode<T: DeserializeOwned>(params: Value) -> RpcResult<T> {
            serde_json::from_value(params).map_err(|_| schedule::invalid("invalid parameters"))
        }
        match method.strip_prefix("account-warmup.").unwrap_or(method) {
            "read" => self.read(kernel, decode(params)?).await,
            "configure" => {
                let input: ConfigureParams = decode(params)?;
                self.database(move |store| {
                    store.configure(input.expected_revision, &input.settings)
                })
                .await?;
                self.0.wake.notify_one();
                self.read(
                    kernel,
                    ReadParams {
                        account_ids: None,
                        include_models: None,
                        model: None,
                    },
                )
                .await
            }
            "run" => {
                let input: RunParams = decode(params)?;
                if !schedule::identifier(&input.submission_id)
                    || !matches!(input.scope.as_str(), "all" | "selected")
                    || input
                        .model
                        .as_deref()
                        .is_some_and(|value| !schedule::identifier(value))
                    || input
                        .prompt
                        .as_deref()
                        .is_some_and(|value| !schedule::prompt(value))
                    || input
                        .reasoning_effort
                        .as_deref()
                        .is_some_and(|value| !schedule::reasoning_effort(value))
                {
                    return Err(schedule::invalid("invalid run parameters"));
                }
                let ids = input.account_ids.unwrap_or_default();
                schedule::ids(&ids)?;
                if input.scope == "selected" && ids.is_empty() {
                    return Err(schedule::invalid("selected scope requires accountIds"));
                }
                if input.scope == "all" && !ids.is_empty() {
                    return Err(schedule::invalid("all scope does not accept accountIds"));
                }
                let submission = input.submission_id.clone();
                if let Some(run) = self
                    .database(move |store| store.existing_submission(&submission))
                    .await?
                {
                    self.0.wake.notify_one();
                    return Ok(json!({"run":run}));
                }
                let mut accounts = self.refresh(kernel, true).await?;
                if input.scope == "selected" {
                    accounts.retain(|account| ids.contains(&account.id));
                    for id in ids {
                        if !accounts.iter().any(|account| account.id == id) {
                            accounts.push(Account {
                                id,
                                provider: String::new(),
                                authentication_kind: String::new(),
                                enabled: false,
                                status: "unavailable".into(),
                                supported: false,
                                reason_code: Some("account_not_found".into()),
                                model_allowed: None,
                                models: None,
                                short_term: None,
                            });
                        }
                    }
                }
                let run = self
                    .database(move |store| {
                        // 手动参数只覆盖本次入队快照，不写回自动设置或账号覆盖。
                        let (_, mut settings) = store.settings()?;
                        if let Some(model) = input.model {
                            settings.default_model = model;
                            for item in &mut settings.account_overrides {
                                item.model = None;
                            }
                        }
                        if let Some(prompt) = input.prompt {
                            settings.prompt = prompt;
                        }
                        if let Some(effort) = input.reasoning_effort {
                            settings.reasoning_effort = effort;
                            for item in &mut settings.account_overrides {
                                item.reasoning_effort = None;
                            }
                        }
                        store.enqueue_manual(&input.submission_id, &accounts, &settings, Utc::now())
                    })
                    .await?;
                self.0.wake.notify_one();
                Ok(json!({"run":run}))
            }
            "history" => {
                let input: HistoryParams = decode(params)?;
                let limit = input.limit.unwrap_or(20);
                if !(1..=100).contains(&limit)
                    || input
                        .run_id
                        .as_deref()
                        .is_some_and(|id| !schedule::identifier(id))
                    || input
                        .source
                        .as_deref()
                        .is_some_and(|source| !matches!(source, "automatic" | "manual"))
                {
                    return Err(schedule::invalid("invalid history parameters"));
                }
                self.database(move |store| {
                    store.history_filtered(limit, input.run_id.as_deref(), input.source.as_deref())
                })
                .await
            }
            "cancel" => {
                let input: CancelParams = decode(params)?;
                if !schedule::identifier(&input.run_id) {
                    return Err(schedule::invalid("invalid runId"));
                }
                let run = self
                    .database(move |store| store.cancel(&input.run_id, Utc::now()))
                    .await?;
                Ok(json!({"run":run}))
            }
            _ => Err(RpcError::new(NOT_SUPPORTED, "warmup method")),
        }
    }

    /// 每轮最多认领一项，网络调用完全在数据库事务之外。
    pub async fn tick(&self, kernel: &Kernel, now: DateTime<Utc>) -> RpcResult<()> {
        self.database(move |store| store.recover(now)).await?;
        if self.0.stopped.load(Ordering::SeqCst) {
            return Ok(());
        }
        // 手动任务已经持久化了账号/模型快照；目录短暂不可用时仍应继续
        // 认领这一项，而不是把整个队列卡在刷新上。目录失败只跳过本轮
        // 自动排程，错误在本轮末返回给后台循环以保持退避。
        let auto_enabled = self
            .database(|store| Ok(store.settings()?.1.enabled))
            .await?;
        let mut directory_error = None;
        if auto_enabled {
            match self.refresh(kernel, false).await {
                Ok(accounts) => {
                    self.database(move |store| {
                        let (_, settings) = store.settings()?;
                        store.schedule(&accounts, &settings, now)
                    })
                    .await?;
                }
                Err(error) => directory_error = Some(error),
            }
        }
        if self.0.stopped.load(Ordering::SeqCst) {
            return Ok(());
        }
        let owner = self.0.owner.clone();
        // 在阻塞线程真正进入 SQLite 事务前再次检查停止标志，避免停止信号与
        // spawn_blocking 排队交错时仍认领一项新任务。检查之后的事务开始点
        // 是本轮认领的线性化点；停止只会影响下一轮。
        let inner = self.0.clone();
        let item = self
            .database(move |store| {
                if inner.stopped.load(Ordering::SeqCst) {
                    Ok(None)
                } else {
                    store.claim(&owner, now)
                }
            })
            .await?;
        let Some(item) = item else {
            return directory_error.map_or(Ok(()), Err);
        };
        // 从 claim 开始到结果持久化都不在停止信号上 select，已认领的发送在预算内收尾。
        let result: RpcResult<Outcome> = kernel.call("accounts.warmup.run", &json!({"accountId":item.account_id,"model":item.model,"prompt":item.prompt,"reasoningEffort":item.reasoning_effort,"timeoutMs":15000,"attemptId":item.id}), EXECUTION_BUDGET).await;
        let outcome = result.unwrap_or_else(|_| Outcome {
            outcome: "unknown".into(),
            reason_code: Some("kernel_call_interrupted".into()),
            status: None,
            duration_ms: 0,
            send_state: "ambiguous".into(),
            short_term: None,
        });
        let observed = outcome.short_term.clone();
        let account_id = item.account_id.clone();
        let id = item.id;
        let owner = self.0.owner.clone();
        self.database(move |store| store.finish(&id, &owner, &outcome, Utc::now()))
            .await?;
        if let Some(window) = observed {
            let mut account = self
                .cached()
                .into_iter()
                .find(|account| account.id == account_id);
            if let Some(account) = account.as_mut() {
                account.short_term = Some(window);
            }
            if let Some(account) = account {
                self.database(move |store| store.observe(&[account]))
                    .await?;
            }
        }
        directory_error.map_or(Ok(()), Err)
    }

    /// main 在 SDK 结束后等待此句柄；避免 runtime 返回时直接丢弃后台发送 future。
    pub async fn join_background(&self, budget: Duration) -> RpcResult<()> {
        let task = self.0.background.lock().expect("warmup background").take();
        if let Some(mut task) = task {
            match tokio::time::timeout(budget, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Err(RpcError::new(INTERNAL, "warmup background task failed")),
                Err(_) => {
                    task.abort();
                    return Err(RpcError::new(INTERNAL, "warmup shutdown deadline exceeded"));
                }
            }
        }
        Ok(())
    }
}

impl Plugin for WarmupService {
    fn start(&self, kernel: Kernel) {
        let mut background = self.0.background.lock().expect("warmup background");
        if background.is_some() || self.0.stopped.load(Ordering::SeqCst) {
            return;
        }
        let service = self.clone();
        *background = Some(tokio::spawn(async move {
            loop {
                if service.0.stopped.load(Ordering::SeqCst) {
                    break;
                }
                let delay = if let Err(error) = service.tick(&kernel, Utc::now()).await {
                    // 只记录稳定错误码；内核RPC正文可能携带内部信息，不写入任务历史。
                    eprintln!("account warmup cycle unavailable: {}", error.code);
                    Duration::from_secs(5)
                } else {
                    Duration::from_secs(1)
                };
                tokio::select! { () = service.0.wake.notified() => {}, () = tokio::time::sleep(delay) => {} }
            }
        }));
    }

    fn stop_background(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
        self.0.wake.notify_one();
    }

    async fn health(&self, _: Context) -> RpcResult<HealthResult> {
        Ok(HealthResult {
            ready: !self.0.stopped.load(Ordering::SeqCst),
            detail: None,
        })
    }

    async fn admin_call(&self, context: Context, params: AdminCallParams) -> RpcResult<Value> {
        if params.actor.role != "admin" {
            return Err(RpcError::new(FORBIDDEN, "administrator required"));
        }
        self.dispatch(&context.kernel(), &params.method, params.params)
            .await
    }
}
