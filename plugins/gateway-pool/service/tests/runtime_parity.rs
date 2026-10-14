//! 运行状态机与 Python 实现的一致性对照。默认忽略：需要 `CPR_GP_PARITY_DIR` 指向由 `tests/parity/` 里的 Python 脚本生成的产物目录（见 `tests/parity/README.md`）。
//! 每个序列在全新数据库上按 Python 记录的消息与时钟逐步重放，比较每一步的返回值（整数与浮点严格区分），
//! 序列结束后比较整库内容；同时把 Rust 写出的数据库保存下来，供 Python 读取后再比较一次。
//!
//! 唯一承认的有意差异是 012 加入的“恢复检测领取超过 5 分钟仍没有结果可重新领取”：Python 没有这条规则
//! （同一代次只领取一次）。判为有意差异必须同时满足：步骤是 `recoveryCheck`，Python 返回 `check: null`，Rust 返回
//! 该账号此前已领取的同一代次，该领取距当前超过 300 秒，且此后没有 `recoveryResult`；序列末尾的数据库差异只限
//! 这些账号的 `recovery_checks.claimed_at`。判为有意差异的每一步与字段都写进报告的 `intentionalDifferences`，
//! 其余任何差异仍然失败。

mod common;

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

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
/// 领取超过这么久没有结果即可重新领取（`recovery_candidate` 里的 300 秒）。
const STALE_CLAIM_SECONDS: f64 = 300.0;

/// 序列重放过程中账号最近一次领取的恢复检测（取自 Rust 实际返回的结果，不读数据库）。
struct Claim {
    generation: String,
    clock: f64,
    /// 领取之后是否有过 `recoveryResult`。
    resulted: bool,
}

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

/// 有意差异的步骤判定：Python 返回 `check: null`，Rust 重新领取了该账号此前已领取、超过 300 秒仍没有结果的同一代次。
fn is_stale_reclaim(expect: &Value, actual: &Value, prior: Option<&Claim>, clock: f64) -> bool {
    let (Some(prior), Some(generation)) = (prior, actual["ok"]["check"]["generation"].as_str())
    else {
        return false;
    };
    *expect == json!({"ok": {"check": null}})
        && prior.generation == generation
        && !prior.resulted
        && clock - prior.clock > STALE_CLAIM_SECONDS
}

/// 序列末尾的数据库差异只允许是有意重新领取的账号的 `recovery_checks.claimed_at`：
/// 该行除 `claimed_at` 外必须与 Python 完全一致，Rust 的值必须是最后一次重新领取的时刻且晚于 Python 的值。
/// 满足的行在返回的副本里换成 Python 的值（之后照常整库比较），并给出记录。
fn accept_reclaimed_rows(
    sequence: &str,
    rust: &Value,
    python: &Value,
    reclaimed: &BTreeMap<String, f64>,
) -> (Value, Vec<Value>) {
    let mut adjusted = rust.clone();
    let mut notes = Vec::new();
    let python_rows = python["recovery_checks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let Some(rows) = adjusted["recovery_checks"].as_array_mut() else {
        return (adjusted, notes);
    };
    for row in rows.iter_mut() {
        let Some(account) = row["account"].as_str().map(str::to_owned) else {
            continue;
        };
        let (Some(reclaimed_at), Some(counterpart)) = (
            reclaimed.get(&account),
            python_rows
                .iter()
                .find(|candidate| candidate["account"] == account.as_str()),
        ) else {
            continue;
        };
        if row == counterpart {
            continue;
        }
        let mut candidate = row.clone();
        candidate["claimed_at"] = counterpart["claimed_at"].clone();
        let (rust_at, python_at) = (
            row["claimed_at"].as_f64(),
            counterpart["claimed_at"].as_f64(),
        );
        if candidate == *counterpart
            && rust_at == Some(*reclaimed_at)
            && matches!((rust_at, python_at), (Some(r), Some(p)) if r > p)
        {
            notes.push(json!({
                "sequence": sequence, "table": "recovery_checks", "account": account, "column": "claimed_at",
                "rust": row["claimed_at"], "python": counterpart["claimed_at"],
            }));
            *row = counterpart.clone();
        }
    }
    (adjusted, notes)
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
    let mut all_intentional: Vec<Value> = Vec::new();
    for sequence in document["sequences"].as_array().expect("sequences") {
        let name = sequence["name"].as_str().expect("name").to_owned();
        let scratch = TestDir::new("runtime-parity");
        let clock = ManualClock::new(10_000.0);
        let ids = sequential_ids();
        let mut store = open(scratch.path(), &clock, &ids);
        let mut step_results = Vec::new();
        let mut mismatches = 0_usize;
        let mut claims: HashMap<String, Claim> = HashMap::new();
        let mut reclaimed: BTreeMap<String, f64> = BTreeMap::new();
        let mut intentional = Vec::new();
        for (index, step) in sequence["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .enumerate()
        {
            let now = step["clock"].as_f64().expect("clock");
            clock.set(now);
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
            let mut same = actual == step["expect"];
            let mut stale = false;
            if op == "runtime" {
                let account = step["message"]["accountId"].as_str().map(str::to_owned);
                match (label.as_str(), account) {
                    ("recoveryCheck", Some(account)) => {
                        if !same
                            && is_stale_reclaim(&step["expect"], &actual, claims.get(&account), now)
                        {
                            let prior = &claims[&account];
                            intentional.push(json!({
                                "sequence": name, "step": index, "action": label, "account": account,
                                "generation": prior.generation, "claimedAt": prior.clock, "now": now,
                                "secondsSinceClaim": now - prior.clock,
                                "python": step["expect"]["ok"], "rust": actual["ok"],
                            }));
                            reclaimed.insert(account.clone(), now);
                            stale = true;
                            same = true;
                        }
                        // 无论一致还是有意差异，Rust 返回的领取都是该账号最近一次领取。
                        if let Some(generation) = actual["ok"]["check"]["generation"].as_str() {
                            claims.insert(
                                account,
                                Claim {
                                    generation: generation.to_owned(),
                                    clock: now,
                                    resulted: false,
                                },
                            );
                        }
                    }
                    ("recoveryResult", Some(account)) if actual.get("ok").is_some() => {
                        if let Some(claim) = claims.get_mut(&account) {
                            claim.resulted = true;
                        }
                    }
                    _ => {}
                }
            }
            if !same {
                mismatches += 1;
                if failures.len() < 30 {
                    failures.push(format!(
                        "{name}#{index} {label}: rust={actual} python={}",
                        step["expect"]
                    ));
                }
            }
            step_results.push(
                json!({"step": index, "op": op, "action": label, "match": same, "intentional": stale}),
            );
        }
        let database = scratch.path().join(gateway_pool::DB_FILE);
        let rust_dump = dump_database(&database);
        let (accepted_dump, database_notes) =
            accept_reclaimed_rows(&name, &rust_dump, &sequence["database"], &reclaimed);
        let database_same = accepted_dump == sequence["database"];
        if !database_same && failures.len() < 30 {
            failures.push(format!(
                "{name}: database differs: {}",
                first_database_difference(&accepted_dump, &sequence["database"])
                    .unwrap_or_default()
            ));
        }
        intentional.extend(database_notes);
        // 写出 Rust 的最终库，供 Python 读取。
        let bytes = std::fs::read(&database).expect("read database");
        std::fs::write(written.join(format!("{name}.sqlite3")), bytes)
            .expect("write database copy");
        report.push(json!({
            "sequence": name,
            "steps": step_results.len(),
            "stepMismatches": mismatches,
            "databaseMatch": database_same,
            "intentionalDifferences": intentional.clone(),
            "results": step_results,
        }));
        all_intentional.extend(intentional);
    }
    let total: usize = report
        .iter()
        .map(|item| item["steps"].as_u64().unwrap_or(0) as usize)
        .sum();
    let summary = json!({"sequences": report.len(), "steps": total, "failures": failures.len(),
        "intentionalDifferences": all_intentional.len(), "report": report});
    std::fs::write(
        directory.join("runtime-parity-intentional.json"),
        serde_json::to_string_pretty(&all_intentional).expect("serialize intentional differences"),
    )
    .expect("write intentional differences");
    for item in &all_intentional {
        println!("intentional difference: {item}");
    }
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

fn claim(resulted: bool) -> Claim {
    Claim {
        generation: "g1".to_owned(),
        clock: 1000.0,
        resulted,
    }
}

/// 有意差异的判定必须同时满足全部条件，缺任何一条都按差异失败。
#[test]
fn stale_reclaim_requires_every_condition() {
    let python = json!({"ok": {"check": null}});
    let rust = |generation: &str| json!({"ok": {"check": {"generation": generation, "gateway": "x", "ageSeconds": 1}}});
    // 全部满足：同一代次、超过 300 秒、此后没有结果、Python 为 null。
    assert!(is_stale_reclaim(
        &python,
        &rust("g1"),
        Some(&claim(false)),
        1300.5
    ));
    // 恰好 300 秒不算超过；此后有过 recoveryResult；代次不同；此前没有领取。
    assert!(!is_stale_reclaim(
        &python,
        &rust("g1"),
        Some(&claim(false)),
        1300.0
    ));
    assert!(!is_stale_reclaim(
        &python,
        &rust("g1"),
        Some(&claim(true)),
        5000.0
    ));
    assert!(!is_stale_reclaim(
        &python,
        &rust("g2"),
        Some(&claim(false)),
        5000.0
    ));
    assert!(!is_stale_reclaim(&python, &rust("g1"), None, 5000.0));
    // Python 不是 null（例如领取了别的东西），或 Rust 没有返回领取。
    assert!(!is_stale_reclaim(
        &rust("g1"),
        &rust("g1"),
        Some(&claim(false)),
        5000.0
    ));
    assert!(!is_stale_reclaim(
        &python,
        &json!({"ok": {"check": null}}),
        Some(&claim(false)),
        5000.0
    ));
    assert!(!is_stale_reclaim(
        &python,
        &json!({"error": "x"}),
        Some(&claim(false)),
        5000.0
    ));
}

/// 数据库差异只放过有意重新领取的账号的 `claimed_at`，行里别的列、别的账号、别的表都不放过。
#[test]
fn only_claimed_at_of_reclaimed_accounts_is_accepted_in_the_database() {
    let row = |account: &str, claimed_at: f64, last_at: Value| {
        json!({"account": account, "claimed_at": claimed_at, "claimed_generation": "g1",
            "last_at": last_at, "last_outcome": null, "paused_until": 0.0})
    };
    let python =
        json!({"recovery_checks": [row("A", 1000.0, json!(null)), row("B", 1000.0, json!(null))]});
    let reclaimed = BTreeMap::from([("A".to_owned(), 5000.0)]);
    // 只有 claimed_at 不同，且是最后一次重新领取的时刻：接受并记录。
    let rust =
        json!({"recovery_checks": [row("A", 5000.0, json!(null)), row("B", 1000.0, json!(null))]});
    let (adjusted, notes) = accept_reclaimed_rows("s", &rust, &python, &reclaimed);
    assert_eq!(adjusted, python);
    assert_eq!(notes.len(), 1);
    // 同一行里别的列也不同：不接受。
    let rust =
        json!({"recovery_checks": [row("A", 5000.0, json!(7.0)), row("B", 1000.0, json!(null))]});
    let (adjusted, notes) = accept_reclaimed_rows("s", &rust, &python, &reclaimed);
    assert_ne!(adjusted, python);
    assert!(notes.is_empty());
    // claimed_at 不是最后一次重新领取的时刻，或不晚于 Python 的值：不接受。
    for at in [4000.0, 900.0] {
        let rust =
            json!({"recovery_checks": [row("A", at, json!(null)), row("B", 1000.0, json!(null))]});
        assert_ne!(
            accept_reclaimed_rows("s", &rust, &python, &reclaimed).0,
            python
        );
    }
    // 没有重新领取过的账号的 claimed_at 差异：不接受。
    let rust =
        json!({"recovery_checks": [row("A", 5000.0, json!(null)), row("B", 5000.0, json!(null))]});
    assert_ne!(
        accept_reclaimed_rows("s", &rust, &python, &reclaimed).0,
        python
    );
}
