//! 运行状态机与 Python 实现的一致性对照。默认忽略：需要 `CPR_GP_PARITY_DIR` 指向由 `tests/parity/` 里的 Python 脚本生成的产物目录（见 `tests/parity/README.md`）。
//! 每个序列在全新数据库上按 Python 记录的消息与时钟逐步重放，比较每一步的返回值（整数与浮点严格区分），
//! 序列结束后比较整库内容；同时把 Rust 写出的数据库保存下来，供 Python 读取后再比较一次。

mod common;

use std::path::{Path, PathBuf};

use common::TestDir;
use gateway_pool::{Failure, ManualClock, Store, dispatch, sequential_ids};
use rusqlite::{Connection, types::ValueRef};
use serde_json::{Map, Value, json};

const TABLES: [&str; 18] = [
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
    "gp_batches",
    "gp_windows",
    "gp_accesses",
    "gp_segments",
    "gp_cooling",
    "gp_account_queue",
    "gp_queue_revision",
];
const JSON_COLUMNS: [&str; 4] = ["data", "candidates", "processed", "remaining"];

fn parity_dir() -> PathBuf {
    match std::env::var_os("CPR_GP_PARITY_DIR") {
        Some(path) => PathBuf::from(path),
        None => panic!("CPR_GP_PARITY_DIR must point at the parity artifact directory"),
    }
}

fn object(value: &Value) -> Map<String, Value> {
    match value {
        Value::Object(object) => object.clone(),
        other => panic!("expected an object, got {other}"),
    }
}

/// 把一次调用折叠为与 Python 期望相同的形状。
fn outcome(result: Result<Value, Failure>, expect: &Value) -> Value {
    match result {
        Ok(value) => json!({"ok": value}),
        // Python 的 KeyError/TypeError 没有稳定消息，只比较“参数无效”这一类。
        Err(Failure::Invalid(_)) if expect.get("invalid").is_some() => json!({"invalid": true}),
        Err(Failure::Invalid(message)) => json!({"error": message}),
        Err(Failure::Forbidden(message)) => json!({"forbidden": message}),
        Err(other) => panic!("unexpected failure: {other}"),
    }
}

fn open(directory: &Path, clock: &ManualClock, ids: &gateway_pool::IdSource) -> Store {
    Store::open_with_ids(directory, clock.clock(), ids.clone()).expect("open store")
}

fn cell(row: &rusqlite::Row<'_>, index: usize, name: &str) -> Value {
    match row.get_ref(index).expect("cell") {
        ValueRef::Null | ValueRef::Blob(_) => Value::Null,
        ValueRef::Integer(value) => json!(value),
        ValueRef::Real(value) => json!(value),
        ValueRef::Text(bytes) => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            if JSON_COLUMNS.contains(&name) {
                serde_json::from_str(&text).unwrap_or_else(|error| panic!("{name}: {error}"))
            } else {
                json!(text)
            }
        }
    }
}

/// 与 Python `dump_database` 相同的整库内容：每张表按 rowid 排列，JSON 列解析后比较。
fn dump_database(path: &Path) -> Value {
    let connection = Connection::open(path).expect("open database");
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .ok();
    let mut result = Map::new();
    for table in TABLES {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .expect("prepare dump");
        let names: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let rows: Vec<Value> = statement
            .query_map([], |row| {
                let mut entry = Map::new();
                for (index, name) in names.iter().enumerate() {
                    entry.insert(name.clone(), cell(row, index, name));
                }
                Ok(Value::Object(entry))
            })
            .expect("query dump")
            .map(|row| row.expect("dump row"))
            .collect();
        result.insert(table.to_owned(), Value::Array(rows));
    }
    Value::Object(result)
}

/// 找出整库内容的第一处差异，便于定位。
fn first_database_difference(rust: &Value, python: &Value) -> Option<String> {
    for table in TABLES {
        let (left, right) = (&rust[table], &python[table]);
        if left == right {
            continue;
        }
        let (left_rows, right_rows) = (
            left.as_array().cloned().unwrap_or_default(),
            right.as_array().cloned().unwrap_or_default(),
        );
        if left_rows.len() != right_rows.len() {
            return Some(format!(
                "{table}: row count rust={} python={}",
                left_rows.len(),
                right_rows.len()
            ));
        }
        for (index, (a, b)) in left_rows.iter().zip(&right_rows).enumerate() {
            if a != b {
                return Some(format!("{table}[{index}]: rust={a} python={b}"));
            }
        }
    }
    None
}

#[test]
#[ignore = "需要 CPR_GP_PARITY_DIR：由 tests/parity 里的 Python 脚本产生"]
fn runtime_sequences_match_python() {
    let directory = parity_dir();
    let text = std::fs::read_to_string(directory.join("runtime-cases.json")).expect("read cases");
    let document: Value = serde_json::from_str(&text).expect("parse cases");
    let written = directory.join("rust-written");
    std::fs::create_dir_all(&written).expect("create output directory");

    let mut report = Vec::new();
    let mut failures = Vec::new();
    for sequence in document["sequences"].as_array().expect("sequences") {
        let name = sequence["name"].as_str().expect("name").to_owned();
        let scratch = TestDir::new("runtime-parity");
        let clock = ManualClock::new(10_000.0);
        let ids = sequential_ids();
        let mut store = open(scratch.path(), &clock, &ids);
        let mut step_results = Vec::new();
        let mut mismatches = 0_usize;
        for (index, step) in sequence["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .enumerate()
        {
            clock.set(step["clock"].as_f64().expect("clock"));
            let op = step["op"].as_str().expect("op");
            let label = match op {
                "runtime" => step["message"]["action"].as_str().unwrap_or("?").to_owned(),
                "admin" => step["method"].as_str().unwrap_or("?").to_owned(),
                other => other.to_owned(),
            };
            let actual = match op {
                "runtime" => outcome(store.runtime(&step["message"]), &step["expect"]),
                "admin" => outcome(
                    dispatch(
                        &store,
                        step["method"].as_str().expect("method"),
                        &object(&step["data"]),
                    )
                    .map(|(value, _)| value),
                    &step["expect"],
                ),
                "restart" => {
                    drop(store);
                    store = open(scratch.path(), &clock, &ids);
                    json!({"ok": null})
                }
                "enable" => {
                    store.set_enabled(step["flag"].as_bool().expect("flag"));
                    json!({"ok": null})
                }
                other => panic!("unknown step kind {other}"),
            };
            let same = actual == step["expect"];
            if !same {
                mismatches += 1;
                if failures.len() < 30 {
                    failures.push(format!(
                        "{name}#{index} {label}: rust={actual} python={}",
                        step["expect"]
                    ));
                }
            }
            step_results.push(json!({"step": index, "op": op, "action": label, "match": same}));
        }
        let database = scratch.path().join(gateway_pool::DB_FILE);
        let rust_dump = dump_database(&database);
        let database_same = rust_dump == sequence["database"];
        if !database_same && failures.len() < 30 {
            failures.push(format!(
                "{name}: database differs: {}",
                first_database_difference(&rust_dump, &sequence["database"]).unwrap_or_default()
            ));
        }
        // 写出 Rust 的最终库，供 Python 读取。
        let bytes = std::fs::read(&database).expect("read database");
        std::fs::write(written.join(format!("{name}.sqlite3")), bytes)
            .expect("write database copy");
        report.push(json!({
            "sequence": name,
            "steps": step_results.len(),
            "stepMismatches": mismatches,
            "databaseMatch": database_same,
            "results": step_results,
        }));
    }
    let total: usize = report
        .iter()
        .map(|item| item["steps"].as_u64().unwrap_or(0) as usize)
        .sum();
    let summary = json!({"sequences": report.len(), "steps": total, "failures": failures.len(), "report": report});
    std::fs::write(
        directory.join("runtime-parity-report.json"),
        serde_json::to_string_pretty(&summary).expect("serialize report"),
    )
    .expect("write report");
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
