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
}

impl Kernel {
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
}

impl Context {
    pub fn data_dir(&self) -> &Path {
        &self.data
    }
    pub fn kernel(&self) -> Kernel {
        self.kernel.clone()
    }

    pub async fn call<P: Serialize + Sync, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        timeout: Duration,
    ) -> RpcResult<R> {
        self.peer.call(method, params, timeout).await
    }

    pub async fn log(&self, params: &LogParams, timeout: Duration) -> RpcResult<()> {
        send_log(&self.peer, params, timeout).await
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
