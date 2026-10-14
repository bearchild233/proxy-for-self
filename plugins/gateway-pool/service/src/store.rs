//! 网关池持久化入口，对应 `gateway_pool.py` 中的存储、设置、队列与管理读取部分。
//! 每次操作打开独立连接：写入用 `BEGIN IMMEDIATE`，报表读取用只读快照，与 Python 的事务语义一致。

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior, types::Value as SqlValue};
use serde_json::{Value, json};

use crate::{
    error::Failure,
    history, history_query,
    ids::{IdSource, random_ids},
    num::Num,
    policy, queue, settings,
    util::{
        Object, decode_object, decode_value, exec, merged, one, python_int, rows, set, sql_of,
        sql_of_num, str_field, text, truthy, where_clause,
    },
};

/// 数据库文件名：`$CPR_PLUGIN_DATA/state.sqlite3`。
pub const DB_FILE: &str = "state.sqlite3";

/// 时钟返回 Unix 秒（浮点），测试时可以注入。
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64())
    })
}

/// 可手动推进的时钟，供测试与一致性对照使用。
#[derive(Clone)]
pub struct ManualClock(Arc<Mutex<f64>>);

impl ManualClock {
    pub fn new(value: f64) -> Self {
        Self(Arc::new(Mutex::new(value)))
    }

    pub fn set(&self, value: f64) {
        *self.0.lock().expect("clock lock") = value;
    }

    pub fn advance(&self, seconds: f64) {
        *self.0.lock().expect("clock lock") += seconds;
    }

    pub fn clock(&self) -> Clock {
        let inner = self.0.clone();
        Arc::new(move || *inner.lock().expect("clock lock"))
    }
}

/// 网关池归属：`ownedGroups` 来自 `groupIds`；动态分组开启时 `watchedGroups` 为正常分组。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ownership {
    pub owned_groups: Vec<String>,
    pub watched_groups: Vec<String>,
}

fn ownership_of(settings: &Object) -> Ownership {
    let owned_groups = settings
        .get("groupIds")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let dynamic = matches!(settings.get("dynamicGroups"), Some(Value::Bool(true)));
    let watched_groups = match str_field(settings, "normalGroupId") {
        Some(normal) if dynamic && !normal.is_empty() => vec![normal.to_owned()],
        _ => Vec::new(),
    };
    Ownership {
        owned_groups,
        watched_groups,
    }
}

/// 与 Python `GatewayPool` 的 `__init__` 中 executescript 相同的基础表结构。
const SCHEMA: &str = "
        CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY CHECK(id=1), data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS pairs (account TEXT NOT NULL, gateway TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY(account,gateway));
        CREATE TABLE IF NOT EXISTS leases (id TEXT PRIMARY KEY, account TEXT NOT NULL, gateway TEXT NOT NULL, generation TEXT NOT NULL, kind TEXT NOT NULL, expires REAL NOT NULL, sent INTEGER NOT NULL DEFAULT 0, data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS observations (id INTEGER PRIMARY KEY, at REAL NOT NULL, account TEXT NOT NULL, gateway TEXT NOT NULL, data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS switches (id INTEGER PRIMARY KEY, at REAL NOT NULL, account TEXT NOT NULL, data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS materials (account TEXT NOT NULL, generation TEXT NOT NULL, expires REAL NOT NULL, data TEXT NOT NULL, PRIMARY KEY(account,generation));
        CREATE TABLE IF NOT EXISTS catalogs (account TEXT PRIMARY KEY, at REAL NOT NULL, data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS model_tickets (account TEXT NOT NULL, generation TEXT NOT NULL, model TEXT NOT NULL, expires REAL NOT NULL, data TEXT NOT NULL, PRIMARY KEY(account,generation,model));
        CREATE INDEX IF NOT EXISTS observations_pair ON observations(account,gateway,at);
        CREATE TABLE IF NOT EXISTS group_events (id TEXT PRIMARY KEY, at REAL NOT NULL, account TEXT NOT NULL, direction TEXT NOT NULL, from_group TEXT NOT NULL, to_group TEXT NOT NULL, model TEXT, status TEXT NOT NULL, finished_at REAL);
        CREATE INDEX IF NOT EXISTS group_events_account ON group_events(account,direction,at);
        CREATE TABLE IF NOT EXISTS recovery_checks (account TEXT PRIMARY KEY, paused_until REAL NOT NULL DEFAULT 0, claimed_generation TEXT, claimed_at REAL, last_outcome TEXT, last_at REAL);
    ";

pub struct Store {
    path: PathBuf,
    pub(crate) clock: Clock,
    /// 租约、代次与分组事件标识的来源；测试与对照可以替换为确定的序列。
    pub(crate) ids: IdSource,
    /// 插件是否启用，默认启用。插件进程在运行就是启用（禁用由监督停止进程），生产代码从不置为 false；
    /// 保留开关只为与 Python 实现的一致性对照。置为 false 时旧在途请求仍可归还租约（`finish`），其余运行动作被拒绝。
    enabled: AtomicBool,
}

impl Store {
    /// 打开（必要时创建）`data_dir/state.sqlite3`，并完成与 Python 相同的表结构初始化。
    pub fn open(data_dir: &Path, clock: Clock) -> Result<Self, Failure> {
        Self::open_with_ids(data_dir, clock, random_ids())
    }

    /// 同 `open`，但指定标识来源。
    pub fn open_with_ids(data_dir: &Path, clock: Clock, ids: IdSource) -> Result<Self, Failure> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join(DB_FILE);
        {
            let connection = Connection::open(&path)?;
            connection.busy_timeout(Duration::from_secs(5))?;
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        let store = Self {
            path,
            clock,
            ids,
            enabled: AtomicBool::new(true),
        };
        let now = store.now_num();
        store.write(|db| {
            db.execute_batch(SCHEMA)?;
            history::install(db, now)?;
            queue::install(db)
        })?;
        set_file_mode(&store.path)?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection, Failure> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(connection)
    }

    /// 写事务：`BEGIN IMMEDIATE`；出错时事务丢弃即回滚。
    pub(crate) fn write<T>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = work(&transaction)?;
        transaction.commit()?;
        Ok(value)
    }

    /// 只读快照事务，报表读取不阻塞运行租约的写入。
    fn read<T>(&self, work: impl FnOnce(&Connection) -> Result<T, Failure>) -> Result<T, Failure> {
        let mut connection =
            Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let transaction = connection.transaction()?;
        let value = work(&transaction)?;
        transaction.commit()?;
        Ok(value)
    }

    pub(crate) fn now_num(&self) -> Num {
        Num::Float((self.clock)())
    }

    /// 插件是否启用（默认启用，生产代码从不改变）。
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// 仅测试与一致性对照使用；插件进程不调用。
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    /// 租约所属账号；租约不存在（已归还或已回收）时返回 None。
    /// 插件重启或热更新后接管未结束的租约时，用它找到归还租约所需的账号。
    pub fn lease_account(&self, lease: &str) -> Result<Option<String>, Failure> {
        self.read(|db| {
            Ok(one(
                db,
                "SELECT account FROM leases WHERE id=?",
                vec![text(lease)],
            )?
            .and_then(|row| str_field(&row, "account").map(str::to_owned)))
        })
    }

    /// 当前有效的归属（启动时用于声明插件描述）。
    pub fn ownership(&self) -> Result<Ownership, Failure> {
        self.read(|db| Ok(ownership_of(&effective_settings(db, true)?)))
    }

    /// `configure`：校验并保存设置，执行候选集合变化与限定网关退役的副作用。返回对外设置与新归属。
    pub fn configure(&self, data: &Object) -> Result<(Object, Ownership), Failure> {
        self.write(|db| {
            let previous = effective_settings(db, true)?;
            let settings = settings::prepare(&previous, data)?;
            if settings::candidates_changed(&previous, &settings) {
                history::record(db, |db| {
                    history::interrupt(db, self.now_num(), "configuration")
                })?;
                // 候选集合变更后下一轮按新目录重新开始，不等待已移除的网关。
                db.execute_batch("UPDATE gp_account_queue SET remaining=NULL")?;
            }
            let gateways: Vec<String> = settings
                .get("gateways")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            // 限定网关时，被移出的候选立即退役；已发送的租约仍可排空，未发送的旧预约会被 sent 拒绝。
            for row in rows(db, "SELECT data FROM pairs", vec![])? {
                let mut pair = decode_object(&string_of(&row, "data"))?;
                let gateway = string_of(&pair, "gateway");
                if !gateways.is_empty() && !gateways.contains(&gateway) && policy::is_serving(&pair)
                {
                    policy::retire(&mut pair, &settings, false, false);
                    policy::settle(&mut pair);
                    self.save(db, &pair)?;
                }
            }
            let ownership = ownership_of(&settings);
            exec(
                db,
                "INSERT OR REPLACE INTO settings VALUES (1,?)",
                vec![text(&serde_json::to_string(&settings)?)],
            )?;
            Ok((effective_settings(db, false)?, ownership))
        })
    }

    /// `update_queue`：队列排序或开关；停用账号时立即退役其组合。
    pub fn update_queue(&self, data: &Object) -> Result<Value, Failure> {
        self.write(|db| {
            let result = queue::update(db, data)?;
            if data.get("enabled") == Some(&Value::Bool(false)) {
                let settings = effective_settings(db, false)?;
                self.retire_account(db, &string_of(data, "accountId"), &settings)?;
            }
            Ok(result)
        })
    }

    pub(crate) fn retire_account(
        &self,
        db: &Connection,
        account: &str,
        settings: &Object,
    ) -> Result<(), Failure> {
        for row in rows(
            db,
            "SELECT data FROM pairs WHERE account=?",
            vec![text(account)],
        )? {
            let mut pair = decode_object(&string_of(&row, "data"))?;
            if policy::is_serving(&pair) {
                policy::retire(&mut pair, settings, false, false);
                policy::settle(&mut pair);
                self.save(db, &pair)?;
            }
        }
        Ok(())
    }

    /// 保存组合并同步历史；保存前后的状态变化决定是否从本轮剩余网关中移除。
    pub(crate) fn save(&self, db: &Connection, pair: &Object) -> Result<(), Failure> {
        let account = string_of(pair, "accountId");
        let gateway = string_of(pair, "gateway");
        let previous = stored_state(db, &account, &gateway)?;
        queue::retired(db, pair, &previous)?;
        let now = self.now_num();
        history::record(db, |db| history::sync(db, pair, now))?;
        exec(
            db,
            "INSERT OR REPLACE INTO pairs VALUES (?,?,?)",
            vec![
                text(&account),
                text(&gateway),
                text(&serde_json::to_string(pair)?),
            ],
        )?;
        Ok(())
    }

    /// `view`：页面读取的组合、观察、切换、目录、队列与分组事件。`limit` 与 `before` 与 Python 的参数语义一致。
    pub fn view(
        &self,
        account: &Value,
        gateway: &Value,
        limit: &Value,
        before: &Value,
    ) -> Result<Object, Failure> {
        let limit = python_int(limit)?.clamp(1, 500);
        let account = keep_truthy(account);
        let gateway = keep_truthy(gateway);
        let mut conditions: Vec<&str> = Vec::new();
        let mut args: Vec<SqlValue> = Vec::new();
        if let Some(value) = &account {
            conditions.push("account=?");
            args.push(sql_of(value));
        }
        if let Some(value) = &gateway {
            conditions.push("gateway=?");
            args.push(sql_of(value));
        }
        self.read(|db| {
            let mut settings = stored_settings(db)?.unwrap_or_default();
            let token = settings.remove("poolToken");
            set(&mut settings, "poolTokenConfigured", json!(truthy(token.as_ref())));

            let mut pairs = Vec::new();
            let pair_sql = format!("SELECT data FROM pairs{} ORDER BY account,gateway", where_clause(&conditions));
            for row in rows(db, &pair_sql, args.clone())? {
                let mut pair = decode_object(&string_of(&row, "data"))?;
                let account_id = string_of(&pair, "accountId");
                let gateway_id = string_of(&pair, "gateway");
                let summary = history_query::cooling_summary(db, &account_id, &gateway_id)?;
                set(&mut pair, "coolingSummary", summary);
                let models: Vec<Value> = if str_field(&pair, "state") == Some("ready") {
                    let generation = pair.get("generation").cloned().unwrap_or(Value::Null);
                    rows(
                        db,
                        "SELECT model FROM model_tickets WHERE account=? AND generation=? AND expires>? ORDER BY model",
                        vec![text(&account_id), sql_of(&generation), sql_of_num(self.now_num())],
                    )?
                    .iter()
                    .filter_map(|row| row.get("model").cloned())
                    .collect()
                } else {
                    Vec::new()
                };
                set(&mut pair, "ticketModels", Value::Array(models));
                pairs.push(Value::Object(pair));
            }

            let mut sample_conditions = conditions.clone();
            let mut sample_args = args.clone();
            if !before.is_null() {
                sample_conditions.push("id<?");
                sample_args.push(SqlValue::Integer(python_int(before)?));
            }
            sample_args.push(SqlValue::Integer(limit));
            let sample_sql = format!(
                "SELECT * FROM observations{} ORDER BY id DESC LIMIT ?",
                where_clause(&sample_conditions)
            );
            let mut observations = Vec::new();
            for row in rows(db, &sample_sql, sample_args)? {
                let data = decode_object(&string_of(&row, "data"))?;
                observations.push(Value::Object(merged(data, identity_columns(&row))));
            }

            let mut switch_conditions: Vec<&str> = Vec::new();
            let mut switch_args: Vec<SqlValue> = Vec::new();
            if let Some(value) = &account {
                switch_conditions.push("account=?");
                switch_args.push(sql_of(value));
            }
            if let Some(value) = &gateway {
                switch_conditions.push("(json_extract(data,'$.fromGateway')=? OR json_extract(data,'$.toGateway')=?)");
                switch_args.push(sql_of(value));
                switch_args.push(sql_of(value));
            }
            switch_args.push(SqlValue::Integer(limit));
            let switch_sql = format!(
                "SELECT * FROM switches{} ORDER BY id DESC LIMIT ?",
                where_clause(&switch_conditions)
            );
            let mut switches = Vec::new();
            for row in rows(db, &switch_sql, switch_args)? {
                let mut entry = decode_object(&string_of(&row, "data"))?;
                set(&mut entry, "at", row.get("at").cloned().unwrap_or(Value::Null));
                set(&mut entry, "accountId", row.get("account").cloned().unwrap_or(Value::Null));
                switches.push(Value::Object(entry));
            }

            let mut catalogs = Object::new();
            let (catalog_sql, catalog_args) = match &account {
                Some(value) => ("SELECT * FROM catalogs WHERE account=?", vec![sql_of(value)]),
                None => ("SELECT * FROM catalogs", Vec::new()),
            };
            for row in rows(db, catalog_sql, catalog_args)? {
                let mut entry = Object::new();
                set(&mut entry, "at", row.get("at").cloned().unwrap_or(Value::Null));
                set(&mut entry, "gateways", decode_value(&string_of(&row, "data"))?);
                set(&mut catalogs, &string_of(&row, "account"), Value::Object(entry));
            }

            let mut events = Vec::new();
            for row in rows(db, "SELECT * FROM group_events ORDER BY at DESC LIMIT 200", vec![])? {
                events.push(json!({
                    "id": column(&row, "id"),
                    "at": column(&row, "at"),
                    "accountId": column(&row, "account"),
                    "direction": column(&row, "direction"),
                    "fromGroup": column(&row, "from_group"),
                    "toGroup": column(&row, "to_group"),
                    "model": column(&row, "model"),
                    "status": column(&row, "status"),
                    "finishedAt": column(&row, "finished_at"),
                }));
            }

            let mut result = Object::new();
            set(&mut result, "settings", Value::Object(settings));
            set(&mut result, "pairs", Value::Array(pairs));
            set(&mut result, "observations", Value::Array(observations));
            set(&mut result, "switches", Value::Array(switches));
            set(&mut result, "catalogs", Value::Object(catalogs));
            set(&mut result, "accountQueue", queue::view(db)?);
            set(&mut result, "groupEvents", Value::Array(events));
            Ok(result)
        })
    }

    /// `read` 管理方法：页面读取（组合与观察）加上对外设置。
    pub fn read_admin(&self, data: &Object) -> Result<Value, Failure> {
        let mut view = self.view(
            data.get("accountId").unwrap_or(&Value::Null),
            data.get("gateway").unwrap_or(&Value::Null),
            data.get("limit").unwrap_or(&json!(100)),
            data.get("before").unwrap_or(&Value::Null),
        )?;
        let settings = self.read(|db| effective_settings(db, false))?;
        set(&mut view, "settings", Value::Object(settings));
        Ok(Value::Object(view))
    }

    /// `history.read`：窗口统计。先按保留规则关闭空闲分段（写事务），再做只读查询。
    pub fn statistics(&self, query: &Object) -> Result<Value, Failure> {
        let now = self.now_num();
        self.write(|db| history::record(db, |db| history::close_idle(db, now, None)))?;
        self.read(|db| history_query::read(db, now, query))
    }
}

#[cfg(unix)]
fn set_file_mode(path: &Path) -> Result<(), Failure> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path) -> Result<(), Failure> {
    Ok(())
}

/// 已保存设置与默认值合并；对外隐藏凭据，只暴露是否已配置。
pub(crate) fn effective_settings(db: &Connection, private: bool) -> Result<Object, Failure> {
    let mut settings = merged(policy::defaults(), stored_settings(db)?.unwrap_or_default());
    if !private {
        let token = settings.remove("poolToken");
        set(
            &mut settings,
            "poolTokenConfigured",
            json!(truthy(token.as_ref())),
        );
    }
    Ok(settings)
}

pub(crate) fn stored_settings(db: &Connection) -> Result<Option<Object>, Failure> {
    match one(db, "SELECT data FROM settings WHERE id=1", vec![])? {
        Some(row) => Ok(Some(decode_object(&string_of(&row, "data"))?)),
        None => Ok(None),
    }
}

/// 组合当前状态；不存在时为 unknown（与 Python `pair()` 的默认值一致）。
fn stored_state(db: &Connection, account: &str, gateway: &str) -> Result<String, Failure> {
    Ok(
        match one(
            db,
            "SELECT data FROM pairs WHERE account=? AND gateway=?",
            vec![text(account), text(gateway)],
        )? {
            Some(row) => string_of(&decode_object(&string_of(&row, "data"))?, "state"),
            None => "unknown".to_owned(),
        },
    )
}

fn keep_truthy(value: &Value) -> Option<Value> {
    truthy(Some(value)).then(|| value.clone())
}

/// 观察行附带的标识列（与 Python 中 `{k: r[k] for k in ('id','at','account','gateway')}` 相同）。
fn identity_columns(row: &Object) -> Object {
    let mut columns = Object::new();
    for key in ["id", "at", "account", "gateway"] {
        set(
            &mut columns,
            key,
            row.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    columns
}

fn column(row: &Object, key: &str) -> Value {
    row.get(key).cloned().unwrap_or(Value::Null)
}

fn string_of(row: &Object, key: &str) -> String {
    str_field(row, key).unwrap_or_default().to_owned()
}
