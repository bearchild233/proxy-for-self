//! 测试用双向模拟内核：能力脚本按到达顺序消费，钩子可并发调用。
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::{net::UnixStream, task::JoinHandle};

use crate::{
    RpcResult,
    protocol::*,
    transport::{Peer, internal, unavailable, writer},
};

#[derive(Clone)]
pub struct CapabilityReply {
    pub method: String,
    pub params: Value,
    pub result: RpcResult<Value>,
}

pub struct MockKernel {
    peer: Arc<Peer>,
    task: JoinHandle<()>,
    script: Arc<Mutex<VecDeque<CapabilityReply>>>,
    notifications: Arc<Mutex<Vec<(String, Value)>>>,
    pub hello: HelloResult,
}

impl MockKernel {
    pub async fn connect(
        path: impl AsRef<Path>,
        hello: HelloParams,
        timeout: Duration,
    ) -> RpcResult<Self> {
        tokio::time::timeout(timeout, Self::connect_inner(path.as_ref(), hello))
            .await
            .map_err(|_| RpcError::new(TIMEOUT, "hello"))?
    }

    async fn connect_inner(path: &Path, hello: HelloParams) -> RpcResult<Self> {
        let mut stream = UnixStream::connect(path).await.map_err(internal)?;
        write_frame(
            &mut stream,
            &Message::Request {
                id: 1,
                method: "hello".into(),
                params: serde_json::to_value(&hello).map_err(internal)?,
            },
        )
        .await
        .map_err(internal)?;
        let Message::Response {
            id: 1,
            payload: ResponsePayload::Result(value),
        } = read_frame(&mut stream).await.map_err(internal)?
        else {
            return Err(internal("invalid hello response"));
        };
        let plugin: HelloResult = serde_json::from_value(value).map_err(internal)?;
        if !is_compatible(hello.kernel_api, plugin.kernel_api) {
            return Err(internal("incompatible kernel API"));
        }
        let (peer, rx) = Peer::new(2);
        let script = Arc::new(Mutex::new(VecDeque::<CapabilityReply>::new()));
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let task_peer = peer.clone();
        let task_script = script.clone();
        let task_notifications = notifications.clone();
        let task = tokio::spawn(async move {
            let (mut reader, write_half) = stream.into_split();
            let write_loop = writer(write_half, rx);
            tokio::pin!(write_loop);
            loop {
                let message = tokio::select! {
                    _ = &mut write_loop => break,
                    message = read_frame::<_, Message>(&mut reader) => match message { Ok(message) => message, Err(_) => break },
                };
                match message {
                    Message::Response { id, payload } => task_peer.resolve(id, payload),
                    Message::Request { id, method, params } => {
                        let reply = task_script.lock().expect("script lock").pop_front();
                        let result = match reply {
                            Some(reply) if reply.method == method && reply.params == params => {
                                reply.result
                            }
                            Some(_) => {
                                Err(RpcError::new(INVALID_PARAMS, "capability script mismatch"))
                            }
                            None => Err(RpcError::new(NOT_SUPPORTED, method)),
                        };
                        // writer 在同一 task 中驱动，不能在这里等待写完再回到 select。
                        let response = async { task_peer.respond(id, result).await };
                        tokio::pin!(response);
                        tokio::select! { _ = &mut write_loop => break, _ = &mut response => {} }
                    }
                    Message::Notify { method, params } => task_notifications
                        .lock()
                        .expect("notifications lock")
                        .push((method, params)),
                }
            }
            task_peer.close();
        });
        Ok(Self {
            peer,
            task,
            script,
            notifications,
            hello: plugin,
        })
    }

    pub fn push_reply(&self, reply: CapabilityReply) {
        self.script.lock().expect("script lock").push_back(reply);
    }
    pub fn remaining_replies(&self) -> usize {
        self.script.lock().expect("script lock").len()
    }
    pub fn notifications(&self) -> Vec<(String, Value)> {
        self.notifications
            .lock()
            .expect("notifications lock")
            .clone()
    }

    pub async fn call<P: Serialize + Sync, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        timeout: Duration,
    ) -> RpcResult<R> {
        self.peer.call(method, params, timeout).await
    }

    pub async fn notify<P: Serialize + Sync>(&self, method: &str, params: &P) -> RpcResult<()> {
        self.peer
            .send(&Message::Notify {
                method: method.into(),
                params: serde_json::to_value(params).map_err(internal)?,
            })
            .await
    }

    pub async fn closed(&self) -> RpcResult<()> {
        if self.peer.is_open() {
            Err(unavailable())
        } else {
            Ok(())
        }
    }
}

impl Drop for MockKernel {
    fn drop(&mut self) {
        self.peer.close();
        self.task.abort();
    }
}
