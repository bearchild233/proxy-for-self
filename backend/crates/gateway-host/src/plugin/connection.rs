use super::{
    registry::PluginRegistration,
    state::{ConnectionState, State},
    transport::{Outbound, Peer, internal, writer},
};
use gateway_core::{
    lifecycle::CancellationToken,
    plugin::{PluginAccess, PluginCapabilities},
};
use kernel_protocol::*;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::UnixStream, sync::mpsc, task::JoinSet};

/// 单个注册 ID 的连接任务。
///
/// - `stop`：禁用、删除或关停时立即结束会话。
/// - `superseded`：注册表只改了 socket，新 socket 的任务已接管。本任务的会话继续服务，
///   直到它被新连接退役并排空；之后不再重连旧 socket。
pub(super) async fn run(
    registration: PluginRegistration,
    state: Arc<Mutex<State>>,
    stop: CancellationToken,
    kernel: KernelInfo,
    capabilities: Arc<dyn PluginCapabilities>,
    superseded: CancellationToken,
) {
    let mut backoff = Duration::from_millis(100);
    loop {
        if stop.is_cancelled() || superseded.is_cancelled() {
            break;
        }
        let connected = tokio::select! {
            _ = stop.cancelled() => break,
            _ = superseded.cancelled() => break,
            result = connect(&registration, &kernel) => result,
        };
        match connected {
            Ok((stream, hello, uid)) => {
                backoff = Duration::from_millis(100);
                let (peer, rx) = Peer::new(2);
                let result = session(
                    stream,
                    hello,
                    uid,
                    peer.clone(),
                    rx,
                    &registration,
                    state.clone(),
                    stop.clone(),
                    capabilities.clone(),
                )
                .await;
                detach(&state, &registration.id, Some(&peer), result.err());
                // 被新连接退役的会话排空后结束，它的旧 socket 已不再属于本任务。
                if peer.is_retired() {
                    break;
                }
            }
            Err(error) => detach(&state, &registration.id, None, Some(error)),
        }
        if stop.is_cancelled() || superseded.is_cancelled() {
            break;
        }
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = superseded.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {},
        }
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

/// 会话或连接尝试结束后的状态收尾。
///
/// 只有当前活动连接（或尚无活动连接）结束时才清空状态并标记不可用；
/// 已被新连接替换的旧会话只记录错误，不能把正在服务的新连接标记为不可用。
fn detach(state: &Mutex<State>, id: &str, peer: Option<&Arc<Peer>>, error: Option<RpcError>) {
    let mut state = state.lock().expect("plugin state");
    let Some(entry) = state.entries.get_mut(id) else {
        return;
    };
    if let Some(error) = error {
        entry.error(error);
    }
    let current = match (&entry.peer, peer) {
        (Some(active), Some(mine)) => Arc::ptr_eq(active, mine),
        (Some(_), None) => false,
        (None, _) => true,
    };
    if !current {
        return;
    }
    if let Some(active) = entry.peer.take() {
        active.close();
    }
    entry.snapshot.state = ConnectionState::Unavailable;
}

/// 只有活动连接的归属报告才会写入状态；旧连接的通知在切换后必须被忽略。
fn is_active(state: &State, id: &str, peer: &Arc<Peer>) -> bool {
    state
        .entries
        .get(id)
        .and_then(|entry| entry.peer.as_ref())
        .is_some_and(|active| Arc::ptr_eq(active, peer))
}

async fn connect(
    registration: &PluginRegistration,
    kernel: &KernelInfo,
) -> Result<(UnixStream, HelloResult, u32), RpcError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = UnixStream::connect(&registration.socket)
            .await
            .map_err(internal)?;
        let uid = stream.peer_cred().map_err(internal)?.uid();
        if registration.uid.is_some_and(|expected| expected != uid) {
            return Err(internal("plugin UID mismatch"));
        }
        write_frame(
            &mut stream,
            &Message::Request {
                id: 1,
                method: "hello".into(),
                params: serde_json::to_value(HelloParams {
                    kernel_api: KernelApi::default(),
                    kernel: kernel.clone(),
                    // 内核发起的连接；管理连接由插件管理面另行握手。
                    role: ConnectionRole::Kernel,
                })
                .map_err(internal)?,
            },
        )
        .await
        .map_err(internal)?;
        let hello = match read_frame(&mut stream).await.map_err(internal)? {
            Message::Response {
                id: 1,
                payload: ResponsePayload::Result(value),
            } => serde_json::from_value::<HelloResult>(value).map_err(internal)?,
            _ => return Err(internal("invalid hello response")),
        };
        if hello.plugin.id != registration.id {
            return Err(internal("plugin ID mismatch"));
        }
        if !is_compatible(KernelApi::default(), hello.kernel_api) {
            return Err(internal("incompatible kernel API"));
        }
        Ok((stream, hello, uid))
    })
    .await
    .map_err(|_| RpcError::new(TIMEOUT, "hello"))?
}

#[allow(clippy::too_many_arguments)]
async fn session(
    stream: UnixStream,
    hello: HelloResult,
    uid: u32,
    peer: Arc<Peer>,
    rx: mpsc::UnboundedReceiver<Outbound>,
    registration: &PluginRegistration,
    state: Arc<Mutex<State>>,
    stop: CancellationToken,
    capabilities: Arc<dyn PluginCapabilities>,
) -> Result<(), RpcError> {
    let grants: Vec<_> = registration
        .grants
        .iter()
        .filter(|grant| hello.capabilities.contains(grant))
        .cloned()
        .collect();
    let _close = ClosePeer(peer.clone());
    // 握手成功即激活：新调用立即发往本连接，原活动连接转为退役，等在途调用排空后关闭。
    // 归属以本连接 hello 报告为准。
    let previous = {
        let mut state = state.lock().expect("plugin state");
        let entry = state
            .entries
            .get_mut(&registration.id)
            .expect("registered plugin");
        let previous = entry.peer.replace(peer.clone());
        entry.snapshot.state = ConnectionState::Available;
        entry.snapshot.version = Some(hello.plugin.version);
        entry.snapshot.kernel_api = Some(hello.kernel_api);
        entry.snapshot.granted_capabilities = grants.clone();
        entry.snapshot.peer_uid = Some(uid);
        state.ownership(&registration.id, hello.owned_groups, hello.watched_groups);
        previous
    };
    if let Some(previous) = previous {
        previous.retire();
    }
    let (mut reader, write_half) = stream.into_split();
    let write_loop = writer(write_half, rx);
    let health_loop = health(peer.clone());
    tokio::pin!(write_loop, health_loop);
    // 退役后等待本连接已准入的调用全部返回或超时（各自受钩子超时约束），再结束会话。
    let retired_peer = peer.clone();
    let drained = async move {
        retired_peer.retired().await;
        retired_peer.drained().await;
    };
    tokio::pin!(drained);
    let mut calls = JoinSet::new();
    let mut last_id = 0;
    let stopping = stop.cancelled();
    tokio::pin!(stopping);
    loop {
        // 健康计时与能力响应不会取消正在读取的半帧。
        let read = read_frame::<_, Message>(&mut reader);
        tokio::pin!(read);
        let message = loop {
            tokio::select! {
                _ = &mut stopping => return Ok(()),
                _ = &mut drained => return Ok(()),
                _ = &mut write_loop => return Err(internal("plugin writer disconnected")),
                error = &mut health_loop => return Err(error),
                Some(_) = calls.join_next(), if !calls.is_empty() => {},
                message = &mut read => break message.map_err(internal)?,
            }
        };
        match message {
            Message::Response { id, payload } => peer.resolve(id, payload),
            Message::Notify { method, params } if method == "ownership.changed" => {
                let params: OwnershipChangedParams =
                    serde_json::from_value(params).map_err(internal)?;
                let mut state = state.lock().expect("plugin state");
                if is_active(&state, &registration.id, &peer) {
                    state.ownership(&registration.id, params.owned_groups, params.watched_groups);
                }
            }
            Message::Request { id, method, params } => {
                if id <= last_id {
                    return Err(internal("non-monotonic plugin request ID"));
                }
                last_id = id;
                let allowed = grants.contains(&method);
                let peer = peer.clone();
                let capabilities = capabilities.clone();
                let plugin = Access {
                    id: registration.id.clone(),
                    state: state.clone(),
                };
                calls.spawn(async move {
                    let result = if allowed {
                        capabilities.call(&plugin, &method, params).await
                    } else {
                        Err(RpcError::new(FORBIDDEN, method))
                    };
                    let _ = peer.respond(id, result).await;
                });
            }
            Message::Notify { method, params } if method == "log" => {
                // SDK 0.1 的 log 是无 id 通知；同样先授权，错误只能记健康状态，不能伪造响应。
                let allowed = grants.contains(&method);
                let capabilities = capabilities.clone();
                let state = state.clone();
                let plugin = Access {
                    id: registration.id.clone(),
                    state: state.clone(),
                };
                calls.spawn(async move {
                    let result = if allowed {
                        capabilities.call(&plugin, &method, params).await
                    } else {
                        Err(RpcError::new(FORBIDDEN, method))
                    };
                    if let Err(error) = result
                        && let Some(entry) = state
                            .lock()
                            .expect("plugin state")
                            .entries
                            .get_mut(plugin.id())
                    {
                        entry.error(error);
                    }
                });
            }
            _ => return Err(internal("unexpected plugin notification")),
        }
    }
}

async fn health(peer: Arc<Peer>) -> RpcError {
    let mut failures = 0;
    let mut ticks = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(5),
        Duration::from_secs(5),
    );
    loop {
        ticks.tick().await;
        match peer
            .call::<_, HealthResult>("health", &EmptyParams {}, Duration::from_secs(1))
            .await
        {
            Ok(HealthResult { ready: true, .. }) => failures = 0,
            Ok(_) => return RpcError::new(UNAVAILABLE, "plugin health ready=false"),
            Err(error) => {
                failures += 1;
                if failures >= 2 {
                    return error;
                }
            }
        }
    }
}

struct ClosePeer(Arc<Peer>);
impl Drop for ClosePeer {
    fn drop(&mut self) {
        self.0.close();
    }
}

struct Access {
    id: String,
    state: Arc<Mutex<State>>,
}
impl PluginAccess for Access {
    fn id(&self) -> &str {
        &self.id
    }
    fn groups(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("plugin state")
            .entries
            .get(&self.id)
            .filter(|entry| entry.registration.enabled)
            .map(|entry| {
                entry
                    .snapshot
                    .owned_groups
                    .iter()
                    .chain(&entry.snapshot.watched_groups)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
    fn fetch_hosts(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("plugin state")
            .entries
            .get(&self.id)
            .filter(|entry| entry.registration.enabled)
            .map(|entry| entry.registration.fetch_hosts.clone())
            .unwrap_or_default()
    }
}
