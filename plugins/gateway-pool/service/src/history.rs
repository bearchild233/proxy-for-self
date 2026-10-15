//! 网关池历史事实的表结构与写入辅助：窗口、访问、分段、批次与静置周期。
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

/// `begin`：探测开始时登记窗口；账号没有进行中的批次时开启新批次（批次号递增）。
pub fn begin(db: &Connection, pair: &Object, gateways: &[String], now: Num) -> Result<(), Failure> {
    let account = str_field(pair, "accountId").unwrap_or_default();
    let generation = str_field(pair, "generation").unwrap_or_default();
    let previous = one(
        db,
        "SELECT * FROM gp_batches WHERE account=? ORDER BY number DESC LIMIT 1",
        vec![text(account)],
    )?;
    let (batch_id, number) = match previous {
        Some(row) if str_field(&row, "status") == Some("active") => (
            string_of(&row, "id"),
            num_field(&row, "number").unwrap_or(Num::Int(0)),
        ),
        previous => {
            let number = previous
                .as_ref()
                .and_then(|row| num_field(row, "number"))
                .map_or(Num::Int(1), |number| number.plus(Num::Int(1)));
            let candidates: std::collections::BTreeSet<&String> = gateways.iter().collect();
            exec(
                db,
                "INSERT INTO gp_batches VALUES (?,?,?,?,NULL,?,?,?,NULL)",
                vec![
                    text(generation),
                    text(account),
                    sql_of_num(number),
                    sql_of_num(now),
                    text("active"),
                    text(&encode(&json!(candidates))),
                    text("{}"),
                ],
            )?;
            (generation.to_owned(), number)
        }
    };
    let mut window = Object::new();
    for (key, value) in [
        ("id", json!(generation)),
        ("accountId", json!(account)),
        (
            "gateway",
            pair.get("gateway").cloned().unwrap_or(Value::Null),
        ),
        ("batchId", json!(batch_id)),
        ("batchNumber", number.to_json()),
        ("preparationStartedAt", now.to_json()),
        ("probeEndedAt", Value::Null),
        ("probeResult", Value::Null),
        ("firstContactAt", Value::Null),
        ("verifiedAt", Value::Null),
        ("retiredAt", Value::Null),
        ("drainedAt", Value::Null),
        ("state", json!("preparing")),
        ("reason", Value::Null),
        ("incomplete", json!(false)),
    ] {
        set(&mut window, key, value);
    }
    write_window(db, &window)
}

pub fn interrupt(db: &Connection, now: Num, reason: &str) -> Result<(), Failure> {
    exec(
        db,
        "UPDATE gp_batches SET status='interrupted',ended=?,reason=? WHERE status='active'",
        vec![sql_of_num(now), text(reason)],
    )?;
    Ok(())
}

/// 进程启动时清理上一个进程留下的业务访问：没有结束时间、也不属于任何仍有效的在途租约的记录标为中断。
/// 只改 `outcome`；是否计入时长仍由统计按“结束未知”处理，不改动其他事实。
pub fn interrupt_orphans(db: &Connection, now: Num) -> Result<(), Failure> {
    exec(
        db,
        "UPDATE gp_accesses SET outcome='interrupted'
         WHERE ended IS NULL AND kind='business' AND COALESCE(outcome,'')<>'interrupted'
           AND NOT EXISTS (SELECT 1 FROM leases l WHERE l.id=gp_accesses.lease AND l.expires>?)",
        vec![sql_of_num(now)],
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
    if matches!(state.as_str(), "ready" | "standby") && is_null_or_missing(&item, "verifiedAt") {
        let verified = pair
            .get("verifiedAt")
            .cloned()
            .unwrap_or_else(|| now.to_json());
        set(&mut item, "verifiedAt", verified);
        sync_learned_rest(db, pair, &item)?;
    }
    // 备用票：拿到时间、启用时间与转正后窗口的计时起点，页面据此区分“备用期间”与窗口。
    if state == "standby" {
        set(&mut item, "standby", json!(true));
    }
    for key in ["standbyAt", "activatedAt", "windowStartedAt"] {
        if is_null_or_missing(&item, key) && !is_null_or_missing(pair, key) {
            set(
                &mut item,
                key,
                pair.get(key).cloned().unwrap_or(Value::Null),
            );
        }
    }
    if matches!(state.as_str(), "draining" | "cooling" | "unknown")
        && is_null_or_missing(&item, "retiredAt")
    {
        // 备用票到期是惰性丢弃的：结束时间记为它应当到期的时刻，而不是实际处理的时刻。
        let expiry = (str_field(pair, "retiredReason") == Some("standby_expired"))
            .then(|| num_field(pair, "standbyAt"))
            .flatten()
            .map(|at| at.plus(Num::Int(crate::policy::STANDBY_LIFETIME_SECONDS)));
        set(&mut item, "retiredAt", expiry.unwrap_or(now).to_json());
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
    // 从未启用的备用票没有服务过，不算遍历（与队列的待完成清单一致）。
    let unserved_standby = truthy(item.get("standby")) && is_null_or_missing(item, "activatedAt");
    if !is_null_or_missing(item, "firstContactAt") && !unserved_standby {
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

/// 手动调整静置（取消或缩短）后，把进行中周期最后一步的到期时间与计划时长改成新值，并记下调整时刻。
pub(crate) fn adjust_cooling(
    db: &Connection,
    pair: &Object,
    now: f64,
    reason: &str,
) -> Result<(), Failure> {
    let account = str_field(pair, "accountId").unwrap_or_default();
    let gateway = str_field(pair, "gateway").unwrap_or_default();
    let Some(row) = one(
        db,
        "SELECT id,data FROM gp_cooling WHERE account=? AND gateway=? AND recovered IS NULL ORDER BY id DESC LIMIT 1",
        vec![text(account), text(gateway)],
    )?
    else {
        return Ok(());
    };
    let mut data = decode_object(&string_of(&row, "data"))?;
    let Some(step) = data
        .get_mut("steps")
        .and_then(Value::as_array_mut)
        .and_then(|steps| steps.last_mut())
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    if !step.get("probeAt").is_none_or(Value::is_null) {
        return Ok(());
    }
    set(
        step,
        "dueAt",
        pair.get("restUntil").cloned().unwrap_or(Value::Null),
    );
    set(
        step,
        "plannedSeconds",
        pair.get("restSeconds").cloned().unwrap_or(Value::Null),
    );
    set(step, "adjustedAt", json!(now));
    set(step, "adjustedReason", json!(reason));
    exec(
        db,
        "UPDATE gp_cooling SET data=? WHERE id=?",
        vec![
            text(&encode(&Value::Object(data))),
            sql_of(row.get("id").unwrap_or(&Value::Null)),
        ],
    )?;
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
        // 这一轮静置的原因、所在的档与当时的起始档；复测失败的原因在 `failure`。
        "reason": pair.get("retiredReason").cloned().unwrap_or(Value::Null),
        "tierSeconds": pair.get("tierSeconds").or_else(|| pair.get("restSeconds")).cloned().unwrap_or(Value::Null),
        "startTierSeconds": pair.get("startTierSeconds").cloned().unwrap_or(Value::Null),
        "failure": null,
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

/// `sent`：记录一次实际发出的访问；缺少新版计时标识的旧调用按 legacy 处理（缺口、不计时长）。
pub fn sent(db: &Connection, lease: &Object, payload: &Object, now: Num) -> Result<(), Failure> {
    let mut kind = match payload.get("purpose") {
        Some(Value::String(purpose))
            if matches!(
                purpose.as_str(),
                "business" | "ticket" | "bootstrap" | "replay"
            ) =>
        {
            purpose.clone()
        }
        // Python 用集合判断成员，不可哈希的列表与对象会抛出 TypeError，由 `record` 记为缺口。
        Some(Value::Array(_) | Value::Object(_)) => {
            return Err(Failure::Internal("unhashable type".to_owned()));
        }
        _ => "legacy".to_owned(),
    };
    let timing_v2 = matches!(payload.get("timingVersion"), Some(Value::Number(number)) if number.as_f64() == Some(2.0));
    let access_id = match payload.get("accessId") {
        Some(Value::String(id)) if timing_v2 && (1..=80).contains(&id.chars().count()) => {
            id.clone()
        }
        _ => {
            kind = "legacy".to_owned();
            format!("legacy:{}", string_of(lease, "id"))
        }
    };
    close_idle(db, now, str_field(lease, "generation"))?;
    exec(
        db,
        "INSERT OR IGNORE INTO gp_accesses(id,lease,generation,kind,observed,incomplete) VALUES (?,?,?,?,?,?)",
        vec![
            text(&access_id),
            text(&string_of(lease, "id")),
            text(&string_of(lease, "generation")),
            text(&kind),
            sql_of_num(now),
            rusqlite::types::Value::Integer(i64::from(kind == "legacy")),
        ],
    )?;
    Ok(())
}

/// 当前周期（未恢复）的最新记录。
fn open_cycle(db: &Connection, account: &str, gateway: &str) -> Result<Option<Object>, Failure> {
    one(
        db,
        "SELECT * FROM gp_cooling WHERE account=? AND gateway=? AND recovered IS NULL ORDER BY id DESC LIMIT 1",
        vec![text(account), text(gateway)],
    )
}

fn window_item(db: &Connection, generation: &str) -> Result<Option<Object>, Failure> {
    match one(
        db,
        "SELECT data FROM gp_windows WHERE id=?",
        vec![text(generation)],
    )? {
        Some(row) => Ok(Some(decode_object(&string_of(&row, "data"))?)),
        None => Ok(None),
    }
}

/// `report`：用网关回报的计时补全访问事实（起止时间、分段、窗口首次接触与静置步骤的探测时间）。
pub fn report(db: &Connection, lease: &Object, payload: &Object, now: Num) -> Result<(), Failure> {
    let Some(Value::Object(access)) = payload.get("access") else {
        return Ok(());
    };
    let Some(row) = one(
        db,
        "SELECT * FROM gp_accesses WHERE id=? AND lease=?",
        vec![
            sql_of(access.get("id").unwrap_or(&Value::Null)),
            text(&string_of(lease, "id")),
        ],
    )?
    else {
        return Ok(());
    };
    if !is_null_or_missing(&row, "ended") {
        return Ok(());
    }
    let (Some(start), Some(elapsed)) = (
        num_field(access, "startedAt"),
        num_field(access, "elapsedMs"),
    ) else {
        return Failure::invalid("访问计时无效");
    };
    let observed = num_field(&row, "observed").map_or(0.0, Num::as_f64);
    let (start_f, elapsed_f, now_f) = (start.as_f64(), elapsed.as_f64(), now.as_f64());
    if !(0.0..=86_400_000.0).contains(&elapsed_f)
        || !(observed - 5.0 <= start_f && start_f <= now_f + 5.0)
        || start_f + elapsed_f / 1000.0 > now_f + 5.0
    {
        return Failure::invalid("访问计时超出范围");
    }
    let end = (access.get("ended") == Some(&Value::Bool(true)))
        .then(|| Num::Float(start_f + elapsed_f / 1000.0));
    let outcome = match access.get("outcome") {
        Some(Value::String(outcome))
            if matches!(
                outcome.as_str(),
                "completed" | "failed" | "cancelled" | "interrupted" | "rejected" | "headers"
            ) =>
        {
            outcome.clone()
        }
        Some(Value::Array(_) | Value::Object(_)) => {
            return Err(Failure::Internal("unhashable type".to_owned()));
        }
        _ => "interrupted".to_owned(),
    };
    let generation = string_of(lease, "generation");
    let mut segment = str_field(&row, "segment")
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned);
    if str_field(&row, "kind") == Some("business") && segment.is_none() {
        let previous = one(
            db,
            "SELECT * FROM gp_segments WHERE generation=? AND (closed IS NULL OR closed>?) ORDER BY started DESC LIMIT 1",
            vec![text(&generation), sql_of_num(start)],
        )?;
        if let Some(previous) = previous {
            let id = string_of(&previous, "id");
            exec(
                db,
                "UPDATE gp_segments SET started=MIN(started,?) WHERE id=?",
                vec![sql_of_num(start), text(&id)],
            )?;
            segment = Some(id);
        } else {
            let id = string_of(&row, "id");
            let item = window_item(db, &generation)?.unwrap_or_default();
            exec(
                db,
                "INSERT INTO gp_segments VALUES (?,?,?,NULL,?,?)",
                vec![
                    text(&id),
                    text(&generation),
                    sql_of_num(start),
                    sql_of(item.get("retiredAt").unwrap_or(&Value::Null)),
                    sql_of(item.get("reason").unwrap_or(&Value::Null)),
                ],
            )?;
            segment = Some(id);
        }
    }
    exec(
        db,
        "UPDATE gp_accesses SET started=?,ended=?,elapsed=?,segment=?,outcome=?,status=COALESCE(?,status),classification=COALESCE(?,classification) WHERE id=?",
        vec![
            sql_of_num(start),
            end.map_or(rusqlite::types::Value::Null, sql_of_num),
            rusqlite::types::Value::Real(elapsed_f / 1000.0),
            segment
                .as_deref()
                .map_or(rusqlite::types::Value::Null, text),
            text(&outcome),
            sql_of(payload.get("status").unwrap_or(&Value::Null)),
            sql_of(payload.get("classification").unwrap_or(&Value::Null)),
            text(&string_of(&row, "id")),
        ],
    )?;
    if let (Some(segment), Some(end)) = (&segment, end) {
        exec(
            db,
            "UPDATE gp_segments SET last_end=MAX(COALESCE(last_end,0),?) WHERE id=?",
            vec![sql_of_num(end), text(segment)],
        )?;
    }
    if let Some(mut item) = window_item(db, &generation)? {
        let first = match num_field(&item, "firstContactAt") {
            Some(first) => first.min(start),
            None => start,
        };
        set(&mut item, "firstContactAt", first.to_json());
        write_window(db, &item)?;
    }
    if str_field(lease, "kind") == Some("probe") {
        let account = string_of(lease, "account");
        let gateway = string_of(lease, "gateway");
        if let Some(cycle) = open_cycle(db, &account, &gateway)? {
            let mut data = decode_object(&string_of(&cycle, "data"))?;
            let step = last_step(&mut data)?;
            if is_null_or_missing(step, "probeAt") {
                set(step, "probeAt", start.to_json());
                set(step, "generation", json!(generation));
                exec(
                    db,
                    "UPDATE gp_cooling SET data=? WHERE id=?",
                    vec![
                        text(&encode(&Value::Object(data))),
                        sql_of(cycle.get("id").unwrap_or(&Value::Null)),
                    ],
                )?;
            }
        }
    }
    Ok(())
}

/// 静置周期的最后一步；周期没有步骤时对应 Python 的 `IndexError`。
fn last_step(data: &mut Object) -> Result<&mut Object, Failure> {
    data.get_mut("steps")
        .and_then(Value::as_array_mut)
        .and_then(|steps| steps.last_mut())
        .and_then(Value::as_object_mut)
        .ok_or_else(|| Failure::Internal("静置周期缺少步骤".to_owned()))
}

/// `finish`：租约结束时补全最后一次回报；没有收到结束回报的访问标为缺口（不伪造时长）。
pub fn finish(
    db: &Connection,
    lease: &Object,
    payload: &Object,
    now: Num,
    incomplete: bool,
) -> Result<(), Failure> {
    report(db, lease, payload, now)?;
    let lease_id = string_of(lease, "id");
    exec(
        db,
        "UPDATE gp_accesses SET incomplete=1,outcome=COALESCE(outcome,?) WHERE lease=? AND ended IS NULL",
        vec![text("interrupted"), text(&lease_id)],
    )?;
    let missing = incomplete
        || one(
            db,
            "SELECT 1 FROM gp_accesses WHERE lease=? AND incomplete=1 LIMIT 1",
            vec![text(&lease_id)],
        )?
        .is_some();
    if missing && let Some(mut item) = window_item(db, &string_of(lease, "generation"))? {
        set(&mut item, "incomplete", json!(true));
        write_window(db, &item)?;
    }
    Ok(())
}

/// `result`：探测结果写入窗口与静置步骤。`verdict` 为 None 表示检测失败。
pub fn result(
    db: &Connection,
    lease: &Object,
    payload: &Object,
    verdict: Option<&str>,
    now: Num,
) -> Result<(), Failure> {
    let mut reported = payload.clone();
    set(
        &mut reported,
        "classification",
        verdict.map_or(Value::Null, |verdict| json!(verdict)),
    );
    report(db, lease, &reported, now)?;
    if str_field(lease, "kind") != Some("probe") {
        return Ok(());
    }
    let generation = string_of(lease, "generation");
    if let Some(mut item) = window_item(db, &generation)? {
        set(&mut item, "probeEndedAt", now.to_json());
        set(&mut item, "probeResult", json!(verdict.unwrap_or("failed")));
        if let Some(duration) = num_field(payload, "durationMs")
            && (0.0..=60_000.0).contains(&duration.as_f64())
        {
            let seconds = duration.as_f64() / 1000.0;
            set(&mut item, "preparationSeconds", json!(seconds));
            let started = num_field(&item, "preparationStartedAt").map_or(0.0, Num::as_f64);
            if let Some(measured) = num_field(payload, "preparationStartedAt")
                && started - 5.0 <= measured.as_f64()
                && measured.as_f64() <= now.as_f64()
            {
                set(&mut item, "preparationMeasuredAt", measured.to_json());
                set(
                    &mut item,
                    "probeEndedAt",
                    json!(measured.as_f64() + seconds),
                );
            }
        }
        // 旧网关仅掌握许可时间，历史窗口保留缺口，不伪造实际访问时间。
        if truthy(lease.get("sent")) && is_null_or_missing(&item, "firstContactAt") {
            set(&mut item, "incomplete", json!(true));
        }
        write_window(db, &item)?;
    }
    let account = string_of(lease, "account");
    let gateway = string_of(lease, "gateway");
    if let Some(cycle) = open_cycle(db, &account, &gateway)?
        && truthy(lease.get("sent"))
    {
        let mut data = decode_object(&string_of(&cycle, "data"))?;
        let step = last_step(&mut data)?;
        if is_null_or_missing(step, "resultAt") {
            let last = one(
                db,
                "SELECT MAX(ended) AS ended FROM gp_accesses WHERE generation=?",
                vec![text(&generation)],
            )?
            .and_then(|row| row.get("ended").cloned())
            .unwrap_or(Value::Null);
            let observed = if last.is_null() { now.to_json() } else { last };
            set(step, "resultAt", observed.clone());
            set(step, "result", json!(verdict.unwrap_or("failed")));
            // 检测失败的类别（超时、无响应、缺材料、Cookie host 不符等），状态码记在 `status`。
            set(
                step,
                "failure",
                if verdict.is_none() {
                    payload.get("failure").cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                },
            );
            set(
                step,
                "status",
                payload.get("status").cloned().unwrap_or(Value::Null),
            );
            exec(
                db,
                "UPDATE gp_cooling SET data=?,recovered=? WHERE id=?",
                vec![
                    text(&encode(&Value::Object(data))),
                    if verdict == Some("21") {
                        sql_of(&observed)
                    } else {
                        rusqlite::types::Value::Null
                    },
                    sql_of(cycle.get("id").unwrap_or(&Value::Null)),
                ],
            )?;
        }
    }
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
