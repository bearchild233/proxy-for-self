//! 与 Python 实现的一致性对照。默认忽略：需要 `CPR_GP_PARITY_DIR` 指向 `.build` 中由 Python 生成的产物目录。
//! - `python_expectations_match_rust_plugin`：重放 Python 生成的写路径与快照读取，逐项比较 JSON（整数与浮点严格区分）。
//! - `rust_writes_database_for_python`：Rust 写入数据库并记录期望输出，供 Python 读取后比较。

mod common;

use std::path::{Path, PathBuf};

use common::{Harness, TestDir};
use gateway_pool::{Failure, ManualClock, Store, dispatch};
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue, types::ValueRef};
use serde_json::{Value, json};

fn parity_dir() -> PathBuf {
    match std::env::var_os("CPR_GP_PARITY_DIR") {
        Some(path) => PathBuf::from(path),
        None => panic!("CPR_GP_PARITY_DIR must point at the parity artifact directory"),
    }
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn cell(row: &rusqlite::Row<'_>, index: usize) -> Value {
    match row.get_ref(index).expect("cell") {
        ValueRef::Null | ValueRef::Blob(_) => Value::Null,
        ValueRef::Integer(value) => json!(value),
        ValueRef::Real(value) => json!(value),
        ValueRef::Text(bytes) => json!(String::from_utf8_lossy(bytes)),
    }
}

fn sql_value(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(flag) => SqlValue::Integer(i64::from(*flag)),
        Value::Number(number) => match number.as_i64() {
            Some(integer) => SqlValue::Integer(integer),
            None => SqlValue::Real(number.as_f64().expect("finite number")),
        },
        Value::String(text) => SqlValue::Text(text.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}

/// 与 Python `schema_of` 相同的结构摘要。
fn schema_of(path: &Path) -> Value {
    let connection = Connection::open(path).expect("open database");
    let mut statement = connection
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name")
        .expect("prepare schema");
    let rows: Vec<Vec<Value>> = statement
        .query_map([], |row| Ok((0..4).map(|index| cell(row, index)).collect()))
        .expect("query schema")
        .map(|row| row.expect("schema row"))
        .collect();
    let mut entries = Vec::new();
    for row in rows {
        let type_ = row[0].as_str().unwrap_or_default().to_owned();
        let name = row[1].as_str().unwrap_or_default().to_owned();
        let sql = row[3]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut entry = json!({"type": type_, "name": name, "tableName": row[2], "sql": sql});
        if type_ == "table" {
            let columns = table_rows(&connection, &format!("PRAGMA table_info(\"{name}\")"), 6);
            entry["columns"] = Value::Array(columns);
        }
        if type_ == "index" {
            let columns = table_rows(&connection, &format!("PRAGMA index_info(\"{name}\")"), 3);
            entry["indexColumns"] = Value::Array(columns);
        }
        entries.push(entry);
    }
    Value::Array(entries)
}

fn table_rows(connection: &Connection, sql: &str, width: usize) -> Vec<Value> {
    let mut statement = connection.prepare(sql).expect("prepare pragma");
    statement
        .query_map([], |row| {
            Ok(Value::Array(
                (0..width).map(|index| cell(row, index)).collect(),
            ))
        })
        .expect("query pragma")
        .map(|row| row.expect("pragma row"))
        .collect()
}

/// 把一次调用的结果折叠为与 Python 期望相同的形状：`{"ok": v}` 或 `{"error": 消息}`。
fn outcome(result: Result<Value, Failure>) -> Value {
    match result {
        Ok(value) => json!({"ok": value}),
        Err(Failure::Invalid(message)) => json!({"error": message}),
        Err(other) => panic!("unexpected failure: {other}"),
    }
}

fn call(store: &Store, method: &str, data: &Value) -> Value {
    let object = match data {
        Value::Object(object) => object.clone(),
        other => panic!("request must be an object: {other}"),
    };
    outcome(dispatch(store, method, &object).map(|(value, _)| value))
}

fn run_sql(path: &Path, sql: &str, args: &[Value]) {
    let connection = Connection::open(path).expect("open database");
    let args: Vec<SqlValue> = args.iter().map(sql_value).collect();
    connection
        .execute(sql, params_from_iter(args))
        .expect("execute parity SQL");
}

/// 写路径与快照读取的逐项比较；返回不一致的步骤摘要。
#[test]
#[ignore = "需要 CPR_GP_PARITY_DIR：由 .build 中的 Python 生成脚本产生"]
fn python_expectations_match_rust_plugin() {
    let directory = parity_dir();
    let document = read_json(&directory.join("cases.json"));
    let mut total = 0_usize;
    let mut mismatches = Vec::new();
    let mut report = Vec::new();

    // 1. 结构：Rust 初始化的表与索引与 Python 逐项一致。
    let scratch = TestDir::new("parity-schema");
    let fresh =
        Store::open(scratch.path(), ManualClock::new(10_000.0).clock()).expect("open fresh store");
    drop(fresh);
    let rust_schema = schema_of(&scratch.path().join(gateway_pool::DB_FILE));
    total += 1;
    let schema_match = rust_schema == document["schema"];
    if !schema_match {
        mismatches.push(format!(
            "schema differs: rust={rust_schema} python={}",
            document["schema"]
        ));
    }
    report.push(json!({"step": "schema", "match": schema_match}));

    // 2. 写路径用例：每个用例使用全新数据库，按 Python 的顺序执行。
    for case in document["cases"].as_array().expect("cases") {
        let harness = Harness::new("parity-case", 10_000.0);
        for (index, step) in case["steps"].as_array().expect("steps").iter().enumerate() {
            let clock = step["clock"].as_f64().expect("clock");
            harness.clock.set(clock);
            total += 1;
            let label = format!("{}#{index}", case["name"].as_str().unwrap_or_default());
            if step["op"] == json!("sql") {
                run_sql(
                    &harness.dir.path().join(gateway_pool::DB_FILE),
                    step["sql"].as_str().unwrap_or_default(),
                    step["args"].as_array().expect("args"),
                );
                report.push(json!({"step": label, "match": true}));
                continue;
            }
            let actual = call(
                &harness.store,
                step["method"].as_str().unwrap_or_default(),
                &step["data"],
            );
            let same = actual == step["expect"];
            if !same {
                mismatches.push(format!(
                    "{label} {} differs: rust={actual} python={}",
                    step["method"], step["expect"]
                ));
            }
            report.push(json!({"step": label, "method": step["method"], "match": same}));
        }
    }

    // 3. 快照库：Python 测试场景生成的真实运行数据，读取与统计逐项比较。
    for snapshot in document["snapshots"].as_array().expect("snapshots") {
        let scratch = TestDir::new("parity-snapshot");
        let file = directory.join(snapshot["file"].as_str().expect("snapshot file"));
        std::fs::copy(&file, scratch.path().join(gateway_pool::DB_FILE)).expect("copy snapshot");
        let clock = ManualClock::new(snapshot["now"].as_f64().expect("now"));
        let store = Store::open(scratch.path(), clock.clock()).expect("open snapshot");
        for (index, step) in snapshot["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .enumerate()
        {
            total += 1;
            let label = format!("{}#{index}", snapshot["name"].as_str().unwrap_or_default());
            let actual = call(
                &store,
                step["method"].as_str().unwrap_or_default(),
                &step["data"],
            );
            let same = actual == step["expect"];
            if !same {
                mismatches.push(format!(
                    "{label} {} differs: rust={actual} python={}",
                    step["method"], step["expect"]
                ));
            }
            report.push(json!({"step": label, "method": step["method"], "match": same}));
        }
    }

    let summary = json!({"total": total, "mismatches": mismatches.len(), "steps": report});
    std::fs::write(
        directory.join("rust-parity-report.json"),
        serde_json::to_string_pretty(&summary).expect("serialize report"),
    )
    .expect("write report");
    assert!(
        mismatches.is_empty(),
        "{} of {total} expectations differ:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Rust 写入数据库，记录期望输出，供 Python 读取后比较。
#[test]
#[ignore = "需要 CPR_GP_PARITY_DIR：由 .build 中的 Python 生成脚本产生"]
fn rust_writes_database_for_python() {
    let directory = parity_dir().join("rust-written");
    std::fs::create_dir_all(&directory).expect("create output directory");
    const NOW: f64 = 10_000.0;
    let harness = Harness::new("parity-rust-writes", NOW);
    let database = harness.dir.path().join(gateway_pool::DB_FILE);
    let mut steps = Vec::new();
    let mut run = |method: &str, data: Value| {
        let result = call(&harness.store, method, &data);
        steps.push(json!({"method": method, "data": data, "clock": NOW, "expect": result}));
    };
    run(
        "configure",
        json!({"gateways": ["unified-142", "unified-143"], "initialRestSeconds": 3600, "poolToken": "fake-pool-secret"}),
    );
    run(
        "queue.update",
        json!({"revision": 0, "accountIds": ["A", "B", "C"]}),
    );
    let revision = harness.call_ok("read", json!({}))["accountQueue"]["revision"].clone();
    run(
        "queue.update",
        json!({"revision": revision, "accountIds": ["C", "A", "B"]}),
    );
    run("configure", json!({"probeIntervalMs": 50.5}));
    run(
        "configure",
        json!({"windowSeconds": 1.5, "mode": "continuous"}),
    );

    // 与 Python 对照相同的数据形状，并通过 SQL 写入组合、观察、切换、目录与分组事件。
    let pair = |account: &str, gateway: &str, extra: Value| {
        let mut object = json!({"accountId": account, "gateway": gateway});
        for (key, value) in extra.as_object().expect("object") {
            object[key] = value.clone();
        }
        object.to_string()
    };
    let inserts: Vec<(&str, Vec<Value>)> = vec![
        (
            "INSERT OR REPLACE INTO pairs VALUES (?,?,?)",
            vec![
                json!("A"),
                json!("unified-142"),
                json!(pair(
                    "A",
                    "unified-142",
                    json!({"state": "ready", "inFlight": 0, "generation": "g1", "firstContactAt": 9000.0, "lastContactAt": 9500.0, "restSeconds": 3600.0, "verifiedAt": 9100.0, "cookieExpiresAt": 10600.0, "probeRestSeconds": 7200})
                )),
            ],
        ),
        (
            "INSERT OR REPLACE INTO pairs VALUES (?,?,?)",
            vec![
                json!("B"),
                json!("unified-142"),
                json!(pair(
                    "B",
                    "unified-142",
                    json!({"state": "cooling", "inFlight": 0, "generation": "g3", "firstContactAt": 8000.0, "lastContactAt": 8500.0, "restSeconds": 7200.0, "restUntil": 15700.0, "learnedRestSeconds": 3600.0})
                )),
            ],
        ),
        (
            "INSERT INTO model_tickets VALUES (?,?,?,?,?)",
            vec![
                json!("A"),
                json!("g1"),
                json!("gpt-6-astra"),
                json!(10600.0),
                json!("{\"state\":\"s\",\"cookie\":\"c\",\"expires\":10600.0}"),
            ],
        ),
        (
            "INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)",
            vec![
                json!(9100.0),
                json!("A"),
                json!("unified-142"),
                json!(
                    "{\"generation\":\"g1\",\"source\":\"probe\",\"status\":200,\"classification\":\"21\",\"stateReturned\":false}"
                ),
            ],
        ),
        (
            "INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)",
            vec![
                json!(9200.0),
                json!("A"),
                json!("unified-142"),
                json!(
                    "{\"generation\":\"g1\",\"source\":\"business\",\"status\":200,\"classification\":null}"
                ),
            ],
        ),
        (
            "INSERT INTO switches(at,account,data) VALUES (?,?,?)",
            vec![
                json!(9100.0),
                json!("A"),
                json!(
                    "{\"fromAccount\":null,\"fromGateway\":null,\"toGateway\":\"unified-142\",\"previousWindowSeconds\":null,\"preparationSeconds\":12.5}"
                ),
            ],
        ),
        (
            "INSERT INTO catalogs VALUES (?,?,?)",
            vec![
                json!("A"),
                json!(9000.0),
                json!("[{\"gateway\":\"unified-142\",\"validForS\":600,\"country\":\"CH\"}]"),
            ],
        ),
        (
            "INSERT INTO group_events VALUES (?,?,?,?,?,?,?,?,?)",
            vec![
                json!("a".repeat(32)),
                json!(9300.0),
                json!("A"),
                json!("degraded"),
                json!("g1"),
                json!("g2"),
                json!("gpt-6.1-sol"),
                json!("moved"),
                json!(9301.0),
            ],
        ),
    ];
    for (sql, args) in &inserts {
        run_sql(&database, sql, args);
    }
    for (method, data) in [
        ("read", json!({})),
        ("read", json!({"accountId": "A"})),
        ("read", json!({"accountId": "A", "limit": 1})),
        (
            "read",
            json!({"accountId": "A", "gateway": "unified-142", "limit": 2}),
        ),
        ("read", json!({"before": 5})),
        ("read", json!({"gateway": "unified-999"})),
        ("history.read", json!({})),
        ("history.read", json!({"view": "summary"})),
        ("history.read", json!({"view": "cooling"})),
        (
            "history.read",
            json!({"view": "records", "pageSize": 1, "page": 2}),
        ),
        (
            "history.read",
            json!({"from": 9000, "to": 9500, "accountId": "A"}),
        ),
        ("history.read", json!({"to": 10061})),
    ] {
        run(method, data);
    }

    // 关闭连接并截断 WAL，使主库文件自身包含全部数据。
    harness
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .expect("checkpoint");
    // 读取后写入而不是 fs::copy：后者会复制权限位，在 Windows 挂载目录上会失败。
    let bytes = std::fs::read(&database).expect("read database");
    std::fs::write(directory.join("state.sqlite3"), bytes).expect("write database copy");
    let expect = json!({"now": NOW, "steps": steps});
    std::fs::write(
        directory.join("rust-expect.json"),
        serde_json::to_string_pretty(&expect).expect("serialize expectations"),
    )
    .expect("write expectations");
}
