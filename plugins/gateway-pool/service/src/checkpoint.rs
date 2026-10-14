//! 把 WAL checkpoint 的磁盘同步移出钩子事务；阈值沿用当前 SQLite 自动 checkpoint 配置。

use std::{
    path::Path,
    sync::mpsc::{SyncSender, sync_channel},
    thread::{self, JoinHandle},
};

use rusqlite::Connection;

use crate::error::Failure;

pub(crate) struct Checkpoint {
    threshold: i64,
    sender: Option<SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl Checkpoint {
    pub(crate) fn start(path: &Path, writer: &Connection) -> Result<Self, Failure> {
        let threshold = writer.pragma_query_value(None, "wal_autocheckpoint", |row| row.get(0))?;
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        // 只保留一个待处理通知，合并并发提交；不按时间轮询，也不新增页数阈值。
        let (sender, receiver) = sync_channel(1);
        let worker = thread::Builder::new()
            .name("gateway-pool-checkpoint".into())
            .spawn(move || {
                while receiver.recv().is_ok() {
                    // PASSIVE 不等待正在写入的事务或旧读快照；未完成部分在后续提交时再次通知。
                    if let Err(error) = connection.execute_batch("PRAGMA wal_checkpoint(PASSIVE)") {
                        eprintln!("gateway pool checkpoint unavailable: {error}");
                    }
                }
            })?;
        let checkpoint = Self {
            threshold,
            sender: Some(sender),
            worker: Some(worker),
        };
        writer.pragma_update(None, "wal_autocheckpoint", 0)?;
        Ok(checkpoint)
    }

    /// 事务已经提交；checkpoint 的错误不能把成功提交改报为失败。
    pub(crate) fn committed(&self, writer: &Connection) {
        // bundled SQLite 3.51.3 支持 NOOP：只查询 WAL 页数，不复制页面、不执行 fsync。
        // 其他连接正在 checkpoint 时返回 busy/-1，由下一次提交再检查。
        let pages = writer.query_row("PRAGMA wal_checkpoint(NOOP)", [], |row| {
            row.get::<_, i64>(1)
        });
        match pages {
            Ok(pages) if self.threshold > 0 && pages >= self.threshold => {
                if let Some(sender) = &self.sender {
                    let _ = sender.try_send(());
                }
            }
            Ok(_) => {}
            Err(error) => eprintln!("gateway pool checkpoint unavailable: {error}"),
        }
    }
}

impl Drop for Checkpoint {
    fn drop(&mut self) {
        // 关闭通知通道并等待当前 PASSIVE 完成；最后一个连接关闭仍由 SQLite 完成最终 checkpoint。
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
