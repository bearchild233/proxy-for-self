use super::{
    registry::PluginRegistration,
    state::{ConnectionState, State},
    transport::{Peer, internal, writer},
};
use gateway_core::{lifecycle::CancellationToken, plugin::PluginCapabilities};
use kernel_protocol::*;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::UnixStream, task::JoinSet};

pub(super) async fn run(
    registration: PluginRegistration,
    state: Arc<Mutex<State>>,
    stop: CancellationToken,
    kernel: KernelInfo,
    capabilities: Arc<dyn PluginCapabilities>,
) {
    let mut backoff = Duration::from_millis(100);
    let stopping = stop.cancelled();
    tokio::pin!(stopping);
    loop {
        let result = tokio::select! {
            _ = &mut stopping => break,
            result = connect(&registration, &kernel) => result,
        };
        let result = match result {
            Ok((stream, hello, uid)) => {
                backoff = Duration::from_millis(100);
                session(
                    stream,
                    hello,
                    uid,
                    &registration,
                    state.clone(),
                    stop.clone(),
                    capabilities.clone(),
                )
                .await
            }
            Err(error) => Err(error),
        };
        {
            let mut state = state.lock().expect("plugin state");
            if let Some(entry) = state.entries.get_mut(&registration.id) {
                if let Some(peer) = entry.peer.take() {
                    peer.close();
                }
                entry.snapshot.state = ConnectionState::Unavailable;
                if let Err(error) = result {
                    entry.error(error);
                }
            }
        }
        tokio::select! { _ = &mut stopping => break, _ = tokio::time::sleep(backoff) => {} }
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
    if let Some(entry) = state
        .lock()
        .expect("plugin state")
        .entries
        .get_mut(&registration.id)
    {
        if let Some(peer) = entry.peer.take() {
            peer.close();
        }
        entry.snapshot.state = ConnectionState::Unavailable;
    }
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
        // TODO(design): 注册表尚无期望 UID / 用户映射；只能记录 SO_PEERCRED，不能自行指定允许用户。
        write_frame(
            &mut stream,
            &Message::Request {
                id: 1,
                method: "hello".into(),
                params: serde_json::to_value(HelloParams {
                    kernel_api: KernelApi::default(),
                    kernel: kernel.clone(),
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

async fn session(
    stream: UnixStream,
    hello: HelloResult,
    uid: u32,
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
    let (peer, rx) = Peer::new(2);
    let _close = ClosePeer(peer.clone());
    {
        let mut state = state.lock().expect("plugin state");
        let entry = state
            .entries
            .get_mut(&registration.id)
            .expect("registered plugin");
        entry.peer = Some(peer.clone());
        entry.snapshot.state = ConnectionState::Available;
        entry.snapshot.version = Some(hello.plugin.version);
        entry.snapshot.kernel_api = Some(hello.kernel_api);
        entry.snapshot.granted_capabilities = grants.clone();
        entry.snapshot.peer_uid = Some(uid);
        state.ownership(&registration.id, hello.owned_groups);
    }
    let (mut reader, write_half) = stream.into_split();
    let write_loop = writer(write_half, rx);
    let health_loop = health(peer.clone());
    tokio::pin!(write_loop, health_loop);
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
                state
                    .lock()
                    .expect("plugin state")
                    .ownership(&registration.id, params.owned_groups);
            }
            Message::Request { id, method, params } => {
                if id <= last_id {
                    return Err(internal("non-monotonic plugin request ID"));
                }
                last_id = id;
                let allowed = grants.contains(&method);
                let peer = peer.clone();
                let capabilities = capabilities.clone();
                let plugin = registration.id.clone();
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
                let plugin = registration.id.clone();
                calls.spawn(async move {
                    let result = if allowed {
                        capabilities.call(&plugin, &method, params).await
                    } else {
                        Err(RpcError::new(FORBIDDEN, method))
                    };
                    if let Err(error) = result
                        && let Some(entry) =
                            state.lock().expect("plugin state").entries.get_mut(&plugin)
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
