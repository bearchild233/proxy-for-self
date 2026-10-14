use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::{
    io::AsyncWriteExt,
    net::unix::OwnedWriteHalf,
    sync::{mpsc, oneshot, watch},
};

use kernel_protocol::*;
type RpcResult<T> = Result<T, RpcError>;

pub(crate) struct Outbound {
    frame: Vec<u8>,
    written: Option<oneshot::Sender<RpcResult<()>>>,
}

pub(crate) struct Peer {
    tx: mpsc::UnboundedSender<Outbound>,
    pending: Mutex<HashMap<u64, oneshot::Sender<RpcResult<Value>>>>,
    next: AtomicU64,
    send_order: Mutex<()>,
    closed: AtomicBool,
    // 已被运行时选中、尚未返回的钩子或管理调用数。退役连接等它归零后才能关闭。
    admitted: watch::Sender<usize>,
    // 连接被新 socket 的连接替换后置位：不再接收新调用，只完成已准入的调用。
    retired: watch::Sender<bool>,
}

/// 一次钩子或管理调用对连接的占用；释放时计数减一。
pub(crate) struct Admission(Arc<Peer>);
impl Drop for Admission {
    fn drop(&mut self) {
        self.0.admitted.send_modify(|count| *count -= 1);
    }
}

pub(crate) fn unavailable() -> RpcError {
    RpcError::new(UNAVAILABLE, "connection unavailable")
}
pub(crate) fn internal(error: impl std::fmt::Display) -> RpcError {
    RpcError::new(INTERNAL, error.to_string())
}

impl Peer {
    pub(crate) fn new(next: u64) -> (Arc<Self>, mpsc::UnboundedReceiver<Outbound>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                tx,
                pending: Mutex::new(HashMap::new()),
                next: AtomicU64::new(next),
                send_order: Mutex::new(()),
                closed: AtomicBool::new(false),
                admitted: watch::channel(0).0,
                retired: watch::channel(false).0,
            }),
            rx,
        )
    }

    pub(crate) fn is_open(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
    }

    /// 在选中连接的同一把状态锁内调用，保证退役前准入的调用一定被计入排空等待。
    pub(crate) fn admit(self: &Arc<Self>) -> Admission {
        self.admitted.send_modify(|count| *count += 1);
        Admission(self.clone())
    }

    pub(crate) fn retire(&self) {
        self.retired.send_replace(true);
    }

    pub(crate) fn is_retired(&self) -> bool {
        *self.retired.borrow()
    }

    pub(crate) async fn retired(&self) {
        let mut receiver = self.retired.subscribe();
        let _ = receiver.wait_for(|retired| *retired).await;
    }

    pub(crate) async fn drained(&self) {
        let mut receiver = self.admitted.subscribe();
        let _ = receiver.wait_for(|count| *count == 0).await;
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.pending.lock().expect("pending lock").clear();
    }

    pub(crate) fn resolve(&self, id: u64, payload: ResponsePayload) {
        if let Some(sender) = self.pending.lock().expect("pending lock").remove(&id) {
            let result = match payload {
                ResponsePayload::Result(value) => Ok(value),
                ResponsePayload::Error(error) => Err(error),
            };
            let _ = sender.send(result);
        }
    }

    fn enqueue(
        &self,
        message: &Message,
        written: Option<oneshot::Sender<RpcResult<()>>>,
    ) -> RpcResult<()> {
        let frame = encode_frame(message).map_err(internal)?;
        if !self.is_open() {
            return Err(unavailable());
        }
        self.tx
            .send(Outbound { frame, written })
            .map_err(|_| unavailable())
    }

    pub(crate) async fn send(&self, message: &Message) -> RpcResult<()> {
        let (tx, rx) = oneshot::channel();
        self.enqueue(message, Some(tx))?;
        rx.await.map_err(|_| unavailable())?
    }

    pub(crate) fn notify(&self, message: &Message) -> RpcResult<()> {
        self.enqueue(message, None)
    }

    pub(crate) async fn call<P: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        budget: Duration,
    ) -> RpcResult<R> {
        // 独立 writer 持有完整帧；调用超时或取消不会在半帧处取消写入。
        let (cleanup, rx) = {
            let _order = self.send_order.lock().expect("send order lock");
            let id = self
                .next
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .map_err(|_| internal("request ids exhausted"))?;
            let (tx, rx) = oneshot::channel();
            let cleanup = Pending { peer: self, id };
            self.pending.lock().expect("pending lock").insert(id, tx);
            self.enqueue(
                &Message::Request {
                    id,
                    method: method.into(),
                    params: serde_json::to_value(params).map_err(internal)?,
                },
                None,
            )?;
            (cleanup, rx)
        };
        let result = tokio::time::timeout(budget, rx)
            .await
            .map_err(|_| RpcError::new(TIMEOUT, method))?
            .map_err(|_| unavailable())??;
        drop(cleanup);
        serde_json::from_value(result).map_err(internal)
    }

    pub(crate) async fn respond(&self, id: u64, result: RpcResult<Value>) -> RpcResult<()> {
        let payload = match result {
            Ok(value) => ResponsePayload::Result(value),
            Err(error) => ResponsePayload::Error(error),
        };
        self.send(&Message::Response { id, payload }).await
    }
}

struct Pending<'a> {
    peer: &'a Peer,
    id: u64,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.peer
            .pending
            .lock()
            .expect("pending lock")
            .remove(&self.id);
    }
}

pub(crate) async fn writer(mut stream: OwnedWriteHalf, mut rx: mpsc::UnboundedReceiver<Outbound>) {
    while let Some(outbound) = rx.recv().await {
        let result = stream
            .write_all(&outbound.frame)
            .await
            .map_err(|_| unavailable());
        let failed = result.is_err();
        if let Some(tx) = outbound.written {
            let _ = tx.send(result);
        }
        if failed {
            tracing::warn!("plugin socket write failed; queued notifications were not delivered");
            break;
        }
    }
}
