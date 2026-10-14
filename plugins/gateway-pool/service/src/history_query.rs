//! 网关池历史只读投影，逐项对应 `gateway_pool_history_query.py`。
//! 完整范围聚合与当前页分开；未知时间不补零；查询过程不访问上游。

use std::collections::HashMap;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::{
    error::Failure,
    num::Num,
    util::{
        Object, bounded, decode_object, is_null_or_missing, merged, num_field, num_of, one, rows,
        set, sql_of_num, str_field, text, truthy,
    },
};

const METRICS: [&str; 3] = ["preparationSeconds", "windowSeconds", "businessSeconds"];
const MAX_WINDOWS: usize = 20000;
const MAX_ACCESSES: usize = 200000;
const OVERFLOW: &str = "统计范围记录较多，请缩短时间或选择账号后查询";
const BOUNDARIES: [i64; 4] = [7200, 14400, 28800, 57600];
const BIN_LABELS: [&str; 5] = ["不足 2h", "2–4h", "4–8h", "8–16h", "16h 及以上"];
/// 冷却分析的结果分类，顺序与 `COOLING_KEYS` 对应。
const COOLING_KEYS: [&str; 4] = ["recovered", "unrecovered", "failed", "pending"];

/// 查询的时间边界：下界、上界（已截断到当前时间）与当前时间。
#[derive(Clone, Copy)]
struct Span {
    lower: Num,
    upper: Num,
    now: Num,
}

struct Access {
    kind: String,
    observed: Num,
    started: Option<Num>,
    ended: Option<Num>,
    expires: Option<Num>,
    incomplete: bool,
    segment: Option<String>,
    missing: bool,
}

impl Access {
    fn from_row(row: &Object) -> Self {
        Self {
            kind: str_field(row, "kind").unwrap_or_default().to_owned(),
            observed: num_field(row, "observed").unwrap_or(Num::Int(0)),
            started: num_field(row, "started"),
            ended: num_field(row, "ended"),
            expires: num_field(row, "expires"),
            incomplete: truthy(row.get("incomplete")),
            segment: str_field(row, "segment").map(str::to_owned),
            missing: false,
        }
    }

    /// 访问的时间锚点：起点缺失时退回观察时间。
    fn anchor(&self) -> Num {
        self.started.unwrap_or(self.observed)
    }
}

struct Segment {
    id: String,
    started: Num,
    last_end: Option<Num>,
    closed: Option<Num>,
    reason: Value,
}

impl Segment {
    fn from_row(row: &Object) -> Self {
        Self {
            id: str_field(row, "id").unwrap_or_default().to_owned(),
            started: num_field(row, "started").unwrap_or(Num::Int(0)),
            last_end: num_field(row, "last_end"),
            closed: num_field(row, "closed"),
            reason: row.get("reason").cloned().unwrap_or(Value::Null),
        }
    }
}

fn opt_json(value: Option<Num>) -> Value {
    value.map_or(Value::Null, Num::to_json)
}

fn or_zero(value: Option<Num>) -> Num {
    match value {
        Some(number) if number.truthy() => number,
        _ => Num::Int(0),
    }
}

/// 相同端点的区间可能交叠；返回排序后的并集长度。
fn union_seconds(intervals: &[(Num, Num)]) -> Num {
    let mut sorted = intervals.to_vec();
    sorted.sort_by(|a, b| {
        a.0.as_f64()
            .total_cmp(&b.0.as_f64())
            .then(a.1.as_f64().total_cmp(&b.1.as_f64()))
    });
    let mut total = Num::Int(0);
    let mut previous = Num::Float(f64::NEG_INFINITY);
    for (start, end) in sorted {
        total = total.plus(Num::Int(0).max(end.minus(start.max(previous))));
        previous = previous.max(end);
    }
    total
}

/// 访问在查询范围内的区间；缺失或未知结束的访问不计入已知时长。
fn intervals(accesses: &[&Access], span: Span) -> Vec<(Num, Num)> {
    accesses
        .iter()
        .filter_map(|access| {
            let started = access.started?;
            if access.missing {
                return None;
            }
            let end = access.ended.unwrap_or(span.now);
            (started.lt(span.upper) && end.ge(span.lower))
                .then(|| (span.lower.max(started), span.upper.min(end)))
        })
        .collect()
}

fn clip(start: Option<Num>, end: Option<Num>, lower: Num, upper: Num) -> Option<Num> {
    let (start, end) = (start?, end?);
    Some(Num::Int(0).max(upper.min(end).minus(lower.max(start))))
}

fn sum_of(intervals: &[(Num, Num)]) -> Num {
    Num::sum(intervals.iter().map(|(start, end)| end.minus(*start)))
}

/// 按 `METRICS` 聚合窗口行；与 Python `aggregate` 的键与类型一致。
fn aggregate(rows: &[&Object]) -> Object {
    let mut result = Object::new();
    for key in METRICS {
        let total = Num::sum(rows.iter().map(|row| or_zero(num_field(row, key))));
        set(&mut result, key, total.to_json());
    }
    let mut missing = Object::new();
    for key in METRICS {
        let count = rows
            .iter()
            .filter(|row| {
                num_field(row, key).is_none()
                    && (key != "windowSeconds" || !is_null_or_missing(row, "verifiedAt"))
            })
            .count();
        set(&mut missing, key, json!(count));
    }
    set(&mut result, "missing", Value::Object(missing));
    let requests = Num::sum(
        rows.iter()
            .map(|row| num_field(row, "requests").unwrap_or(Num::Int(0))),
    );
    set(&mut result, "requests", requests.to_json());
    set(&mut result, "records", json!(rows.len()));
    let incomplete = rows
        .iter()
        .filter(|row| truthy(row.get("incomplete")))
        .count();
    set(&mut result, "incomplete", json!(incomplete));
    result
}

fn known_business_total(rows: &[&Object]) -> Num {
    Num::sum(
        rows.iter()
            .map(|row| num_field(row, "knownBusinessSeconds").unwrap_or(Num::Int(0))),
    )
}

/// 投影单个窗口：计时、访问区间与分段，全部限定在查询范围内。
fn window_row(
    mut item: Object,
    mut accesses: Vec<Access>,
    segments: &[Segment],
    span: Span,
) -> Object {
    let Span { lower, upper, now } = span;
    for access in &mut accesses {
        access.missing = access.incomplete
            || access.started.is_none()
            || (access.ended.is_none() && access.expires.is_none_or(|expires| expires.le(now)));
    }
    // 确定在范围外的缺失访问不污染本次计时；结束未知仍保守保留。
    let accesses: Vec<Access> = accesses
        .into_iter()
        .filter(|access| {
            access.anchor().lt(upper) && access.ended.is_none_or(|ended| ended.ge(lower))
        })
        .collect();
    let business: Vec<&Access> = accesses
        .iter()
        .filter(|access| access.kind == "business")
        .collect();
    let legacy = accesses.iter().any(|access| access.kind == "legacy");
    let tickets: Vec<&Access> = accesses
        .iter()
        .filter(|access| access.kind == "ticket")
        .collect();

    let active = intervals(&business, span);
    let known_business = union_seconds(&active);
    let missing_business = business.iter().any(|access| access.missing) || legacy;

    let preparation_start = match item.get("preparationMeasuredAt") {
        Some(value) => num_of(Some(value)),
        None => num_field(&item, "preparationStartedAt"),
    };
    let preparation_end = num_field(&item, "probeEndedAt")
        .or_else(|| (str_field(&item, "state") == Some("preparing")).then_some(now));
    let preparation = clip(preparation_start, preparation_end, lower, upper);
    let window = if is_null_or_missing(&item, "verifiedAt") {
        None
    } else {
        clip(
            num_field(&item, "firstContactAt"),
            Some(num_field(&item, "retiredAt").unwrap_or(now)),
            lower,
            upper,
        )
    };
    let requests = business
        .iter()
        .filter(|access| lower.le(access.anchor()) && access.anchor().lt(upper))
        .count();
    let partial = num_field(&item, "preparationStartedAt").is_some_and(|started| started.lt(lower))
        || num_field(&item, "drainedAt").unwrap_or(now).gt(upper);
    let incomplete = truthy(item.get("incomplete")) || accesses.iter().any(|access| access.missing);

    set(&mut item, "preparationSeconds", opt_json(preparation));
    set(&mut item, "windowSeconds", opt_json(window));
    set(
        &mut item,
        "businessSeconds",
        if missing_business {
            Value::Null
        } else {
            known_business.to_json()
        },
    );
    set(&mut item, "knownBusinessSeconds", known_business.to_json());
    set(&mut item, "requestSeconds", sum_of(&active).to_json());
    let ticket_intervals = intervals(&tickets, span);
    set(
        &mut item,
        "ticketSeconds",
        sum_of(&ticket_intervals).to_json(),
    );
    set(&mut item, "requests", json!(requests));
    set(&mut item, "incomplete", json!(incomplete));
    set(&mut item, "partial", json!(partial));

    let mut projected = Vec::new();
    for segment in segments {
        let visits: Vec<&Access> = business
            .iter()
            .copied()
            .filter(|access| access.segment.as_deref() == Some(segment.id.as_str()))
            .collect();
        let closed_end = match (segment.closed, segment.last_end) {
            (Some(_), Some(last_end)) => last_end,
            _ => now,
        };
        if segment.started.ge(upper) || closed_end.lt(lower) {
            continue;
        }
        let interval = intervals(&visits, span);
        let mut row = Object::new();
        set(&mut row, "id", json!(segment.id));
        set(&mut row, "startedAt", segment.started.to_json());
        set(&mut row, "endedAt", opt_json(segment.last_end));
        set(&mut row, "closedAt", opt_json(segment.closed));
        set(&mut row, "reason", segment.reason.clone());
        set(
            &mut row,
            "businessSeconds",
            union_seconds(&interval).to_json(),
        );
        set(&mut row, "requestSeconds", sum_of(&interval).to_json());
        let segment_requests = visits
            .iter()
            .filter(|access| {
                access
                    .started
                    .is_some_and(|started| lower.le(started) && started.lt(upper))
            })
            .count();
        set(&mut row, "requests", json!(segment_requests));
        set(
            &mut row,
            "incomplete",
            json!(visits.iter().any(|access| access.missing)),
        );
        projected.push(Value::Object(row));
    }
    set(&mut item, "segments", Value::Array(projected));
    item
}

/// 按第一次出现的顺序分组，保证网关列表的顺序与原实现一致。
fn group_by<'a>(records: &'a [Object], key: &str) -> Vec<(String, Vec<&'a Object>)> {
    let mut groups: Vec<(String, Vec<&'a Object>)> = Vec::new();
    for record in records {
        let name = str_field(record, key).unwrap_or_default();
        match groups.iter_mut().find(|(existing, _)| existing == name) {
            Some((_, members)) => members.push(record),
            None => groups.push((name.to_owned(), vec![record])),
        }
    }
    groups
}

fn string_of(row: &Object, key: &str) -> String {
    str_field(row, key).unwrap_or_default().to_owned()
}

/// `cooling_summary`：卡片只读最近的实际再访问，未访问的等待不生成结果或间隔。
pub fn cooling_summary(db: &Connection, account: &str, gateway: &str) -> Result<Value, Failure> {
    let history = rows(
        db,
        "SELECT recovered,data FROM gp_cooling WHERE account=? AND gateway=? ORDER BY id DESC",
        vec![text(account), text(gateway)],
    )?;
    let mut current = Value::Null;
    let mut visits: Vec<Value> = Vec::new();
    for (index, row) in history.iter().enumerate() {
        let data = decode_object(&string_of(row, "data"))?;
        let steps: Vec<Value> = data
            .get("steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if index == 0 && is_null_or_missing(row, "recovered") {
            current = steps.last().cloned().unwrap_or(Value::Null);
        }
        for step in steps.iter().rev() {
            let Some(step_object) = step.as_object() else {
                continue;
            };
            let Some(probe) = num_field(step_object, "probeAt") else {
                continue;
            };
            let started = num_field(step_object, "startedAt");
            let due = num_field(step_object, "dueAt")
                .ok_or_else(|| Failure::Internal("静置步骤缺少到期时间".to_owned()))?;
            let mut visit = step_object.clone();
            let idle = started.map_or(Value::Null, |started| {
                Num::Int(0).max(probe.minus(started)).to_json()
            });
            set(&mut visit, "idleSeconds", idle);
            set(
                &mut visit,
                "afterDueSeconds",
                Num::Int(0).max(probe.minus(due)).to_json(),
            );
            visits.push(Value::Object(visit));
            if visits.len() == 6 {
                visits.truncate(5);
                return Ok(json!({"current": current, "visits": visits, "hasMore": true}));
            }
        }
    }
    Ok(json!({"current": current, "visits": visits, "hasMore": false}))
}

/// `median`：奇数个取中间值本身；偶数个取两者之和除以二（浮点）。
fn median(mut values: Vec<Num>) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    values.sort_by(|a, b| a.as_f64().total_cmp(&b.as_f64()));
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle].to_json()
    } else {
        values[middle - 1]
            .plus(values[middle])
            .over(Num::Int(2))
            .to_json()
    }
}

/// 冷却周期的实际再访问分析：按实际访问起点归属范围；失败、待结果与缺失间隔不冒充恢复判定。
fn cooling_analysis(
    db: &Connection,
    account: &str,
    gateway: &str,
    lower: Num,
    upper: Num,
) -> Result<Value, Failure> {
    let mut conditions = vec![
        "json_extract(s.value,'$.probeAt')>=?".to_owned(),
        "json_extract(s.value,'$.probeAt')<?".to_owned(),
    ];
    let mut args = vec![sql_of_num(lower), sql_of_num(upper)];
    if !account.is_empty() {
        conditions.push("c.account=?".to_owned());
        args.push(text(account));
    }
    if !gateway.is_empty() {
        conditions.push("c.gateway=?".to_owned());
        args.push(text(gateway));
    }
    let sql = format!(
        "SELECT c.account,c.gateway,s.value FROM gp_cooling c,json_each(c.data,'$.steps') s WHERE {}",
        conditions.join(" AND ")
    );
    let found = bounded(db, &sql, args, MAX_ACCESSES, OVERFLOW)?;
    let mut totals = [0_usize; 4];
    let mut bins = [[0_usize; 4]; 5];
    let mut pairs = std::collections::BTreeSet::new();
    let mut missing = 0_usize;
    let mut recovered_intervals = Vec::new();
    for row in &found {
        let step = decode_object(&string_of(row, "value"))?;
        pairs.insert((string_of(row, "account"), string_of(row, "gateway")));
        let result = match num_field(&step, "resultAt") {
            Some(resulted) if resulted.le(upper) => step.get("result").and_then(Value::as_str),
            _ => None,
        };
        let key = match result {
            Some("21") => 0,
            Some("29") => 1,
            Some("failed") => 2,
            _ => 3,
        };
        totals[key] += 1;
        let (Some(started), Some(probe)) =
            (num_field(&step, "startedAt"), num_field(&step, "probeAt"))
        else {
            missing += 1;
            continue;
        };
        if probe.lt(started) {
            missing += 1;
            continue;
        }
        let idle = probe.minus(started);
        let index = BOUNDARIES
            .iter()
            .filter(|boundary| idle.ge(Num::Int(**boundary)))
            .count();
        bins[index][key] += 1;
        if key == 0 {
            recovered_intervals.push(idle);
        }
    }
    let bin_values: Vec<Value> = BIN_LABELS
        .iter()
        .zip(bins)
        .map(|(label, counts)| {
            let mut bin = Object::new();
            set(&mut bin, "label", json!(label));
            for (name, count) in COOLING_KEYS.iter().zip(counts) {
                set(&mut bin, name, json!(count));
            }
            Value::Object(bin)
        })
        .collect();
    let mut result = Object::new();
    for (name, count) in COOLING_KEYS.iter().zip(totals) {
        set(&mut result, name, json!(count));
    }
    set(&mut result, "total", json!(found.len()));
    set(&mut result, "gateways", json!(pairs.len()));
    set(&mut result, "missingInterval", json!(missing));
    set(&mut result, "bins", Value::Array(bin_values));
    set(
        &mut result,
        "recoveredMedianSeconds",
        median(recovered_intervals),
    );
    Ok(Value::Object(result))
}

fn int_param(
    value: Option<&Value>,
    default: i64,
    minimum: i64,
    maximum: i64,
) -> Result<i64, Failure> {
    match value {
        None => Ok(default),
        Some(Value::Number(number)) => match number.as_i64() {
            Some(integer) if (minimum..=maximum).contains(&integer) => Ok(integer),
            _ => Failure::invalid("分页参数无效"),
        },
        Some(_) => Failure::invalid("分页参数无效"),
    }
}

fn finite_time(value: &Value) -> Result<Num, Failure> {
    match num_of(Some(value)) {
        Some(number) if number.as_f64().is_finite() => Ok(number),
        _ => Failure::invalid("统计时间范围无效"),
    }
}

fn text_filter(query: &Object, key: &str) -> Result<String, Failure> {
    match query.get(key) {
        None => Ok(String::new()),
        Some(value) if !truthy(Some(value)) => Ok(String::new()),
        Some(Value::String(value)) if value.chars().count() <= 160 => Ok(value.clone()),
        Some(_) => Failure::invalid("筛选参数无效"),
    }
}

/// `read`：统计查询的入口。`now` 由调用方注入，便于测试。
pub fn read(db: &Connection, now: Num, query: &Object) -> Result<Value, Failure> {
    let meta = one(db, "SELECT * FROM gp_history_meta WHERE id=1", vec![])?
        .ok_or_else(|| Failure::Internal("历史元数据缺失".to_owned()))?;
    let recorded_since = num_field(&meta, "started").unwrap_or(now);
    let lower = match query.get("from").filter(|value| !value.is_null()) {
        Some(value) => finite_time(value)?,
        None => recorded_since,
    };
    let upper = match query.get("to").filter(|value| !value.is_null()) {
        Some(value) => finite_time(value)?,
        None => now,
    };
    if !(Num::Int(0).le(lower) && lower.le(upper) && upper.le(now.plus(Num::Int(60)))) {
        return Failure::invalid("统计时间范围无效");
    }
    let upper = upper.min(now);
    let page = int_param(query.get("page"), 1, 1, 100_000)?;
    let size = int_param(query.get("pageSize"), 20, 1, 100)?;
    let view = match query.get("view") {
        None => "records".to_owned(),
        Some(Value::String(view)) if matches!(view.as_str(), "records" | "summary" | "cooling") => {
            view.clone()
        }
        Some(_) => return Failure::invalid("统计视图无效"),
    };
    let account = text_filter(query, "accountId")?;
    let gateway = text_filter(query, "gateway")?;
    let batch = text_filter(query, "batchId")?;
    let reason = text_filter(query, "reason")?;

    let mut conditions = vec![
        "w.started<=?".to_owned(),
        "(w.ended IS NULL OR w.ended>=?)".to_owned(),
    ];
    let mut args = vec![sql_of_num(upper), sql_of_num(lower)];
    for (column, value) in [
        ("account", &account),
        ("gateway", &gateway),
        ("batch", &batch),
    ] {
        if !value.is_empty() {
            conditions.push(format!("w.{column}=?"));
            args.push(text(value));
        }
    }
    if !reason.is_empty() {
        conditions.push("json_extract(w.data,'$.reason')=?".to_owned());
        args.push(text(&reason));
    }
    let condition = conditions.join(" AND ");
    let span = Span { lower, upper, now };

    let windows = if view == "cooling" {
        Vec::new()
    } else {
        bounded(
            db,
            &format!(
                "SELECT w.* FROM gp_windows w WHERE {condition} ORDER BY w.started DESC,w.id DESC"
            ),
            args.clone(),
            MAX_WINDOWS,
            OVERFLOW,
        )?
    };
    let mut accesses: HashMap<String, Vec<Access>> = HashMap::new();
    let mut segments: HashMap<String, Vec<Segment>> = HashMap::new();
    if !windows.is_empty() {
        let access_sql = format!(
            "SELECT a.*,l.expires FROM gp_accesses a JOIN gp_windows w ON w.id=a.generation LEFT JOIN leases l ON l.id=a.lease WHERE {condition}"
        );
        for row in bounded(db, &access_sql, args.clone(), MAX_ACCESSES, OVERFLOW)? {
            accesses
                .entry(string_of(&row, "generation"))
                .or_default()
                .push(Access::from_row(&row));
        }
        let segment_sql = format!(
            "SELECT s.* FROM gp_segments s JOIN gp_windows w ON w.id=s.generation WHERE {condition} ORDER BY s.started"
        );
        for row in bounded(db, &segment_sql, args.clone(), MAX_ACCESSES, OVERFLOW)? {
            segments
                .entry(string_of(&row, "generation"))
                .or_default()
                .push(Segment::from_row(&row));
        }
    }
    let mut records: Vec<Object> = Vec::with_capacity(windows.len());
    for window in &windows {
        let id = string_of(window, "id");
        let item = decode_object(&string_of(window, "data"))?;
        let window_accesses = accesses.remove(&id).unwrap_or_default();
        let window_segments = segments.get(&id).map_or(&[][..], Vec::as_slice);
        records.push(window_row(item, window_accesses, window_segments, span));
    }

    let record_refs: Vec<&Object> = records.iter().collect();
    let mut summary = aggregate(&record_refs);
    set(
        &mut summary,
        "businessSeconds",
        known_business_total(&record_refs).to_json(),
    );

    let mut gateway_rows = Vec::new();
    for (name, members) in group_by(&records, "gateway") {
        let mut total = aggregate(&members);
        set(
            &mut total,
            "businessSeconds",
            known_business_total(&members).to_json(),
        );
        let mut row = Object::new();
        set(&mut row, "gateway", json!(name));
        gateway_rows.push(Value::Object(merged(row, total)));
    }

    let mut batches: Vec<Value> = Vec::new();
    let mut by_batch: HashMap<String, Vec<&Object>> = HashMap::new();
    for (name, members) in group_by(&records, "batchId") {
        by_batch.insert(name, members);
    }
    if view == "summary" {
        let batch_sql = format!(
            "SELECT b.* FROM gp_batches b WHERE EXISTS (SELECT 1 FROM gp_windows w WHERE w.batch=b.id AND {condition}) ORDER BY b.started DESC,b.number DESC"
        );
        for row in bounded(db, &batch_sql, args.clone(), MAX_WINDOWS, OVERFLOW)? {
            let batch_id = string_of(&row, "id");
            let Some(entries) = by_batch.get(&batch_id) else {
                continue;
            };
            let candidates: Vec<String> = serde_json::from_str(&string_of(&row, "candidates"))?;
            let processed: Object = decode_object(&string_of(&row, "processed"))?
                .into_iter()
                .filter(|(key, _)| candidates.contains(key))
                .collect();
            let served = processed
                .values()
                .filter(|value| value.as_str() == Some("served"))
                .count();
            let mut totals = aggregate(entries);
            set(
                &mut totals,
                "businessSeconds",
                known_business_total(entries).to_json(),
            );
            let started = num_field(&row, "started").unwrap_or(Num::Int(0));
            let ended = num_field(&row, "ended").unwrap_or(now);
            let partial = started.lt(lower)
                || ended.gt(upper)
                || entries.iter().any(|record| truthy(record.get("partial")))
                || !gateway.is_empty()
                || !reason.is_empty();
            let mut batch = Object::new();
            set(&mut batch, "id", json!(batch_id));
            set(
                &mut batch,
                "accountId",
                row.get("account").cloned().unwrap_or(Value::Null),
            );
            set(
                &mut batch,
                "number",
                row.get("number").cloned().unwrap_or(Value::Null),
            );
            set(
                &mut batch,
                "startedAt",
                row.get("started").cloned().unwrap_or(Value::Null),
            );
            set(
                &mut batch,
                "endedAt",
                row.get("ended").cloned().unwrap_or(Value::Null),
            );
            set(
                &mut batch,
                "status",
                row.get("status").cloned().unwrap_or(Value::Null),
            );
            set(
                &mut batch,
                "reason",
                row.get("reason").cloned().unwrap_or(Value::Null),
            );
            set(&mut batch, "candidates", json!(candidates));
            set(&mut batch, "processed", Value::Object(processed));
            set(&mut batch, "served", json!(served));
            set(&mut batch, "partial", json!(partial));
            batches.push(Value::Object(merged(batch, totals)));
        }
    }

    let mut cycles: Vec<Value> = Vec::new();
    if view == "cooling" {
        let mut cooling_conditions = vec![
            "started<=?".to_owned(),
            "(recovered IS NULL OR recovered>=?)".to_owned(),
        ];
        let mut cooling_args = vec![sql_of_num(upper), sql_of_num(lower)];
        if !account.is_empty() {
            cooling_conditions.push("account=?".to_owned());
            cooling_args.push(text(&account));
        }
        if !gateway.is_empty() {
            cooling_conditions.push("gateway=?".to_owned());
            cooling_args.push(text(&gateway));
        }
        let sql = format!(
            "SELECT * FROM gp_cooling WHERE {} ORDER BY started DESC,id DESC",
            cooling_conditions.join(" AND ")
        );
        for row in bounded(db, &sql, cooling_args, MAX_WINDOWS, OVERFLOW)? {
            let data = decode_object(&string_of(&row, "data"))?;
            let steps: Vec<Value> = data
                .get("steps")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let step = steps
                .last()
                .and_then(Value::as_object)
                .ok_or_else(|| Failure::Internal("静置周期缺少步骤".to_owned()))?;
            let state = if !is_null_or_missing(&row, "recovered") {
                "recovered"
            } else if !is_null_or_missing(step, "probeAt") && is_null_or_missing(step, "resultAt") {
                "probing"
            } else if num_field(step, "dueAt").is_some_and(|due| due.le(now)) {
                "due"
            } else {
                "cooling"
            };
            let incomplete = steps
                .iter()
                .any(|item| item.get("startedAt").is_none_or(Value::is_null));
            let mut cycle = Object::new();
            set(
                &mut cycle,
                "id",
                row.get("id").cloned().unwrap_or(Value::Null),
            );
            set(
                &mut cycle,
                "accountId",
                row.get("account").cloned().unwrap_or(Value::Null),
            );
            set(
                &mut cycle,
                "gateway",
                row.get("gateway").cloned().unwrap_or(Value::Null),
            );
            set(
                &mut cycle,
                "startedAt",
                row.get("started").cloned().unwrap_or(Value::Null),
            );
            set(
                &mut cycle,
                "recoveredAt",
                row.get("recovered").cloned().unwrap_or(Value::Null),
            );
            set(&mut cycle, "state", json!(state));
            set(&mut cycle, "incomplete", json!(incomplete));
            cycles.push(Value::Object(merged(cycle, data)));
        }
    }
    let status = match query.get("status") {
        None => String::new(),
        Some(value) if !truthy(Some(value)) => String::new(),
        Some(Value::String(value))
            if matches!(
                value.as_str(),
                "recovered" | "probing" | "due" | "cooling" | "incomplete"
            ) =>
        {
            value.clone()
        }
        Some(_) => return Failure::invalid("恢复状态无效"),
    };
    if !status.is_empty() {
        cycles.retain(|cycle| {
            if status == "incomplete" {
                cycle.get("incomplete").and_then(Value::as_bool) == Some(true)
            } else {
                cycle.get("state").and_then(Value::as_str) == Some(status.as_str())
            }
        });
    }

    let record_values: Vec<Value> = records.into_iter().map(Value::Object).collect();
    let current = match view.as_str() {
        "cooling" => cycles,
        "summary" => batches,
        _ => record_values,
    };
    let count = current.len();
    let size_usize = usize::try_from(size).unwrap_or(1);
    let pages = count.div_ceil(size_usize).max(1);
    let page = usize::try_from(page).unwrap_or(1).min(pages);
    let start = ((page - 1) * size_usize).min(count);
    let end = (start + size_usize).min(count);
    let mut selected: Vec<Value> = current[start..end].to_vec();
    if view == "records" {
        for item in &mut selected {
            let Some(object) = item.as_object_mut() else {
                continue;
            };
            let generation = string_of(object, "id");
            let counted = one(
                db,
                "SELECT COUNT(*) AS count FROM observations WHERE json_extract(data,'$.generation')=?",
                vec![text(&generation)],
            )?
            .unwrap_or_default();
            set(
                object,
                "observationCount",
                counted.get("count").cloned().unwrap_or(json!(0)),
            );
            let mut observations = Vec::new();
            for row in rows(
                db,
                "SELECT * FROM observations WHERE json_extract(data,'$.generation')=? ORDER BY id DESC LIMIT 100",
                vec![text(&generation)],
            )? {
                let data = decode_object(&string_of(&row, "data"))?;
                let mut sample = Object::new();
                for key in ["id", "at", "account", "gateway"] {
                    set(
                        &mut sample,
                        key,
                        row.get(key).cloned().unwrap_or(Value::Null),
                    );
                }
                observations.push(Value::Object(merged(data, sample)));
            }
            set(object, "observations", Value::Array(observations));
        }
    }

    let accounts: Vec<Value> = rows(
        db,
        "SELECT DISTINCT account FROM gp_windows UNION SELECT DISTINCT account FROM gp_cooling ORDER BY account",
        vec![],
    )?
    .iter()
    .map(|row| row.get("account").cloned().unwrap_or(Value::Null))
    .collect();
    let (gateway_sql, gateway_args) = if account.is_empty() {
        (
            "SELECT DISTINCT gateway FROM gp_windows ORDER BY gateway",
            vec![],
        )
    } else {
        (
            "SELECT DISTINCT gateway FROM gp_windows WHERE account=? ORDER BY gateway",
            vec![text(&account)],
        )
    };
    let gateway_options: Vec<Value> = rows(db, gateway_sql, gateway_args)?
        .iter()
        .map(|row| row.get("gateway").cloned().unwrap_or(Value::Null))
        .collect();
    let (batch_sql, batch_args) = if account.is_empty() {
        (
            "SELECT id,account,number FROM gp_batches ORDER BY started DESC LIMIT 1000",
            vec![],
        )
    } else {
        (
            "SELECT id,account,number FROM gp_batches WHERE account=? ORDER BY started DESC LIMIT 1000",
            vec![text(&account)],
        )
    };
    let batch_options: Vec<Value> = rows(db, batch_sql, batch_args)?
        .iter()
        .map(|row| {
            json!({
                "id": row.get("id").cloned().unwrap_or(Value::Null),
                "accountId": row.get("account").cloned().unwrap_or(Value::Null),
                "number": row.get("number").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    let mut result = Object::new();
    set(&mut result, "version", json!(2));
    set(&mut result, "recordedSince", recorded_since.to_json());
    set(
        &mut result,
        "collectionGaps",
        meta.get("gaps").cloned().unwrap_or(json!(0)),
    );
    set(&mut result, "from", lower.to_json());
    set(&mut result, "to", upper.to_json());
    let mut page_info = Object::new();
    set(&mut page_info, "number", json!(page));
    set(&mut page_info, "size", json!(size));
    set(&mut page_info, "total", json!(count));
    set(&mut page_info, "pages", json!(pages));
    set(&mut result, "page", Value::Object(page_info));
    set(&mut result, "summary", Value::Object(summary));
    set(&mut result, "gateways", Value::Array(gateway_rows));
    let cooling = if view == "summary" {
        cooling_analysis(db, &account, &gateway, lower, upper)?
    } else {
        Value::Null
    };
    set(&mut result, "coolingAnalysis", cooling);
    let (records_out, batches_out, recoveries_out) = match view.as_str() {
        "records" => (selected, Vec::new(), Vec::new()),
        "summary" => (Vec::new(), selected, Vec::new()),
        _ => (Vec::new(), Vec::new(), selected),
    };
    set(&mut result, "records", Value::Array(records_out));
    set(&mut result, "batches", Value::Array(batches_out));
    set(&mut result, "recoveries", Value::Array(recoveries_out));
    let mut options = Object::new();
    set(&mut options, "accounts", Value::Array(accounts));
    set(&mut options, "gateways", Value::Array(gateway_options));
    set(&mut options, "batches", Value::Array(batch_options));
    set(&mut result, "options", Value::Object(options));
    Ok(Value::Object(result))
}
