use async_trait::async_trait;
use gateway_core::{
    lifecycle::CancellationToken,
    plugin::{Ownership, PluginCapabilities, PluginHooks, watch_matches},
};
use kernel_protocol::*;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::task::JoinHandle;

use super::{
    connection,
    registry::{PluginRegistration, PluginRegistry, PluginRuntimeConfig},
    state::{ConnectionState, Entry, RuntimeSnapshot, State, read_cache, update_cache},
    transport::unavailable,
};

pub struct PluginRuntime {
    state: Arc<Mutex<State>>,
    stop: CancellationToken,
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl PluginRuntime {
    pub fn start(
        config: PluginRuntimeConfig,
        kernel: KernelInfo,
        capabilities: Arc<dyn PluginCapabilities>,
    ) -> Result<Option<Self>, std::io::Error> {
        let (path, cache) = match (config.registry_path, config.ownership_cache_path) {
            (None, None) => return Ok(None),
            (Some(path), Some(cache)) => (path, cache),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "both plugin paths are required",
                ));
            }
        };
        let mut initial = State {
            entries: BTreeMap::new(),
            cache_error: None,
            registry_error: None,
            registry_error_at: None,
            cache,
        };
        // 返回端口前恢复缓存，调用方不能在监督任务首次调度前看到错误的 NotOwned。
        match PluginRegistry::load(&path) {
            Ok(registry) => {
                let cache = match read_cache(&initial.cache) {
                    Ok(cache) => cache,
                    Err(error) => {
                        initial.cache_error = Some(format!("ownership cache: {error}"));
                        tracing::error!(%error, "plugin ownership cache unavailable; using empty cache");
                        Default::default()
                    }
                };
                for registration in registry.plugins {
                    let id = registration.id.clone();
                    let groups = cache.plugins.get(&id).cloned().unwrap_or_default();
                    let enabled = registration.enabled;
                    initial
                        .entries
                        .insert(id.clone(), Entry::new(registration, vec![]));
                    if enabled {
                        initial.ownership(
                            &id,
                            groups,
                            cache.watched_groups.get(&id).cloned().unwrap_or_default(),
                        );
                    }
                }
            }
            Err(error) => initial.registry_error(error),
        }
        let state = Arc::new(Mutex::new(initial));
        let stop = CancellationToken::new();
        let task = tokio::spawn(supervise(
            path,
            state.clone(),
            stop.clone(),
            kernel,
            capabilities,
        ));
        Ok(Some(Self {
            state,
            stop,
            task: tokio::sync::Mutex::new(Some(task)),
        }))
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        let state = self.state.lock().expect("plugin state");
        RuntimeSnapshot {
            plugins: state
                .entries
                .values()
                .map(|entry| entry.snapshot.clone())
                .collect(),
            cache_error: state.cache_error.clone(),
            registry_error: state.registry_error.clone(),
            registry_error_at: state.registry_error_at,
        }
    }

    pub async fn shutdown(&self) {
        self.stop.cancel();
        // 监督任务退出前回收全部连接；不向插件发送 shutdown。
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }

    async fn call<P: Serialize + Sync, R: DeserializeOwned>(
        &self,
        plugin: &str,
        method: &str,
        params: &P,
        budget: Duration,
    ) -> Result<R, RpcError> {
        // 选择连接与准入在同一把状态锁内完成：连接切换时已准入的旧调用必定被排空等待。
        let (peer, admission) = {
            let state = self.state.lock().expect("plugin state");
            let peer = state
                .entries
                .get(plugin)
                .filter(|entry| entry.snapshot.state == ConnectionState::Available)
                .and_then(|entry| entry.peer.clone())
                .ok_or_else(unavailable)?;
            let admission = peer.admit();
            (peer, admission)
        };
        let result = peer.call(method, params, budget).await;
        drop(admission);
        if let Err(error) = &result {
            if let Some(entry) = self
                .state
                .lock()
                .expect("plugin state")
                .entries
                .get_mut(plugin)
            {
                entry.error(error);
            }
            if matches!(error.code.as_str(), TIMEOUT | UNAVAILABLE) {
                return Err(RpcError::new(
                    UNAVAILABLE,
                    format!("plugin {plugin}: {method} unavailable"),
                ));
            }
        }
        result
    }

    pub async fn admin_call(
        &self,
        plugin: &str,
        params: AdminCallParams,
    ) -> Result<serde_json::Value, RpcError> {
        self.call(plugin, "admin.call", &params, Duration::from_secs(10))
            .await
    }
}

impl Drop for PluginRuntime {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[async_trait]
impl PluginHooks for PluginRuntime {
    fn ownership(&self, group: &str) -> Ownership {
        let state = self.state.lock().expect("plugin state");
        if let Some(entry) = state.entries.values().find(|entry| {
            entry
                .snapshot
                .owned_groups
                .iter()
                .any(|owned| owned == group)
        }) {
            return Ownership::Owned {
                plugin: entry.registration.id.clone(),
                available: entry.snapshot.state == ConnectionState::Available
                    && entry.peer.as_ref().is_some_and(|peer| peer.is_open()),
            };
        }
        let plugins: Vec<_> = state
            .entries
            .values()
            .filter(|entry| {
                entry
                    .snapshot
                    .watched_groups
                    .iter()
                    .any(|watched| watched == group)
            })
            .map(|entry| entry.registration.id.clone())
            .collect();
        if plugins.is_empty() {
            Ownership::NotOwned
        } else {
            Ownership::Watched { plugins }
        }
    }

    /// 调用方已确认账号不归任何插件所有；按各插件的监控规则（含 `*`、`!分组`）汇总监控者。
    fn watchers(&self, account_groups: &[String]) -> Vec<String> {
        let state = self.state.lock().expect("plugin state");
        state
            .entries
            .values()
            .filter(|entry| watch_matches(&entry.snapshot.watched_groups, account_groups))
            .map(|entry| entry.registration.id.clone())
            .collect()
    }

    async fn route_select(
        &self,
        plugin: &str,
        params: RouteSelectParams,
    ) -> Result<RouteSelectResult, RpcError> {
        self.call(plugin, "route.select", &params, Duration::from_secs(2))
            .await
    }
    async fn response_observe(
        &self,
        plugin: &str,
        params: ResponseObserveParams,
    ) -> Result<ResponseObserveResult, RpcError> {
        // 请求截止时间由 Provider 的 bounded 同时约束；确认探测允许占用 15 秒。
        self.call(plugin, "response.observe", &params, Duration::from_secs(20))
            .await
    }
    async fn route_prepare(
        &self,
        plugin: &str,
        params: RoutePrepareParams,
    ) -> Result<RoutePrepareResult, RpcError> {
        self.call(plugin, "route.prepare", &params, Duration::from_secs(2))
            .await
    }
    async fn response_head(
        &self,
        plugin: &str,
        params: ResponseHeadParams,
    ) -> Result<ResponseHeadResult, RpcError> {
        self.call(plugin, "response.head", &params, Duration::from_secs(2))
            .await
    }
    async fn request_finish(&self, plugin: &str, params: RequestFinishParams) {
        let peer = self
            .state
            .lock()
            .expect("plugin state")
            .entries
            .get(plugin)
            .and_then(|entry| entry.peer.clone());
        let result = match peer {
            Some(peer) => peer.notify(&Message::Notify {
                method: "request.finish".into(),
                params: serde_json::to_value(params).expect("finish params"),
            }),
            None => Err(unavailable()),
        };
        if let Err(error) = result {
            tracing::warn!(plugin_id = plugin, %error, "request.finish delivery failed");
        }
    }
}

/// 注册 ID 当前代表的连接任务。
struct Task {
    registration: PluginRegistration,
    stop: CancellationToken,
    superseded: CancellationToken,
    handle: JoinHandle<()>,
}

/// 已被新 socket 任务接管的旧任务，只负责排空它上面的在途调用。
struct Retiring {
    id: String,
    stop: CancellationToken,
    handle: JoinHandle<()>,
}

/// 取消并等待一个插件的全部任务（当前任务与仍在排空的旧任务）。
async fn stop_plugin(tasks: &mut BTreeMap<String, Task>, retiring: &mut Vec<Retiring>, id: &str) {
    if let Some(task) = tasks.remove(id) {
        task.stop.cancel();
        let _ = task.handle.await;
    }
    let (stopped, kept): (Vec<_>, Vec<_>) = std::mem::take(retiring)
        .into_iter()
        .partition(|old| old.id == id);
    *retiring = kept;
    for old in stopped {
        old.stop.cancel();
        let _ = old.handle.await;
    }
}

async fn supervise(
    path: PathBuf,
    state: Arc<Mutex<State>>,
    stop: CancellationToken,
    kernel: KernelInfo,
    capabilities: Arc<dyn PluginCapabilities>,
) {
    let mut tasks: BTreeMap<String, Task> = BTreeMap::new();
    let mut retiring: Vec<Retiring> = Vec::new();
    let mut stamp = None;
    let mut ticker = tokio::time::interval(Duration::from_secs(5));
    let stopping = stop.cancelled();
    tokio::pin!(stopping);
    loop {
        tokio::select! {
            _ = &mut stopping => break,
            _ = ticker.tick() => {},
        }
        retiring.retain(|old| !old.handle.is_finished());
        let current = std::fs::metadata(&path).and_then(|meta| Ok((meta.modified()?, meta.len())));
        if current.as_ref().ok() == stamp.as_ref() && stamp.is_some() {
            continue;
        }
        let registry = match PluginRegistry::load(&path) {
            Ok(registry) => registry,
            Err(error) => {
                state.lock().expect("plugin state").registry_error(error);
                continue;
            }
        };
        stamp = current.ok();
        // 先取消并回收被删除、禁用或改动了除 socket 外字段的旧连接，再发布新表，
        // 避免旧连接把禁用后的归属写回来。只改 socket 的插件不在此列，由下面的平滑切换处理。
        let restart: Vec<String> = {
            let state = state.lock().expect("plugin state");
            state
                .entries
                .iter()
                .filter(|(id, entry)| {
                    match registry.plugins.iter().find(|plugin| &plugin.id == *id) {
                        None => true,
                        Some(new) => {
                            *new != entry.registration
                                && !new.is_socket_switch_of(&entry.registration)
                        }
                    }
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in &restart {
            stop_plugin(&mut tasks, &mut retiring, id).await;
        }
        {
            let mut state = state.lock().expect("plugin state");
            state.registry_error = None;
            state.registry_error_at = None;
            let cache = match read_cache(&state.cache) {
                Ok(cache) => cache,
                Err(error) => {
                    state.cache_error = Some(format!("ownership cache: {error}"));
                    tracing::error!(%error, "plugin ownership cache unavailable; using empty cache");
                    Default::default()
                }
            };
            let enabled: Vec<_> = registry
                .plugins
                .iter()
                .filter(|p| p.enabled)
                .map(|p| p.id.clone())
                .collect();
            if let Err(error) = update_cache(&state.cache, |cache| {
                cache.plugins.retain(|id, _| enabled.contains(id));
                cache.watched_groups.retain(|id, _| enabled.contains(id));
            }) {
                state.registry_error(format!("ownership cache: {error}"));
            }
            state
                .entries
                .retain(|id, _| registry.plugins.iter().any(|p| &p.id == id));
            for registration in &registry.plugins {
                if state
                    .entries
                    .get(&registration.id)
                    .is_some_and(|entry| entry.registration == *registration)
                {
                    continue;
                }
                // 只改 socket：保留活动连接、快照与归属，新连接握手成功后再接管。
                if let Some(entry) = state.entries.get_mut(&registration.id)
                    && registration.is_socket_switch_of(&entry.registration)
                {
                    entry.registration = registration.clone();
                    continue;
                }
                let groups = if registration.enabled {
                    state
                        .entries
                        .get(&registration.id)
                        .map(|entry| entry.snapshot.owned_groups.clone())
                        .or_else(|| cache.plugins.get(&registration.id).cloned())
                        .unwrap_or_default()
                } else {
                    vec![]
                };
                let watched = if registration.enabled {
                    state
                        .entries
                        .get(&registration.id)
                        .map(|entry| entry.snapshot.watched_groups.clone())
                        .or_else(|| cache.watched_groups.get(&registration.id).cloned())
                        .unwrap_or_default()
                } else {
                    vec![]
                };
                state.entries.insert(
                    registration.id.clone(),
                    Entry::new(registration.clone(), vec![]),
                );
                if registration.enabled {
                    state.ownership(&registration.id, groups, watched);
                }
            }
        }
        for registration in registry.plugins.into_iter().filter(|p| p.enabled) {
            if tasks
                .get(&registration.id)
                .is_some_and(|task| task.registration == registration)
            {
                continue;
            }
            let switching = tasks
                .get(&registration.id)
                .is_some_and(|task| registration.is_socket_switch_of(&task.registration));
            if switching {
                // 旧任务保持服务；新任务握手成功并激活后会退役它。旧任务不再重连旧 socket。
                let old = tasks.remove(&registration.id).expect("switched task");
                old.superseded.cancel();
                retiring.push(Retiring {
                    id: registration.id.clone(),
                    stop: old.stop,
                    handle: old.handle,
                });
            } else if tasks.contains_key(&registration.id) {
                stop_plugin(&mut tasks, &mut retiring, &registration.id).await;
            }
            let stop = CancellationToken::new();
            let superseded = CancellationToken::new();
            let handle = tokio::spawn(connection::run(
                registration.clone(),
                state.clone(),
                stop.clone(),
                kernel.clone(),
                capabilities.clone(),
                superseded.clone(),
            ));
            tasks.insert(
                registration.id.clone(),
                Task {
                    registration,
                    stop,
                    superseded,
                    handle,
                },
            );
        }
    }
    for task in tasks.values() {
        task.stop.cancel();
    }
    for old in &retiring {
        old.stop.cancel();
    }
    for task in tasks.into_values() {
        let _ = task.handle.await;
    }
    for old in retiring {
        let _ = old.handle.await;
    }
}
