//! 存储层：表结构、索引、WAL、文件权限与重开数据保留。
//! 与 Python 表结构逐项比对由 `tests/parity/` 里的一致性对照完成；这里验证 Rust 侧自身的初始化结果。

mod common;

use std::collections::BTreeSet;

use common::Harness;
use serde_json::json;

const TABLES: [&str; 19] = [
    "settings",
    "pairs",
    "leases",
    "observations",
    "switches",
    "materials",
    "catalogs",
    "model_tickets",
    "group_events",
    "recovery_checks",
    "gp_history_meta",
    "gp_hook_leases",
    "gp_batches",
    "gp_windows",
    "gp_accesses",
    "gp_segments",
    "gp_cooling",
    "gp_account_queue",
    "gp_queue_revision",
];

const INDEXES: [&str; 14] = [
    "observations_pair",
    "group_events_account",
    "gp_windows_account_time",
    "gp_windows_time",
    "gp_windows_batch",
    "gp_accesses_lease",
    "gp_accesses_generation",
    "gp_accesses_segment",
    "gp_segments_generation",
    "gp_segments_open",
    "gp_cooling_pair",
    "gp_cooling_time",
    "gp_cooling_pair_recent",
    "observations_generation",
];

#[test]
fn open_creates_every_table_and_index_of_the_python_schema() {
    let harness = Harness::new("storage-schema", 10_000.0);
    let connection = harness.connection();
    let names: BTreeSet<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','index') AND name NOT LIKE 'sqlite_%'")
        .expect("prepare")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .map(|name| name.expect("name"))
        .collect();
    let expected: BTreeSet<String> = TABLES
        .iter()
        .chain(INDEXES.iter())
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(names, expected);
}

#[test]
fn database_uses_wal_and_seeds_the_meta_and_queue_rows() {
    let harness = Harness::new("storage-wal", 10_000.0);
    let connection = harness.connection();
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal mode");
    assert_eq!(mode, "wal");
    let started: f64 = connection
        .query_row(
            "SELECT started FROM gp_history_meta WHERE id=1",
            [],
            |row| row.get(0),
        )
        .expect("history meta");
    assert_eq!(started, 10_000.0);
    let revision: i64 = connection
        .query_row(
            "SELECT revision FROM gp_queue_revision WHERE id=1",
            [],
            |row| row.get(0),
        )
        .expect("queue revision");
    assert_eq!(revision, 0);
}

#[cfg(unix)]
#[test]
fn database_file_is_readable_by_owner_and_group_only() {
    use std::os::unix::fs::PermissionsExt;
    let harness = Harness::new("storage-mode", 10_000.0);
    // 常驻连接保留 WAL/SHM，侧文件也不能放宽数据库材料的读取权限。
    for suffix in ["", "-wal", "-shm"] {
        let file = format!("{}{suffix}", gateway_pool::DB_FILE);
        let mode = std::fs::metadata(harness.dir.path().join(&file))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o640, "{file}");
    }
}

#[test]
fn reopening_keeps_settings_queue_and_original_history_start() {
    let mut harness = Harness::new("storage-reopen", 10_000.0);
    harness.call_ok("configure", json!({"initialRestSeconds": 1800}));
    harness.call_ok(
        "queue.update",
        json!({"revision": 0, "accountIds": ["A", "B"]}),
    );
    harness.clock.set(20_000.0);
    harness.reopen();
    let view = harness.call_ok("read", json!({}));
    assert_eq!(
        view["settings"]["initialRestSeconds"].as_f64(),
        Some(1800.0)
    );
    assert_eq!(
        view["accountQueue"]["accounts"].as_array().map(Vec::len),
        Some(2)
    );
    let view = harness.call_ok("history.read", json!({}));
    assert_eq!(view["recordedSince"].as_f64(), Some(10_000.0));
}

#[test]
fn failed_action_rolls_back_and_the_writer_can_be_reused() {
    let harness = Harness::new("storage-rollback", 10_000.0);
    // active 先登记候选，再读取组合；损坏的组合使事务在写入之后失败。
    harness
        .connection()
        .execute(
            "INSERT INTO pairs VALUES ('A','unified-142','invalid json')",
            [],
        )
        .unwrap();
    let message = json!({"action": "active", "candidates": [{"id": "A", "exhausted": false}]});
    assert!(harness.store.runtime(&message).is_err());
    assert_eq!(harness.count("gp_account_queue"), 0);
    // 管理视图会解析损坏的组合，所以这里直接读修订号。
    let revision: i64 = harness
        .connection()
        .query_row(
            "SELECT revision FROM gp_queue_revision WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision, 0);
    harness
        .connection()
        .execute("DELETE FROM pairs", [])
        .unwrap();
    assert_eq!(
        harness.store.runtime(&message).unwrap()["accounts"],
        json!(["A"])
    );
    assert_eq!(harness.count("gp_account_queue"), 1);
}

#[test]
fn independent_writers_preserve_revision_conflicts_and_see_new_commits() {
    use std::sync::{Arc, Barrier};

    let harness = Harness::new("storage-shared-writers", 10_000.0);
    harness.call_ok(
        "queue.update",
        json!({"revision": 0, "accountIds": ["A", "B"]}),
    );
    let revision = harness.queue_revision();
    let second = gateway_pool::Store::open(harness.dir.path(), harness.clock.clock()).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    // 两个 Store 模拟热更新的新旧进程；SQLite 仍裁决跨连接事务，不能缓存队列事实。
    std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            barrier.wait();
            harness.store.update_queue(&common::object_of(json!({
                "revision": revision, "accountId": "A", "enabled": false
            })))
        });
        let two = scope.spawn(|| {
            barrier.wait();
            second.update_queue(&common::object_of(json!({
                "revision": revision, "accountId": "B", "enabled": false
            })))
        });
        let results = [one.join().unwrap(), two.join().unwrap()];
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        let error = results.into_iter().find_map(Result::err).unwrap();
        assert_eq!(error.to_string(), "账号队列已变化，请刷新后重试");
    });
    assert_eq!(harness.queue_revision(), revision + 1);
    assert_eq!(
        harness.store.read_admin(&Default::default()).unwrap(),
        second.read_admin(&Default::default()).unwrap()
    );
    second
        .update_queue(&common::object_of(json!({
            "revision": revision + 1, "accountIds": ["B", "A"]
        })))
        .unwrap();
    assert_eq!(harness.queue_order(), vec!["B", "A"]);
}

#[test]
fn background_checkpoint_drains_without_invalidating_a_read_snapshot() {
    let dir = common::TestDir::new("storage-checkpoint");
    let clock = gateway_pool::ManualClock::new(10_000.0);
    let store = gateway_pool::Store::open(dir.path(), clock.clock()).unwrap();
    let mut reader = rusqlite::Connection::open(dir.path().join(gateway_pool::DB_FILE)).unwrap();
    let threshold: i64 = reader
        .pragma_query_value(None, "wal_autocheckpoint", |row| row.get(0))
        .unwrap();
    let snapshot = reader.transaction().unwrap();
    let revision = |db: &rusqlite::Connection| -> i64 {
        db.query_row(
            "SELECT revision FROM gp_queue_revision WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(revision(&snapshot), 0);
    // 用既有 SQLite 阈值产生足够 WAL 页面；读快照不能被后台 checkpoint 或写入打断。
    for n in 0..threshold {
        store
            .runtime(&json!({"action": "quota", "accountId": "A", "exhausted": n % 2 == 0}))
            .unwrap();
    }
    assert_eq!(revision(&snapshot), 0);
    snapshot.commit().unwrap();
    assert!(revision(&reader) > 0);
    store
        .configure(&common::object_of(json!({"probeIntervalMs": 50})))
        .unwrap();
    // reader 连接仍在，不能靠“关闭最后一个连接”的 checkpoint 掩盖后台未完成的问题。
    drop(store);
    let (pages, checkpointed): (i64, i64) = reader
        .query_row("PRAGMA wal_checkpoint(NOOP)", [], |row| {
            Ok((row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(pages, checkpointed);
}

#[test]
fn a_failed_report_rolls_back_alone_and_the_finish_still_runs() {
    let harness = Harness::new("storage-report-finish", 10_000.0);
    let report = json!({"action": "probeResult", "accountId": "A"});
    let finish = json!({"action": "finish", "accountId": "A", "leaseId": "missing"});
    let (first, second) = harness.store.runtime_then_finish(&report, &finish).unwrap();
    assert!(first.is_err());
    assert_eq!(second.unwrap(), json!({"accepted": false}));
    // 保存点已释放，常驻写连接可以继续开启下一个事务。
    assert!(harness.store.runtime(&json!({"action": "config"})).is_ok());
}
