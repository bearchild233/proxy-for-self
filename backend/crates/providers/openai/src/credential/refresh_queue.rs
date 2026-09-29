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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn overlapping_refreshes_share_result_but_new_manual_refresh_is_fresh() {
        let queue = RefreshQueue::<usize, ()>::default();
        let calls = AtomicUsize::new(0);
        let query = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(7)
        };
        let (a, b) = tokio::join!(
            queue.run("a:1".into(), false, Duration::ZERO, (), query()),
            queue.run("a:1".into(), false, Duration::ZERO, (), query())
        );
        assert_eq!((a, b), (Ok(7), Ok(7)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        queue
            .run("a:1".into(), false, Duration::ZERO, (), query())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn revision_change_bypasses_success_cache() {
        let queue = RefreshQueue::<usize, ()>::default();
        assert_eq!(
            queue
                .run("a:1".into(), false, Duration::from_secs(30), (), async {
                    Ok(1)
                })
                .await,
            Ok(1)
        );
        assert_eq!(
            queue
                .run("a:1".into(), false, Duration::from_secs(30), (), async {
                    Ok(2)
                })
                .await,
            Ok(1)
        );
        assert_eq!(
            queue
                .run("a:2".into(), false, Duration::from_secs(30), (), async {
                    Ok(2)
                })
                .await,
            Ok(2)
        );
    }

    #[tokio::test]
    async fn cancelled_leader_does_not_block_followers() {
        let queue = RefreshQueue::<usize, ()>::default();
        let result = tokio::time::timeout(
            Duration::from_millis(10),
            queue.run(
                "a".into(),
                false,
                Duration::ZERO,
                (),
                std::future::pending(),
            ),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            queue
                .run("a".into(), false, Duration::ZERO, (), async { Ok(3) })
                .await,
            Ok(3)
        );
    }

    #[tokio::test]
    async fn manual_join_promotes_queued_background_request() {
        let queue = RefreshQueue::<usize, ()>::default();
        let _occupied = queue.background.acquire().await.unwrap();
        let background = queue.run("a".into(), true, Duration::ZERO, (), async { Ok(4) });
        let manual = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            queue
                .run("a".into(), false, Duration::ZERO, (), async { Ok(5) })
                .await
        };
        let result = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(background, manual)
        })
        .await
        .unwrap();
        assert_eq!(result, (Ok(4), Ok(4)));
    }

    #[tokio::test]
    async fn failure_is_shared_but_not_reused_by_later_refresh() {
        let queue = RefreshQueue::<usize, ()>::default();
        let ttl = Duration::from_secs(30);
        let a = queue.run("a".into(), false, ttl, (), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Err(())
        });
        let b = queue.run("a".into(), false, ttl, (), async { Ok(1) });
        assert_eq!(tokio::join!(a, b), (Err(()), Err(())));
        assert_eq!(
            queue.run("a".into(), false, ttl, (), async { Ok(2) }).await,
            Ok(2)
        );
    }
}
