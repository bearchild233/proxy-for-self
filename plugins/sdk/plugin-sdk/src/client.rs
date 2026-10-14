use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use crate::{
    RpcResult,
    protocol::*,
    transport::{Peer, internal, unavailable},
};
use serde::{Serialize, de::DeserializeOwned};

/// 后台能力调用不重试；挑选一条已握手连接，避免重复产生能力副作用。
#[derive(Clone, Default)]
pub struct Kernel {
    pub(crate) peers: Arc<Mutex<Vec<Weak<Peer>>>>,
    // 同一把异步锁串行化 hello 注册与归属广播，避免新连接错过设置更新。
    pub(crate) ownership: Arc<tokio::sync::Mutex<OwnershipChangedParams>>,
}

impl Kernel {
    pub async fn set_owned_groups(&self, groups: Vec<String>) -> RpcResult<()> {
        let mut current = self.ownership.lock().await;
        current.owned_groups = groups;
        self.broadcast_ownership(&current).await
    }

    pub async fn set_ownership(
        &self,
        owned_groups: Vec<String>,
        watched_groups: Vec<String>,
    ) -> RpcResult<()> {
        let mut current = self.ownership.lock().await;
        *current = OwnershipChangedParams {
            owned_groups,
            watched_groups,
        };
        self.broadcast_ownership(&current).await
    }

    async fn broadcast_ownership(&self, current: &OwnershipChangedParams) -> RpcResult<()> {
        let peers: Vec<_> = self
            .peers
            .lock()
            .expect("peers lock")
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|peer| peer.is_open())
            .collect();
        let message = Message::Notify {
            method: "ownership.changed".into(),
            params: serde_json::to_value(current).map_err(internal)?,
        };
        let mut result = Ok(());
        for peer in peers {
            if let Err(error) = peer.send(&message).await {
                result = Err(error);
            }
        }
        result
    }

    pub(crate) fn register(&self, peer: &Arc<Peer>) {
        let mut peers = self.peers.lock().expect("peers lock");
        peers.retain(|weak| weak.upgrade().is_some_and(|peer| peer.is_open()));
        peers.push(Arc::downgrade(peer));
    }

    fn peer(&self) -> RpcResult<Arc<Peer>> {
        self.peers
            .lock()
            .expect("peers lock")
            .iter()
            .filter_map(Weak::upgrade)
            .find(|peer| peer.is_open())
            .ok_or_else(unavailable)
    }

    pub async fn call<P: Serialize + Sync, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        timeout: Duration,
    ) -> RpcResult<R> {
        self.peer()?.call(method, params, timeout).await
    }

    pub async fn log(&self, params: &LogParams, timeout: Duration) -> RpcResult<()> {
        send_log(self.peer()?.as_ref(), params, timeout).await
    }
}

#[derive(Clone)]
pub struct Context {
    pub(crate) peer: Arc<Peer>,
    pub(crate) data: Arc<PathBuf>,
    pub(crate) kernel: Kernel,
    pub(crate) role: ConnectionRole,
}

impl Context {
    pub fn data_dir(&self) -> &Path {
        &self.data
    }
    pub fn kernel(&self) -> Kernel {
        self.kernel.clone()
    }

    /// 能力调用只走内核连接：内核连接上的钩子沿用原连接，管理连接上的调用改由内核连接发出。
    fn capability_peer(&self) -> RpcResult<Arc<Peer>> {
        match self.role {
            ConnectionRole::Kernel => Ok(self.peer.clone()),
            ConnectionRole::Admin => self.kernel.peer(),
        }
    }

    pub async fn call<P: Serialize + Sync, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        timeout: Duration,
    ) -> RpcResult<R> {
        self.capability_peer()?.call(method, params, timeout).await
    }

    pub async fn log(&self, params: &LogParams, timeout: Duration) -> RpcResult<()> {
        send_log(self.capability_peer()?.as_ref(), params, timeout).await
    }
}

async fn send_log(peer: &Peer, params: &LogParams, timeout: Duration) -> RpcResult<()> {
    let message = Message::Notify {
        method: "log".into(),
        params: serde_json::to_value(params).map_err(internal)?,
    };
    tokio::time::timeout(timeout, peer.send(&message))
        .await
        .map_err(|_| RpcError::new(TIMEOUT, "log"))?
}
