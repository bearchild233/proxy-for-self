//! 运行状态机：逐项对应 `gateway_pool.py` 的 `GatewayPool.runtime` 及其辅助函数
//! （`availability`、`no_candidate`、`regroup`、`recovery_*`、`reap`、`catalog` 等）。
//!
//! 这是插件内部接口（接受与 Python 相同的 JSON 消息、返回相同的 JSON），钩子在后续步骤接到它上面。
//! Python 抛出 `ValueError` 的地方返回带相同消息的 `Failure::Invalid`；`PermissionError` 对应
//! `Failure::Forbidden`；`KeyError`/`TypeError`（缺字段、类型不符）在宿主处同样按 400 处理，
//! 这里统一返回 `Failure::Invalid`。

use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::{Value, json};

use crate::{
    error::Failure,
    history,
    num::Num,
    policy, queue,
    settings::{account_id, gateway_id},
    store::{Store, effective_settings, stored_settings},
    util::{
        Object, decode_object, decode_value, encode, exec, is_null_or_missing, num_field, one,
        rows, set, sql_of, sql_of_num, str_field, text, truthy,
    },
};

/// 请求缺少必需字段或类型不符（Python 中的 `KeyError`/`TypeError`）。
fn missing(key: &str) -> Failure {
    Failure::Invalid(format!("缺少字段或类型无效：{key}"))
}

fn required<'a>(data: &'a Object, key: &str) -> Result<&'a Value, Failure> {
    data.get(key).ok_or_else(|| missing(key))
}

fn string_of(row: &Object, key: &str) -> String {
    str_field(row, key).unwrap_or_default().to_owned()
}

fn field(row: &Object, key: &str) -> Value {
    row.get(key).cloned().unwrap_or(Value::Null)
}

/// Python 的 `x is True`。
fn is_true(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Bool(true)))
}

/// 非空值（Python 的 `x is not None`，其中缺失与 null 都是 None）。
fn present<'a>(data: &'a Object, key: &str) -> Option<&'a Value> {
    data.get(key).filter(|value| !value.is_null())
}

fn gateway_list(settings: &Object) -> Vec<String> {
    settings
        .get("gateways")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// 模型标识：`[a-z0-9.-]{1,64}`。
fn model_name(value: &str) -> bool {
    (1..=64).contains(&value.chars().count())
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || c == '.' || c == '-')
}

fn model_of(data: &Object) -> Result<String, Failure> {
    match data.get("model") {
        Some(Value::String(model)) if model_name(model) => Ok(model.clone()),
        _ => Failure::invalid("模型标识无效"),
    }
}

/// Python `float(value)`：数字、布尔与数字文本可转换，其他类型视为类型错误。
fn python_float(value: &Value) -> Result<f64, Failure> {
    match value {
        Value::Number(number) => number.as_f64().ok_or_else(|| missing("deadline")),
        Value::Bool(flag) => Ok(f64::from(u8::from(*flag))),
        Value::String(text) => text
            .trim()
            .parse::<f64>()
            .map_err(|_| Failure::Invalid(format!("could not convert string to float: '{text}'"))),
        _ => Err(missing("deadline")),
    }
}

/// 把秒数写成人能读的中文时长，用于状态说明（对应 `span`）。
fn span(seconds: Num) -> String {
    let whole = match seconds {
        Num::Int(value) => value,
        Num::Float(value) => value.trunc() as i64,
    }
    .max(0);
    if whole < 90 {
        format!("{whole} 秒")
    } else if whole < 5400 {
        // Python 的 `round` 是银行家舍入。
        format!("{} 分钟", (whole as f64 / 60.0).round_ties_even() as i64)
    } else {
        format!("{:.1} 小时", whole as f64 / 3600.0)
    }
}

/// 账号当前不能被调度的原因。
struct Reason {
    code: &'static str,
    message: String,
    next_recovery_at: Option<Num>,
}

impl Reason {
    fn to_json(&self) -> Value {
        let mut object = Object::new();
        set(&mut object, "code", json!(self.code));
        set(&mut object, "message", json!(self.message));
        if let Some(next) = self.next_recovery_at {
            set(&mut object, "nextRecoveryAt", next.to_json());
        }
        Value::Object(object)
    }
}

/// 账号当前能否被调度；`reason` 仅在不可用时有值。
struct Availability {
    usable: bool,
    live: bool,
    reason: Option<Reason>,
}

impl Availability {
    fn open(live: bool) -> Self {
        Self {
            usable: true,
            live,
            reason: None,
        }
    }

    fn blocked(reason: Reason) -> Self {
        Self {
            usable: false,
            live: false,
            reason: Some(reason),
        }
    }
}

/// 读取组合；不存在时为 unknown（与 Python `pair()` 的默认值一致）。
fn load_pair(db: &Connection, account: &str, gateway: &str) -> Result<Object, Failure> {
    match one(
        db,
        "SELECT data FROM pairs WHERE account=? AND gateway=?",
        vec![text(account), text(gateway)],
    )? {
        Some(row) => decode_object(&string_of(&row, "data")),
        None => {
            let mut pair = Object::new();
            set(&mut pair, "accountId", json!(account));
            set(&mut pair, "gateway", json!(gateway));
            set(&mut pair, "state", json!("unknown"));
            set(&mut pair, "inFlight", json!(0));
            Ok(pair)
        }
    }
}

/// 账号的全部组合（按存储顺序）。
fn account_pairs(db: &Connection, account: &str) -> Result<Vec<Object>, Failure> {
    rows(
        db,
        "SELECT data FROM pairs WHERE account=?",
        vec![text(account)],
    )?
    .iter()
    .map(|row| decode_object(&string_of(row, "data")))
    .collect()
}

fn add_observation(
    db: &Connection,
    now: Num,
    account: &str,
    gateway: &str,
    sample: &Value,
) -> Result<(), Failure> {
    exec(
        db,
        "INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)",
        vec![
            sql_of_num(now),
            text(account),
            text(gateway),
            text(&encode(sample)),
        ],
    )?;
    Ok(())
}

fn add_lease(
    db: &Connection,
    values: (&str, &str, &str, &str, &str, SqlValue),
) -> Result<(), Failure> {
    let (id, account, gateway, generation, kind, expires) = values;
    exec(
        db,
        "INSERT INTO leases VALUES (?,?,?,?,?,?,0,?)",
        vec![
            text(id),
            text(account),
            text(gateway),
            text(generation),
            text(kind),
            expires,
            text("{}"),
        ],
    )?;
    Ok(())
}

/// 配套材料：只允许 state、cookie、expires 三项，文本无换行，到期时间在一年之内。
fn check_material(material: &Value, now: Num) -> Result<(), Failure> {
    let invalid = || Failure::Invalid("配套材料无效".to_owned());
    let Value::Object(object) = material else {
        return Err(invalid());
    };
    if object.len() != 3 {
        return Err(invalid());
    }
    for key in ["state", "cookie"] {
        match object.get(key) {
            Some(Value::String(value))
                if (1..=16384).contains(&value.chars().count())
                    && !value.contains(['\r', '\n']) => {}
            _ => return Err(invalid()),
        }
    }
    let expires = match object.get("expires") {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::Bool(flag)) => Some(f64::from(u8::from(*flag))),
        _ => None,
    }
    .ok_or_else(invalid)?;
    let now = now.as_f64();
    if !(now < expires && expires <= now + 86400.0 * 365.0) {
        return Err(invalid());
    }
    Ok(())
}

/// 保存网关传来的池服务目录；未传时使用 5 分钟内的上一份。
fn catalog(
    db: &Connection,
    account: &str,
    supplied: Option<&Value>,
    now: Num,
) -> Result<Option<Vec<Object>>, Failure> {
    if let Some(supplied) = supplied {
        let invalid = || Failure::Invalid("网关目录无效".to_owned());
        let Value::Array(items) = supplied else {
            return Err(invalid());
        };
        if items.len() > 512 {
            return Err(invalid());
        }
        let mut entries = Vec::new();
        for item in items {
            let Value::Object(entry) = item else {
                return Err(invalid());
            };
            let gateway_ok =
                matches!(entry.get("gateway"), Some(Value::String(gateway)) if gateway_id(gateway));
            let valid_ok = num_field(entry, "validForS")
                .is_some_and(|valid| (0.0..=86400.0).contains(&valid.as_f64()));
            let country_ok = match entry.get("country") {
                None | Some(Value::Null) => true,
                Some(Value::String(country)) => {
                    country.chars().count() == 2 && country.chars().all(|c| c.is_ascii_uppercase())
                }
                Some(_) => false,
            };
            if !gateway_ok || !valid_ok || !country_ok {
                return Err(invalid());
            }
            let mut normalized = Object::new();
            set(&mut normalized, "gateway", field(entry, "gateway"));
            set(&mut normalized, "validForS", field(entry, "validForS"));
            set(&mut normalized, "country", field(entry, "country"));
            entries.push(normalized);
        }
        exec(
            db,
            "INSERT OR REPLACE INTO catalogs VALUES (?,?,?)",
            vec![
                text(account),
                sql_of_num(now),
                text(&encode(&Value::Array(
                    entries.iter().cloned().map(Value::Object).collect(),
                ))),
            ],
        )?;
        return Ok(Some(entries));
    }
    let Some(row) = one(
        db,
        "SELECT at, data FROM catalogs WHERE account=?",
        vec![text(account)],
    )?
    else {
        return Ok(None);
    };
    let at = num_field(&row, "at").unwrap_or(Num::Int(0));
    if now.minus(at).le(Num::Int(300)) {
        let Value::Array(items) = decode_value(&string_of(&row, "data"))? else {
            return Err(Failure::Internal("存储的目录不是列表".to_owned()));
        };
        return Ok(Some(
            items
                .into_iter()
                .filter_map(|item| match item {
                    Value::Object(object) => Some(object),
                    _ => None,
                })
                .collect(),
        ));
    }
    Ok(None)
}

/// 候选网关：默认跟随池服务目录；设置了限定网关时只在其中挑选。目录不可用时退回限定列表。
fn candidate_gateways(catalog: Option<&Vec<Object>>, settings: &Object) -> Vec<String> {
    let limited = gateway_list(settings);
    match catalog {
        Some(entries) => {
            let all = entries.iter().map(|entry| string_of(entry, "gateway"));
            if limited.is_empty() {
                all.collect()
            } else {
                all.filter(|gateway| limited.contains(gateway)).collect()
            }
        }
        None => limited,
    }
}

impl Store {
    /// 运行状态机入口：与 `GatewayPool.runtime(message)` 接受并返回相同的 JSON。
    pub fn runtime(&self, message: &Value) -> Result<Value, Failure> {
        let Value::Object(data) = message else {
            return Failure::invalid("本机消息必须为对象");
        };
        let action = match data.get("action") {
            Some(Value::String(action)) => Some(action.as_str()),
            _ => None,
        };
        // 插件进程在运行就是启用（禁用由监督停止进程），生产代码从不调用 `set_enabled`，`config` 恒返回启用。
        // 停用分支只为与 Python 实现的对照（`disabled-plugin-inflight` 等序列）保留；停用后仍允许旧在途请求归还租约。
        if action == Some("finish") {
            return self.write(|db| self.finish_lease(db, data));
        }
        if action == Some("config") && !self.enabled() {
            self.write(|db| {
                history::record(db, |db| history::interrupt(db, self.now_num(), "disabled"))
            })?;
            return Ok(json!({"enabled": false}));
        }
        if !self.enabled() {
            return Err(Failure::Forbidden("网关池插件未启用".to_owned()));
        }
        let now = self.now_num();
        if !data.contains_key("action") {
            return Err(missing("action"));
        }
        let action = action.unwrap_or_default();
        self.write(|db| self.dispatch_action(db, data, action, now))
    }

    /// 在途请求归还租约（`finish`）：不读取策略，只用已保存的设置完成探测退役。
    fn finish_lease(&self, db: &Connection, data: &Object) -> Result<Value, Failure> {
        let Some(lease) = one(
            db,
            "SELECT * FROM leases WHERE id=? AND account=?",
            vec![
                sql_of(required(data, "leaseId")?),
                sql_of(required(data, "accountId")?),
            ],
        )?
        else {
            return Ok(json!({"accepted": false}));
        };
        history::record(db, |db| {
            history::finish(db, &lease, data, self.now_num(), false)
        })?;
        let mut pair = load_pair(
            db,
            &string_of(&lease, "account"),
            &string_of(&lease, "gateway"),
        )?;
        decrement_in_flight(&mut pair);
        let sent = truthy(lease.get("sent"));
        if sent {
            let last = num_field(&pair, "lastContactAt").unwrap_or(Num::Int(0));
            set(
                &mut pair,
                "lastContactAt",
                last.max(self.now_num()).to_json(),
            );
        }
        if str_field(&lease, "kind") == Some("probe")
            && str_field(&pair, "state") == Some("preparing")
            && pair.get("generation") == lease.get("generation")
        {
            // 停用或结果上报中断时，仍完成探测退役，不留下永久 preparing。
            let config = stored_settings(db)?.unwrap_or_default();
            let rest = match num_field(&pair, "probeRestSeconds") {
                Some(waited) => waited.times(Num::Int(2)),
                None => match num_field(&pair, "learnedRestSeconds") {
                    Some(learned) if learned.truthy() => learned,
                    _ => num_field(&config, "initialRestSeconds").unwrap_or(Num::Int(3600)),
                },
            };
            let maximum = num_field(&config, "maximumRestSeconds").unwrap_or(Num::Int(86400));
            set(&mut pair, "state", json!("draining"));
            set(&mut pair, "restSeconds", maximum.min(rest).to_json());
            set(&mut pair, "retiredReason", json!("probe_interrupted"));
        }
        policy::settle(&mut pair);
        self.save(db, &pair)?;
        exec(
            db,
            "DELETE FROM leases WHERE id=?",
            vec![text(&string_of(&lease, "id"))],
        )?;
        Ok(json!({"accepted": true}))
    }

    /// 请求租约超时后保守地从超时点重新静置；重启不能把未完成访问当成已休息。
    fn reap(&self, db: &Connection, settings: &Object, now: Num) -> Result<(), Failure> {
        for lease in rows(
            db,
            "SELECT * FROM leases WHERE expires<=?",
            vec![sql_of_num(now)],
        )? {
            history::record(db, |db| {
                history::finish(db, &lease, &Object::new(), now, true)
            })?;
            let mut pair = load_pair(
                db,
                &string_of(&lease, "account"),
                &string_of(&lease, "gateway"),
            )?;
            decrement_in_flight(&mut pair);
            if truthy(lease.get("sent")) {
                let last = num_field(&pair, "lastContactAt").unwrap_or(Num::Int(0));
                let expires = num_field(&lease, "expires").unwrap_or(Num::Int(0));
                set(&mut pair, "lastContactAt", last.max(expires).to_json());
            }
            if str_field(&lease, "kind") == Some("probe")
                && pair.get("generation") == lease.get("generation")
            {
                let rest = policy::initial(settings, num_field(&pair, "learnedRestSeconds"));
                set(&mut pair, "state", json!("draining"));
                set(&mut pair, "restSeconds", rest.to_json());
                set(&mut pair, "retiredReason", json!("probe_interrupted"));
            }
            policy::settle(&mut pair);
            self.save(db, &pair)?;
            exec(
                db,
                "DELETE FROM leases WHERE id=?",
                vec![text(&string_of(&lease, "id"))],
            )?;
        }
        Ok(())
    }

    fn dispatch_action(
        &self,
        db: &Connection,
        data: &Object,
        action: &str,
        now: Num,
    ) -> Result<Value, Failure> {
        let settings = effective_settings(db, true)?;
        self.reap(db, &settings, now)?;
        // 接管事实与原租约同寿命；Python 回退归还租约后留下的记录也在此清理。
        db.execute("DELETE FROM gp_hook_leases WHERE NOT EXISTS (SELECT 1 FROM leases WHERE leases.id=gp_hook_leases.id)", [])?;
        exec(
            db,
            "DELETE FROM materials WHERE expires<=?",
            vec![sql_of_num(now)],
        )?;
        // 其他模型的票只属于当前可用窗口；组合退役或换代后立即作废。
        exec(
            db,
            "DELETE FROM model_tickets WHERE expires<=? OR NOT EXISTS (SELECT 1 FROM pairs p WHERE p.account=model_tickets.account AND json_extract(p.data,'$.generation')=model_tickets.generation AND json_extract(p.data,'$.state')='ready')",
            vec![sql_of_num(now)],
        )?;
        if action == "config" {
            let mut result = settings;
            set(&mut result, "enabled", json!(true));
            return Ok(Value::Object(result));
        }
        if action == "active" {
            return self.active(db, data, &settings, now);
        }
        let account = match required(data, "accountId")? {
            Value::String(account) if account_id(account) => account.clone(),
            _ => return Failure::invalid("账号标识无效"),
        };
        match action {
            "quota" => {
                let Some(Value::Bool(exhausted)) = data.get("exhausted") else {
                    return Failure::invalid("额度状态无效");
                };
                queue::quota(db, &account, *exhausted)?;
                if *exhausted {
                    self.retire_account(db, &account, &settings)?;
                }
                return Ok(json!({"accepted": true}));
            }
            "recoveryCheck" => {
                let check = self.recovery_candidate(db, &account, &settings, now)?;
                return Ok(json!({"check": check}));
            }
            "recoveryResult" => return self.recovery_result(db, &settings, &account, data, now),
            "regroup" => return self.regroup(db, &settings, &account, data, now),
            "regroupResult" => return regroup_result(db, &account, data, now),
            _ => {}
        }
        if matches!(action, "plan" | "claim" | "reserve") && !queue::allowed(db, &account)? {
            let info = self.availability(db, &account, &settings, now)?;
            let mut blocked = Object::new();
            if let Some(reason) = &info.reason {
                set(&mut blocked, "code", json!(reason.code));
                set(&mut blocked, "message", json!(reason.message));
            }
            return Ok(json!({"claimed": false, "reserved": false, "blocked": blocked}));
        }
        match action {
            "material" => {
                let generation = sql_of(required(data, "generation")?);
                let row = one(
                    db,
                    "SELECT data FROM materials WHERE account=? AND generation=? AND expires>?",
                    vec![text(&account), generation, sql_of_num(now)],
                )?;
                return match row {
                    Some(row) => decode_value(&string_of(&row, "data")),
                    None => Ok(Value::Null),
                };
            }
            "ticket" => {
                let generation = sql_of(required(data, "generation")?);
                let model = model_of(data)?;
                let row = one(
                    db,
                    "SELECT data FROM model_tickets WHERE account=? AND generation=? AND model=? AND expires>?",
                    vec![text(&account), generation, text(&model), sql_of_num(now)],
                )?;
                return match row {
                    Some(row) => decode_value(&string_of(&row, "data")),
                    None => Ok(Value::Null),
                };
            }
            "plan" | "claim" => {
                return self.plan_or_claim(db, data, action, &account, &settings, now);
            }
            "reserve" => return self.reserve(db, data, &account, now),
            _ => {}
        }
        self.lease_action(db, data, action, &account, &settings, now)
    }

    /// 无候选清单的旧调用返回持有窗口的账号；带候选清单时按账号队列排序并说明被挡原因。
    fn active(
        &self,
        db: &Connection,
        data: &Object,
        settings: &Object,
        now: Num,
    ) -> Result<Value, Failure> {
        if let Some(candidates) = data.get("candidates") {
            let invalid = || Failure::Invalid("调度候选无效".to_owned());
            let Value::Array(list) = candidates else {
                return Err(invalid());
            };
            let mut exhausted = Vec::new();
            for candidate in list {
                match candidate.as_object().and_then(|c| c.get("exhausted")) {
                    Some(Value::Bool(flag)) => exhausted.push(*flag),
                    _ => return Err(invalid()),
                }
            }
            let ids_value = Value::Array(
                list.iter()
                    .map(|candidate| {
                        candidate
                            .as_object()
                            .and_then(|c| c.get("id"))
                            .cloned()
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            );
            let ids = queue::identifiers(&ids_value)?;
            queue::register(db, &ids)?;
            for (id, flag) in ids.iter().zip(&exhausted) {
                queue::quota(db, id, *flag)?;
                if !queue::allowed(db, id)? {
                    self.retire_account(db, id, settings)?;
                }
            }
            let mut info = Vec::new();
            for id in &ids {
                info.push((id.clone(), self.availability(db, id, settings, now)?));
            }
            let view = queue::view(db)?;
            let order: Vec<String> = view["accounts"]
                .as_array()
                .map(|accounts| {
                    accounts
                        .iter()
                        .filter_map(|row| row["accountId"].as_str())
                        .filter(|id| info.iter().any(|(known, _)| known == id))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let lookup = |id: &str| info.iter().find(|(known, _)| known == id).map(|(_, a)| a);
            // 持有窗口的账号先用（同一把窗口继续服务），其余按优先级；优先级不是排队，任何位置都可取。
            let live: Vec<&String> = order
                .iter()
                .filter(|id| lookup(id).is_some_and(|a| a.usable && a.live))
                .collect();
            let idle = order
                .iter()
                .filter(|id| lookup(id).is_some_and(|a| a.usable && !a.live));
            let ordered: Vec<&String> = live.iter().copied().chain(idle).collect();
            let blocked: Vec<Value> = order
                .iter()
                .filter_map(|id| {
                    let reason = lookup(id).filter(|a| !a.usable)?.reason.as_ref()?;
                    let mut entry = Object::new();
                    set(&mut entry, "accountId", json!(id));
                    if let Value::Object(reason) = reason.to_json() {
                        for (key, value) in reason {
                            set(&mut entry, &key, value);
                        }
                    }
                    Some(Value::Object(entry))
                })
                .collect();
            return Ok(
                json!({"accounts": ordered, "live": live, "blocked": blocked, "queue": true}),
            );
        }
        // 无候选清单的旧调用：先返回持有窗口的账号（最新在前），再返回正在找票的账号。
        let mut all = Vec::new();
        for row in rows(db, "SELECT data FROM pairs", vec![])? {
            all.push(decode_object(&string_of(&row, "data"))?);
        }
        let verified = |pair: &Object| num_field(pair, "verifiedAt").map_or(0.0, Num::as_f64);
        let mut ready: Vec<&Object> = all
            .iter()
            .filter(|pair| str_field(pair, "state") == Some("ready"))
            .collect();
        ready.sort_by(|a, b| verified(b).total_cmp(&verified(a)));
        let preparing = all
            .iter()
            .filter(|pair| str_field(pair, "state") == Some("preparing"));
        let mut accounts: Vec<String> = Vec::new();
        for pair in ready.into_iter().chain(preparing) {
            let account = string_of(pair, "accountId");
            if !accounts.contains(&account) {
                accounts.push(account);
            }
        }
        Ok(json!({"accounts": accounts}))
    }

    /// 账号当前能否被网关池调度，以及不能时的原因。只读已有目录和冷却事实，不因选号访问上游。
    /// 优先级不是排队：任何可探测或持有窗口的账号都可被选中，不要求排在队首。
    fn availability(
        &self,
        db: &Connection,
        account: &str,
        settings: &Object,
        now: Num,
    ) -> Result<Availability, Failure> {
        if let Some(row) = one(
            db,
            "SELECT enabled,quota_exhausted FROM gp_account_queue WHERE account=?",
            vec![text(account)],
        )? {
            if !truthy(row.get("enabled")) {
                return Ok(Availability::blocked(Reason {
                    code: "queue_disabled",
                    message: "该账号已在网关池队列中停用".to_owned(),
                    next_recovery_at: None,
                }));
            }
            if truthy(row.get("quota_exhausted")) {
                return Ok(Availability::blocked(Reason {
                    code: "quota_exhausted",
                    message: "该账号额度已耗尽，暂不参与网关池调度".to_owned(),
                    next_recovery_at: None,
                }));
            }
        }
        let mut pairs = account_pairs(db, account)?;
        let ready = pairs
            .iter()
            .find(|pair| str_field(pair, "state") == Some("ready"))
            .cloned();
        if let Some(mut ready) = ready {
            let due = policy::rotation_due(&ready, settings, now, true);
            let gateway = string_of(&ready, "gateway");
            if due
                && queue::remaining(db, account)?.as_deref() == Some(std::slice::from_ref(&gateway))
            {
                // 本轮网关轮完且窗口到期：账号沉到最后，之后重新开始新一轮。
                policy::retire(&mut ready, settings, false, false);
                policy::settle(&mut ready);
                self.save(db, &ready)?;
                pairs = account_pairs(db, account)?;
            } else {
                return Ok(Availability::open(!due));
            }
        }
        if pairs
            .iter()
            .any(|pair| str_field(pair, "state") == Some("preparing"))
        {
            return Ok(Availability::open(true));
        }
        match self.no_candidate(db, account, &pairs, settings, now)? {
            Some(reason) => Ok(Availability::blocked(reason)),
            None => Ok(Availability::open(false)),
        }
    }

    /// 没有任何可探测网关时的原因；有可探测网关返回 None。
    fn no_candidate(
        &self,
        db: &Connection,
        account: &str,
        pairs: &[Object],
        settings: &Object,
        now: Num,
    ) -> Result<Option<Reason>, Failure> {
        let catalog = catalog(db, account, None, now)?;
        let gateways = candidate_gateways(catalog.as_ref(), settings);
        if gateways.is_empty() {
            if catalog.is_none() && gateway_list(settings).is_empty() {
                return Ok(None);
            }
            let message = if gateway_list(settings).is_empty() {
                "池服务目录里没有这个账号可用的网关"
            } else {
                "限定网关与池服务目录没有交集"
            };
            return Ok(Some(Reason {
                code: "catalog_empty",
                message: message.to_owned(),
                next_recovery_at: None,
            }));
        }
        let mut resting: Vec<Num> = Vec::new();
        for gateway in &gateways {
            // `known` 是以网关为键的字典：同名组合以后出现的为准。
            let pair = pairs
                .iter()
                .rev()
                .find(|pair| str_field(pair, "gateway") == Some(gateway.as_str()));
            let state = pair
                .and_then(|pair| str_field(pair, "state"))
                .unwrap_or("unknown");
            let rest_until = pair.and_then(|pair| num_field(pair, "restUntil"));
            if state == "unknown"
                || (state == "cooling" && rest_until.unwrap_or(Num::Int(0)).le(now))
            {
                return Ok(None);
            }
            if state == "cooling" {
                resting.push(rest_until.unwrap_or(now));
            }
        }
        let Some(earliest) = resting.iter().copied().reduce(Num::min) else {
            return Ok(None);
        };
        Ok(Some(Reason {
            code: "all_resting",
            message: format!(
                "该账号可用的 {} 个网关全部在静置，最早 {}后可重新检测",
                gateways.len(),
                span(earliest.minus(now))
            ),
            next_recovery_at: Some(earliest),
        }))
    }

    fn plan_or_claim(
        &self,
        db: &Connection,
        data: &Object,
        action: &str,
        account: &str,
        settings: &Object,
        now: Num,
    ) -> Result<Value, Failure> {
        queue::register(db, &[account.to_owned()])?;
        let turn = one(
            db,
            "SELECT position FROM gp_account_queue WHERE account=?",
            vec![text(account)],
        )?
        .and_then(|row| row.get("position").cloned())
        .unwrap_or(Value::Null);
        // 本 Key 范围内还有别的可用账号时，账号被沉底后才让位；没有退路就继续用本账号。
        let alternatives = match data.get("alternatives") {
            Some(value) if truthy(Some(value)) => queue::identifiers(value)?,
            _ => Vec::new(),
        };
        // 每个候选都要判断：`availability` 可能退役已到期的窗口，不能因为找到一个就跳过其余。
        let mut usable_alternatives = false;
        for other in alternatives {
            if other != account
                && queue::allowed(db, &other)?
                && self.availability(db, &other, settings, now)?.usable
            {
                usable_alternatives = true;
            }
        }
        // 普通账号也是退路：Key 里除了降智账号还有别的账号时，让位后由调度器改用它们。
        let has_exit = usable_alternatives || is_true(data.get("ordinary"));
        if let Some(queue_turn) = present(data, "queueTurn")
            && !same_turn(queue_turn, &turn)
            && has_exit
        {
            return Ok(json!({"claimed": false, "yieldAccount": true}));
        }
        let demand = is_true(data.get("demand"));
        let supplied = present(data, "catalog");
        let catalog = catalog(db, account, supplied, now)?;
        // 默认跟随池服务目录；设置了限定网关时只在其中挑选。目录不可用时退回限定列表。
        let gateways = candidate_gateways(catalog.as_ref(), settings);
        let entries: std::collections::BTreeMap<String, &Object> = catalog
            .iter()
            .flatten()
            .map(|entry| (string_of(entry, "gateway"), entry))
            .collect();
        let mut pairs = Vec::new();
        for gateway in &gateways {
            pairs.push(load_pair(db, account, gateway)?);
        }
        // 正在服务的组合即使暂时不在目录中，也继续服务到窗口或 Cookie 到期。
        let mut ready = account_pairs(db, account)?
            .into_iter()
            .find(|pair| str_field(pair, "state") == Some("ready"));
        let mut due = ready
            .as_ref()
            .is_some_and(|ready| policy::rotation_due(ready, settings, now, demand));
        let mut pending = queue::remaining(db, account)?;
        if due
            && let Some(current) = ready.as_mut()
            && pending.as_deref() == Some(std::slice::from_ref(&string_of(current, "gateway")))
        {
            policy::retire(current, settings, false, false);
            policy::settle(current);
            self.save(db, current)?;
            if has_exit {
                return Ok(json!({"claimed": false, "yieldAccount": true}));
            }
            // 没有退路：账号沉底后立即开始新一轮。
            ready = None;
            due = false;
            pending = queue::remaining(db, account)?;
        }
        let mut candidates: Vec<(bool, policy::CandidateKey, Object)> = pairs
            .iter()
            .filter(|pair| {
                let state = str_field(pair, "state");
                state == Some("unknown")
                    || (state == Some("cooling")
                        && num_field(pair, "restUntil").unwrap_or(Num::Int(0)).le(now))
            })
            .map(|pair| {
                let gateway = string_of(pair, "gateway");
                let outside = pending
                    .as_ref()
                    .is_some_and(|pending| !pending.contains(&gateway));
                (
                    outside,
                    policy::candidate_order(pair, entries.get(&gateway).copied(), settings),
                    pair.clone(),
                )
            })
            .collect();
        candidates.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        let fresh = account_pairs(db, account)?;
        let preparing = fresh
            .iter()
            .any(|pair| str_field(pair, "state") == Some("preparing"));
        let probe = if (demand || str_field(settings, "mode") == Some("continuous"))
            && (ready.is_none() || due)
        {
            candidates.into_iter().next().map(|(_, _, pair)| pair)
        } else {
            None
        };
        if action == "plan" {
            let mut plan = Object::new();
            set(
                &mut plan,
                "ready",
                ready.clone().map_or(Value::Null, Value::Object),
            );
            set(
                &mut plan,
                "probe",
                probe.clone().map_or(Value::Null, Value::Object),
            );
            set(&mut plan, "rotationDue", json!(due));
            set(&mut plan, "preparing", json!(preparing));
            set(&mut plan, "queueTurn", turn);
            if ready.is_none() && probe.is_none() && !preparing {
                let reason = self.no_candidate(db, account, &fresh, settings, now)?;
                set(
                    &mut plan,
                    "blocked",
                    reason.map_or(Value::Null, |reason| reason.to_json()),
                );
            }
            return Ok(Value::Object(plan));
        }
        let Some(mut probe) = probe else {
            return Ok(json!({"claimed": false}));
        };
        if preparing {
            return Ok(json!({"claimed": false}));
        }
        let generation = (self.ids)();
        queue::begin(db, account, &gateways)?;
        let last = (!is_null_or_missing(&probe, "lastContactAt"))
            .then(|| num_field(&probe, "lastContactAt"))
            .flatten();
        let planned = if last.is_some() {
            field(&probe, "restSeconds")
        } else {
            Value::Null
        };
        let idle = last.map_or(Value::Null, |last| {
            Num::Int(0).max(now.minus(last)).to_json()
        });
        let in_flight = num_field(&probe, "inFlight").ok_or_else(|| missing("inFlight"))?;
        set(&mut probe, "generation", json!(generation));
        set(&mut probe, "state", json!("preparing"));
        set(&mut probe, "probeRestSeconds", planned);
        set(&mut probe, "observedIdleSeconds", idle);
        set(&mut probe, "preparationStartedAt", now.to_json());
        set(&mut probe, "firstContactAt", Value::Null);
        set(
            &mut probe,
            "inFlight",
            in_flight.plus(Num::Int(1)).to_json(),
        );
        history::record(db, |db| history::begin(db, &probe, &gateways, now))?;
        self.save(db, &probe)?;
        add_lease(
            db,
            (
                &generation,
                account,
                &string_of(&probe, "gateway"),
                &generation,
                "probe",
                sql_of_num(now.plus(Num::Int(60))),
            ),
        )?;
        Ok(json!({"claimed": true, "leaseId": generation, "pair": probe, "queueTurn": turn}))
    }

    fn reserve(
        &self,
        db: &Connection,
        data: &Object,
        account: &str,
        now: Num,
    ) -> Result<Value, Failure> {
        let gateway = required(data, "gateway")?;
        // 网关标识不是字符串时不会匹配任何组合（Python 里等同于查不到记录）。
        let mut pair = match gateway {
            Value::String(gateway) => load_pair(db, account, gateway)?,
            _ => return Ok(json!({"reserved": false})),
        };
        if str_field(&pair, "state") != Some("ready")
            || pair.get("generation") != Some(required(data, "generation")?)
        {
            return Ok(json!({"reserved": false}));
        }
        let lease_id = (self.ids)();
        let expires = python_float(required(data, "deadline")?)?;
        let now_f = now.as_f64();
        if !(now_f < expires && expires <= now_f + 86400.0) {
            return Failure::invalid("请求截止时间无效");
        }
        let in_flight = num_field(&pair, "inFlight").ok_or_else(|| missing("inFlight"))?;
        set(&mut pair, "inFlight", in_flight.plus(Num::Int(1)).to_json());
        self.save(db, &pair)?;
        add_lease(
            db,
            (
                &lease_id,
                account,
                &string_of(&pair, "gateway"),
                &string_of(&pair, "generation"),
                "business",
                SqlValue::Real(expires),
            ),
        )?;
        Ok(json!({"reserved": true, "leaseId": lease_id}))
    }

    /// 针对已有租约的动作：sent、probeAttempt、ticketResult、probeResult、businessResult、invalidate。
    fn lease_action(
        &self,
        db: &Connection,
        data: &Object,
        action: &str,
        account: &str,
        settings: &Object,
        now: Num,
    ) -> Result<Value, Failure> {
        let Some(lease) = one(
            db,
            "SELECT * FROM leases WHERE id=? AND account=?",
            vec![sql_of(required(data, "leaseId")?), text(account)],
        )?
        else {
            return Ok(json!({"accepted": false}));
        };
        let kind = string_of(&lease, "kind");
        let sent = truthy(lease.get("sent"));
        let lease_gateway = string_of(&lease, "gateway");
        let mut pair = load_pair(db, account, &lease_gateway)?;
        let same_generation = |pair: &Object| pair.get("generation") == lease.get("generation");
        let reject = || Ok(json!({"accepted": false}));
        match action {
            "sent" => {
                if !queue::allowed(db, account)? {
                    return reject();
                }
                if kind == "probe"
                    && (str_field(&pair, "state") != Some("preparing") || !same_generation(&pair))
                {
                    return reject();
                }
                // 保留尚未发送的请求：已切换时重新选择，不能凭旧预约发送到退役组合。
                if kind == "business"
                    && (str_field(&pair, "state") != Some("ready") || !same_generation(&pair))
                {
                    return reject();
                }
                if !sent && kind == "probe" {
                    let previous = num_field(&pair, "lastContactAt");
                    set(&mut pair, "firstContactAt", now.to_json());
                    set(
                        &mut pair,
                        "previousContactAt",
                        previous.map_or(Value::Null, Num::to_json),
                    );
                    set(
                        &mut pair,
                        "observedIdleSeconds",
                        previous.map_or(Value::Null, |previous| {
                            Num::Int(0).max(now.minus(previous)).to_json()
                        }),
                    );
                }
                exec(
                    db,
                    "UPDATE leases SET sent=1 WHERE id=?",
                    vec![text(&string_of(&lease, "id"))],
                )?;
                history::record(db, |db| history::sent(db, &lease, data, now))?;
                set(&mut pair, "lastContactAt", now.to_json());
            }
            "probeAttempt" => {
                let phase_ok = matches!(data.get("phase"), Some(Value::String(phase)) if matches!(phase.as_str(), "bootstrap" | "replay"));
                let retry_ok = match data.get("retry") {
                    Some(Value::Number(number)) => {
                        matches!(number.as_f64(), Some(value) if value == 0.0 || value == 1.0)
                    }
                    Some(Value::Bool(_)) => true,
                    _ => false,
                };
                if kind != "probe" || !sent || !phase_ok || !retry_ok {
                    return Failure::invalid("探测访问记录无效");
                }
                let sample = json!({
                    "generation": field(&lease, "generation"),
                    "source": "probe-attempt",
                    "phase": field(data, "phase"),
                    "retry": field(data, "retry"),
                    "status": field(data, "status"),
                    "stateReturned": is_true(data.get("stateReturned")),
                    "classification": null,
                    "durationMs": field(data, "durationMs"),
                    "plannedRestSeconds": field(&pair, "probeRestSeconds"),
                    "observedIdleSeconds": field(&pair, "observedIdleSeconds"),
                    "firstContactAt": field(&pair, "firstContactAt"),
                    "previousContactAt": field(&pair, "previousContactAt"),
                    "lastContactAt": field(&pair, "lastContactAt"),
                    "completedAt": now.to_json(),
                });
                add_observation(db, now, account, &lease_gateway, &sample)?;
                history::record(db, |db| history::report(db, &lease, data, now))?;
            }
            "ticketResult" => {
                // 窗口内其他模型拿票：只记录事实并保存 state，不做 21/29 判定。
                let model = model_of(data)?;
                if kind != "business" || !sent {
                    return Failure::invalid("拿票记录无效");
                }
                let sample = json!({
                    "generation": field(&lease, "generation"),
                    "source": "ticket",
                    "model": model,
                    "status": field(data, "status"),
                    "stateReturned": is_true(data.get("stateReturned")),
                    "classification": null,
                    "durationMs": field(data, "durationMs"),
                    "firstContactAt": field(&pair, "firstContactAt"),
                    "lastContactAt": field(&pair, "lastContactAt"),
                    "completedAt": now.to_json(),
                });
                add_observation(db, now, account, &lease_gateway, &sample)?;
                history::record(db, |db| history::report(db, &lease, data, now))?;
                if let Some(material) = present(data, "material")
                    && str_field(&pair, "state") == Some("ready")
                    && same_generation(&pair)
                {
                    check_material(material, now)?;
                    exec(
                        db,
                        "INSERT OR REPLACE INTO model_tickets VALUES (?,?,?,?,?)",
                        vec![
                            text(account),
                            text(&string_of(&lease, "generation")),
                            text(&model),
                            sql_of(material.get("expires").unwrap_or(&Value::Null)),
                            text(&encode(material)),
                        ],
                    )?;
                }
            }
            "probeResult" | "businessResult" => {
                return self.result_action(db, data, action, account, settings, now, &lease, pair);
            }
            "invalidate" => policy::retire(&mut pair, settings, false, false),
            _ => return Failure::invalid("运行操作无效"),
        }
        policy::settle(&mut pair);
        self.save(db, &pair)?;
        Ok(json!({"accepted": true}))
    }

    /// 探测结果与业务结果：判定 21/29，探测通过时发布新窗口并退役旧窗口。
    #[allow(clippy::too_many_arguments)]
    fn result_action(
        &self,
        db: &Connection,
        data: &Object,
        action: &str,
        account: &str,
        settings: &Object,
        now: Num,
        lease: &Object,
        mut pair: Object,
    ) -> Result<Value, Failure> {
        let kind = string_of(lease, "kind");
        let sent = truthy(lease.get("sent"));
        let lease_gateway = string_of(lease, "gateway");
        let lease_generation = string_of(lease, "generation");
        if (action == "probeResult") != (kind == "probe") {
            return Failure::invalid("租约类型不匹配");
        }
        let verdict = policy::classify(
            data.get("status"),
            is_true(data.get("stateReturned")),
            is_true(data.get("paired")),
            is_true(data.get("gatewayMatches")),
            is_true(data.get("failed")),
        );
        if verdict.is_some() && !sent {
            return Failure::invalid("未发送请求不能判定");
        }
        let model = if present(data, "model").is_some() {
            json!(model_of(data)?)
        } else {
            Value::Null
        };
        let probing = kind == "probe";
        let sample = json!({
            "generation": lease_generation,
            "source": kind,
            "model": model,
            "status": field(data, "status"),
            "stateReturned": is_true(data.get("stateReturned")),
            "classification": verdict,
            "durationMs": field(data, "durationMs"),
            "plannedRestSeconds": if probing { field(&pair, "probeRestSeconds") } else { Value::Null },
            "observedIdleSeconds": if probing { field(&pair, "observedIdleSeconds") } else { Value::Null },
            "gatewayMatches": is_true(data.get("gatewayMatches")),
            "firstContactAt": field(&pair, "firstContactAt"),
            "previousContactAt": field(&pair, "previousContactAt"),
            "lastContactAt": field(&pair, "lastContactAt"),
            "preparationStartedAt": field(&pair, "preparationStartedAt"),
            "completedAt": now.to_json(),
        });
        add_observation(db, now, account, &lease_gateway, &sample)?;
        history::record(db, |db| history::result(db, lease, data, verdict, now))?;
        if pair.get("generation") == lease.get("generation") {
            if probing {
                let in_scope = {
                    let limited = gateway_list(settings);
                    limited.is_empty() || limited.contains(&string_of(&pair, "gateway"))
                };
                if verdict == Some("21")
                    && queue::allowed(db, account)?
                    && in_scope
                    && str_field(&pair, "state") == Some("preparing")
                {
                    self.publish_window(db, data, account, settings, now, lease, &mut pair)?;
                } else {
                    // 检测失败与 29 分开留痕，两者均按最后一次访问重新退避。
                    policy::retire(
                        &mut pair,
                        settings,
                        verdict == Some("29"),
                        verdict.is_none(),
                    );
                    if !sent {
                        // 池服务失败没触及上游，不重置真实静置；短暂退让避免反复抓同一候选。
                        let planned = field(&pair, "probeRestSeconds");
                        let rest = if truthy(Some(&planned)) {
                            planned
                        } else {
                            policy::initial(settings, num_field(&pair, "learnedRestSeconds"))
                                .to_json()
                        };
                        set(&mut pair, "state", json!("cooling"));
                        set(&mut pair, "restUntil", now.plus(Num::Int(5)).to_json());
                        set(&mut pair, "restSeconds", rest);
                    }
                }
            } else if verdict == Some("29")
                || data.get("gatewayMatches") == Some(&Value::Bool(false))
            {
                policy::retire(&mut pair, settings, verdict == Some("29"), false);
            }
        }
        self.save(db, &pair)?;
        Ok(json!({"accepted": true, "classification": verdict}))
    }

    /// 21 通过：保存配套材料、学习静置、退役本账号的旧窗口并记录切换，新窗口就绪。
    #[allow(clippy::too_many_arguments)]
    fn publish_window(
        &self,
        db: &Connection,
        data: &Object,
        account: &str,
        settings: &Object,
        now: Num,
        lease: &Object,
        pair: &mut Object,
    ) -> Result<(), Failure> {
        let lease_gateway = string_of(lease, "gateway");
        if let Some(material) = present(data, "material") {
            check_material(material, now)?;
            exec(
                db,
                "INSERT OR REPLACE INTO materials VALUES (?,?,?,?)",
                vec![
                    text(account),
                    text(&string_of(lease, "generation")),
                    sql_of(material.get("expires").unwrap_or(&Value::Null)),
                    text(&encode(material)),
                ],
            )?;
            set(pair, "cookieExpiresAt", field_of_value(material, "expires"));
        }
        policy::recover(pair, num_field(pair, "probeRestSeconds"));
        // 新窗口就绪后，本账号的旧窗口退役；未发送的请求随之切到新窗口。其他账号的窗口属于别的 Key，不受影响。
        let mut previous_pair: Option<Object> = None;
        for row in rows(
            db,
            "SELECT data FROM pairs WHERE account=? AND gateway<>?",
            vec![text(account), text(&lease_gateway)],
        )? {
            let mut old = decode_object(&string_of(&row, "data"))?;
            if str_field(&old, "state") == Some("ready") {
                policy::retire(&mut old, settings, false, false);
                policy::settle(&mut old);
                self.save(db, &old)?;
                previous_pair = Some(old);
            }
        }
        if previous_pair.is_none() {
            // 29 先退役旧组合，探测通过时旧状态已 cooling；切换来源仍是上一已发布组合。
            if let Some(last_switch) = one(
                db,
                "SELECT data FROM switches WHERE account=? ORDER BY id DESC LIMIT 1",
                vec![text(account)],
            )? {
                let switch = decode_object(&string_of(&last_switch, "data"))?;
                let old_gateway = str_field(&switch, "toGateway")
                    .ok_or_else(|| missing("toGateway"))?
                    .to_owned();
                let mut old = load_pair(db, account, &old_gateway)?;
                // 同一网关静置后恢复也保留旧代次的窗口起点，而不是本轮 preparing 的起点。
                let prior = one(
                    db,
                    "SELECT data FROM observations WHERE account=? AND gateway=? AND at<? AND json_extract(data,'$.firstContactAt') IS NOT NULL ORDER BY id DESC LIMIT 1",
                    vec![
                        text(account),
                        text(&old_gateway),
                        sql_of(pair.get("preparationStartedAt").unwrap_or(&Value::Null)),
                    ],
                )?;
                if let Some(prior) = prior {
                    let observed = decode_object(&string_of(&prior, "data"))?;
                    set(
                        &mut old,
                        "firstContactAt",
                        field(&observed, "firstContactAt"),
                    );
                }
                previous_pair = Some(old);
            }
        }
        let started = num_field(pair, "preparationStartedAt")
            .ok_or_else(|| missing("preparationStartedAt"))?;
        let previous_window = previous_pair
            .as_ref()
            .and_then(|old| num_field(old, "firstContactAt"))
            .map(|first| now.minus(first));
        let switch = json!({
            "fromAccount": previous_pair.as_ref().map_or(Value::Null, |old| field(old, "accountId")),
            "fromGateway": previous_pair.as_ref().map_or(Value::Null, |old| field(old, "gateway")),
            "toGateway": field(pair, "gateway"),
            "previousWindowSeconds": previous_window.map_or(Value::Null, Num::to_json),
            "preparationSeconds": now.minus(started).to_json(),
        });
        exec(
            db,
            "INSERT INTO switches(at,account,data) VALUES (?,?,?)",
            vec![sql_of_num(now), text(account), text(&encode(&switch))],
        )?;
        set(pair, "state", json!("ready"));
        set(pair, "verifiedAt", now.to_json());
        set(pair, "restUntil", Value::Null);
        Ok(())
    }

    /// 记录一次动态分组决定，并告诉网关该移动哪个分组。移动本身由 Rust 网关侧完成。
    fn regroup(
        &self,
        db: &Connection,
        settings: &Object,
        account: &str,
        data: &Object,
        now: Num,
    ) -> Result<Value, Failure> {
        let direction = match data.get("direction") {
            Some(Value::String(direction))
                if matches!(direction.as_str(), "degraded" | "recovered") =>
            {
                direction.clone()
            }
            _ => return Failure::invalid("分组方向无效"),
        };
        let model = match present(data, "model") {
            None => Value::Null,
            Some(Value::String(model)) if model_name(model) => json!(model),
            Some(_) => return Failure::invalid("模型无效"),
        };
        let normal = string_of(settings, "normalGroupId");
        let degraded = string_of(settings, "degradedGroupId");
        if !(truthy(settings.get("dynamicGroups"))
            && !normal.is_empty()
            && !degraded.is_empty()
            && normal != degraded)
        {
            return Ok(json!({"move": null, "reason": "disabled"}));
        }
        let (source, target) = if direction == "degraded" {
            (normal, degraded)
        } else {
            (degraded, normal)
        };
        // 同一账号同一方向 10 分钟内已经移动过，或刚发出还没回报，不重复移动。
        let recent = one(
            db,
            "SELECT 1 FROM group_events WHERE account=? AND direction=? AND ((status='moved' AND at>?) OR (status='pending' AND at>?))",
            vec![
                text(account),
                text(&direction),
                sql_of_num(now.minus(Num::Int(600))),
                sql_of_num(now.minus(Num::Int(120))),
            ],
        )?;
        if recent.is_some() {
            return Ok(json!({"move": null, "reason": "recent"}));
        }
        let event = (self.ids)();
        exec(
            db,
            "INSERT INTO group_events(id,at,account,direction,from_group,to_group,model,status) VALUES (?,?,?,?,?,?,?,'pending')",
            vec![
                text(&event),
                sql_of_num(now),
                text(account),
                text(&direction),
                text(&source),
                text(&target),
                sql_of(&model),
            ],
        )?;
        Ok(json!({"move": {"eventId": event, "removeGroup": source, "addGroup": target}}))
    }

    /// 降智分组里的账号：某代次的 state 已超过恢复时长、整代只有满血判定且材料仍有效，
    /// 就领取一次恢复检测。检测失败的账号暂停检测一段时间。
    fn recovery_candidate(
        &self,
        db: &Connection,
        account: &str,
        settings: &Object,
        now: Num,
    ) -> Result<Value, Failure> {
        if !truthy(settings.get("dynamicGroups")) {
            return Ok(Value::Null);
        }
        let row = one(
            db,
            "SELECT * FROM recovery_checks WHERE account=?",
            vec![text(account)],
        )?;
        if let Some(row) = &row
            && num_field(row, "paused_until")
                .unwrap_or(Num::Int(0))
                .gt(now)
        {
            return Ok(Value::Null);
        }
        let recovery_seconds = num_field(settings, "recoverySeconds").unwrap_or(Num::Int(0));
        let mut best: Option<(Num, String, String)> = None;
        for pair in account_pairs(db, account)? {
            let (Some(first), Some(Value::String(generation))) =
                (num_field(&pair, "firstContactAt"), pair.get("generation"))
            else {
                continue;
            };
            if now.minus(first).lt(recovery_seconds) {
                continue;
            }
            if row.as_ref().is_some_and(|row| {
                str_field(row, "claimed_generation") == Some(generation.as_str())
                    && num_field(row, "claimed_at").is_some_and(|claimed| {
                        // 与 Python 有意不同：未回报的领取超过 5 分钟可重领；旧记录无时间视为过期。
                        // 已回报的同代次仍只检测一次，不改变 recoveryResult 的暂停规则。
                        now.minus(claimed).le(Num::Int(300))
                            || num_field(row, "last_at").is_some_and(|last| last.ge(claimed))
                    })
            }) {
                continue;
            }
            // 整代只有满血判定：出现过降智或检测失败的代次不能作为恢复证据。
            let bad = one(
                db,
                "SELECT 1 FROM observations WHERE account=? AND json_extract(data,'$.generation')=? AND json_extract(data,'$.source') IN ('probe','business') AND (json_extract(data,'$.classification') IS NULL OR json_extract(data,'$.classification')='29')",
                vec![text(account), text(generation)],
            )?;
            if bad.is_some() {
                continue;
            }
            let material = one(
                db,
                "SELECT 1 FROM materials WHERE account=? AND generation=? AND expires>?",
                vec![
                    text(account),
                    text(generation),
                    sql_of_num(now.plus(Num::Int(20))),
                ],
            )?;
            if material.is_none() {
                continue;
            }
            if best.as_ref().is_none_or(|(known, _, _)| first.gt(*known)) {
                best = Some((first, generation.clone(), string_of(&pair, "gateway")));
            }
        }
        let Some((first, generation, gateway)) = best else {
            return Ok(Value::Null);
        };
        exec(
            db,
            "INSERT INTO recovery_checks(account,paused_until,claimed_generation,claimed_at) VALUES (?,0,?,?) ON CONFLICT(account) DO UPDATE SET claimed_generation=excluded.claimed_generation, claimed_at=excluded.claimed_at",
            vec![text(account), text(&generation), sql_of_num(now)],
        )?;
        let age = match now.minus(first) {
            Num::Int(value) => value,
            Num::Float(value) => value.trunc() as i64,
        };
        Ok(json!({"generation": generation, "gateway": gateway, "ageSeconds": age}))
    }

    fn recovery_result(
        &self,
        db: &Connection,
        settings: &Object,
        account: &str,
        data: &Object,
        now: Num,
    ) -> Result<Value, Failure> {
        let outcome = match data.get("outcome") {
            Some(Value::String(outcome))
                if matches!(outcome.as_str(), "recovered" | "failed" | "unavailable") =>
            {
                outcome.clone()
            }
            _ => return Failure::invalid("恢复检测结果无效"),
        };
        // 失败暂停较长时间；无法检测（网络、额度）短暂等待；已恢复的账号马上会离开降智分组。
        let pause = match outcome.as_str() {
            "failed" => num_field(settings, "recoveryPauseSeconds").unwrap_or(Num::Int(0)),
            "unavailable" => Num::Int(300),
            _ => Num::Int(600),
        };
        let until = now.plus(pause);
        exec(
            db,
            "INSERT INTO recovery_checks(account,paused_until,last_outcome,last_at) VALUES (?,?,?,?) ON CONFLICT(account) DO UPDATE SET paused_until=excluded.paused_until, last_outcome=excluded.last_outcome, last_at=excluded.last_at",
            vec![
                text(account),
                sql_of_num(until),
                text(&outcome),
                sql_of_num(now),
            ],
        )?;
        Ok(json!({"accepted": true, "pausedUntil": until.to_json()}))
    }
}

fn regroup_result(
    db: &Connection,
    account: &str,
    data: &Object,
    now: Num,
) -> Result<Value, Failure> {
    let event = match data.get("eventId") {
        Some(Value::String(event))
            if event.len() == 32
                && event
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            event.clone()
        }
        _ => return Failure::invalid("分组结果无效"),
    };
    let status = match data.get("status") {
        Some(Value::String(status))
            if matches!(
                status.as_str(),
                "moved" | "unchanged" | "failed" | "unsupported"
            ) =>
        {
            status.clone()
        }
        _ => return Failure::invalid("分组结果无效"),
    };
    exec(
        db,
        "UPDATE group_events SET status=?, finished_at=? WHERE id=? AND account=? AND status=?",
        vec![
            text(&status),
            sql_of_num(now),
            text(&event),
            text(account),
            text("pending"),
        ],
    )?;
    Ok(json!({"accepted": true}))
}

/// `max(0, inFlight - 1)`：缺失按 0 处理。
fn decrement_in_flight(pair: &mut Object) {
    let current = num_field(pair, "inFlight").unwrap_or(Num::Int(0));
    set(
        pair,
        "inFlight",
        Num::Int(0).max(current.minus(Num::Int(1))).to_json(),
    );
}

fn field_of_value(value: &Value, key: &str) -> Value {
    value.get(key).cloned().unwrap_or(Value::Null)
}

/// `queueTurn != turn` 的相反面：JSON 数字与整数位置按数值相等，布尔按 0/1，其余不相等。
fn same_turn(requested: &Value, turn: &Value) -> bool {
    let Some(turn) = turn.as_i64() else {
        return false;
    };
    match requested {
        Value::Number(number) => number.as_f64() == Some(turn as f64),
        Value::Bool(flag) => i64::from(*flag) == turn,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 期望值来自 Python `span()`（银行家舍入、保留一位小时）。
    #[test]
    fn span_matches_the_python_wording() {
        for (seconds, expected) in [
            (-5.0, "0 秒"),
            (0.0, "0 秒"),
            (89.9, "89 秒"),
            (90.0, "2 分钟"),
            (150.0, "2 分钟"),
            (210.0, "4 分钟"),
            (270.0, "4 分钟"),
            (5399.0, "90 分钟"),
            (5400.0, "1.5 小时"),
            (8100.0, "2.2 小时"),
            (8280.0, "2.3 小时"),
            (86400.0, "24.0 小时"),
        ] {
            assert_eq!(span(Num::Float(seconds)), expected, "span({seconds})");
        }
    }

    #[test]
    fn deadline_conversion_follows_python_float() {
        assert_eq!(python_float(&json!(5)).ok(), Some(5.0));
        assert_eq!(python_float(&json!(true)).ok(), Some(1.0));
        assert_eq!(python_float(&json!(" 12.5 ")).ok(), Some(12.5));
        assert!(python_float(&json!("abc")).is_err());
        assert!(python_float(&json!(null)).is_err());
        assert!(python_float(&json!([1])).is_err());
    }

    #[test]
    fn material_must_be_complete_single_line_and_expire_within_a_year() {
        let now = Num::Float(1000.0);
        let good = json!({"state": "s", "cookie": "c", "expires": 1600.0});
        assert!(check_material(&good, now).is_ok());
        for bad in [
            json!({"state": "s", "cookie": "c"}),
            json!({"state": "s", "cookie": "c", "expires": 1600.0, "extra": 1}),
            json!({"state": "", "cookie": "c", "expires": 1600.0}),
            json!({"state": "s\n", "cookie": "c", "expires": 1600.0}),
            json!({"state": "s", "cookie": "c", "expires": 999.0}),
            json!({"state": "s", "cookie": "c", "expires": 1000.0 + 86400.0 * 365.0 + 1.0}),
            json!({"state": "s", "cookie": 3, "expires": 1600.0}),
            json!({"state": "s", "cookie": "c", "expires": "soon"}),
            json!("text"),
        ] {
            assert!(check_material(&bad, now).is_err(), "{bad}");
        }
    }
}
