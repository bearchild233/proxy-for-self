//! 网关池共用账号队列，逐项对应 `gateway_pool_queue.py`。与运行租约共用同一事务。

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::{
    error::Failure,
    settings::{account_id, identifier_list},
    util::{Object, exec, one, rows, str_field, text, truthy},
};

pub const DDL: &str = "
        CREATE TABLE IF NOT EXISTS gp_account_queue (
            account TEXT PRIMARY KEY, position INTEGER NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
            quota_exhausted INTEGER NOT NULL DEFAULT 0, remaining TEXT, reason TEXT);
        CREATE TABLE IF NOT EXISTS gp_queue_revision (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        INSERT OR IGNORE INTO gp_queue_revision VALUES (1,0);
    ";

pub fn install(db: &Connection) -> Result<(), Failure> {
    db.execute_batch(DDL)?;
    // 0.6.0 新增“允许降智使用”：开启的账号不打票，由内核按普通账号调度。
    let has = db
        .prepare("SELECT 1 FROM pragma_table_info('gp_account_queue') WHERE name='degraded_use'")?
        .exists([])?;
    if !has {
        db.execute_batch(
            "ALTER TABLE gp_account_queue ADD COLUMN degraded_use INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    Ok(())
}

/// `identifiers`：账号标识列表，不超过 512 项、格式合法且不重复。
pub fn identifiers(value: &Value) -> Result<Vec<String>, Failure> {
    if !identifier_list(value, 512, account_id) {
        return Failure::invalid("账号列表无效");
    }
    Ok(value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default())
}

fn changed(db: &Connection) -> Result<(), Failure> {
    db.execute_batch("UPDATE gp_queue_revision SET revision=revision+1 WHERE id=1")?;
    Ok(())
}

fn revision(db: &Connection) -> Result<i64, Failure> {
    let row = one(
        db,
        "SELECT revision FROM gp_queue_revision WHERE id=1",
        vec![],
    )?;
    Ok(row
        .and_then(|row| row.get("revision").and_then(Value::as_i64))
        .unwrap_or(0))
}

pub fn register(db: &Connection, accounts: &[String]) -> Result<(), Failure> {
    for account in accounts {
        let inserted = exec(
            db,
            "INSERT OR IGNORE INTO gp_account_queue(account,position) VALUES (?,(SELECT COALESCE(MAX(position),0)+1 FROM gp_account_queue))",
            vec![text(account)],
        )?;
        if inserted > 0 {
            changed(db)?;
        }
    }
    Ok(())
}

pub fn view(db: &Connection) -> Result<Value, Failure> {
    let accounts: Vec<Value> = rows(
        db,
        "SELECT * FROM gp_account_queue ORDER BY position,account",
        vec![],
    )?
    .iter()
    .map(|row| {
        json!({
            "accountId": row.get("account").cloned().unwrap_or(Value::Null),
            "enabled": truthy(row.get("enabled")),
            "degradedUse": truthy(row.get("degraded_use")),
            "quotaExhausted": truthy(row.get("quota_exhausted")),
            "reason": row.get("reason").cloned().unwrap_or(Value::Null),
        })
    })
    .collect();
    Ok(json!({"revision": revision(db)?, "accounts": accounts}))
}

/// `update`：手动排序或切换单个账号。revision 不一致时拒绝，避免覆盖其他页面的修改。
pub fn update(db: &Connection, data: &Object) -> Result<Value, Failure> {
    const FIELDS: [&str; 5] = [
        "revision",
        "accountIds",
        "accountId",
        "enabled",
        "degradedUse",
    ];
    if data.keys().any(|key| !FIELDS.contains(&key.as_str())) {
        return Failure::invalid("账号队列参数无效");
    }
    let requested = data.get("revision").and_then(Value::as_i64);
    if requested.is_none() || requested != Some(revision(db)?) {
        return Failure::invalid("账号队列已变化，请刷新后重试");
    }
    if let Some(accounts) = data.get("accountIds") {
        if data.contains_key("accountId")
            || data.contains_key("enabled")
            || data.contains_key("degradedUse")
        {
            return Failure::invalid("不能同时排序和切换账号");
        }
        let ids = identifiers(accounts)?;
        register(db, &ids)?;
        // 只重排本次提交的账号：沿用它们原有的位置集合，已离开分组的历史账号保留原位。
        let mut slots: Vec<i64> =
            rows(db, "SELECT position,account FROM gp_account_queue", vec![])?
                .iter()
                .filter(|row| {
                    str_field(row, "account")
                        .is_some_and(|account| ids.iter().any(|id| id.as_str() == account))
                })
                .filter_map(|row| row.get("position").and_then(Value::as_i64))
                .collect();
        slots.sort_unstable();
        for (account, position) in ids.iter().zip(slots) {
            exec(
                db,
                "UPDATE gp_account_queue SET position=? WHERE account=?",
                vec![rusqlite::types::Value::Integer(position), text(account)],
            )?;
        }
    } else {
        let account = match data.get("accountId") {
            Some(Value::String(account)) if account_id(account) => account.clone(),
            _ => return Failure::invalid("账号列表无效"),
        };
        // 参与开关与“允许降智使用”可以单独或一起切换。
        let flags = [("enabled", "enabled"), ("degradedUse", "degraded_use")];
        let mut updates = Vec::new();
        for (field, column) in flags {
            match data.get(field) {
                None => {}
                Some(Value::Bool(value)) => updates.push((column, *value)),
                Some(_) => return Failure::invalid("账号开关无效"),
            }
        }
        if updates.is_empty() {
            return Failure::invalid("账号开关无效");
        }
        register(db, std::slice::from_ref(&account))?;
        for (column, value) in updates {
            exec(
                db,
                &format!("UPDATE gp_account_queue SET {column}=? WHERE account=?"),
                vec![
                    rusqlite::types::Value::Integer(i64::from(value)),
                    text(&account),
                ],
            )?;
        }
    }
    changed(db)?;
    view(db)
}

/// `allowed`：账号能否打票——未停用、未允许降智使用且额度未耗尽；未登记的账号视为可用。
pub fn allowed(db: &Connection, account: &str) -> Result<bool, Failure> {
    Ok(
        match one(
            db,
            "SELECT enabled,degraded_use,quota_exhausted FROM gp_account_queue WHERE account=?",
            vec![text(account)],
        )? {
            None => true,
            Some(row) => {
                truthy(row.get("enabled"))
                    && !truthy(row.get("degraded_use"))
                    && !truthy(row.get("quota_exhausted"))
            }
        },
    )
}

/// `remaining`：本轮尚未用完的网关列表；未开始一轮时为 None。
pub fn remaining(db: &Connection, account: &str) -> Result<Option<Vec<String>>, Failure> {
    let row = one(
        db,
        "SELECT remaining FROM gp_account_queue WHERE account=?",
        vec![text(account)],
    )?;
    match row.and_then(|row| {
        row.get("remaining")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }) {
        Some(encoded) => Ok(Some(serde_json::from_str(&encoded)?)),
        None => Ok(None),
    }
}

/// `quota`：记录额度耗尽状态；状态变化才修改队列，首次耗尽时账号移到队尾。
pub fn quota(db: &Connection, account: &str, exhausted: bool) -> Result<(), Failure> {
    register(db, std::slice::from_ref(&account.to_owned()))?;
    let row = one(
        db,
        "SELECT quota_exhausted FROM gp_account_queue WHERE account=?",
        vec![text(account)],
    )?;
    let current = truthy(row.as_ref().and_then(|row| row.get("quota_exhausted")));
    if current != exhausted {
        exec(
            db,
            "UPDATE gp_account_queue SET quota_exhausted=? WHERE account=?",
            vec![
                rusqlite::types::Value::Integer(i64::from(exhausted)),
                text(account),
            ],
        )?;
        if exhausted {
            move_last(db, account, "quota")?;
        } else {
            changed(db)?;
        }
    }
    Ok(())
}

/// `begin`：账号开始一轮探测时记录本轮要走完的网关（已有剩余列表则保持不变）。
pub fn begin(db: &Connection, account: &str, gateways: &[String]) -> Result<(), Failure> {
    register(db, std::slice::from_ref(&account.to_owned()))?;
    exec(
        db,
        "UPDATE gp_account_queue SET remaining=? WHERE account=? AND remaining IS NULL",
        vec![text(&serde_json::to_string(gateways)?), text(account)],
    )?;
    Ok(())
}

/// `move_last`：账号移到队尾，并清空本轮剩余网关。
pub fn move_last(db: &Connection, account: &str, reason: &str) -> Result<(), Failure> {
    exec(
        db,
        "UPDATE gp_account_queue SET position=(SELECT COALESCE(MAX(position),0)+1 FROM gp_account_queue),remaining=NULL,reason=? WHERE account=?",
        vec![text(reason), text(account)],
    )?;
    changed(db)
}

/// `retired`：组合退役后，把它从本轮剩余网关中移除；全部用完则移到队尾。
pub fn retired(db: &Connection, pair: &Object, previous: &str) -> Result<(), Failure> {
    let account = str_field(pair, "accountId").unwrap_or_default();
    let state = str_field(pair, "state").unwrap_or_default();
    let contacted = pair
        .get("firstContactAt")
        .is_some_and(|value| !value.is_null());
    if !allowed(db, account)?
        || !matches!(previous, "ready" | "preparing")
        || !matches!(state, "draining" | "cooling")
        || !contacted
    {
        return Ok(());
    }
    let Some(mut pending) = remaining(db, account)? else {
        return Ok(());
    };
    let gateway = str_field(pair, "gateway").unwrap_or_default();
    let Some(index) = pending.iter().position(|item| item.as_str() == gateway) else {
        return Ok(());
    };
    pending.remove(index);
    if pending.is_empty() {
        move_last(db, account, "gateways")
    } else {
        exec(
            db,
            "UPDATE gp_account_queue SET remaining=? WHERE account=?",
            vec![text(&serde_json::to_string(&pending)?), text(account)],
        )?;
        Ok(())
    }
}
