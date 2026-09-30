//! 只合并只读上游查询；凭据变更用 revision 隔离，取消等待不会遗留占位。
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore};

type Cached<T, E> = Option<(Instant, Result<T, E>)>;
struct Entry<T, E> {
    result: AsyncMutex<Cached<T, E>>,
    manual: AtomicUsize,
    changed: Notify,
}
struct ManualWait<'a>(&'a AtomicUsize);
impl Drop for ManualWait<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) struct RefreshQueue<T, E> {
    entries: Mutex<HashMap<String, Arc<Entry<T, E>>>>,
    waiting: Semaphore,
    active: Semaphore,
    background: Semaphore,
}

impl<T: Clone, E: Clone> Default for RefreshQueue<T, E> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            waiting: Semaphore::new(64),
            active: Semaphore::new(2),
            background: Semaphore::new(1),
        }
    }
}

impl<T: Clone, E: Clone> RefreshQueue<T, E> {
    pub(super) async fn run<F: Future<Output = Result<T, E>>>(
        &self,
        key: String,
        background: bool,
        ttl: Duration,
        unavailable: E,
        operation: F,
    ) -> Result<T, E> {
        let started = Instant::now();
        let _waiting = self
            .waiting
            .try_acquire()
            .map_err(|_| unavailable.clone())?;
        let entry = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !entries.contains_key(&key) && entries.len() >= 64 {
                entries.retain(|_, entry| Arc::strong_count(entry) > 1);
            }
            Arc::clone(entries.entry(key).or_insert_with(|| {
                Arc::new(Entry {
                    result: AsyncMutex::new(None),
                    manual: AtomicUsize::new(0),
                    changed: Notify::new(),
                })
            }))
        };
        let _manual = if background {
            None
        } else {
            entry.manual.fetch_add(1, Ordering::SeqCst);
            entry.changed.notify_one();
            Some(ManualWait(&entry.manual))
        };
        let work = async {
            let mut cached = entry.result.lock().await;
            if let Some((finished, result)) = &*cached {
                // 并发调用共享成功或失败；仅成功结果允许使用业务 TTL。
                if *finished >= started || (result.is_ok() && finished.elapsed() < ttl) {
                    return result.clone();
                }
            }
            // 后台最多占一个执行槽，手动查询始终有独立余量。
            let _background = loop {
                if !background || entry.manual.load(Ordering::SeqCst) > 0 {
                    break None;
                }
                tokio::select! {
                    permit = self.background.acquire() => break Some(permit.map_err(|_| unavailable.clone())?),
                    () = entry.changed.notified() => {},
                }
            };
            let _active = self
                .active
                .acquire()
                .await
                .map_err(|_| unavailable.clone())?;
            let result = operation.await;
            *cached = Some((Instant::now(), result.clone()));
            result
        };
        tokio::time::timeout(Duration::from_secs(90), work)
            .await
            .unwrap_or(Err(unavailable))
    }
}
