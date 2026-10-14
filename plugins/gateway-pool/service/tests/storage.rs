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
    let mode = std::fs::metadata(harness.dir.path().join(gateway_pool::DB_FILE))
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o640);
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
