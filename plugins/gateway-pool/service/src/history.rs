//! 网关池历史事实的表结构与写入辅助，逐项对应 `gateway_pool_history.py` 中本步骤需要的部分。
//! 历史失败不得回滚调度事实：`record` 使用保存点，失败只记为收集缺口。

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::{
    error::Failure,
    num::Num,
    util::{
        Object, decode_object, encode, exec, is_null_or_missing, num_field, one, set, sql_of,
        sql_of_num, str_field, text, truthy,
    },
};

/// 空闲超过两小时的分段自动关闭。
pub const IDLE_SECONDS: i64 = 7200;

pub const DDL: &str = "
        CREATE TABLE IF NOT EXISTS gp_history_meta (id INTEGER PRIMARY KEY CHECK(id=1), started REAL NOT NULL, gaps INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS gp_batches (id TEXT PRIMARY KEY, account TEXT NOT NULL, number INTEGER NOT NULL, started REAL NOT NULL, ended REAL, status TEXT NOT NULL, candidates TEXT NOT NULL, processed TEXT NOT NULL, reason TEXT, UNIQUE(account,number));
        CREATE TABLE IF NOT EXISTS gp_windows (id TEXT PRIMARY KEY, account TEXT NOT NULL, gateway TEXT NOT NULL, batch TEXT NOT NULL, started REAL NOT NULL, ended REAL, data TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS gp_windows_account_time ON gp_windows(account,started);
        CREATE INDEX IF NOT EXISTS gp_windows_time ON gp_windows(started);
        CREATE INDEX IF NOT EXISTS gp_windows_batch ON gp_windows(batch);
        CREATE TABLE IF NOT EXISTS gp_accesses (id TEXT PRIMARY KEY, lease TEXT NOT NULL, generation TEXT NOT NULL, kind TEXT NOT NULL, observed REAL NOT NULL, started REAL, ended REAL, elapsed REAL, segment TEXT, outcome TEXT, status INTEGER, classification TEXT, incomplete INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS gp_accesses_lease ON gp_accesses(lease);
        CREATE INDEX IF NOT EXISTS gp_accesses_generation ON gp_accesses(generation,started);
        CREATE INDEX IF NOT EXISTS gp_accesses_segment ON gp_accesses(segment);
        CREATE TABLE IF NOT EXISTS gp_segments (id TEXT PRIMARY KEY, generation TEXT NOT NULL, started REAL NOT NULL, last_end REAL, closed REAL, reason TEXT);
        CREATE INDEX IF NOT EXISTS gp_segments_generation ON gp_segments(generation,started);
        CREATE INDEX IF NOT EXISTS gp_segments_open ON gp_segments(closed,last_end);
        CREATE TABLE IF NOT EXISTS gp_cooling (id INTEGER PRIMARY KEY, account TEXT NOT NULL, gateway TEXT NOT NULL, started REAL NOT NULL, recovered REAL, data TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS gp_cooling_pair ON gp_cooling(account,gateway,recovered);
        CREATE INDEX IF NOT EXISTS gp_cooling_time ON gp_cooling(started);
        CREATE INDEX IF NOT EXISTS gp_cooling_pair_recent ON gp_cooling(account,gateway,id);
        CREATE INDEX IF NOT EXISTS observations_generation ON observations(json_extract(data,'$.generation'),id);
    ";

pub fn install(db: &Connection, now: Num) -> Result<(), Failure> {
    db.execute_batch(DDL)?;
    exec(
        db,
        "INSERT OR IGNORE INTO gp_history_meta(id,started) VALUES (1,?)",
        vec![sql_of_num(now)],
    )?;
    Ok(())
}

/// 保存点包住历史写入：失败时回滚到保存点、记录一次缺口，调用方的运行状态变更照常提交。
pub fn record(
    db: &Connection,
    operation: impl FnOnce(&Connection) -> Result<(), Failure>,
) -> Result<(), Failure> {
    db.execute_batch("SAVEPOINT pool_history")?;
    if let Err(error) = operation(db) {
        db.execute_batch("ROLLBACK TO pool_history")?;
        eprintln!("gateway pool history unavailable: {error}");
        // 缺口计数失败时不再向上抛出，与原实现一致。
        let _ = db.execute_batch("UPDATE gp_history_meta SET gaps=gaps+1 WHERE id=1");
    }
    db.execute_batch("RELEASE pool_history")?;
    Ok(())
}

pub fn write_window(db: &Connection, item: &Object) -> Result<(), Failure> {
    exec(
        db,
        "INSERT OR REPLACE INTO gp_windows VALUES (?,?,?,?,?,?,?)",
        vec![
            sql_of(item.get("id").unwrap_or(&Value::Null)),
            sql_of(item.get("accountId").unwrap_or(&Value::Null)),
            sql_of(item.get("gateway").unwrap_or(&Value::Null)),
            sql_of(item.get("batchId").unwrap_or(&Value::Null)),
            sql_of(item.get("preparationStartedAt").unwrap_or(&Value::Null)),
            sql_of(item.get("drainedAt").unwrap_or(&Value::Null)),
            text(&encode(&Value::Object(item.clone()))),
        ],
    )?;
    Ok(())
}

pub fn interrupt(db: &Connection, now: Num, reason: &str) -> Result<(), Failure> {
    exec(
        db,
        "UPDATE gp_batches SET status='interrupted',ended=?,reason=? WHERE status='active'",
        vec![sql_of_num(now), text(reason)],
    )?;
    Ok(())
}

/// 关闭空闲超过两小时、且没有未结束业务访问的分段；`generation` 给出时只处理该代次。
pub fn close_idle(db: &Connection, now: Num, generation: Option<&str>) -> Result<(), Failure> {
    let mut sql = String::from(
        "UPDATE gp_segments AS s SET closed=last_end+?,reason='idle'
        WHERE closed IS NULL AND last_end<=?",
    );
    let mut args = vec![
        rusqlite::types::Value::Integer(IDLE_SECONDS),
        sql_of_num(now.minus(Num::Int(IDLE_SECONDS))),
    ];
    if let Some(generation) = generation {
        sql.push_str(" AND s.generation=?");
        args.push(text(generation));
    }
    sql.push_str(
        " AND NOT EXISTS (SELECT 1 FROM gp_accesses a JOIN leases l ON l.id=a.lease
            WHERE a.generation=s.generation AND a.kind IN ('business','legacy') AND a.ended IS NULL AND l.expires>?)",
    );
    args.push(sql_of_num(now));
    exec(db, &sql, args)?;
    Ok(())
}

/// 把组合的最新状态同步到窗口历史：验证、退役、排空、批次完成与静置周期。
pub fn sync(db: &Connection, pair: &Object, now: Num) -> Result<(), Failure> {
    let Some(generation) = str_field(pair, "generation") else {
        return Ok(());
    };
    let Some(row) = one(
        db,
        "SELECT data FROM gp_windows WHERE id=?",
        vec![text(generation)],
    )?
    else {
        return Ok(());
    };
    let mut item = decode_object(&string_of(&row, "data"))?;
    let previous = str_field(&item, "state").unwrap_or_default().to_owned();
    let state = str_field(pair, "state").unwrap_or_default().to_owned();
    set(&mut item, "state", json!(state));
    if state == "ready" && is_null_or_missing(&item, "verifiedAt") {
        let verified = pair
            .get("verifiedAt")
            .cloned()
            .unwrap_or_else(|| now.to_json());
        set(&mut item, "verifiedAt", verified);
        sync_learned_rest(db, pair, &item)?;
    }
    if matches!(state.as_str(), "draining" | "cooling" | "unknown")
        && is_null_or_missing(&item, "retiredAt")
    {
        set(&mut item, "retiredAt", now.to_json());
        let reason = pair
            .get("retiredReason")
            .cloned()
            .unwrap_or_else(|| json!("rotation"));
        set(&mut item, "reason", reason);
        exec(
            db,
            "UPDATE gp_segments SET closed=?,reason=? WHERE generation=? AND closed IS NULL",
            vec![
                sql_of_num(now),
                sql_of(item.get("reason").unwrap_or(&Value::Null)),
                text(generation),
            ],
        )?;
    }
    if !is_null_or_missing(&item, "retiredAt")
        && !truthy(pair.get("inFlight"))
        && is_null_or_missing(&item, "drainedAt")
    {
        let accesses = one(
            db,
            "SELECT MAX(ended) AS ended,MAX(incomplete) AS missing FROM gp_accesses WHERE generation=?",
            vec![text(generation)],
        )?
        .unwrap_or_default();
        let retired = num_field(&item, "retiredAt").unwrap_or(now);
        let drained = match num_field(&accesses, "ended") {
            Some(ended) if !truthy(accesses.get("missing")) && !truthy(item.get("incomplete")) => {
                retired.max(ended)
            }
            _ => now,
        };
        set(&mut item, "drainedAt", drained.to_json());
    }
    write_window(db, &item)?;
    if !is_null_or_missing(&item, "drainedAt") {
        sync_batch(db, &item, now)?;
    }
    if state == "cooling" && previous != "cooling" && !is_null_or_missing(pair, "lastContactAt") {
        let contacted = one(
            db,
            "SELECT 1 FROM gp_accesses WHERE generation=? LIMIT 1",
            vec![text(generation)],
        )?;
        if contacted.is_some() {
            sync_cooling(db, pair, &item)?;
        }
    }
    Ok(())
}

/// 上一周期若是 21 的验证，记录其学习到的静置时长。
fn sync_learned_rest(db: &Connection, pair: &Object, item: &Object) -> Result<(), Failure> {
    let account = str_field(pair, "accountId").unwrap_or_default();
    let gateway = str_field(pair, "gateway").unwrap_or_default();
    let Some(cycle) = one(
        db,
        "SELECT * FROM gp_cooling WHERE account=? AND gateway=? ORDER BY id DESC LIMIT 1",
        vec![text(account), text(gateway)],
    )?
    else {
        return Ok(());
    };
    let mut data = decode_object(&string_of(&cycle, "data"))?;
    let Some(step) = data
        .get_mut("steps")
        .and_then(Value::as_array_mut)
        .and_then(|steps| steps.last_mut())
    else {
        return Err(Failure::Internal("静置周期缺少步骤".to_owned()));
    };
    let Some(step) = step.as_object_mut() else {
        return Err(Failure::Internal("静置步骤格式无效".to_owned()));
    };
    if step.get("generation") == item.get("id")
        && step.get("result").and_then(Value::as_str) == Some("21")
    {
        step.insert(
            "learnedRestSeconds".to_owned(),
            pair.get("learnedRestSeconds")
                .cloned()
                .unwrap_or(Value::Null),
        );
        exec(
            db,
            "UPDATE gp_cooling SET data=? WHERE id=?",
            vec![
                text(&encode(&Value::Object(data))),
                sql_of(cycle.get("id").unwrap_or(&Value::Null)),
            ],
        )?;
    }
    Ok(())
}

fn sync_batch(db: &Connection, item: &Object, now: Num) -> Result<(), Failure> {
    let batch_id = str_field(item, "batchId").unwrap_or_default();
    let Some(batch) = one(
        db,
        "SELECT * FROM gp_batches WHERE id=?",
        vec![text(batch_id)],
    )?
    else {
        return Err(Failure::Internal("窗口所属批次不存在".to_owned()));
    };
    let mut processed = decode_object(&string_of(&batch, "processed"))?;
    if !is_null_or_missing(item, "firstContactAt") {
        // 未触及上游的 Cookie/目录失败不算遍历，重复候选仍留在当前批次。
        let outcome = if !is_null_or_missing(item, "verifiedAt") {
            json!("served")
        } else if truthy(item.get("probeResult")) {
            item.get("probeResult").cloned().unwrap_or(Value::Null)
        } else {
            json!("interrupted")
        };
        let gateway = str_field(item, "gateway").unwrap_or_default().to_owned();
        processed.insert(gateway, outcome);
    }
    exec(
        db,
        "UPDATE gp_batches SET processed=? WHERE id=?",
        vec![
            text(&encode(&Value::Object(processed.clone()))),
            text(batch_id),
        ],
    )?;
    let candidates = string_of(&batch, "candidates");
    let candidates: Vec<String> = serde_json::from_str(&candidates)?;
    let complete = candidates
        .iter()
        .all(|candidate| processed.contains_key(candidate));
    if str_field(&batch, "status") == Some("active") && complete {
        let open = one(
            db,
            "SELECT 1 FROM gp_windows WHERE batch=? AND ended IS NULL LIMIT 1",
            vec![text(batch_id)],
        )?;
        if open.is_none() {
            exec(
                db,
                "UPDATE gp_batches SET status='complete',ended=? WHERE id=?",
                vec![sql_of_num(now), text(batch_id)],
            )?;
        }
    }
    Ok(())
}

fn sync_cooling(db: &Connection, pair: &Object, item: &Object) -> Result<(), Failure> {
    let account = str_field(pair, "accountId").unwrap_or_default();
    let gateway = str_field(pair, "gateway").unwrap_or_default();
    let cycle = one(
        db,
        "SELECT * FROM gp_cooling WHERE account=? AND gateway=? AND recovered IS NULL ORDER BY id DESC LIMIT 1",
        vec![text(account), text(gateway)],
    )?;
    let mut data = match &cycle {
        Some(row) => decode_object(&string_of(row, "data"))?,
        None => {
            let mut fresh = Object::new();
            set(
                &mut fresh,
                "reason",
                item.get("reason").cloned().unwrap_or(Value::Null),
            );
            set(&mut fresh, "steps", json!([]));
            fresh
        }
    };
    let scheduled = pair.get("lastContactAt").cloned().unwrap_or(Value::Null);
    let steps = data
        .get_mut("steps")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| Failure::Internal("静置周期的步骤格式无效".to_owned()))?;
    if steps
        .last()
        .and_then(|step| step.get("scheduledStartAt"))
        .is_some_and(|value| same_number(value, &scheduled))
    {
        return Ok(());
    }
    let last = one(
        db,
        "SELECT MAX(ended) AS ended,MAX(incomplete) AS missing,COUNT(*) AS count FROM gp_accesses WHERE generation=?",
        vec![text(str_field(item, "id").unwrap_or_default())],
    )?
    .unwrap_or_default();
    // 调度的保守时间与准确访问时间分别保存，失联回收不会伪装成准确静置。
    let count = num_field(&last, "count").map_or(0.0, Num::as_f64);
    let actual = if count > 0.0 && !truthy(last.get("missing")) && !truthy(item.get("incomplete")) {
        last.get("ended").cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    steps.push(json!({
        "generation": null,
        "startedAt": actual,
        "scheduledStartAt": scheduled,
        "plannedSeconds": pair.get("restSeconds").cloned().unwrap_or(Value::Null),
        "dueAt": pair.get("restUntil").cloned().unwrap_or(Value::Null),
        "probeAt": null,
        "resultAt": null,
        "result": null,
        "status": null,
        "learnedRestSeconds": pair.get("learnedRestSeconds").cloned().unwrap_or(Value::Null),
    }));
    match cycle {
        Some(row) => exec(
            db,
            "UPDATE gp_cooling SET data=? WHERE id=?",
            vec![
                text(&encode(&Value::Object(data))),
                sql_of(row.get("id").unwrap_or(&Value::Null)),
            ],
        )?,
        None => {
            let started = if actual.is_null() {
                pair.get("lastContactAt").cloned().unwrap_or(Value::Null)
            } else {
                actual
            };
            exec(
                db,
                "INSERT INTO gp_cooling(account,gateway,started,data) VALUES (?,?,?,?)",
                vec![
                    text(account),
                    text(gateway),
                    sql_of(&started),
                    text(&encode(&Value::Object(data))),
                ],
            )?
        }
    };
    Ok(())
}

/// 数值相等（整数与浮点同值视为相等），与 Python `==` 对数字的比较一致。
fn same_number(left: &Value, right: &Value) -> bool {
    match (Num::from_json(left), Num::from_json(right)) {
        (Some(a), Some(b)) => a.as_f64() == b.as_f64(),
        _ => left == right,
    }
}

fn string_of(row: &Object, key: &str) -> String {
    str_field(row, key).unwrap_or_default().to_owned()
}
