use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use plugin_sdk::{
    RpcResult,
    protocol::{INTERNAL, RpcError},
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{
    model::{Account, AccountState, Outcome, RunSummary, Settings, TaskItem},
    schedule,
};

const CATCH_UP_SECONDS: i64 = 6 * 60 * 60;
const LEASE_SECONDS: i64 = 60;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn new_id() -> String {
    format!(
        "{:x}-{:x}-{:x}",
        Utc::now().timestamp_micros(),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn internal(_: impl std::fmt::Display) -> RpcError {
    RpcError::new(INTERNAL, "warmup storage unavailable")
}

fn decode<T: DeserializeOwned>(value: String) -> RpcResult<T> {
    serde_json::from_str(&value).map_err(internal)
}

fn encoded(value: &impl serde::Serialize) -> RpcResult<String> {
    serde_json::to_string(value).map_err(internal)
}

pub struct Store {
    connection: Mutex<Connection>,
}

impl Store {
    pub fn open(data: &Path) -> RpcResult<Self> {
        std::fs::create_dir_all(data).map_err(internal)?;
        let path = data.join("state.sqlite3");
        let connection = Connection::open(&path).map_err(internal)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(internal)?;
        }
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(internal)?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(internal)?;
        // 发送标志和事件幂等键需要断电后也保留，使用 FULL 而非可丢最后提交的 NORMAL。
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(internal)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings(id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL,data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY,submission TEXT UNIQUE NOT NULL,source TEXT NOT NULL,created INTEGER NOT NULL,cancelled INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY,account TEXT NOT NULL,status TEXT NOT NULL,due INTEGER NOT NULL,expires INTEGER,owner TEXT,lease INTEGER,finished INTEGER,data TEXT NOT NULL);
             CREATE UNIQUE INDEX IF NOT EXISTS active_account ON tasks(account) WHERE status IN ('queued','running');
             CREATE INDEX IF NOT EXISTS task_due ON tasks(status,due);
             CREATE INDEX IF NOT EXISTS task_automatic_due ON tasks(due) WHERE status='queued' AND json_extract(data,'$.source') IN ('daily','reset');
             CREATE INDEX IF NOT EXISTS task_account_finished ON tasks(account,finished DESC,id DESC);
             CREATE INDEX IF NOT EXISTS task_account_status_finished ON tasks(account,status,finished DESC,id DESC);
             CREATE INDEX IF NOT EXISTS task_confirmed_reset ON tasks(account,json_extract(data,'$.resetAt'),finished DESC,id DESC) WHERE status='accepted' AND json_extract(data,'$.timerConfirmed')=1;
             CREATE TABLE IF NOT EXISTS run_tasks(run_id TEXT NOT NULL,task_id TEXT NOT NULL,PRIMARY KEY(run_id,task_id));
             CREATE INDEX IF NOT EXISTS run_source_created ON runs(source,created DESC,id DESC);
             CREATE INDEX IF NOT EXISTS run_created ON runs(created DESC,id DESC);
             CREATE TABLE IF NOT EXISTS occurrences(key TEXT PRIMARY KEY,task_id TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS reset_observations(account TEXT NOT NULL,reset_at INTEGER NOT NULL,observed INTEGER NOT NULL,PRIMARY KEY(account,reset_at));
             CREATE INDEX IF NOT EXISTS reset_observation_latest ON reset_observations(account,observed DESC,reset_at DESC);
             CREATE TABLE IF NOT EXISTS dispatch_cursor(id INTEGER PRIMARY KEY CHECK(id=1),next_at INTEGER NOT NULL);
             INSERT OR IGNORE INTO dispatch_cursor VALUES(1,0);"
        ).map_err(internal)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO settings VALUES(1,0,?)",
                [encoded(&Settings::default())?],
            )
            .map_err(internal)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn write<T>(&self, action: impl FnOnce(&Transaction<'_>) -> RpcResult<T>) -> RpcResult<T> {
        let mut connection = self.connection.lock().map_err(internal)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(internal)?;
        let result = action(&transaction)?;
        transaction.commit().map_err(internal)?;
        Ok(result)
    }

    pub fn settings(&self) -> RpcResult<(u64, Settings)> {
        let connection = self.connection.lock().map_err(internal)?;
        let (revision, data): (i64, String) = connection
            .query_row("SELECT revision,data FROM settings WHERE id=1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(internal)?;
        Ok((u64::try_from(revision).map_err(internal)?, decode(data)?))
    }

    pub fn configure(&self, revision: u64, settings: &Settings) -> RpcResult<()> {
        schedule::validate(settings)?;
        let revision =
            i64::try_from(revision).map_err(|_| schedule::invalid("invalid revision"))?;
        let data = encoded(settings)?;
        self.write(|transaction| {
            let count = transaction
                .execute(
                    "UPDATE settings SET revision=revision+1,data=? WHERE id=1 AND revision=?",
                    params![data, revision],
                )
                .map_err(internal)?;
            if count != 1 {
                return Err(RpcError::new("conflict", "revision_conflict"));
            }
            Ok(())
        })
    }

    pub fn existing_submission(&self, submission: &str) -> RpcResult<Option<RunSummary>> {
        let connection = self.connection.lock().map_err(internal)?;
        let id: Option<String> = connection
            .query_row(
                "SELECT id FROM runs WHERE submission=?",
                [submission],
                |row| row.get(0),
            )
            .optional()
            .map_err(internal)?;
        id.map(|id| summary(&connection, &id)).transpose()
    }

    /// 批次与目标快照在同一事务写入。活动任务由多个批次引用，始终只有一次发送。
    pub fn enqueue_manual(
        &self,
        submission: &str,
        accounts: &[Account],
        settings: &Settings,
        now: DateTime<Utc>,
    ) -> RpcResult<RunSummary> {
        self.write(|transaction| {
            let existing: Option<String> = transaction
                .query_row(
                    "SELECT id FROM runs WHERE submission=?",
                    [submission],
                    |row| row.get(0),
                )
                .optional()
                .map_err(internal)?;
            if let Some(id) = existing {
                return summary(transaction, &id);
            }
            let run = new_run(transaction, submission, "manual", now)?;
            let mut seen_accounts = HashSet::new();
            for account in accounts
                .iter()
                .filter(|account| seen_accounts.insert(account.id.as_str()))
            {
                let key = format!("manual:{submission}:{}", account.id);
                let due = now
                    + chrono::Duration::seconds(i64::from(schedule::jitter(
                        &key,
                        settings.jitter_min_seconds,
                        settings.jitter_max_seconds,
                    )));
                insert_task(
                    transaction,
                    &run,
                    &key,
                    account,
                    settings,
                    "manual",
                    now,
                    due,
                    None,
                    false,
                    now,
                )?;
            }
            summary(transaction, &run)
        })
    }

    /// 只持久化内核真实观测的五小时窗口；绝不从最近发送时间推算 reset。
    pub fn observe(&self, accounts: &[Account]) -> RpcResult<()> {
        self.write(|transaction| {
            for account in accounts {
                if let Some(window) = &account.short_term
                    && let Some(reset) = window.observed_reset()
                    && let Some(observed) = window.observed_at
                {
                    transaction
                        .execute(
                            "INSERT INTO reset_observations VALUES(?,?,?) ON CONFLICT(account,reset_at) DO UPDATE SET observed=max(observed,excluded.observed)",
                            params![
                                account.id,
                                reset.timestamp_millis(),
                                observed.timestamp_millis()
                            ],
                        )
                        .map_err(internal)?;
                }
            }
            Ok(())
        })
    }

    pub fn schedule(
        &self,
        accounts: &[Account],
        settings: &Settings,
        now: DateTime<Utc>,
    ) -> RpcResult<()> {
        if !settings.enabled {
            return Ok(());
        }
        self.write(|transaction| {
            for account in accounts.iter().filter(|account| settings.automatic(&account.id, &account.groups)) {
                if settings.daily_enabled {
                    let at = settings.account_override(&account.id).and_then(|item| item.daily_time.as_deref()).unwrap_or(&settings.daily_time);
                    if let Some((nominal, _)) = schedule::daily_points(now, &settings.timezone, at) {
                        let timezone = settings.timezone.parse::<chrono_tz::Tz>().map_err(internal)?;
                        let key = format!("daily:{}:{}", account.id, nominal.with_timezone(&timezone).date_naive());
                        schedule_task(transaction, &key, account, settings, "daily", nominal, now)?;
                    }
                }
                if settings.reset_enabled {
                    let delay = settings.account_override(&account.id).and_then(|item| item.reset_delay_seconds).unwrap_or(settings.reset_delay_seconds);
                    let reset: Option<i64> = transaction.query_row("SELECT max(reset_at) FROM reset_observations WHERE account=? AND reset_at<=?", params![account.id, (now - chrono::Duration::seconds(i64::from(delay))).timestamp_millis()], |row| row.get(0)).map_err(internal)?;
                    if let Some(reset) = reset {
                        let reset = DateTime::from_timestamp_millis(reset).ok_or_else(|| internal("reset"))?;
                        let nominal = reset + chrono::Duration::seconds(i64::from(delay));
                        if nominal <= now {
                            let key = format!("reset:{}:{}", account.id, reset.to_rfc3339());
                            schedule_task(transaction, &key, account, settings, "reset", nominal, now)?;
                        }
                    }
                }
            }
            Ok(())
        })
    }

    /// 新版本不抢旧版本在途任务。租约超时只转 unknown，永不重新放回 queued。
    pub fn recover(&self, now: DateTime<Utc>) -> RpcResult<()> {
        self.write(|transaction| recover(transaction, now))
    }

    pub fn claim(&self, owner: &str, now: DateTime<Utc>) -> RpcResult<Option<TaskItem>> {
        self.claim_with_accounts(owner, now, None)
    }

    pub fn has_due_automatic(&self, now: DateTime<Utc>) -> RpcResult<bool> {
        let connection = self.connection.lock().map_err(internal)?;
        connection.query_row(
            "SELECT next_at<=? AND NOT EXISTS(SELECT 1 FROM tasks WHERE status='running') AND EXISTS(SELECT 1 FROM tasks t WHERE t.status='queued' AND t.due<=? AND json_extract(t.data,'$.source') IN ('daily','reset') AND NOT EXISTS(SELECT 1 FROM run_tasks rt JOIN runs r ON r.id=rt.run_id WHERE rt.task_id=t.id AND r.source='manual' AND r.cancelled=0)) FROM dispatch_cursor WHERE id=1",
            params![now.timestamp_millis(), now.timestamp_millis()], |row| row.get(0),
        ).map_err(internal)
    }

    /// Some 只能来自本轮强刷且声明支持分组的内核目录。None 不允许自动分组发送。
    pub fn claim_with_accounts(
        &self,
        owner: &str,
        now: DateTime<Utc>,
        accounts: Option<&[Account]>,
    ) -> RpcResult<Option<TaskItem>> {
        self.write(|transaction| {
            recover(transaction, now)?;
            let running: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE status='running')", [], |row| row.get(0)).map_err(internal)?;
            if running { return Ok(None); }
            let cursor: i64 = transaction.query_row("SELECT next_at FROM dispatch_cursor WHERE id=1", [], |row| row.get(0)).map_err(internal)?;
            if cursor > now.timestamp_millis() { return Ok(None); }
            let settings_data: String = transaction.query_row("SELECT data FROM settings WHERE id=1", [], |row| row.get(0)).map_err(internal)?;
            let settings: Settings = decode(settings_data)?;
            // 目录暂不可用时保留自动任务，不让其挡住后面的显式手动批次。
            let group_available = settings.target_mode != "groups" || accounts.is_some() || !settings.enabled;
            loop {
                let found: Option<(String, Option<i64>)> = transaction.query_row("SELECT t.data,t.expires FROM tasks t WHERE t.status='queued' AND t.due<=? AND (? OR json_extract(t.data,'$.source')='manual' OR EXISTS(SELECT 1 FROM run_tasks rt JOIN runs r ON r.id=rt.run_id WHERE rt.task_id=t.id AND r.source='manual' AND r.cancelled=0)) ORDER BY t.due,t.id LIMIT 1", params![now.timestamp_millis(), group_available], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(internal)?;
                let Some((data, expires)) = found else { return Ok(None); };
                let mut item: TaskItem = decode(data)?;
                let mut manual = item.source == "manual";
                if item.source != "manual" {
                    manual = transaction.query_row("SELECT EXISTS(SELECT 1 FROM run_tasks rt JOIN runs r ON r.id=rt.run_id WHERE rt.task_id=? AND r.source='manual' AND r.cancelled=0)", [&item.id], |row| row.get(0)).map_err(internal)?;
                    let trigger_enabled = if item.source == "daily" { settings.daily_enabled } else { settings.reset_enabled };
                    let groups = accounts.and_then(|accounts| accounts.iter().find(|account| account.id == item.account_id)).map(|account| account.groups.as_slice()).unwrap_or_default();
                    if !manual && (!settings.automatic(&item.account_id, groups) || !trigger_enabled) {
                        item.status = "skipped".into();
                        item.reason_code = Some(if settings.enabled && trigger_enabled && settings.target_mode == "groups" && settings.account_override(&item.account_id).is_none_or(|item| item.enabled) { "group_membership_changed" } else { "automation_disabled" }.into());
                        item.finished_at = Some(now);
                        save_task(transaction, &item)?;
                        continue;
                    }
                }
                // 显式手动引用仍执行原参数，不能被旧自动任务的补跑期限吞掉。
                if !manual && expires.is_some_and(|expires| expires < now.timestamp_millis()) {
                    item.status = "skipped".into();
                    item.reason_code = Some("missed_schedule".into());
                    item.finished_at = Some(now);
                    save_task(transaction, &item)?;
                    continue;
                }
                item.status = "running".into();
                item.started_at = Some(now);
                transaction.execute("UPDATE tasks SET status='running',owner=?,lease=?,data=? WHERE id=? AND status='queued'", params![owner, (now + chrono::Duration::seconds(LEASE_SECONDS)).timestamp_millis(), encoded(&item)?, item.id]).map_err(internal)?;
                return Ok(Some(item));
            }
        })
    }

    pub fn finish(
        &self,
        id: &str,
        owner: &str,
        outcome: &Outcome,
        now: DateTime<Utc>,
    ) -> RpcResult<()> {
        self.write(|transaction| {
            let data: Option<String> = transaction
                .query_row(
                    "SELECT data FROM tasks WHERE id=? AND status='running' AND owner=?",
                    params![id, owner],
                    |row| row.get(0),
                )
                .optional()
                .map_err(internal)?;
            let Some(data) = data else {
                return Ok(());
            };
            let mut item: TaskItem = decode(data)?;
            let accepted_without_send =
                outcome.outcome == "accepted" && outcome.send_state != "sent";
            item.status = if accepted_without_send {
                "unknown".into()
            } else {
                match outcome.outcome.as_str() {
                    "accepted" | "skipped" | "failed" | "unknown" => outcome.outcome.clone(),
                    _ => "unknown".into(),
                }
            };
            item.reason_code = if accepted_without_send {
                Some("accepted_without_sent_result".into())
            } else {
                outcome.reason_code.clone()
            };
            item.http_status = outcome.status;
            item.duration_ms = Some(outcome.duration_ms);
            item.finished_at = Some(now);
            item.reset_at = outcome
                .short_term
                .as_ref()
                .and_then(|window| window.observed_reset_at(now))
                .filter(|reset| *reset > now);
            item.timer_confirmed = item.status == "accepted" && item.reset_at.is_some();
            if let Some(reset) = item.reset_at
                && let Some(observed) = outcome
                    .short_term
                    .as_ref()
                    .and_then(|window| window.observed_at)
            {
                transaction
                    .execute(
                        "INSERT INTO reset_observations VALUES(?,?,?) ON CONFLICT(account,reset_at) DO UPDATE SET observed=max(observed,excluded.observed)",
                        params![
                            item.account_id,
                            reset.timestamp_millis(),
                            observed.timestamp_millis()
                        ],
                    )
                    .map_err(internal)?;
            }
            save_task(transaction, &item)?;
            advance_dispatch(transaction, &item, now)
        })
    }

    pub fn cancel(&self, id: &str, now: DateTime<Utc>) -> RpcResult<RunSummary> {
        self.write(|transaction| {
            let _ = summary(transaction, id)?;
            transaction.execute("UPDATE runs SET cancelled=1 WHERE id=?", [id]).map_err(internal)?;
            // 活动任务可能被别的批次合并引用；只有没有其它活跃批次时才能取消。
            let tasks = query_tasks(transaction,
                "SELECT t.data FROM tasks t JOIN run_tasks rt ON rt.task_id=t.id WHERE rt.run_id=? AND t.status='queued' AND NOT EXISTS(SELECT 1 FROM run_tasks other JOIN runs r ON r.id=other.run_id WHERE other.task_id=t.id AND other.run_id<>rt.run_id AND r.cancelled=0)", [id])?;
            for mut item in tasks {
                item.status = "cancelled".into();
                item.reason_code = Some("cancelled".into());
                item.finished_at = Some(now);
                save_task(transaction, &item)?;
            }
            summary(transaction, id)
        })
    }

    pub fn history(&self, limit: u32, run_id: Option<&str>) -> RpcResult<Value> {
        self.history_filtered(limit, run_id, None)
    }

    pub fn history_filtered(
        &self,
        limit: u32,
        run_id: Option<&str>,
        source: Option<&str>,
    ) -> RpcResult<Value> {
        let connection = self.connection.lock().map_err(internal)?;
        if source.is_some_and(|source| !matches!(source, "automatic" | "manual")) {
            return Err(schedule::invalid("invalid history source"));
        }
        // 先过滤来源再 LIMIT；指定批次时也必须匹配，避免自动化页混入手动批次。
        let source_filter = match source {
            Some("automatic") => "source IN ('daily','reset')",
            Some("manual") => "source='manual'",
            _ => "1",
        };
        let id_filter = if run_id.is_some() { "id=?1" } else { "1" };
        let sql = format!(
            "SELECT id FROM runs WHERE {id_filter} AND {source_filter} ORDER BY created DESC,id DESC LIMIT ?2"
        );
        let mut statement = connection.prepare(&sql).map_err(internal)?;
        let run_ids: Vec<String> = statement
            .query_map(params![run_id, limit], |row| row.get(0))
            .map_err(internal)?
            .collect::<Result<_, _>>()
            .map_err(internal)?;
        if run_id.is_some() && run_ids.is_empty() {
            return Err(schedule::invalid("run_not_found"));
        }
        let mut runs = Vec::new();
        let mut items: Vec<TaskItem> = Vec::new();
        for id in run_ids {
            runs.push(summary(&connection, &id)?);
            if run_id.is_some() {
                items = query_tasks(
                    &connection,
                    "SELECT t.data FROM tasks t JOIN run_tasks rt ON rt.task_id=t.id WHERE rt.run_id=? ORDER BY t.due,t.id",
                    [id],
                )?;
            }
        }
        Ok(json!({"runs":runs,"items":items}))
    }

    /// 账号页仅查询索引命中的摘要，不读取任务提示词或加载全部历史。
    pub fn account_states(
        &self,
        now: DateTime<Utc>,
        accounts: &[Account],
    ) -> RpcResult<BTreeMap<String, AccountState>> {
        let connection = self.connection.lock().map_err(internal)?;
        let mut states = BTreeMap::new();
        for account in accounts {
            let current_window = current_window(&connection, account, now)?;
            let state = if let Some(active) = task_state(
                &connection,
                "account=? AND status IN ('queued','running') LIMIT 1",
                [&account.id],
            )? {
                active
            } else if !account.supported {
                AccountState {
                    status: "unsupported".into(),
                    task_id: None,
                    updated_at: None,
                    reset_at: None,
                    reason_code: account
                        .reason_code
                        .clone()
                        .or_else(|| Some("unsupported".into())),
                }
            } else if let Some((observed, reset)) = current_window {
                let confirmed = task_state(
                    &connection,
                    "account=? AND status='accepted' AND json_extract(data,'$.timerConfirmed')=1 AND json_extract(data,'$.resetAt')=? ORDER BY finished DESC,id DESC LIMIT 1",
                    params![
                        account.id,
                        reset.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
                    ],
                )?;
                // 有真实业务窗口也表示本窗口已开始；不能据此把 unknown 的手动请求报成功。
                let mut state = confirmed.unwrap_or(AccountState {
                    status: "warmed".into(),
                    task_id: None,
                    updated_at: Some(observed),
                    reset_at: Some(reset),
                    reason_code: None,
                });
                state.status = "warmed".into();
                state.reset_at = Some(reset);
                state
            } else {
                let last = task_state(
                    &connection,
                    "account=? AND finished IS NOT NULL ORDER BY finished DESC,id DESC LIMIT 1",
                    [&account.id],
                )?;
                match last {
                    Some(mut state) => {
                        state.status = match state.status.as_str() {
                            "accepted" if state.reset_at.is_some_and(|reset| reset <= now) => {
                                "pending"
                            }
                            "accepted" => "unconfirmed",
                            "unknown" => "unknown",
                            "failed" => "failed",
                            "skipped" | "cancelled" => "skipped",
                            _ => "pending",
                        }
                        .into();
                        // 过期记录可用于定位最后一次任务，但不能暴露成当前计时。
                        state.reset_at = None;
                        state
                    }
                    None => AccountState {
                        status: "pending".into(),
                        task_id: None,
                        updated_at: None,
                        reset_at: None,
                        reason_code: None,
                    },
                }
            };
            states.insert(account.id.clone(), state);
        }
        Ok(states)
    }

    pub fn runtime(
        &self,
        now: DateTime<Utc>,
        accounts: &[Account],
        settings: &Settings,
    ) -> RpcResult<Value> {
        let connection = self.connection.lock().map_err(internal)?;
        let next: Option<i64> = connection
            .query_row(
                "SELECT min(due) FROM tasks WHERE status='queued'",
                [],
                |row| row.get(0),
            )
            .map_err(internal)?;
        let cursor: i64 = connection
            .query_row(
                "SELECT next_at FROM dispatch_cursor WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(internal)?;
        let mut next = next.and_then(DateTime::<Utc>::from_timestamp_millis);
        // 排队快照保存请求参数，自动参与范围仍按当前配置与最新分组目录判断。
        let mut statement = connection.prepare(
            "SELECT account,due,json_extract(data,'$.source') FROM tasks WHERE status='queued' AND json_extract(data,'$.source') IN ('daily','reset') ORDER BY due,id",
        ).map_err(internal)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(internal)?;
        let mut next_automatic = None;
        for row in rows {
            let (id, due, source) = row.map_err(internal)?;
            let groups = accounts
                .iter()
                .find(|account| account.id == id)
                .map(|account| account.groups.as_slice())
                .unwrap_or_default();
            let trigger_enabled = if source == "daily" {
                settings.daily_enabled
            } else {
                settings.reset_enabled
            };
            if settings.automatic(&id, groups) && trigger_enabled {
                next_automatic = Some(due);
                break;
            }
        }
        let mut next_automatic = next_automatic.and_then(DateTime::<Utc>::from_timestamp_millis);
        if settings.enabled {
            for account in accounts
                .iter()
                .filter(|account| settings.automatic(&account.id, &account.groups))
            {
                if settings.daily_enabled {
                    let at = settings
                        .account_override(&account.id)
                        .and_then(|item| item.daily_time.as_deref())
                        .unwrap_or(&settings.daily_time);
                    if let Some((_, daily)) = schedule::daily_points(now, &settings.timezone, at) {
                        next = Some(next.map_or(daily, |current| current.min(daily)));
                        next_automatic =
                            Some(next_automatic.map_or(daily, |current| current.min(daily)));
                    }
                }
                if settings.reset_enabled {
                    let delay = settings
                        .account_override(&account.id)
                        .and_then(|item| item.reset_delay_seconds)
                        .unwrap_or(settings.reset_delay_seconds);
                    let reset: Option<i64> = connection.query_row("SELECT min(reset_at) FROM reset_observations WHERE account=? AND reset_at>?", params![account.id, (now - chrono::Duration::seconds(i64::from(delay))).timestamp_millis()], |row| row.get(0)).map_err(internal)?;
                    if let Some(reset) = reset.and_then(DateTime::<Utc>::from_timestamp_millis) {
                        let reset = reset + chrono::Duration::seconds(i64::from(delay));
                        next = Some(next.map_or(reset, |current| current.min(reset)));
                        next_automatic =
                            Some(next_automatic.map_or(reset, |current| current.min(reset)));
                    }
                }
            }
        }
        let last: Option<String> = connection
            .query_row(
                "SELECT id FROM runs ORDER BY created DESC,id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(internal)?;
        let last = last.map(|id| summary(&connection, &id)).transpose()?;
        let next = next.map(|next| {
            DateTime::from_timestamp_millis(cursor).map_or(next, |cursor| next.max(cursor))
        });
        let next_automatic = next_automatic.map(|next| {
            DateTime::from_timestamp_millis(cursor).map_or(next, |cursor| next.max(cursor))
        });
        Ok(json!({"nextRunAt":next,"nextAutomaticRunAt":next_automatic,"lastRun":last}))
    }
}

fn new_run(
    transaction: &Transaction<'_>,
    submission: &str,
    source: &str,
    now: DateTime<Utc>,
) -> RpcResult<String> {
    let id = new_id();
    transaction
        .execute(
            "INSERT INTO runs(id,submission,source,created) VALUES(?,?,?,?)",
            params![id, submission, source, now.timestamp_millis()],
        )
        .map_err(internal)?;
    Ok(id)
}

fn schedule_task(
    transaction: &Transaction<'_>,
    key: &str,
    account: &Account,
    settings: &Settings,
    source: &str,
    nominal: DateTime<Utc>,
    now: DateTime<Utc>,
) -> RpcResult<()> {
    let exists: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM occurrences WHERE key=?)",
            [key],
            |row| row.get(0),
        )
        .map_err(internal)?;
    if exists {
        return Ok(());
    }
    let run = new_run(transaction, key, source, now)?;
    let due = nominal
        + chrono::Duration::seconds(i64::from(schedule::jitter(
            key,
            settings.jitter_min_seconds,
            settings.jitter_max_seconds,
        )));
    let expires = nominal + chrono::Duration::seconds(CATCH_UP_SECONDS);
    insert_task(
        transaction,
        &run,
        key,
        account,
        settings,
        source,
        nominal,
        due,
        Some(expires),
        expires < now,
        now,
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_task(
    transaction: &Transaction<'_>,
    run: &str,
    key: &str,
    account: &Account,
    settings: &Settings,
    source: &str,
    nominal: DateTime<Utc>,
    due: DateTime<Utc>,
    expires: Option<DateTime<Utc>>,
    missed: bool,
    now: DateTime<Utc>,
) -> RpcResult<()> {
    let active: Option<String> = if missed {
        None
    } else {
        transaction
            .query_row(
                "SELECT id FROM tasks WHERE account=? AND status IN ('queued','running')",
                [&account.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(internal)?
    };
    let id = if let Some(id) = active {
        id
    } else {
        let id = new_id();
        let recent: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE account=? AND status='accepted' AND finished>=?)", params![account.id, (now - chrono::Duration::minutes(5)).timestamp_millis()], |row| row.get(0)).map_err(internal)?;
        let reason = if missed {
            Some("missed_schedule")
        } else if source != "manual" && recent {
            Some("recently_accepted")
        } else if !account.supported {
            Some(account.reason_code.as_deref().unwrap_or("unsupported"))
        } else {
            None
        };
        let item = TaskItem {
            id: id.clone(),
            account_id: account.id.clone(),
            source: source.into(),
            model: settings.model(&account.id).into(),
            prompt: settings.prompt.clone(),
            reasoning_effort: settings.reasoning_effort(&account.id).into(),
            status: if reason.is_some() {
                "skipped"
            } else {
                "queued"
            }
            .into(),
            scheduled_at: nominal,
            due_at: due,
            started_at: None,
            finished_at: reason.map(|_| now),
            duration_ms: None,
            reason_code: reason.map(str::to_owned),
            http_status: None,
            reset_at: None,
            timer_confirmed: false,
        };
        transaction.execute("INSERT INTO tasks(id,account,status,due,expires,finished,data) VALUES(?,?,?,?,?,?,?)", params![id, account.id, item.status, due.timestamp_millis(), expires.map(|value| value.timestamp_millis()), item.finished_at.map(|value| value.timestamp_millis()), encoded(&item)?]).map_err(internal)?;
        id
    };
    transaction
        .execute("INSERT INTO run_tasks VALUES(?,?)", params![run, id])
        .map_err(internal)?;
    transaction
        .execute("INSERT INTO occurrences VALUES(?,?)", params![key, id])
        .map_err(internal)?;
    Ok(())
}

fn save_task(transaction: &Transaction<'_>, item: &TaskItem) -> RpcResult<()> {
    transaction
        .execute(
            "UPDATE tasks SET status=?,finished=?,data=? WHERE id=?",
            params![
                item.status,
                item.finished_at.map(|value| value.timestamp_millis()),
                encoded(item)?,
                item.id
            ],
        )
        .map_err(internal)?;
    Ok(())
}

fn recover(transaction: &Transaction<'_>, now: DateTime<Utc>) -> RpcResult<()> {
    let stale = query_tasks(
        transaction,
        "SELECT data FROM tasks WHERE status='running' AND lease<=?",
        [now.timestamp_millis()],
    )?;
    for mut item in stale {
        item.status = "unknown".into();
        item.reason_code = Some("executor_lease_expired".into());
        item.finished_at = Some(now);
        save_task(transaction, &item)?;
        advance_dispatch(transaction, &item, now)?;
    }
    Ok(())
}

fn advance_dispatch(
    transaction: &Transaction<'_>,
    item: &TaskItem,
    now: DateTime<Utc>,
) -> RpcResult<()> {
    // 使用入队时已冻结的抖动，不随重启/热更新抽样，也不让过期 due 的批次突发发送。
    let gap = (item.due_at - item.scheduled_at)
        .num_milliseconds()
        .clamp(0, 300000);
    transaction
        .execute(
            "UPDATE dispatch_cursor SET next_at=max(next_at,?) WHERE id=1",
            [now.timestamp_millis().saturating_add(gap)],
        )
        .map_err(internal)?;
    Ok(())
}

fn query_tasks<P: rusqlite::Params>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> RpcResult<Vec<TaskItem>> {
    let mut statement = connection.prepare(sql).map_err(internal)?;
    let rows = statement
        .query_map(parameters, |row| row.get::<_, String>(0))
        .map_err(internal)?;
    rows.map(|value| decode(value.map_err(internal)?)).collect()
}

fn task_state<P: rusqlite::Params>(
    connection: &Connection,
    condition: &str,
    parameters: P,
) -> RpcResult<Option<AccountState>> {
    let sql = format!(
        "SELECT json_object('status',status,'taskId',id,'updatedAt',coalesce(json_extract(data,'$.finishedAt'),json_extract(data,'$.startedAt'),json_extract(data,'$.scheduledAt')),'resetAt',CASE WHEN json_extract(data,'$.timerConfirmed')=1 THEN json_extract(data,'$.resetAt') ELSE NULL END,'reasonCode',json_extract(data,'$.reasonCode')) FROM tasks WHERE {condition}"
    );
    let data: Option<String> = connection
        .query_row(&sql, parameters, |row| row.get(0))
        .optional()
        .map_err(internal)?;
    data.map(decode).transpose()
}

fn current_window(
    connection: &Connection,
    account: &Account,
    now: DateTime<Utc>,
) -> RpcResult<Option<(DateTime<Utc>, DateTime<Utc>)>> {
    let directory = account
        .short_term
        .as_ref()
        .and_then(|window| Some((window.observed_at?, window.observed_reset_at(now)?)));
    let saved: Option<(i64, i64)> = connection.query_row(
        "SELECT observed,reset_at FROM reset_observations WHERE account=? AND observed<=? AND reset_at>observed ORDER BY observed DESC,reset_at DESC LIMIT 1",
        params![account.id, now.timestamp_millis()], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(internal)?;
    let saved = saved.and_then(|(observed, reset)| {
        Some((
            DateTime::from_timestamp_millis(observed)?,
            DateTime::from_timestamp_millis(reset)?,
        ))
    });
    // 以最近一次真实观测为准，包括它已到期的情况；不能从更老记录复活旧窗口。
    let latest = match (directory, saved) {
        (Some(directory), Some(saved)) => Some(if directory.0 >= saved.0 {
            directory
        } else {
            saved
        }),
        (Some(window), None) | (None, Some(window)) => Some(window),
        (None, None) => None,
    };
    Ok(latest.filter(|(_, reset)| *reset > now))
}

fn summary(connection: &Connection, id: &str) -> RpcResult<RunSummary> {
    let row: Option<(String, i64, bool)> = connection
        .query_row(
            "SELECT source,created,cancelled FROM runs WHERE id=?",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((source, created, cancelled)) = row else {
        return Err(schedule::invalid("run_not_found"));
    };
    let mut statement = connection.prepare("SELECT t.status,count(*),max(t.finished) FROM tasks t JOIN run_tasks rt ON rt.task_id=t.id WHERE rt.run_id=? GROUP BY t.status").map_err(internal)?;
    let rows = statement
        .query_map([id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .map_err(internal)?;
    let mut counts = BTreeMap::new();
    let mut finished_at: Option<i64> = None;
    for row in rows {
        let (status, count, last) = row.map_err(internal)?;
        counts.insert(status, count);
        finished_at = finished_at.max(last);
    }
    let count = |status: &str| counts.get(status).copied().unwrap_or(0);
    let pending = count("queued") + count("running");
    let finished = if pending == 0 {
        finished_at
            .and_then(DateTime::from_timestamp_millis)
            .or_else(|| DateTime::from_timestamp_millis(created))
    } else {
        None
    };
    Ok(RunSummary {
        id: id.into(),
        source,
        status: if pending == 0 && cancelled {
            "cancelled"
        } else if pending == 0 {
            "completed"
        } else if count("running") > 0 {
            "running"
        } else {
            "queued"
        }
        .into(),
        created_at: DateTime::from_timestamp_millis(created).ok_or_else(|| internal("created"))?,
        finished_at: finished,
        total: counts.values().copied().sum(),
        accepted: count("accepted"),
        failed: count("failed"),
        skipped: count("skipped") + count("cancelled"),
        unknown: count("unknown"),
        pending,
    })
}
